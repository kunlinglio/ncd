use std::fmt;

use crate::connection::ConnState;

#[derive(Debug)]
pub enum Error {
    /// An error from the underlying libncd protocol layer
    /// (frame decode failures, magic number mismatches, etc.).
    Protocol(libncd::error::Error),

    /// An I/O error from the TCP stream (connection reset, etc.).
    Io(std::io::Error),

    /// The operation cannot proceed because the connection is in the
    /// wrong lifecycle state.
    InvalidState {
        current: ConnState,
        expected: &'static str,
    },

    /// The peer sent ControlClose or the TCP stream returned EOF.
    ConnectionClosed,

    /// The initial handshake with the daemon failed.
    HandshakeFailed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Protocol(e) => write!(f, "Protocol error: {}", e),
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::InvalidState { current, expected } => {
                write!(
                    f,
                    "Invalid connection state: {:?} (expected: {})",
                    current, expected
                )
            }
            Error::ConnectionClosed => write!(f, "Connection closed"),
            Error::HandshakeFailed(msg) => write!(f, "Handshake failed: {}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Protocol(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<libncd::error::Error> for Error {
    fn from(e: libncd::error::Error) -> Self {
        Error::Protocol(e)
    }
}
