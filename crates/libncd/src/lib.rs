//! Protocol layer for Network Character Device Protocol (NCDP).

mod codec;
pub mod error;
pub mod frame;
pub mod packet;

pub(crate) const MAGIC_NUMBER: &[u8; 3] = b"NCD";
pub(crate) const VERSION: u8 = 1;
pub(crate) const MAX_PAYLOAD_SIZE: usize = u16::MAX as usize;

pub use codec::{read_frame, read_packet, write_packet};
