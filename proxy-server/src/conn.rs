use std::sync::{atomic::AtomicU32, Arc, Mutex};

use dashmap::DashMap;
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

pub struct Conns {
    next: AtomicU32,
    conns: DashMap<u32, ConnInfo>,
}

impl Conns {
    pub fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            conns: DashMap::new(),
        }
    }

    pub fn insert(&self, info: ConnInfo) -> u32 {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.conns.entry(id) {
            dashmap::mapref::entry::Entry::Vacant(e) => {
                e.insert(info);
            }
            _ => {
                unreachable!()
            }
        }
        id
    }

    pub fn get_client(&self, id: u32) -> Option<vnsvrbase::tokio_ext::tcp_link::Handle> {
        let v = self.conns.get(&id).take();
        v.map_or(None, |v| Some(v.client.clone()))
    }

    pub fn get_rx(&self, id: u32) -> Option<Arc<Mutex<Receiver<TcpStream>>>> {
        let v = self.conns.get(&id).take();
        v.map_or(None, |v| Some(v.rx.clone()))
    }

    pub fn get_sx(&self, id: u32) -> Option<Arc<Sender<TcpStream>>> {
        let v = self.conns.get(&id).take();
        v.map_or(None, |v| Some(v.sx.clone()))
    }

    pub fn remove(&self, id: u32) {
        self.conns.remove(&id);
    }
}

#[allow(dead_code)]
pub struct ConnInfo {
    id: u32,
    port: u16,
    client: vnsvrbase::tokio_ext::tcp_link::Handle,
    sx: Arc<Sender<TcpStream>>,
    rx: Arc<Mutex<Receiver<TcpStream>>>,
}

impl ConnInfo {
    pub fn new(id: u32, port: u16, client: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        let (sx, rx) = tokio::sync::mpsc::channel(10);
        Self {
            id,
            port,
            client,
            sx: Arc::new(sx),
            rx: Arc::new(Mutex::new(rx)),
        }
    }
}
