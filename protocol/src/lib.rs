#![feature(impl_trait_in_assoc_type)]

mod proto;
pub use proto::*;
use vnpkt::prelude::PacketId;

pub const BOUDARY: u32 = <PacketBoudary as PacketId>::PID;
pub const CLIENT_ID: u32 = 1 << 1;
pub const AGENT_ID: u32 = 1 << 2;

#[inline]
pub fn is_client(data: &u64) -> bool {
    ((data >> 32) as u32) == CLIENT_ID
}

#[inline]
pub fn is_agent(data: &u64) -> bool {
    ((data >> 32) as u32) == AGENT_ID
}

#[inline]
pub fn get_id(data: &u64) -> u32 {
    (data & 0xFFFFFFFF) as _
}

#[inline]
pub fn compose(id: u32, client: bool) -> u64 {
    if client {
        id as u64 | (CLIENT_ID as u64) << 32
    } else {
        id as u64 | (AGENT_ID as u64) << 32
    }
}
