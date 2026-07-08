use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Connect timeout after {timeout:?} to {addr}")]
    ConnectTimeout { addr: SocketAddr, timeout: Duration },

    #[error("Protocol error: {0}")]
    ProtocolError(#[from] libncd::error::Error),

    #[error("Connection closed by peer")]
    ConnectionClosed,
}
