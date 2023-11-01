use std::{future::Future, sync::Arc, time::Duration};

use protocol::{
    PacketHbAgent, ReqAgentLogin, ReqNewConnectionAgent, ReqNewConnectionClient, RspAgentBuild,
    RspAgentLoginOk, RspClientLoginFailed, RspClientNotFound,
};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf, sync::watch::Sender};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::ROUTES;

pub struct Agent {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx: Arc<Sender<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>>,
}

impl Agent {
    pub fn new(
        handle: vnsvrbase::tokio_ext::tcp_link::Handle,
        sx: Sender<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
    ) -> Self {
        Self {
            handle,
            sx: Arc::new(sx),
        }
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

    fn proc(&mut self, _: Box<PacketHbAgent>) -> Self::Output<'_> {
        async { Ok(()) }
    }
}

impl PacketProc<ReqAgentLogin> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, _: Box<ReqAgentLogin>) -> Self::Output<'_> {
        async {
            let _ = send_pkt!(self.handle, RspAgentLoginOk {});
            let sx = self.sx.clone();
            let handle = self.handle.clone();

            tokio::spawn(async move {
                loop {
                    let _ = sx.send_if_modified(|inner| {
                        if inner.is_none() {
                            *inner = Some(handle.clone());
                            true
                        } else {
                            false
                        }
                    });
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
            Ok(())
        }
    }
}

impl PacketProc<RspAgentBuild> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspAgentBuild>) -> Self::Output<'_> {
        async move {
            let client = ROUTES.get_client(pkt.id);
            if pkt.ok && client.is_some() {
                let _ = send_pkt!(client.unwrap(), ReqNewConnectionClient { id: pkt.id });
                let _ = send_pkt!(self.handle, ReqNewConnectionAgent { id: pkt.id });
            } else if !pkt.ok && client.is_some() {
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
