#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{future::Future, process::exit, sync::LazyLock, time::Duration};

use clap::Parser;
use protocol::{
    compose, PacketHbClient, ReqClientLogin, ReqNewConnectionClient, RspClientLoginFailed,
    RspNewConnFailedClient,
};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpStream},
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

#[derive(Parser)]
pub struct Args {
    local: String,
    port: u16,
    server: String,
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
        let _ = main_loop(args).await;
        let _ = quit_rx.changed().await;
    });
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), 60010)).await?;
    println!("Connect Server Success.");
    let handle = tokio::runtime::Handle::current();
    let _ = TcpLink::attach(stream, &handle, &handle, async move |link: &mut TcpLink| {
        let _ = send_pkt!(link.handle(), ReqClientLogin { port: args.port });
        receiving(link, args).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink, args: Args) -> std::io::Result<()> {
    let mut client = Client::new(link.handle().clone(), args);
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
                    r.proc(&mut client).await?;
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

struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    args: Args,
}

impl Client {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle, args: Args) -> Self {
        Self { handle, args }
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<RspClientLoginFailed>();
    }
}

impl PacketProc<PacketHbClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbClient>) -> Self::Output<'_> {
        async {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}

impl PacketProc<RspClientLoginFailed> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientLoginFailed>) -> Self::Output<'_> {
        async {
            println!("Client Login Failed, No Active Proxy Server.");
            // TODO: Retry
            Ok(())
        }
    }
}

impl PacketProc<ReqNewConnectionClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqNewConnectionClient>) -> Self::Output<'_> {
        async move {
            if let Ok(mut local) = TcpStream::connect(self.args.local.as_str()).await {
                if let Ok(mut remote) = TcpStream::connect((self.args.server.as_str(), 60011)).await
                {
                    if remote
                        .write_compressed_u64(compose(pkt.sid, true))
                        .await
                        .is_ok()
                    {
                        let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                    } else {
                        println!("Send Pkt Id to Server Failed.");
                    }
                } else {
                    println!("Connect Remote Server Failed.");
                }
            } else {
                let _ = send_pkt!(self.handle, RspNewConnFailedClient { id: pkt.id });
            }
            Ok(())
        }
    }
}
