#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{future::Future, process::exit, sync::LazyLock, time::Duration};

use clap::Parser;
use protocol::{
    compose, PacketHbClient, ReqClientLogin, RspClientLoginFailed, RspNewConnFailedClient,
    RspNewConnectionClient,
};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpStream},
    sync::watch::{channel, Sender},
};
use vnpkt::tokio_ext::{
    io::{AsyncReadExt, AsyncWriteExt},
    registry::{PacketProc, Registry, RegistryInit},
};
use vnsvrbase::tokio_ext::tcp_link::{send_pkt, TcpLink};

static REGISTRY: LazyLock<Registry<Client>> = LazyLock::new(Registry::new);

#[derive(Parser)]
pub struct Args {
    target: String,
    port: u16,
    server: String,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .enable_io()
        .build()?;
    rt.block_on(main_loop(args))
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), 60010)).await?;
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
    sx: Sender<bool>,
}

impl Client {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle, args: Args) -> Self {
        let (sx, mut rx) = channel(false);
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = rx.changed() => { break; }
                    _ = async {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        let _ = send_pkt!(handle_clone, PacketHbClient {});
                    } => {}
                }
            }
        });
        Self { handle, args, sx }
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

    fn proc(&mut self, _: Box<PacketHbClient>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<RspClientLoginFailed> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientLoginFailed>) -> Self::Output<'_> {
        async {
            println!("Client Login Failed, No Active Proxy Server.");
            let _ = self.sx.send(true);
            Ok(())
        }
    }
}

impl PacketProc<RspNewConnectionClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspNewConnectionClient>) -> Self::Output<'_> {
        async move {
            if let Ok(mut local) = TcpStream::connect(self.args.target.as_str()).await {
                if let Ok(mut remote) = TcpStream::connect((self.args.server.as_str(), 60011)).await
                {
                    if remote
                        .write_compressed_u64(compose(pkt.id, true))
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
