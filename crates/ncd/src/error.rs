use libncd_runtime::error::{ConnectionClosed, ConnectionCreateError};
use thiserror::Error;

// ── 顶层错误 ──

#[derive(Debug, Error)]
pub enum NcdError {
    #[error(transparent)]
    CreateConnectionError(#[from] ConnectionCreateError),

    #[error(transparent)]
    CloseConnectionError(ConnectionClosed),

    #[error(transparent)]
    InnerConnectionError(ConnectionClosed),

    #[error(transparent)]
    RegistryError(#[from] RegistryError),

    #[error(transparent)]
    DeviceError(#[from] DeviceError),
}

// ── Registry 错误 ──

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("Device already registered: {0}")]
    DeviceAlreadyRegistered(String),
}

// ── Device 错误 ──

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
