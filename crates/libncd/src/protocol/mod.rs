mod packet;
pub use packet::Packet;

const MAGIC_NUMBER: &[u8; 3] = b"NCD";
const VERSION: u8 = 1;
