#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{
    future::Future,
    process::exit,
    sync::{atomic::AtomicU32, LazyLock},
    time::Duration,
};

use clap::Parser;
use dashmap::DashMap;
use protocol::{
    compose, PacketHbClient, PacketInfoConnectFailed, ReqClientLogin, ReqNewConnectionClient,
    RspClientLoginFailed, RspClientLoginOk, RspNewConnFailedClient, RspServiceNotFound,
};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpStream},
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

static REGISTRY: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);
static LOCALS: LazyLock<DashMap<u32, JoinHandle<()>>> = LazyLock::new(|| DashMap::new());

#[derive(Parser)]
pub struct Args {
    local: String,
    agentid: u32,
    port: u16,
    server: String,
    server_main_port: u16,
    server_conn_port: u16,
}

fn main() {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .enable_io()
        .build()
        .unwrap();
    let (quit_tx, mut quit_rx) = tokio::sync::watch::channel(false);
    hook_terminate_signal(Some(move || {
        let _ = quit_tx.send(true);
    }));

    rt.block_on(async {
        if let Err(_) = main_loop(args).await {
            println!("connect server failed");
            exit(-1);
        }
        let _ = quit_rx.changed().await;
    });
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), args.server_main_port)).await?;
    println!("connect server success");
    let handle = tokio::runtime::Handle::current();
    let _ = TcpLink::attach(stream, &handle, &handle, async move |link: &mut TcpLink| {
        let _ = send_pkt!(
            link.handle(),
            ReqClientLogin {
                id: args.agentid,
                port: args.port
            }
        );
        receiving(link, args).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink, args: Args) -> std::io::Result<()> {
    let mut client = Client::new(link.handle().clone(), args);
    let cnt = AtomicU32::new(0);

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(20)) => {
                if cnt.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
                    println!("recv from server timeout 3 times, conn closed");
                    link.handle().close();
                    exit(-1);
                }
            }
            res = async {
                let pid = link.read.read_compressed_u64().await?;
                cnt.store(0, std::sync::atomic::Ordering::SeqCst);
                let register = &*REGISTRY;

                if pid > u32::MAX as u64 {
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                } else if let Some(item) = register.query(pid as u32) {
                    let r = item.recv(&mut link.read).await?;
                    r.proc(&mut client).await?;
                    Ok(())
                } else {
                    std::io::Result::<()>::Err(std::io::ErrorKind::InvalidData.into())
                }
            } => {
                if let Err(e) = res {
                    link.handle().close();
                    println!("error: {}", e);
                    exit(-1);
                }
            }
        }
    }
}

struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    args: Args,
    id: Option<u32>,
}

impl Client {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle, args: Args) -> Self {
        Self {
            handle,
            args,
            id: None,
        }
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<RspClientLoginFailed>();
        register.insert::<ReqNewConnectionClient>();
        register.insert::<RspServiceNotFound>();
        register.insert::<PacketInfoConnectFailed>();
        register.insert::<RspClientLoginOk>();
    }
}

impl PacketProc<PacketHbClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<PacketHbClient>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<RspClientLoginFailed> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientLoginFailed>) -> Self::Output<'_> {
        async {
            println!("client login failed, proxy build listener failed");
            self.handle.close();
            exit(-1);
        }
    }
}

impl PacketProc<ReqNewConnectionClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqNewConnectionClient>) -> Self::Output<'_> {
        let agent_id = self.args.agentid;
        async move {
            if let Ok(mut local) = TcpStream::connect(self.args.local.as_str()).await {
                if let Ok(mut remote) =
                    TcpStream::connect((self.args.server.as_str(), self.args.server_conn_port))
                        .await
                {
                    if async {
                        use tokio::io::AsyncWriteExt;
                        remote.write_u32(pkt.agent_id).await?;
                        remote.write_u32(pkt.id).await?;
                        remote.write_compressed_u64(compose(pkt.sid, true)).await?;
                        std::io::Result::Ok(())
                    }
                    .await
                    .is_ok()
                    {
                        let task = tokio::spawn(async move {
                            let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                        });
                        LOCALS.insert(pkt.sid, task);
                    } else {
                        println!("send pkt id to server failed");
                        let _ = send_pkt!(
                            self.handle,
                            RspNewConnFailedClient {
                                agent_id,
                                id: pkt.id,
                                sid: pkt.sid,
                            }
                        );
                    }
                } else {
                    println!("connect remote server failed");
                    let _ = send_pkt!(
                        self.handle,
                        RspNewConnFailedClient {
                            agent_id,
                            id: pkt.id,
                            sid: pkt.sid
                        }
                    );
                }
            } else {
                println!("connect target failed");
                let _ = send_pkt!(
                    self.handle,
                    RspNewConnFailedClient {
                        agent_id,
                        id: pkt.id,
                        sid: pkt.sid
                    }
                );
            }
            Ok(())
        }
    }
}

impl PacketProc<RspServiceNotFound> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspServiceNotFound>) -> Self::Output<'_> {
        async {
            println!("service is not found");
            self.handle.close();
            exit(-1);
        }
    }
}

impl PacketProc<PacketInfoConnectFailed> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketInfoConnectFailed>) -> Self::Output<'_> {
        async move {
            if pkt.agent_id == self.args.agentid && self.id.is_some_and(|v| v == pkt.cid) {
                println!("Info AgentConnectFailed");
                LOCALS.remove(&pkt.sid).map(|(_, v)| v.abort());
            }
            Ok(())
        }
    }
}

impl PacketProc<RspClientLoginOk> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspClientLoginOk>) -> Self::Output<'_> {
        async move {
            if self.args.agentid == pkt.agent_id {
                let handle = self.handle.clone();
                self.id = Some(pkt.id);
                tokio::spawn(async move {
                    loop {
                        let _ = send_pkt!(handle, PacketHbClient { id: pkt.id });
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                });
            } else {
                self.handle.close();
                exit(-1);
            }
            Ok(())
        }
    }
}
