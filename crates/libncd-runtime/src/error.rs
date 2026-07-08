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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_error_displays_connect_timeout() {
        let err = ConnectionCreateError::ConnectTimeout {
            addr: "127.0.0.1:8080".parse().unwrap(),
            timeout: Duration::from_secs(3),
        };
        let msg = err.to_string();
        assert!(msg.contains("timeout"), "expected 'timeout' in: {msg}");
        assert!(msg.contains("3s"), "expected '3s' in: {msg}");
    }

    #[test]
    fn create_error_displays_handshake() {
        let err = ConnectionCreateError::HandshakeError("bad packet".into());
        assert!(err.to_string().contains("bad packet"));
    }

    #[test]
    fn create_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = ConnectionCreateError::from(io_err);
        let msg = err.to_string();
        assert!(msg.contains("missing"), "expected 'missing' in: {msg}");
    }

    #[test]
    fn closed_displays_normal() {
        assert!(ConnectionClosed::Normal.to_string().contains("normally"));
    }

    #[test]
    fn closed_displays_error() {
        let e = ConnectionClosed::Error(ConnectionError::PeerInactiveTimeout);
        assert!(e.to_string().contains("Peer inactive timeout"));
    }

    #[test]
    fn connection_error_clone() {
        let e1 = ConnectionError::PeerClosed;
        let e2 = e1.clone();
        assert_eq!(e1.to_string(), e2.to_string());
    }

    #[test]
    fn protocol_error_conversion() {
        let pkt_err = libncd::error::PacketDecodeError::UnknownTag(0xFF);
        let lib_err = libncd::error::Error::from(pkt_err);
        let conn_err = ConnectionError::ProtocolError(lib_err);
        assert!(conn_err.to_string().contains("0xff"));
    }
}
