use super::{DEFAULT_MAX_PAYLOAD_SIZE, MAGIC_NUMBER, VERSION};
use crate::error::Error;

/// Frame represents a single frame in the Network Character Device Protocol.
/// Frame Structure:
/// 0                  16      24        32 bit
/// |       Magic Number       | Version |
/// |  Payload Length  |  Flag |  Type   |
/// |         Payload (variable)         |
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    // Magic Number and Version are handled by encoding/decoding functions
    pub flag: Flag,
    pub ty: u8, // type
    pub payload: Vec<u8>,
}
pub const HEADER_SIZE_BYTE: usize = 8;

/// Sequence flags
/// If the user request is larger than the maximum payload size, the request will be split into multiple frames.
/// - The first frame: More (1)
/// - Middle frames: More (1)
/// - The Last frame: End (0)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Frame {
    /// TODO: Optimize memory allocation
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_SIZE_BYTE + self.payload.len());
        buf.extend_from_slice(MAGIC_NUMBER); // 24 bit
        buf.extend_from_slice(&VERSION.to_be_bytes()); // 8 bit
        assert!(
            self.payload.len() <= DEFAULT_MAX_PAYLOAD_SIZE,
            "Payload length exceeds DEFAULT_MAX_PAYLOAD_SIZE ({} > {})",
            self.payload.len(),
            DEFAULT_MAX_PAYLOAD_SIZE
        );
        let length = self.payload.len() as u16;
        buf.extend_from_slice(&length.to_be_bytes()); // 16 bit
        buf.push(self.flag as u8); // 8 bit
        buf.push(self.ty); // 8 bit
        assert_eq!(buf.len(), HEADER_SIZE_BYTE);

        buf.extend_from_slice(&self.payload);
        buf
    }

    pub(crate) fn decode(src: &[u8]) -> Result<Self, Error> {
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
        let ty = src[7];
        if src.len() < HEADER_SIZE_BYTE + length {
            return Err(Error::PacketDecodeError(
                "Source data is too short for payload".into(),
            ));
        }
        let payload = src[HEADER_SIZE_BYTE..HEADER_SIZE_BYTE + length].to_vec();

        Ok(Frame {
            flag: Flag::try_from(flag)?,
            ty,
            payload,
        })
    }

    /// Returns: (type, flag, payload length)
    pub(crate) fn peek_head(src: &[u8]) -> Result<Option<(u8, Flag, usize)>, Error> {
        if src.len() < HEADER_SIZE_BYTE {
            return Ok(None);
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
        let ty = src[7];

        Ok(Some((ty, Flag::try_from(flag)?, length)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty_payload() {
        let frame = Frame {
            flag: Flag::End,
            ty: 0x01,
            payload: vec![],
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn roundtrip_data_payload() {
        let frame = Frame {
            flag: Flag::End,
            ty: 0x06,
            payload: b"hello world".to_vec(),
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn roundtrip_with_more_flag() {
        let frame = Frame {
            flag: Flag::More,
            ty: 0x06,
            payload: vec![0xAA; 1024],
        };
        let bytes = frame.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn roundtrip_max_payload() {
        let frame = Frame {
            flag: Flag::End,
            ty: 0x06,
            payload: vec![0xBB; DEFAULT_MAX_PAYLOAD_SIZE],
        };
        let bytes = frame.encode();
        assert_eq!(bytes.len(), HEADER_SIZE_BYTE + DEFAULT_MAX_PAYLOAD_SIZE);
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn peek_head_parses_correctly() {
        let frame = Frame {
            flag: Flag::End,
            ty: 0x04,
            payload: vec![0x00, 0x00, 0x00, 0x2A], // id = 42
        };
        let bytes = frame.encode();
        let Some((ty, flag, payload_len)) = Frame::peek_head(&bytes).unwrap() else {
            panic!("Failed to peek at frame header");
        };
        assert_eq!(ty, frame.ty);
        assert_eq!(flag, frame.flag);
        assert_eq!(payload_len, frame.payload.len());
    }

    #[test]
    fn peek_head_on_zero_payload() {
        let frame = Frame {
            flag: Flag::End,
            ty: 0x01,
            payload: vec![],
        };
        let bytes = frame.encode();
        let Some((_, _, payload_len)) = Frame::peek_head(&bytes).unwrap() else {
            panic!("Failed to peek at frame header");
        };
        assert_eq!(payload_len, 0);
    }

    #[test]
    fn peek_head_rejects_short_data() {
        let empty = Frame::peek_head(&[0x4E, 0x43]).unwrap();
        assert_eq!(empty, None, "Expected None for short data");
    }

    #[test]
    fn decode_rejects_short_header() {
        let err = Frame::decode(&[0x4E, 0x43]).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("too short")),
            "expected 'too short' error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_bad_magic() {
        let mut bytes = vec![0xFF; 8];
        bytes[3] = super::VERSION;
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("magic")),
            "expected magic error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_bad_version() {
        let mut bytes = vec![];
        bytes.extend_from_slice(super::MAGIC_NUMBER);
        bytes.push(0xFF); // bad version
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.push(0x00); // flag
        bytes.push(0x01); // type
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("version")),
            "expected version error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_truncated_payload() {
        let mut bytes = vec![];
        bytes.extend_from_slice(super::MAGIC_NUMBER);
        bytes.push(super::VERSION);
        bytes.extend_from_slice(&10u16.to_be_bytes()); // claim 10-byte payload
        bytes.push(0x00); // flag
        bytes.push(0x01); // type
        // No payload appended
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("too short")),
            "expected 'too short' error, got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_invalid_flag() {
        let mut bytes = vec![];
        bytes.extend_from_slice(super::MAGIC_NUMBER);
        bytes.push(super::VERSION);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.push(0xFF); // invalid flag
        bytes.push(0x01); // type
        let err = Frame::decode(&bytes).unwrap_err();
        assert!(
            matches!(&err, Error::PacketDecodeError(msg) if msg.contains("flag")),
            "expected flag error, got {err:?}"
        );
    }
}
