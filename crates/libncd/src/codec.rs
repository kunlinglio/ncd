use crate::MAX_PAYLOAD_SIZE;
use crate::error::{AssemblePacketError, Error};
use crate::frame::{Flag, Frame};
use crate::packet::Packet;

/// Fragment a Packet into multiple Frames if necessary, based on the maximum payload size.
/// The buf will be replaced by returned frames.
/// This will consumed Packet for performance.
pub fn fragment_packet(packet: Packet, buf: &mut Vec<Frame>) {
    buf.clear();
    let payload_length = packet.length();
    if payload_length <= MAX_PAYLOAD_SIZE {
        buf.reserve(1);
        let (ty, payload) = Packet::encode(packet);
        buf.push(Frame {
            flag: Flag::End,
            ty,
            payload,
        });
    } else {
        let frame_num = (payload_length + MAX_PAYLOAD_SIZE - 1) / MAX_PAYLOAD_SIZE;
        buf.reserve(frame_num);
        let (ty, bytes) = Packet::encode(packet);
        assert_eq!(bytes.len(), payload_length, "Packet length mismatch");
        bytes
            .chunks(MAX_PAYLOAD_SIZE)
            .enumerate()
            .for_each(|(i, chunk)| {
                let flag = if i == frame_num - 1 {
                    Flag::End
                } else {
                    Flag::More
                };
                buf.push(Frame {
                    flag,
                    ty,
                    payload: chunk.to_vec(),
                });
            });
        assert_eq!(buf.len(), frame_num, "Frame count mismatch");
    }
}

/// Reassembles a Packet from a sequence of Frames
/// Returns: (consumed_frames, Packet)
pub fn try_assemble_packet(frames: &[Frame]) -> Result<Option<(usize, Packet)>, Error> {
    // Verify and calculate the range
    let Some(end) = frames.iter().position(|f| f.flag == Flag::End) else {
        return Ok(None);
    };
    let frames = &frames[0..=end];

    // Calculate buffer size
    let buffer_size: usize = frames.iter().map(|f| f.payload.len()).sum();

    let ty = frames[0].ty; // if slice are empty, the function will returned above, so this is safe
    let mut payload = Vec::with_capacity(buffer_size);
    for frame in &frames[0..=end] {
        if frame.ty != ty {
            return Err(AssemblePacketError::MismatchedTypes {
                first_frame_type: ty,
                frame_type: frame.ty,
            }
            .into());
        }
        payload.extend_from_slice(&frame.payload);
    }
    let packet = Packet::decode(ty, &payload)?;
    Ok(Some((end + 1, packet)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_to_packet_incomplete_returns_none() {
        let frames = vec![Frame {
            flag: Flag::More, // no End — need more data
            ty: 0x06,
            payload: b"partial".to_vec(),
        }];
        assert!(try_assemble_packet(&frames).unwrap().is_none());
    }

    #[test]
    fn frames_to_packet_empty_returns_none() {
        assert!(try_assemble_packet(&[]).unwrap().is_none());
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
            try_assemble_packet(&frames).unwrap_err(),
            Error::AssemblePacketError(AssemblePacketError::MismatchedTypes {
                first_frame_type: 0x01,
                frame_type: 0x06,
            })
        ));
    }
}
