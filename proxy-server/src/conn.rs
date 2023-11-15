use std::{
    collections::HashMap,
    sync::{atomic::AtomicU32, Arc, RwLock},
};

use tokio::net::TcpStream;

#[allow(dead_code)]
pub struct Agent {
    id: u32,
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    seed: AtomicU32,
    clients: Arc<RwLock<HashMap<u32, Client>>>,
}

impl Agent {
    pub fn new(id: u32, handle: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self {
            id,
            handle,
            seed: AtomicU32::new(0),
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn get_client_handle(&self, id: u32) -> Option<vnsvrbase::tokio_ext::tcp_link::Handle> {
        let clients = self.clients.read().unwrap();
        clients.get(&id).map(|v| v.handle.clone())
    }

    pub fn insert_client(&self, handle: vnsvrbase::tokio_ext::tcp_link::Handle, port: u16) -> u32 {
        let cid = self.seed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self.clients.write().unwrap();
        guard
            .insert(cid, Client::new(cid, handle, port))
            .map(|v| v.handle.close());
        cid
    }

    pub fn remove_client(&self, id: u32) {
        let mut guard = self.clients.write().unwrap();
        guard.remove(&id);
    }

    #[inline]
    pub fn handle(&self) -> vnsvrbase::tokio_ext::tcp_link::Handle {
        self.handle.clone()
    }

    pub fn clear(&self) {
        let clients = self.clients.write().unwrap();
        for (_, client) in clients.iter() {
            client.handle.close();
        }
        self.handle.close();
    }

    pub fn insert_conn(&self, cid: u32, sid: u32) {
        let clients = self.clients.read().unwrap();
        clients.get(&cid).map(|client| {
            let mut conns = client.conns.write().unwrap();
            conns.insert(sid, Conn::new(sid));
        });
    }

    pub fn remove_conn(&self, cid: u32, sid: u32) {
        let clients = self.clients.write().unwrap();
        clients.get(&cid).map(|client| {
            let mut conns = client.conns.write().unwrap();
            conns.remove(&sid);
        });
    }

    pub fn get_conn_rx(
        &self,
        cid: u32,
        sid: u32,
    ) -> Option<tokio::sync::oneshot::Receiver<TcpStream>> {
        let clients = self.clients.read().unwrap();
        clients.get(&cid).map_or(None, |client| {
            let mut conns = client.conns.write().unwrap();
            conns.get_mut(&sid).map_or(None, |conn| conn.rx.take())
        })
    }

    pub fn get_conn_sx(
        &self,
        cid: u32,
        sid: u32,
    ) -> Option<tokio::sync::oneshot::Sender<TcpStream>> {
        let clients = self.clients.read().unwrap();
        clients.get(&cid).map_or(None, |client| {
            let mut conns = client.conns.write().unwrap();
            conns.get_mut(&sid).map_or(None, |conn| conn.sx.take())
        })
    }
}

#[allow(dead_code)]
pub struct Client {
    id: u32,
    port: u16,
    handle: vnsvrbase::tokio_ext::tcp_link::Handle,
    conns: Arc<RwLock<HashMap<u32, Conn>>>,
}

impl Client {
    pub fn new(id: u32, handle: vnsvrbase::tokio_ext::tcp_link::Handle, port: u16) -> Self {
        Self {
            id,
            port,
            handle,
            conns: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[allow(dead_code)]
pub struct Conn {
    id: u32,
    sx: Option<tokio::sync::oneshot::Sender<TcpStream>>,
    rx: Option<tokio::sync::oneshot::Receiver<TcpStream>>,
}

impl Conn {
    pub fn new(id: u32) -> Self {
        let (sx, rx) = tokio::sync::oneshot::channel();
        Self {
            id,
            sx: Some(sx),
            rx: Some(rx),
        }
    }
}
