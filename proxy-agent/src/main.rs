#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{
    future::Future,
    net::Ipv4Addr,
    process::exit,
    sync::{atomic::AtomicU32, Arc, LazyLock},
    time::Duration,
};

use clap::Parser;
use dashmap::{DashMap, DashSet};
use protocol::{
    compose, PacketHbAgent, ReqAgentBuild, ReqAgentLogin, ReqNewConnectionAgent, RspAgentBuild,
    RspAgentLoginOk,
};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpListener, TcpStream},
};
use vnpkt::tokio_ext::{
    io::{AsyncReadExt, AsyncWriteExt},
    registry::{PacketProc, Registry, RegistryInit},
};
use vnsvrbase::{
    process::hook_terminate_signal,
    tokio_ext::tcp_link::{send_pkt, TcpLink},
};

static REGISTRY: LazyLock<Registry<Handler>> = LazyLock::new(Registry::new);

#[derive(Parser)]
struct Args {
    server: String,
}

fn main() {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();
    let (quit_tx, mut quit_rx) = tokio::sync::watch::channel(false);
    hook_terminate_signal(Some(move || {
        let _ = quit_tx.send(true);
    }));

    rt.block_on(async {
        let _ = main_loop(args).await;
        let _ = quit_rx.changed().await;
    });
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), 60010)).await?;
    let handle = tokio::runtime::Handle::current();
    let _ = TcpLink::attach(stream, &handle, &handle, async move |link: &mut TcpLink| {
        let _ = send_pkt!(link.handle(), ReqAgentLogin {});
        receiving(link, args).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink, args: Args) -> std::io::Result<()> {
    let mut handler = Handler {
        handle: link.handle().clone(),
        conns: Arc::new(DashMap::new()),
        listens: Arc::new(DashSet::new()),
        seed: Arc::new(AtomicU32::new(0)),
        args,
    };
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                link.handle().close();
                eprintln!("recv from server time out.");
                exit(-1);
            }
            res = async {
                let pid = link.read.read_compressed_u64().await?;
                let register = &*REGISTRY;

                if pid > u32::MAX as u64 {
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                } else if let Some(item) = register.query(pid as u32) {
                    let r = item.recv(&mut link.read).await?;
                    r.proc(&mut handler).await?;
                    Ok(())
                } else {
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                }
            } => {
                if let Err(e) = res {
                    eprintln!("error: {}", e);
                }
            }
        }
    }
}

struct Handler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    conns: Arc<DashMap<(u32, u32), TcpStream>>,
    listens: Arc<DashSet<u16>>,
    seed: Arc<AtomicU32>,
    args: Args,
}

impl RegistryInit for Handler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbAgent>();
        register.insert::<RspAgentLoginOk>();
        register.insert::<ReqAgentBuild>();
    }
}

impl PacketProc<PacketHbAgent> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<PacketHbAgent>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<RspAgentLoginOk> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspAgentLoginOk>) -> Self::Output<'_> {
        async {
            let handle = self.handle.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    let _ = send_pkt!(handle, PacketHbAgent {});
                }
            });
            Ok(())
        }
    }
}

impl PacketProc<ReqAgentBuild> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqAgentBuild>) -> Self::Output<'_> {
        async {
            let handle = self.handle.clone();
            let conns = self.conns.clone();
            let listens = self.listens.clone();
            let seed = self.seed.clone();

            tokio::spawn(async move {
                if listens.insert(pkt.port) {
                    match TcpListener::bind((Ipv4Addr::UNSPECIFIED, pkt.port)).await {
                        Ok(listener) => loop {
                            if let Ok((stream, _)) = listener.accept().await {
                                let _ = stream.set_nodelay(true);
                                let _ = stream.set_linger(None);
                                let sid = seed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                conns.insert((pkt.id, sid), stream);
                                // send to server
                                let _ = send_pkt!(
                                    handle,
                                    RspAgentBuild {
                                        id: pkt.id,
                                        sid,
                                        ok: true
                                    }
                                );
                            }
                        },
                        _ => {
                            let _ = send_pkt!(
                                handle,
                                RspAgentBuild {
                                    id: pkt.id,
                                    sid: 0,
                                    ok: false
                                }
                            );
                            listens.remove(&pkt.port);
                        }
                    }
                }
            });
            Ok(())
        }
    }
}

impl PacketProc<ReqNewConnectionAgent> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqNewConnectionAgent>) -> Self::Output<'_> {
        async move {
            if let Ok(mut remote) = TcpStream::connect((self.args.server.as_str(), 60011)).await {
                if remote
                    .write_compressed_u64(compose(pkt.sid, false))
                    .await
                    .is_ok()
                {
                    match self.conns.remove(&(pkt.id, pkt.sid)) {
                        Some((_, mut local)) => {
                            let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                        }
                        _ => {
                            // TODO
                        }
                    }
                }
            } else {
                // TODO
            }
            Ok(())
        }
    }
}
