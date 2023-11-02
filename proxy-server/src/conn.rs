use std::{
    cell::SyncUnsafeCell,
    sync::{atomic::AtomicU32, Arc},
};

use dashmap::DashMap;
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

pub struct ClientConns {
    next: AtomicU32,
    conns: DashMap<u32, ClientInfo>,
}

impl ClientConns {
    pub fn new() -> Self {
        Self {
            // 0 is flag
            next: AtomicU32::new(1),
            conns: DashMap::new(),
        }
    }

    #[inline]
    pub fn next(&self) -> u32 {
        let prev = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if prev == u32::MAX {
            self.next.store(1, std::sync::atomic::Ordering::SeqCst);
        }
        prev
    }

    pub fn insert(&self, info: ClientInfo) -> u32 {
        let id = info.id;
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

    pub fn remove(&self, id: u32) {
        self.conns.remove(&id);
    }
}

#[allow(dead_code)]
pub struct ClientInfo {
    id: u32,
    port: u16,
    client: vnsvrbase::tokio_ext::tcp_link::Handle,
}

impl ClientInfo {
    pub fn new(id: u32, port: u16, client: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self { id, port, client }
    }
}

pub struct Conns {
    conns: DashMap<u32, ConnInfo>,
}

impl Conns {
    pub fn new() -> Self {
        Self {
            conns: DashMap::new(),
        }
    }

    pub fn insert(&self, info: ConnInfo) -> u32 {
        let id = info.id;
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

    pub fn get_rx(&self, id: u32) -> Option<Arc<SyncUnsafeCell<Receiver<TcpStream>>>> {
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
    sx: Arc<Sender<TcpStream>>,
    rx: Arc<SyncUnsafeCell<Receiver<TcpStream>>>,
}

impl ConnInfo {
    pub fn new(id: u32) -> Self {
        let (sx, rx) = tokio::sync::mpsc::channel(10);
        Self {
            id,
            sx: Arc::new(sx),
            rx: Arc::new(SyncUnsafeCell::new(rx)),
        }
    }
}
