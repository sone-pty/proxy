use std::future::Future;

use protocol::{
    PacketHbAgent, PacketInfoClientClosed, PacketInfoConnectFailed, ReqAgentLogin,
    ReqNewConnectionAgent, ReqNewConnectionClient, RspAgentBuild, RspAgentLoginFailed,
    RspAgentLoginOk, RspClientLoginFailed, RspClientNotFound,
};
use slog::error;
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::tokio_ext::registry::{PacketProc, Registry, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::{conn::Agent, AGENTS, LOGGER};

pub struct AgentHandler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    id: Option<u32>,
}

impl AgentHandler {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self { handle, id: None }
    }
}

impl Drop for AgentHandler {
    fn drop(&mut self) {
        self.id.map(|id| {
            let mut agents = AGENTS.write().unwrap();
            agents.remove(&id).map(|v| v.clear());
        });
    }
}

impl RegistryInit for AgentHandler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<ReqAgentLogin>();
        register.insert::<PacketHbAgent>();
        register.insert::<RspAgentBuild>();
        register.insert::<PacketInfoConnectFailed>();
    }
}

impl PacketProc<PacketHbAgent> for AgentHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbAgent>) -> Self::Output<'_> {
        async move {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}

impl PacketProc<ReqAgentLogin> for AgentHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqAgentLogin>) -> Self::Output<'_> {
        async move {
            let mut agents = AGENTS.write().unwrap();
            if agents.contains_key(&pkt.id) {
                let _ = send_pkt!(self.handle, RspAgentLoginFailed {});
                return Ok(());
            } else {
                agents.insert(pkt.id, Agent::new(pkt.id, self.handle.clone()));
            }
            self.id = Some(pkt.id);
            let _ = send_pkt!(self.handle, RspAgentLoginOk {});
            Ok(())
        }
    }
}

impl PacketProc<RspAgentBuild> for AgentHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspAgentBuild>) -> Self::Output<'_> {
        async move {
            let agents = AGENTS.read().unwrap();
            let agent = agents.get(&pkt.agent_id);

            if agent.is_none() {
                error!(LOGGER, "No agent with id = {}", pkt.agent_id);
                Err(std::io::ErrorKind::InvalidData.into())
            } else {
                let client_handle = agent.unwrap().get_client_handle(pkt.id);

                match (pkt.ok, client_handle.is_some()) {
                    (true, true) => {
                        agent.unwrap().insert_conn(pkt.id, pkt.sid);
                        let _ = send_pkt!(
                            client_handle.unwrap(),
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
                    }
                    (false, true) => {
                        let _ = send_pkt!(client_handle.unwrap(), RspClientLoginFailed {});
                        let _ = send_pkt!(self.handle, PacketInfoClientClosed { id: pkt.id });
                        agent.unwrap().remove_client(pkt.id);
                        error!(LOGGER, "agent-build check (false, true), agent-id = {}, client-id = {}", pkt.agent_id, pkt.id)
                    }
                    (true, false) => {
                        let _ = send_pkt!(self.handle, RspClientNotFound { id: pkt.id });
                        error!(LOGGER, "agent-build check (true, false), agent-id = {}, client-id = {}", pkt.agent_id, pkt.id)
                    }
                    _ => error!(LOGGER, "agent-build check (false, false), agent-id = {}, client-id = {}", pkt.agent_id, pkt.id)
                }
                Ok(())
            }
        }
    }
}

impl PacketProc<PacketInfoConnectFailed> for AgentHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketInfoConnectFailed>) -> Self::Output<'_> {
        async move {
            let agents = AGENTS.read().unwrap();
            agents.get(&pkt.agent_id).map(|agent| {
                agent.remove_conn(pkt.cid, pkt.sid);
                agent.get_client_handle(pkt.cid).map(|v| {
                    let _ = send_pkt!(v, pkt);
                })
            });
            Ok(())
        }
    }
}
