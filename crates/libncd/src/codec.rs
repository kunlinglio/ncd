use crate::error::{AssemblePacketError, Error};

use super::MAX_PAYLOAD_SIZE;
use super::frame::{Flag, Frame};
use super::packet::Packet;

/// Fragment a Packet into multiple Frames if necessary, based on the maximum payload size.
pub fn packet_to_frames(packet: &Packet) -> Vec<Frame> {
    let payload_length = packet.length();
    if payload_length <= MAX_PAYLOAD_SIZE {
        let (ty, payload) = packet.encode();
        vec![Frame {
            flag: Flag::End,
            ty,
            payload,
        }]
    } else {
        let mut frames = Vec::new();
        let mut offset = 0;
        let (ty, bytes) = packet.encode();
        assert_eq!(bytes.len(), payload_length, "Packet length mismatch");
        while offset < payload_length {
            let remaining = payload_length - offset;
            let chunk_size = std::cmp::min(remaining, MAX_PAYLOAD_SIZE);
            let chunk_payload = bytes[offset..offset + chunk_size].to_vec();
            let flag = if remaining > chunk_size {
                Flag::More
            } else {
                Flag::End
            };
            frames.push(Frame {
                flag,
                ty,
                payload: chunk_payload,
            });
            offset += chunk_size;
        }
        frames
    }
}

/// Reassembles a Packet from a sequence of Frames
/// Returns: (consumed_frames, Packet)
pub fn frames_to_packet(frames: &[Frame]) -> Result<Option<(usize, Packet)>, Error> {
    if frames.is_empty() {
        return Ok(None);
    }

    let first_frame = &frames[0];
    let ty = first_frame.ty;
    let mut payload = Vec::new();

    let mut consumed_frames = 0;
    let mut finished_packet = false;
    for frame in frames {
        if frame.ty != ty {
            return Err(AssemblePacketError::MismatchedTypes {
                first_frame_type: ty,
                frame_type: frame.ty,
            }
            .into());
        }
        payload.extend_from_slice(&frame.payload);
        consumed_frames += 1;
        if frame.flag == Flag::End {
            finished_packet = true;
            break;
        }
    }
    if !finished_packet {
        return Ok(None);
    }

    let packet = Packet::decode(ty, &payload)?;
    Ok(Some((consumed_frames, packet)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::io::Cursor;

    /// A BufRead wrapper that only exposes `limit` bytes at a time via
    /// `fill_buf`, simulating a TCP stream that hasn't buffered a full
    /// frame yet.
    struct ChunkedReader {
        inner: Cursor<Vec<u8>>,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                inner: Cursor::new(data),
                chunk_size,
            }
        }
    }

    impl std::io::Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let limit = buf.len().min(self.chunk_size);
            self.inner.read(&mut buf[..limit])
        }
    }

    impl BufRead for ChunkedReader {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            let available = self.inner.fill_buf()?;
            let limit = available.len().min(self.chunk_size);
            Ok(&available[..limit])
        }

        fn consume(&mut self, amt: usize) {
            self.inner.consume(amt);
        }
    }

    fn all_variants() -> Vec<Packet> {
        vec![
            Packet::ControlHello {
                keep_alive_interval_ms: 1000,
            },
            Packet::ControlClose,
            Packet::ControlKeepAlive,
            Packet::ControlPing { id: 42 },
            Packet::ControlPong { id: 99 },
            Packet::Data(b"hello world".to_vec()),
            Packet::Data(vec![]),
            Packet::Data(vec![0xCC; MAX_PAYLOAD_SIZE + 1]), // multi-frame
        ]
    }

    #[test]
    fn frames_to_packet_incomplete_returns_none() {
        let frames = vec![Frame {
            flag: Flag::More, // no End — need more data
            ty: 0x06,
            payload: b"partial".to_vec(),
        }];
        assert!(frames_to_packet(&frames).unwrap().is_none());
    }

    #[test]
    fn frames_to_packet_empty_returns_none() {
        assert!(frames_to_packet(&[]).unwrap().is_none());
    }

    #[test]
    fn frames_to_packet_type_mismatch() {
        let frames = vec![
            Frame {
                flag: Flag::More,
                ty: 0x01,
                payload: vec![],
            },
            Frame {
                flag: Flag::End,
                ty: 0x06,
                payload: b"data".to_vec(),
            },
        ];
        assert!(matches!(
            frames_to_packet(&frames).unwrap_err(),
            Error::AssemblePacketError(AssemblePacketError::MismatchedTypes {
                first_frame_type: 0x01,
                frame_type: 0x06,
            })
        ));
    }
}
