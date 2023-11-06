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
use dashmap::DashMap;
use protocol::{
    compose, PacketHbAgent, PacketInfoClientClosed, ReqAgentBuild, ReqAgentLogin,
    ReqNewConnectionAgent, RspAgentBuild, RspAgentLoginOk, RspClientNotFound,
};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpListener, TcpStream},
    task::JoinHandle,
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
static PROXYS: LazyLock<DashMap<u32, (u16, JoinHandle<()>, Vec<JoinHandle<()>>)>> =
    LazyLock::new(DashMap::new);

type Conns = DashMap<u32, DashMap<u32, TcpStream>>;

#[derive(Parser)]
struct Args {
    server: String,
    server_main_port: u16,
    server_conn_port: u16,
}

fn main() {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
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
    let stream = TcpStream::connect((args.server.as_str(), args.server_main_port)).await?;
    println!("Connect Server Success.");
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
                    link.handle().close();
                    eprintln!("error: {}", e);
                    exit(-1);
                }
            }
        }
    }
}

struct Handler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    conns: Arc<Conns>,
    seed: Arc<AtomicU32>,
    args: Args,
}

impl RegistryInit for Handler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbAgent>();
        register.insert::<RspAgentLoginOk>();
        register.insert::<ReqAgentBuild>();
        register.insert::<RspClientNotFound>();
        register.insert::<ReqNewConnectionAgent>();
        register.insert::<PacketInfoClientClosed>();
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
                    match send_pkt!(handle, PacketHbAgent {}) {
                        Ok(_) => {}
                        Err(_) => {
                            // TODO
                        }
                    }
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
            let seed = self.seed.clone();
            let port = pkt.port;
            let cid = pkt.id;

            let proxy = tokio::spawn(async move {
                match TcpListener::bind((Ipv4Addr::UNSPECIFIED, pkt.port)).await {
                    Ok(listener) => {
                        loop {
                            if let Ok((stream, _)) = listener.accept().await {
                                let _ = stream.set_nodelay(true);
                                let _ = stream.set_linger(None);
                                let sid = seed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                                use dashmap::mapref::entry::Entry;
                                match conns.entry(pkt.id) {
                                    Entry::Vacant(e) => {
                                        let conns = DashMap::new();
                                        conns.insert(sid, stream);
                                        e.insert(conns);
                                    }
                                    Entry::Occupied(e) => {
                                        e.get().insert(sid, stream);
                                    }
                                }
                                println!("With Client.{}, Local Conn.{} Build.", pkt.id, sid);
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
                        }
                    }
                    _ => {
                        let _ = send_pkt!(
                            handle,
                            RspAgentBuild {
                                id: pkt.id,
                                sid: 0,
                                ok: false
                            }
                        );
                    }
                }
            });

            PROXYS.insert(cid, (port, proxy, Vec::new()));
            Ok(())
        }
    }
}

impl PacketProc<ReqNewConnectionAgent> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqNewConnectionAgent>) -> Self::Output<'_> {
        async move {
            if let Ok(mut remote) =
                TcpStream::connect((self.args.server.as_str(), self.args.server_conn_port)).await
            {
                if async {
                    use tokio::io::AsyncWriteExt;
                    remote.write_u32(pkt.id).await?;
                    remote.write_compressed_u64(compose(pkt.sid, false)).await?;
                    std::io::Result::Ok(())
                }
                .await
                .is_ok()
                {
                    if let Some(conns) = self.conns.get(&pkt.id) {
                        match conns.remove(&pkt.sid) {
                            Some((_, mut local)) => {
                                let task = tokio::spawn(async move {
                                    match tokio::io::copy_bidirectional(&mut local, &mut remote)
                                        .await
                                    {
                                        Ok(_) => println!("Local Conn Disconnected"),
                                        Err(e) => println!("Local Conn Error: {}", e),
                                    }
                                });

                                use dashmap::mapref::entry::Entry;
                                match PROXYS.entry(pkt.id) {
                                    Entry::Occupied(mut e) => {
                                        e.get_mut().2.push(task);
                                    }
                                    _ => {}
                                }
                            }
                            _ => {
                                // TODO
                            }
                        }
                    } else {
                        // TODO: Client Not Found
                    }
                }
            } else {
                // TODO
            }
            Ok(())
        }
    }
}

impl PacketProc<RspClientNotFound> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientNotFound>) -> Self::Output<'_> {
        async {
            // TODO
            println!("Client Not Found");
            Ok(())
        }
    }
}

impl PacketProc<PacketInfoClientClosed> for Handler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketInfoClientClosed>) -> Self::Output<'_> {
        async move {
            self.conns.remove(&pkt.id);
            if let Some((_, (_, listen, conns))) = PROXYS.remove(&pkt.id) {
                listen.abort();
                for v in conns {
                    v.abort();
                }
            }
            Ok(())
        }
    }
}
