use crate::error::{Error, PacketDecodeError};

const CONTROL_HELLO_TAG: u8 = 0x01;
const CONTROL_HELLO_ACK_TAG: u8 = 0x02;
const CONTROL_CLOSE_TAG: u8 = 0x03;
const CONTROL_KEEP_ALIVE_TAG: u8 = 0x04;
const CONTROL_PING_TAG: u8 = 0x05;
const CONTROL_PONG_TAG: u8 = 0x06;
const DATA_TAG: u8 = 0x07;

#[repr(u8)]
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    ControlHello { keep_alive_interval_ms: u32 } = CONTROL_HELLO_TAG,
    ControlHelloAck { keep_alive_interval_ms: u32 } = CONTROL_HELLO_ACK_TAG,
    ControlClose = CONTROL_CLOSE_TAG,
    ControlKeepAlive = CONTROL_KEEP_ALIVE_TAG,
    ControlPing { id: u32 } = CONTROL_PING_TAG,
    ControlPong { id: u32 } = CONTROL_PONG_TAG,
    Data(Vec<u8>) = DATA_TAG,
}

impl Packet {
    fn tag(&self) -> u8 {
        match self {
            Self::ControlHello { .. } => CONTROL_HELLO_TAG,
            Self::ControlHelloAck { .. } => CONTROL_HELLO_ACK_TAG,
            Self::ControlClose => CONTROL_CLOSE_TAG,
            Self::ControlKeepAlive => CONTROL_KEEP_ALIVE_TAG,
            Self::ControlPing { .. } => CONTROL_PING_TAG,
            Self::ControlPong { .. } => CONTROL_PONG_TAG,
            Self::Data(_) => DATA_TAG,
        }
    }

    fn fixed_length(tag: u8) -> Option<usize> {
        match tag {
            CONTROL_CLOSE_TAG | CONTROL_KEEP_ALIVE_TAG => Some(0),
            CONTROL_HELLO_TAG | CONTROL_HELLO_ACK_TAG | CONTROL_PING_TAG | CONTROL_PONG_TAG => {
                Some(4)
            }
            DATA_TAG => None,
            _ => None,
        }
    }

    pub(crate) fn length(&self) -> usize {
        match self {
            Self::ControlHello { .. }
            | Self::ControlHelloAck { .. }
            | Self::ControlClose
            | Self::ControlKeepAlive
            | Self::ControlPing { .. }
            | Self::ControlPong { .. } => {
                let tag = self.tag();
                Self::fixed_length(tag).expect("Fixed length should be defined for control packets")
            }
            Self::Data(data) => data.len(),
        }
    }

    /// TODO: Optimize memory allocation
    pub(crate) fn encode(&self) -> (u8, Vec<u8>) {
        let tag = self.tag();
        let payload = match self {
            Self::ControlHello {
                keep_alive_interval_ms,
            } => keep_alive_interval_ms.to_be_bytes().to_vec(),
            Self::ControlHelloAck {
                keep_alive_interval_ms,
            } => keep_alive_interval_ms.to_be_bytes().to_vec(),
            Self::ControlClose | Self::ControlKeepAlive => vec![],
            Self::ControlPing { id } => id.to_be_bytes().to_vec(),
            Self::ControlPong { id } => id.to_be_bytes().to_vec(),
            Self::Data(data) => data.clone(),
        };
        (tag, payload)
    }

    pub(crate) fn decode(tag: u8, src: &[u8]) -> Result<Self, Error> {
        let expected_length = Self::fixed_length(tag);
        if let Some(expected_length) = expected_length {
            if src.len() != expected_length {
                return Err(PacketDecodeError::DataLengthMismatch {
                    expected: expected_length,
                    got: src.len(),
                    details: format!("Source data length mismatch for tag {tag:#04x}"),
                }
                .into());
            }
        }
        let typed_payload = match tag {
            CONTROL_HELLO_TAG => Ok(Self::ControlHello {
                keep_alive_interval_ms: u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
            }),
            CONTROL_CLOSE_TAG => Ok(Self::ControlClose),
            CONTROL_KEEP_ALIVE_TAG => Ok(Self::ControlKeepAlive),
            CONTROL_PING_TAG => Ok(Self::ControlPing {
                id: u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
            }),
            CONTROL_PONG_TAG => Ok(Self::ControlPong {
                id: u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
            }),
            DATA_TAG => Ok(Self::Data(src.to_vec())),
            _ => Err(PacketDecodeError::UnknownTag(tag)),
        }?;
        Ok(typed_payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hello() {
        let pkt = Packet::ControlHello {
            keep_alive_interval_ms: 1000,
        };
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_close() {
        let pkt = Packet::ControlClose;
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_keepalive() {
        let pkt = Packet::ControlKeepAlive;
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_ping() {
        let pkt = Packet::ControlPing { id: 42 };
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_pong() {
        let pkt = Packet::ControlPong { id: 99 };
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_data() {
        let pkt = Packet::Data(b"hello world".to_vec());
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_empty_data() {
        let pkt = Packet::Data(vec![]);
        let (ty, body) = pkt.encode();
        let decoded = Packet::decode(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn decode_rejects_short_ping() {
        let err = Packet::decode(CONTROL_PING_TAG, &[0x00, 0x00, 0x00]).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::PacketDecodeError(PacketDecodeError::DataLengthMismatch {
                    expected: 4,
                    got: 3,
                    ..
                })
            ),
            "expected DataLengthMismatch(4, 3, ...) error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_short_pong() {
        let err = Packet::decode(CONTROL_PONG_TAG, &[0x00]).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::PacketDecodeError(PacketDecodeError::DataLengthMismatch {
                    expected: 4,
                    got: 1,
                    ..
                })
            ),
            "expected DataLengthMismatch(4, 1, ...) error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        let err = Packet::decode(0xFF, &[]).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::PacketDecodeError(PacketDecodeError::UnknownTag(0xFF))
            ),
            "expected UnknownTag(0xFF) error, got {err:?}"
        );
    }
}
