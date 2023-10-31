#![feature(impl_trait_in_assoc_type)]

mod proto;
pub use proto::*;

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

#[test]
pub fn te() {
    let data: u64 = (1u64 << 33) | 1;
    assert_eq!(1, get_id(&data));
    assert!(is_client(&data));
    let data_1 = compose(123, false);
    assert!(is_agent(&data_1));
    assert_eq!(123, get_id(&data_1));
}
