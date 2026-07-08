use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum Error {
    #[error(transparent)]
    PacketDecodeError(#[from] PacketDecodeError),

    #[error(transparent)]
    FrameDecodeError(#[from] FrameDecodeError),

    #[error(transparent)]
    AssemblePacketError(#[from] AssemblePacketError),
}

#[derive(Debug, Error, Clone)]
pub enum PacketDecodeError {
    #[error("Packet data length mismatch: expected {expected}, got {got}. {details}")]
    DataLengthMismatch {
        expected: usize,
        got: usize,
        details: String,
    },
    #[error("Unknown packet tag: {0:#04x}")]
    UnknownTag(u8),
}

#[derive(Debug, Error, Clone)]
pub enum FrameDecodeError {
    #[error("Invalid flag value: {0:#04x}")]
    InvalidFlag(u8),
    #[error("Invalid magic number: expected {expected:?}, got {actual:?}")]
    InvalidMagicNumber { expected: [u8; 3], actual: [u8; 3] },
    #[error("Invalid version: expected {expected}, got {actual}")]
    InvalidVersion { expected: u8, actual: u8 },
    #[error("Data length shorter than header size: expected at least {expected}, got {got}")]
    DataShorterThanHeader { expected: usize, got: usize },
    #[error("Data length shorter than total length: expected at least {expected}, got {got}")]
    DataTooShort { expected: usize, got: usize },
}

#[derive(Debug, Error, Clone)]
pub enum AssemblePacketError {
    #[error("Mismatched types in frames")]
    MismatchedTypes {
        first_frame_type: u8,
        frame_type: u8,
    },
}
