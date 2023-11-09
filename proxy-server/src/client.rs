use std::future::Future;

use protocol::{
    PacketHbClient, PacketInfoClientClosed, ReqAgentBuild, ReqClientLogin, RspClientLoginOk,
    RspNewConnFailedClient, RspServiceNotFound,
};
use tokio::{io::BufReader, net::tcp::OwnedReadHalf};
use vnpkt::tokio_ext::registry::{PacketProc, Registry, RegistryInit};
use vnsvrbase::tokio_ext::tcp_link::send_pkt;

use crate::AGENTS;

pub struct ClientHandler {
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    id: Option<u32>,
    aid: Option<u32>,
}

impl ClientHandler {
    pub fn new(handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self {
            handle,
            id: None,
            aid: None,
        }
    }
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        if self.aid.is_some() && self.id.is_some() {
            let aid = self.aid.unwrap();
            let id = self.id.unwrap();
            let agents = AGENTS.read().unwrap();
            agents.get(&aid).map(|v| {
                let _ = send_pkt!(v.handle(), PacketInfoClientClosed { id });
                v.remove_client(id);
                println!("In the proxy.{}, Client.{} disconnected", aid, id);
            });
        }
    }
}

impl RegistryInit for ClientHandler {
    type AsyncRead = BufReader<OwnedReadHalf>;

    fn init(register: &mut Registry<Self>) {
        register.insert::<ReqClientLogin>();
        register.insert::<PacketHbClient>();
        register.insert::<RspNewConnFailedClient>();
    }
}

impl PacketProc<PacketHbClient> for ClientHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<PacketHbClient>) -> Self::Output<'_> {
        async {
            let _ = send_pkt!(self.handle, pkt);
            Ok(())
        }
    }
}

impl PacketProc<ReqClientLogin> for ClientHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<ReqClientLogin>) -> Self::Output<'_> {
        async move {
            let aid = pkt.id;
            let port = pkt.port;
            let agents = AGENTS.read().unwrap();
            let agent = agents.get(&aid);

            if agent.is_none() {
                let _ = send_pkt!(self.handle, RspServiceNotFound {});
                return Ok(());
            }
            let cid = agent.unwrap().insert_client(self.handle.clone(), port);
            self.id = Some(cid);

            let _ = send_pkt!(
                self.handle,
                RspClientLoginOk {
                    agent_id: aid,
                    id: cid,
                }
            );
            let _ = send_pkt!(agent.unwrap().handle(), ReqAgentBuild { port, id: cid });

            Ok(())
        }
    }
}

impl PacketProc<RspNewConnFailedClient> for ClientHandler {
    type Output<'a> = impl Future<Output = std::io::Result<()>> + 'a where Self: 'a;

    fn proc(&mut self, pkt: Box<RspNewConnFailedClient>) -> Self::Output<'_> {
        async move {
            self.handle.close();
            let agents = AGENTS.read().unwrap();
            agents.get(&pkt.agent_id).map(|agent| {
                let _ = send_pkt!(agent.handle(), PacketInfoClientClosed { id: pkt.id });
            });
            Ok(())
        }
    }
}
