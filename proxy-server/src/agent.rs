use std::future::Future;

use protocol::{
    PacketHbAgent, ReqAgentLogin, ReqNewConnectionAgent, ReqNewConnectionClient, RspAgentBuild,
    RspAgentLoginOk, RspClientLoginFailed, RspClientNotFound,
};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{conn::ConnInfo, CHANNEL, CLIENTS, CONNS};

pub struct Agent {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
}

impl Agent {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self { handle }
    }
}

impl RegistryInit for Agent {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut vnpkt::tokio_ext::registry::Registry<Self>) {
        register.insert::<PacketHbAgent>();
        register.insert::<ReqAgentLogin>();
        register.insert::<RspAgentBuild>();
    }
}

impl PacketProc<PacketHbAgent> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbAgent>) -> Self::Output<'_> {
        async {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}

impl PacketProc<ReqAgentLogin> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<ReqAgentLogin>) -> Self::Output<'_> {
        async {
            let _ = send_pkt!(self.handle, RspAgentLoginOk {});
            CHANNEL.0.send_replace(Some(self.handle.clone()));
            Ok(())
        }
    }
}

impl PacketProc<RspAgentBuild> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspAgentBuild>) -> Self::Output<'_> {
        async move {
            let client = CLIENTS.get_client(pkt.id);
            if pkt.ok && client.is_some() {
                CONNS.insert(ConnInfo::new(pkt.sid));
                let _ = send_pkt!(
                    client.unwrap(),
                    ReqNewConnectionClient {
                        id: pkt.id,
                        sid: pkt.sid
                    }
                );
                let _ = send_pkt!(
                    self.handle,
                    ReqNewConnectionAgent {
                        id: pkt.id,
                        sid: pkt.sid
                    }
                );
            } else if !pkt.ok && client.is_some() {
                let _ = send_pkt!(client.unwrap(), RspClientLoginFailed {});
                CLIENTS.remove(pkt.id);
            } else {
                let _ = send_pkt!(self.handle, RspClientNotFound {});
            }
            Ok(())
        }
    }
}
