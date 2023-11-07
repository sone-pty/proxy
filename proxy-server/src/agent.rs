use std::{future::Future, time::Duration};

use protocol::{
    PacketHbAgent, ReqAgentLogin, ReqNewConnectionAgent, ReqNewConnectionClient, RspAgentBuild,
    RspAgentLoginOk, RspClientLoginFailed, RspClientNotFound,
};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::tokio_ext::registry::{PacketProc, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{
    conn::{ConnInfo, Conns},
    reset, AGENTS, CLIENTS, CONNS,
};

pub struct Agent {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx: Option<tokio::sync::watch::Sender<()>>,
}

impl Agent {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self { handle, sx: None }
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
        self.sx.as_ref().map(|v| v.send_replace(()));
        let id = pkt.id;
        async move {
            match send_pkt!(self.handle, pkt) {
                Ok(_) => {}
                Err(_) => {
                    self.handle.close();
                    reset(id);
                }
            }
            Ok(())
        }
    }
}

impl PacketProc<ReqAgentLogin> for Agent {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqAgentLogin>) -> Self::Output<'_> {
        async move {
            use dashmap::mapref::entry::Entry;
            match AGENTS.entry(pkt.id) {
                Entry::Vacant(e) => {
                    e.insert(self.handle.clone());
                }
                _ => {}
            }
            let _ = send_pkt!(self.handle, RspAgentLoginOk {});

            // Check Agent
            let (sx, mut rx) = tokio::sync::watch::channel(());
            let handle = self.handle.clone();
            self.sx = Some(sx);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        r = rx.changed() => {
                            match r {
                                Ok(_) => {
                                    rx.borrow_and_update();
                                }
                                Err(_) => {
                                    handle.close();
                                    reset(pkt.id);
                                    break;
                                }
                            }
                        }
                        _ = tokio::time::sleep(Duration::from_secs(15)) => {
                            handle.close();
                            reset(pkt.id);
                            break;
                        }
                    }
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
            match CLIENTS.get(&pkt.agent_id) {
                Some(clientconns) => {
                    let client = clientconns.get_client(pkt.id);
                    if pkt.ok && client.is_some() {
                        use dashmap::mapref::entry::Entry;
                        match CONNS.entry((pkt.agent_id, pkt.id)) {
                            Entry::Occupied(e) => {
                                e.get().insert(ConnInfo::new(pkt.sid));
                            }
                            Entry::Vacant(e) => {
                                let conns = Conns::new(pkt.id);
                                conns.insert(ConnInfo::new(pkt.sid));
                                e.insert(conns);
                            }
                        }

                        let _ = send_pkt!(
                            client.unwrap(),
                            ReqNewConnectionClient {
                                agent_id: pkt.agent_id,
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
                        clientconns.remove(pkt.id);
                    } else {
                        let _ = send_pkt!(self.handle, RspClientNotFound {});
                    }
                }
                None => {}
            }
            Ok(())
        }
    }
}
