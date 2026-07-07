//! Protocol layer for Network Character Device Protocol (NCDP).

pub mod codec;
pub mod frame;
pub mod packet;

pub(crate) const MAGIC_NUMBER: &[u8; 3] = b"NCD";
pub(crate) const VERSION: u8 = 1;
pub(crate) const DEFAULT_MAX_PAYLOAD_SIZE: usize = u16::MAX as usize;
