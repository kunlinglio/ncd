use libncd_runtime::error::{ConnectionClosed, ConnectionCreateError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NcdError {
    #[error(transparent)]
    CreateConnectionError(#[from] ConnectionCreateError),

    #[error(transparent)]
    CloseConnectionError(ConnectionClosed),

    #[error(transparent)]
    InnerConnectionError(ConnectionClosed),

    #[error(transparent)]
    DeviceError(#[from] DeviceError),
}

/// Device-level errors.
#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("Device is not open: {0}")]
    NotOpen(String),

    #[error("Device is already open: {0}")]
    AlreadyOpen(String),

    #[error("Device I/O error: {0}")]
    Io(String),

    #[error("Unsupported device operation '{operation}' for device '{device}'")]
    UnsupportedOperation {
        device: String,
        operation: &'static str,
    },
}
