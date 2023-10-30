#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{future::Future, process::exit, sync::LazyLock, time::Duration};

use clap::Parser;
use protocol::{PacketHbClient, ReqClientLogin, RspClientLoginFailed, RspClientLoginOk};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::{tcp::OwnedReadHalf, TcpStream},
    sync::watch::{channel, Sender},
};
use vnpkt::tokio_ext::{
    io::AsyncReadExt,
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
        receiving(link).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
    let mut client = Client::new(link.handle().clone());
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
    hb: tokio::task::JoinHandle<()>,
    sx: Sender<bool>,
}

impl Client {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        let (sx, mut rx) = channel(false);
        let handle_clone = handle.clone();
        let hb = tokio::spawn(async move {
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
        Self { handle, hb, sx }
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<RspClientLoginOk>();
        register.insert::<RspClientLoginFailed>();
    }
}

impl PacketProc<PacketHbClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<PacketHbClient>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<RspClientLoginOk> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientLoginOk>) -> Self::Output<'_> {
        async {
            println!("Client Login Finished, Proxy Server is listening.");
            Ok(())
        }
    }
}

impl PacketProc<RspClientLoginFailed> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<RspClientLoginFailed>) -> Self::Output<'_> {
        async {
            println!("Client Login Failed, No Active Proxy Server.");
            Ok(())
        }
    }
}
