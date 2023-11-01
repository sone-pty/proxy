use std::{collections::HashMap, future::Future, time::Duration};

use protocol::{PacketHbClient, ReqAgentBuild, ReqClientLogin, RspNewConnFailedClient};
use tokio::{
    io::BufReader,
    net::tcp::OwnedReadHalf,
    sync::watch::{channel, Receiver, Sender},
};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{conn::ConnInfo, ROUTES};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx_clients: HashMap<u32, Sender<bool>>,
    rx_agent: Receiver<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
}

impl Client {
    pub fn new(
        handle: vnsvrbase::tokio_ext::tcp_link::Handle,
        rx_agent: Receiver<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
    ) -> Self {
        Self {
            handle,
            sx_clients: HashMap::new(),
            rx_agent,
        }
    }
}

impl RegistryInit for Client {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut vnpkt::tokio_ext::registry::Registry<Self>) {
        register.insert::<PacketHbClient>();
        register.insert::<ReqClientLogin>();
        register.insert::<RspNewConnFailedClient>();
    }
}

impl PacketProc<PacketHbClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbClient>) -> Self::Output<'_> {
        async move {
            self.sx_clients.get(&pkt.id).map(|sx| {
                let now = *sx.borrow();
                let _ = sx.send(!now);
            });
            Ok(())
        }
    }
}

impl PacketProc<ReqClientLogin> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqClientLogin>) -> Self::Output<'_> {
        async move {
            let id = ROUTES.insert(ConnInfo::new(ROUTES.next(), pkt.port, self.handle.clone()));
            let handle = self.handle.clone();
            let routes = &*ROUTES;
            let (tsx, trx) = channel(false);
            self.sx_clients.insert(id, tsx);

            tokio::spawn(async move {
                loop {
                    let (sx, mut rx) = channel(0);
                    tokio::select! {
                        _ = rx.changed() => {
                            let id = *rx.borrow();
                            routes.remove(id);
                            break;
                        }
                        _ = async {
                            tokio::time::sleep(Duration::from_secs(10)).await;
                            let _ = send_pkt!(handle, PacketHbClient {id});
                            // check heartbeat
                            let mut hbrx = trx.clone();
                            tokio::spawn(async move {
                                tokio::select! {
                                    _ = hbrx.changed() => {}
                                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                                        let _ = sx.send(id);
                                    }
                                }
                            });
                        } => {}
                    }
                }
            });

            let _ = self.rx_agent.changed().await;
            let agent_hd = self.rx_agent.borrow_and_update();
            agent_hd.as_ref().map(|v| {
                let _ = send_pkt!(v, ReqAgentBuild { port: pkt.port, id });
            });
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
