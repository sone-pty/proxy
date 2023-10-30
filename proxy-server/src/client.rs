use std::future::Future;

use protocol::{PacketHbAgent, PacketHbClient, ReqClientLogin};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::{
    tokio_ext::{
        io::AsyncReadExt,
        registry::{PacketProc, RegistryInit},
    },
    util::async_action::AsyncAction,
};
use vnsvrbase::tokio_ext::tcp_link::{send_pkt, TcpLink};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    next_state: Option<AsyncAction<TcpLink, std::io::Result<()>>>,
}

impl Client {
    pub async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
        let mut client = Client {
            handle: link.handle().clone(),
            next_state: None,
        };
        let register = &*super::REGISTRY;

        let pid = link.read.read_compressed_u64().await?;
        if pid <= u32::MAX as _ {
            if let Some(item) = register.query(pid as u32) {
                let r = item.recv(&mut link.read).await?;
                r.proc(&mut client).await?;
                link.next_state = client.next_state.take();
                return Ok(());
            }
        }
        Err(std::io::ErrorKind::InvalidData.into())
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;
    fn init(register: &mut vnpkt::tokio_ext::registry::Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<ReqClientLogin>();
    }
}

impl PacketProc<PacketHbClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbClient>) -> Self::Output<'_> {
        async move {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}

impl PacketProc<ReqClientLogin> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqClientLogin>) -> Self::Output<'_> {
        async move { Ok(()) }
    }
}

impl PacketProc<PacketHbAgent> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbAgent>) -> Self::Output<'_> {
        async move {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}
