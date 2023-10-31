use std::{collections::HashMap, future::Future, time::Duration};

use protocol::{
    PacketHbAgent, PacketHbClient, ReqClientLogin, ReqNewConnectionAgent, ReqNewConnectionClient,
    RspAgentBuild, RspClientLoginFailed, RspClientNotFound, RspNewConnFailedClient,
};
use tokio::{
    io::BufReader,
    net::tcp::OwnedReadHalf,
    sync::watch::{channel, Sender},
};
use vnpkt::tokio_ext::{
    io::AsyncReadExt,
    registry::{PacketProc, RegistryInit},
};
use vnsvrbase::tokio_ext::tcp_link::{send_pkt, TcpLink};

use crate::{conn::ConnInfo, ROUTES};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx_clients: HashMap<u32, Sender<u32>>,
}

impl Client {
    pub async fn receiving(link: &mut TcpLink) -> std::io::Result<()> {
        let mut client = Client {
            handle: link.handle().clone(),
            sx_clients: HashMap::new(),
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

    fn proc(&mut self, _: Box<PacketHbClient>) -> Self::Output<'_> {
        async move { Ok(()) }
    }
}

impl PacketProc<ReqClientLogin> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqClientLogin>) -> Self::Output<'_> {
        async move {
            let id = ROUTES.insert(ConnInfo::new(ROUTES.next(), pkt.port, self.handle.clone()));
            let handle = self.handle.clone();
            let routes = &*ROUTES;
            let (sx, mut rx) = channel(0);
            self.sx_clients.insert(id, sx);

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rx.changed() => {
                            routes.remove(*rx.borrow());
                            break;
                        }
                        _ = async {
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            let _ = send_pkt!(handle, PacketHbClient {});
                        } => {}
                    }
                }
            });
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
                let _ = send_pkt!(client.unwrap(), ReqNewConnectionClient { id: pkt.id });
                let _ = send_pkt!(self.handle, ReqNewConnectionAgent { id: pkt.id });
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
