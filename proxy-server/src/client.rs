use std::{future::Future, sync::Arc, time::Duration};

use dashmap::DashMap;
use protocol::{
    PacketHbClient, PacketInfoClientClosed, ReqAgentBuild, ReqClientLogin, RspNewConnFailedClient,
    RspServiceNotFound,
};
use tokio::{
    io::BufReader,
    net::tcp::OwnedReadHalf,
    sync::watch::{channel, Sender},
};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{
    conn::{ClientConns, ClientInfo},
    AGENTS, CLIENTS, CONNS,
};

pub struct Client {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx_clients: Arc<DashMap<u32, Sender<bool>>>,
}

impl Client {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self {
            handle,
            sx_clients: Arc::new(DashMap::new()),
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
            let agent_id = pkt.id;
            let port = pkt.port;
            let agent_wrap = AGENTS.get(&agent_id);

            if agent_wrap.is_none() {
                let _ = send_pkt!(self.handle, RspServiceNotFound {});
                return Ok(());
            }
            let agent = agent_wrap.unwrap().clone();

            use dashmap::mapref::entry::Entry;
            let id = match CLIENTS.entry(agent_id) {
                Entry::Vacant(e) => {
                    let clientconns = ClientConns::new();
                    let id = clientconns.insert(ClientInfo::new(
                        clientconns.next(),
                        port,
                        self.handle.clone(),
                        agent_id,
                    ));
                    e.insert(clientconns);
                    id
                }
                Entry::Occupied(e) => {
                    let clientconns = e.get();
                    clientconns.insert(ClientInfo::new(
                        clientconns.next(),
                        port,
                        self.handle.clone(),
                        agent_id,
                    ))
                }
            };

            let handle = self.handle.clone();
            let agent_clone = agent.clone();
            let clients = CLIENTS.get(&agent_id).unwrap();
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
                                println!("With Proxy.{}, Client.{} Disconnected", agent_id, id);
                                handle.close();
                                let _ = send_pkt!(agent_clone, PacketInfoClientClosed {id});
                                clients.remove(id);
                                sx_clients.remove(&id);
                                CONNS.remove(&(agent_id, id));
                                break 'hb;
                            }
                        }
                        ret = async {
                            match send_pkt!(handle, PacketHbClient {id}) {
                                Err(_) => {
                                    println!("With Proxy.{}, Client.{} Disconnected", agent_id, id);
                                    handle.close();
                                    let _ = send_pkt!(agent_clone, PacketInfoClientClosed {id});
                                    clients.remove(id);
                                    sx_clients.remove(&id);
                                    CONNS.remove(&(agent_id, id));
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

            let _ = send_pkt!(agent, ReqAgentBuild { port, id });
            Ok(())
        }
    }
}

impl PacketProc<RspNewConnFailedClient> for Client {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspNewConnFailedClient>) -> Self::Output<'_> {
        async move {
            println!(
                "With Proxy.{}, Client.{} Disconnected",
                pkt.agent_id, pkt.id
            );
            self.handle.close();
            AGENTS.get(&pkt.agent_id).map(|v| v.close());
            Ok(())
        }
    }
}
