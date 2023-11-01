#![feature(async_closure)]
#![feature(lazy_cell)]
#![feature(impl_trait_in_assoc_type)]

use std::{future::Future, process::exit, sync::LazyLock, time::Duration};

use clap::Parser;
use protocol::{PacketHbAgent, ReqAgentLogin, RspAgentLoginOk};
use tokio::{
    io::BufReader,
    net::{tcp::OwnedReadHalf, TcpStream},
};
use vnpkt::tokio_ext::{
    io::AsyncReadExt,
    registry::{PacketProc, Registry, RegistryInit},
};
use vnsvrbase::tokio_ext::tcp_link::{send_pkt, TcpLink};

static REGISTRY: LazyLock<Registry<Handler>> = LazyLock::new(Registry::new);

#[derive(Parser)]
struct Args {
    server: String,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    rt.block_on(main_loop(args))
}

async fn main_loop(args: Args) -> std::io::Result<()> {
    let stream = TcpStream::connect((args.server.as_str(), 60010)).await?;
    let handle = tokio::runtime::Handle::current();
    let _ = TcpLink::attach(stream, &handle, &handle, async move |link: &mut TcpLink| {
        let _ = send_pkt!(link.handle(), ReqAgentLogin {});
        receiving(link).await
    });
    Ok(())
}

async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
    let mut client = Handler {
        handle: link.handle().clone(),
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

struct Handler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
}

impl RegistryInit for Handler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<PacketHbAgent>();
        register.insert::<RspAgentLoginOk>();
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
