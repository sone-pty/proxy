use std::{future::Future, sync::Arc, time::Duration};

use dashmap::DashMap;
use protocol::{
    PacketHbClient, PacketInfoClientClosed, ReqAgentBuild, ReqClientLogin, RspNewConnFailedClient,
};
use tokio::{
    io::BufReader,
    net::tcp::OwnedReadHalf,
    sync::watch::{channel, Receiver, Sender},
};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{conn::ClientInfo, CLIENTS, CONNS};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx_clients: Arc<DashMap<u32, Sender<bool>>>,
    rx_agent: Receiver<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
}

impl Client {
    pub fn new(
        handle: vnsvrbase::tokio_ext::tcp_link::Handle,
        rx_agent: Receiver<Option<vnsvrbase::tokio_ext::tcp_link::Handle>>,
    ) -> Self {
        Self {
            handle,
            sx_clients: Arc::new(DashMap::new()),
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
                sx.send_replace(!now);
            });
            Ok(())
        }
    }
}

impl PacketProc<ReqClientLogin> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqClientLogin>) -> Self::Output<'_> {
        async move {
            let id = CLIENTS.insert(ClientInfo::new(
                CLIENTS.next(),
                pkt.port,
                self.handle.clone(),
            ));
            let handle = self.handle.clone();
            let clients = &*CLIENTS;
            let sx_clients = self.sx_clients.clone();
            let (tsx, trx) = channel(false);
            self.sx_clients.insert(id, tsx);

            tokio::spawn(async move {
                'hb: loop {
                    let (sx, mut rx) = channel(0);
                    tokio::select! {
                        _ = rx.changed() => {
                            let id = *rx.borrow();
                            if id > 0 {
                                println!("Client.{} disconnected", id);
                                handle.close();
                                // CLIENTS.get_agent(id).map(|v| v.close());
                                CLIENTS.get_agent(id).map(|v| send_pkt!(v, PacketInfoClientClosed {id}));
                                clients.remove(id);
                                sx_clients.remove(&id);
                                CONNS.remove(&id);
                                break 'hb;
                            }
                        }
                        ret = async {
                            match send_pkt!(handle, PacketHbClient {id}) {
                                Err(_) => {
                                    println!("Client.{} disconnected", id);
                                    handle.close();
                                    // CLIENTS.get_agent(id).map(|v| v.close());
                                    CLIENTS.get_agent(id).map(|v| send_pkt!(v, PacketInfoClientClosed {id}));
                                    clients.remove(id);
                                    sx_clients.remove(&id);
                                    CONNS.remove(&id);
                                    return false;
                                }
                                _ => {}
                            }
                            // check heartbeat
                            let mut hbrx = trx.clone();
                            tokio::spawn(async move {
                                tokio::select! {
                                    _ = hbrx.changed() => {
                                        hbrx.borrow_and_update();
                                    }
                                    _ = tokio::time::sleep(Duration::from_secs(5)) => {
                                        let _ = sx.send(id);
                                    }
                                }
                            });
                            true
                        } => {
                            if !ret {
                                break 'hb;
                            }
                        }
                    }
                    // HB interval
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });

            let mut rx_agent = self.rx_agent.clone();
            tokio::spawn(async move {
                match rx_agent.wait_for(|v| v.is_some()).await {
                    Ok(agent) => {
                        agent.as_ref().map(|v| {
                            let _ = send_pkt!(v, ReqAgentBuild { port: pkt.port, id });
                            CLIENTS.set_agent(id, v.clone());
                        });
                    }
                    _ => {}
                }
                // reset
                // CHANNEL.0.send_replace(None);
            });
            Ok(())
        }
    }
}

impl PacketProc<RspNewConnFailedClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspNewConnFailedClient>) -> Self::Output<'_> {
        async move {
            println!("Client.{} disconnected", pkt.id);
            self.handle.close();
            CLIENTS
                .get_agent(pkt.id)
                .map(|v| send_pkt!(v, PacketInfoClientClosed { id: pkt.id }));
            // CLIENTS.get_agent(pkt.id).map(|v| v.close());
            Ok(())
        }
    }
}
