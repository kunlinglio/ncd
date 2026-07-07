use std::collections::VecDeque;
use std::io::{BufRead, Write};

use crate::error::Error;

use super::DEFAULT_MAX_PAYLOAD_SIZE;
use super::frame::{Flag, Frame, HEADER_SIZE_BYTE};
use super::packet::Packet;

/// Fragment a Packet into multiple Frames if necessary, based on the maximum payload size.
fn packet_to_frames(packet: &Packet) -> Vec<Frame> {
    let payload_length = packet.length();
    if payload_length <= DEFAULT_MAX_PAYLOAD_SIZE {
        let (ty, payload) = packet.encode_body();
        vec![Frame {
            flag: Flag::End,
            ty,
            payload,
        }]
    } else {
        let mut frames = Vec::new();
        let mut offset = 0;
        let (ty, bytes) = packet.encode_body();
        while offset < payload_length {
            let remaining = payload_length - offset;
            let chunk_size = std::cmp::min(remaining, DEFAULT_MAX_PAYLOAD_SIZE);
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
fn frames_to_packet(frames: &[Frame]) -> Result<Option<(usize, Packet)>, Error> {
    if frames.is_empty() {
        return Err(Error::PacketDecodeError(
            "No frames provided for packet reconstruction".into(),
        ));
    }

    let first_frame = &frames[0];
    let ty = first_frame.ty;
    let mut payload = Vec::new();

    let mut consumed_frames = 0;
    let mut finished_packet = false;
    for frame in frames {
        if frame.ty != ty {
            return Err(Error::PacketDecodeError(
                "Mismatched types in frames".into(),
            ));
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

    let packet = Packet::decode_body(ty, &payload)?;
    Ok(Some((consumed_frames, packet)))
}

pub fn write_packet<W: Write>(writer: &mut W, packet: &Packet) -> Result<(), Error> {
    let frames = packet_to_frames(packet);
    for frame in frames {
        let bytes = frame.encode();
        writer.write_all(&bytes)?;
    }
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: BufRead>(reader: &mut R) -> Result<Option<Frame>, Error> {
    let available = reader.fill_buf()?;
    let Some((_ty, _flag, payload_len)) = Frame::peek_head(available)? else {
        return Ok(None);
    };
    let frame_len = HEADER_SIZE_BYTE + payload_len;

    if available.len() < frame_len {
        return Ok(None);
    }

    let frame = Frame::decode(&available[..frame_len])?;
    reader.consume(frame_len);

    Ok(Some(frame))
}

pub fn read_packet(frame_buf: &mut VecDeque<Frame>) -> Result<Option<Packet>, Error> {
    if frame_buf.is_empty() {
        return Ok(None);
    }

    let Some(res) = frames_to_packet(&frame_buf.make_contiguous())? else {
        return Ok(None);
    };
    let (consumed, packet) = res;

    for _ in 0..consumed {
        frame_buf.pop_front();
    }

    Ok(Some(packet))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Packet::ControlHello,
            Packet::ControlClose,
            Packet::ControlKeepAlive,
            Packet::ControlPing { id: 42 },
            Packet::ControlPong { id: 99 },
            Packet::Data(b"hello world".to_vec()),
            Packet::Data(vec![]),
            Packet::Data(vec![0xCC; DEFAULT_MAX_PAYLOAD_SIZE + 1]), // multi-frame
        ]
    }

    #[test]
    fn roundtrip_all_variants() {
        for pkt in &all_variants() {
            let mut wire = Vec::new();
            write_packet(&mut wire, pkt).unwrap();

            let mut cursor = Cursor::new(wire);
            let mut frames: Vec<Frame> = Vec::new();
            while let Some(frame) = read_frame(&mut cursor).unwrap() {
                frames.push(frame);
            }

            assert!(!frames.is_empty(), "no frames for {pkt:?}");
            let (consumed, decoded) = frames_to_packet(&frames).unwrap().unwrap();
            assert_eq!(consumed, frames.len());
            assert_eq!(&decoded, pkt, "roundtrip mismatch for {pkt:?}");
        }
    }

    #[test]
    fn full_roundtrip_all_variants() {
        for pkt in &all_variants() {
            let mut wire = Vec::new();
            write_packet(&mut wire, pkt).unwrap();

            let mut frame_buf: VecDeque<Frame> = VecDeque::new();
            {
                let mut cursor = Cursor::new(wire);
                while let Some(frame) = read_frame(&mut cursor).unwrap() {
                    frame_buf.push_back(frame);
                }
            }

            let assembled = read_packet(&mut frame_buf).unwrap().unwrap();
            assert_eq!(&assembled, pkt, "full roundtrip mismatch for {pkt:?}");
            assert!(frame_buf.is_empty(), "buffer not drained for {pkt:?}");
        }
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
    fn frames_to_packet_empty_is_error() {
        assert!(matches!(
            frames_to_packet(&[]).unwrap_err(),
            Error::PacketDecodeError(msg) if msg.contains("No frames")
        ));
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
            Error::PacketDecodeError(msg) if msg.contains("Mismatched")
        ));
    }

    #[test]
    fn read_frame_incomplete_header_returns_none() {
        let mut reader = ChunkedReader::new(vec![0x4E, 0x43, 0x44], 3);
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn read_frame_incomplete_frame_returns_none() {
        let mut data = vec![];
        data.extend_from_slice(super::super::MAGIC_NUMBER);
        data.push(super::super::VERSION);
        data.extend_from_slice(&100u16.to_be_bytes()); // claim 100-byte payload
        data.push(0x00); // flag End
        data.push(0x01); // type Hello
        assert_eq!(data.len(), 8);
        assert!(read_frame(&mut Cursor::new(data)).unwrap().is_none());
    }

    #[test]
    fn read_frame_rejects_bad_magic() {
        let mut data = vec![0xFF; 8];
        data[3] = super::super::VERSION;
        assert!(matches!(
            read_frame(&mut Cursor::new(data)).unwrap_err(),
            Error::PacketDecodeError(msg) if msg.contains("magic")
        ));
    }

    #[test]
    fn read_packet_buffer_empty_returns_none() {
        assert!(read_packet(&mut VecDeque::new()).unwrap().is_none());
    }

    #[test]
    fn read_packet_buffer_incomplete_returns_none() {
        let mut buf = VecDeque::new();
        buf.push_back(Frame {
            flag: Flag::More,
            ty: 0x06,
            payload: b"partial".to_vec(),
        });
        assert!(read_packet(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 1, "incomplete frames must stay in buffer");
    }

    #[test]
    fn read_packet_drains_only_consumed() {
        let mut buf = VecDeque::new();
        buf.push_back(Frame {
            flag: Flag::More,
            ty: 0x06,
            payload: b"hello ".to_vec(),
        });
        buf.push_back(Frame {
            flag: Flag::End,
            ty: 0x06,
            payload: b"world".to_vec(),
        });
        buf.push_back(Frame {
            flag: Flag::End,
            ty: 0x01,
            payload: vec![],
        }); // next pkt

        let pkt = read_packet(&mut buf).unwrap().unwrap();
        assert!(matches!(&pkt, Packet::Data(d) if d == b"hello world"));
        assert_eq!(buf.len(), 1, "unconsumed frame should remain");
    }
}
