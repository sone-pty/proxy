use std::future::Future;

use protocol::{
    PacketHbAgent, PacketHbClient, ReqAgentBuild, ReqClientLogin, RspAgentBuild,
    RspClientLoginFailed, RspClientNotFound, RspNewConnFailedClient, RspNewConnectionAgent,
    RspNewConnectionClient,
};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::tokio_ext::{
    io::AsyncReadExt,
    registry::{PacketProc, RegistryInit},
};
use vnsvrbase::tokio_ext::tcp_link::{send_pkt, TcpLink};

use crate::{conn::ConnInfo, ROUTES};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
}

impl Client {
    pub async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
        let mut client = Client {
            handle: link.handle().clone(),
        };
        let register = &*super::REGISTRY;

        loop {
            let pid = link.read.read_compressed_u64().await?;
            if pid <= u32::MAX as _ {
                if let Some(item) = register.query(pid as u32) {
                    let r = item.recv(&mut link.read).await?;
                    r.proc(&mut client).await?;
                    continue;
                }
            }
            break Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut vnpkt::tokio_ext::registry::Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<ReqClientLogin>();
        register.insert::<RspAgentBuild>();
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
        async move {
            let id = ROUTES.insert(ConnInfo::new(0, pkt.port, self.handle.clone()));
            let _ = send_pkt!(self.handle, ReqAgentBuild { port: pkt.port, id });
            Ok(())
        }
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

impl PacketProc<RspAgentBuild> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspAgentBuild>) -> Self::Output<'_> {
        async move {
            let client = ROUTES.get_client(pkt.id);
            if pkt.ok && client.is_some() {
                let _ = send_pkt!(client.unwrap(), RspNewConnectionClient { id: pkt.id });
                let _ = send_pkt!(self.handle, RspNewConnectionAgent { id: pkt.id });
            } else if !pkt.ok {
                let _ = send_pkt!(client.unwrap(), RspClientLoginFailed {});
                ROUTES.remove(pkt.id);
            } else {
                let _ = send_pkt!(self.handle, RspClientNotFound {});
                ROUTES.remove(pkt.id);
            }
            Ok(())
        }
    }
}

impl PacketProc<RspNewConnFailedClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspNewConnFailedClient>) -> Self::Output<'_> {
        async move {
            ROUTES.remove(pkt.id);
            Ok(())
        }
    }
}
