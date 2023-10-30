use std::sync::atomic::AtomicU32;

use dashmap::DashMap;

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
}

pub struct ConnInfo {
    id: u32,
    port: u16,
    client: vnsvrbase::tokio_ext::tcp_link::Handle,
}

impl ConnInfo {
    pub fn new(id: u32, port: u16, client: vnsvrbase::tokio_ext::tcp_link::Handle) -> Self {
        Self { id, port, client }
    }
}
