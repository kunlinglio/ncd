use crate::error::Error;

#[repr(u8)]
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    ControlHello = 0x01,
    ControlClose = 0x02,
    ControlKeepAlive = 0x03,
    ControlPing { id: u32 } = 0x04,
    ControlPong { id: u32 } = 0x05,
    Data(Vec<u8>) = 0x06,
}

impl Packet {
    fn tag(&self) -> u8 {
        match self {
            Self::ControlHello => 0x01,
            Self::ControlClose => 0x02,
            Self::ControlKeepAlive => 0x03,
            Self::ControlPing { .. } => 0x04,
            Self::ControlPong { .. } => 0x05,
            Self::Data(_) => 0x06,
        }
    }

    /// TODO: Optimize memory allocation
    pub(crate) fn encode_body(&self) -> (u8, Vec<u8>) {
        let tag = self.tag();
        let payload = match self {
            Self::ControlHello | Self::ControlClose | Self::ControlKeepAlive => vec![],
            Self::ControlPing { id } => id.to_be_bytes().to_vec(),
            Self::ControlPong { id } => id.to_be_bytes().to_vec(),
            Self::Data(data) => data.clone(),
        };
        (tag, payload)
    }

    pub(crate) fn decode_body(tag: u8, src: &[u8]) -> Result<Self, Error> {
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

    pub(crate) fn length(&self) -> usize {
        match self {
            Self::ControlHello | Self::ControlClose | Self::ControlKeepAlive => 0,
            Self::ControlPing { .. } | Self::ControlPong { .. } => 4,
            Self::Data(data) => data.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hello() {
        let pkt = Packet::ControlHello;
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_close() {
        let pkt = Packet::ControlClose;
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_keepalive() {
        let pkt = Packet::ControlKeepAlive;
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_ping() {
        let pkt = Packet::ControlPing { id: 42 };
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_pong() {
        let pkt = Packet::ControlPong { id: 99 };
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_data() {
        let pkt = Packet::Data(b"hello world".to_vec());
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn roundtrip_empty_data() {
        let pkt = Packet::Data(vec![]);
        let (ty, body) = pkt.encode_body();
        let decoded = Packet::decode_body(ty, &body).unwrap();
        assert_eq!(decoded, pkt);
    }

    #[test]
    fn decode_body_rejects_short_ping() {
        let err = Packet::decode_body(0x04, &[0x00, 0x00, 0x00]).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("ControlPing")),
            "expected ControlPing truncation error, got {err:?}"
        );
    }

    #[test]
    fn decode_body_rejects_short_pong() {
        let err = Packet::decode_body(0x05, &[0x00]).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("ControlPong")),
            "expected ControlPong truncation error, got {err:?}"
        );
    }

    #[test]
    fn decode_body_rejects_unknown_tag() {
        let err = Packet::decode_body(0xFF, &[]).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("Unknown tag")),
            "expected unknown tag error, got {err:?}"
        );
    }
}
