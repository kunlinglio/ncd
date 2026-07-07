//! # Packet Structure
//! 0                  16      24        32 bit
//! |       Magic Number       | Version |
//! |  Payload Length  |  Flag |  Type   |
//! |         Payload (variable)         |

use super::{MAGIC_NUMBER, VERSION};
use crate::error::Error;

/// Packet represents a single packet in the Network Character Device Protocol.
#[derive(Debug)]
pub struct Packet {
    // Magic Number and Version are handled by encoding/decoding functions
    pub flag: Flag,
    pub typed_payload: TypedPayload,
}
const HEADER_SIZE_BYTE: usize = 8;

/// Sequence flags.
/// If the user request is larger than the maximum payload size, the request will be split into multiple packets.
/// - The first packet: More (1)
/// - Middle packets: More (1)
/// - The Last packet: End (0)
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Flag {
    End = 0x00,
    More = 0x01,
}

impl TryFrom<u8> for Flag {
    type Error = Error;
    fn try_from(v: u8) -> Result<Self, Error> {
        match v {
            0x00 => Ok(Flag::End),
            0x01 => Ok(Flag::More),
            _ => Err(Error::PacketDecodeError(format!(
                "Invalid flag value: {}",
                v
            ))),
        }
    }
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE_BYTE + self.typed_payload.length());
        buf.extend_from_slice(MAGIC_NUMBER); // 24 bit
        buf.extend_from_slice(&VERSION.to_be_bytes()); // 8 bit
        buf.extend_from_slice(&self.typed_payload.length().to_be_bytes()); // 16 bit
        buf.push(self.flag as u8); // 8 bit
        buf.push(self.typed_payload.tag()); // 8 bit
        assert_eq!(buf.len(), HEADER_SIZE_BYTE);

        buf.extend_from_slice(&self.typed_payload.encode_body());
        buf
    }

    pub fn decode(src: &[u8]) -> Result<Self, Error> {
        if src.len() < HEADER_SIZE_BYTE {
            return Err(Error::PacketDecodeError("Source data is too short".into()));
        }
        let magic = &src[0..3];
        if magic != MAGIC_NUMBER {
            return Err(Error::PacketDecodeError(format!(
                "Invalid magic number: {:?}",
                magic
            )));
        }
        let version = src[3];
        if version != VERSION {
            return Err(Error::PacketDecodeError(format!(
                "Invalid version: {}",
                version
            )));
        }
        let length = u16::from_be_bytes([src[4], src[5]]) as usize;
        let flag = src[6];
        let tag = src[7];
        if src.len() < HEADER_SIZE_BYTE + length {
            return Err(Error::PacketDecodeError(
                "Source data is too short for payload".into(),
            ));
        }
        let payload = &src[HEADER_SIZE_BYTE..HEADER_SIZE_BYTE + length];

        let typed_payload = TypedPayload::decode_body(tag, payload)?;

        Ok(Packet {
            flag: Flag::try_from(flag)?,
            typed_payload,
        })
    }
}

#[repr(u8)]
#[derive(Debug)]
pub enum TypedPayload {
    ControlHello = 0x01,
    ControlClose = 0x02,
    ControlKeepAlive = 0x03,
    ControlPing { id: u32 } = 0x04,
    ControlPong { id: u32 } = 0x05,
    Data(Vec<u8>) = 0x06,
}

impl TypedPayload {
    pub fn tag(&self) -> u8 {
        match self {
            Self::ControlHello => 0x01,
            Self::ControlClose => 0x02,
            Self::ControlKeepAlive => 0x03,
            Self::ControlPing { .. } => 0x04,
            Self::ControlPong { .. } => 0x05,
            Self::Data(_) => 0x06,
        }
    }

    pub fn encode_body(&self) -> Vec<u8> {
        match self {
            Self::ControlHello | Self::ControlClose | Self::ControlKeepAlive => vec![],
            Self::ControlPing { id } => id.to_be_bytes().to_vec(),
            Self::ControlPong { id } => id.to_be_bytes().to_vec(),
            Self::Data(data) => data.clone(),
        }
    }

    pub fn decode_body(tag: u8, src: &[u8]) -> Result<Self, Error> {
        let typed_payload = match tag {
            0x01 => Ok(Self::ControlHello),
            0x02 => Ok(Self::ControlClose),
            0x03 => Ok(Self::ControlKeepAlive),
            0x04 => {
                if src.len() != 4 {
                    return Err(Error::PacketDecodeError(
                        "Source data is too short for ControlPing".into(),
                    ));
                }
                Ok(Self::ControlPing {
                    id: u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
                })
            }
            0x05 => {
                if src.len() != 4 {
                    return Err(Error::PacketDecodeError(
                        "Source data is too short for ControlPong".into(),
                    ));
                }
                Ok(Self::ControlPong {
                    id: u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
                })
            }
            0x06 => Ok(Self::Data(src.to_vec())),
            _ => Err(Error::PacketDecodeError(format!("Unknown tag: {}", tag))),
        }?;
        Ok(typed_payload)
    }

    pub fn length(&self) -> usize {
        match self {
            Self::ControlHello | Self::ControlClose | Self::ControlKeepAlive => 0,
            Self::ControlPing { .. } | Self::ControlPong { .. } => 4,
            Self::Data(data) => data.len(),
        }
    }
}
