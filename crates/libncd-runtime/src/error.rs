use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error, Clone)]
pub enum ConnectionCreateError {
    #[error("IO error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("Connect timeout after {timeout:?} to {addr}")]
    ConnectTimeout { addr: SocketAddr, timeout: Duration },

    #[error("Handshake error: {0}")]
    HandshakeError(String),

    #[error("Send/Receive packet error: {0}")]
    PacketTransferError(#[from] ConnectionError),
}

impl From<std::io::Error> for ConnectionCreateError {
    fn from(err: std::io::Error) -> Self {
        ConnectionCreateError::Io(Arc::new(err))
    }
}

#[derive(Debug, Error, Clone)]
pub enum ConnectionClosed {
    #[error("Closed normally")]
    Normal,

    #[error("Closed due to error: {0}")]
    Error(#[from] ConnectionError),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

#[derive(Debug, Error, Clone)]
pub enum ConnectionError {
    #[error("IO error: {0}")]
    Io(Arc<std::io::Error>),

    #[error("Protocol error: {0}")]
    ProtocolError(#[from] libncd::error::Error),

    #[error("Peer inactive timeout")]
    PeerInactiveTimeout,

    #[error("Received unexpected EOF")]
    UnexpectedEOF,

    #[error("Close timeout")]
    CloseTimeout,

    #[error("Peer closed the connection")]
    PeerClosed,
}

impl From<std::io::Error> for ConnectionError {
    fn from(err: std::io::Error) -> Self {
        ConnectionError::Io(Arc::new(err))
    }
}
