//! Async runtime implementation for the Network Character Device Protocol based on tokio.

mod config;
mod connection;
pub use connection::ConnHandler;
pub mod error;
mod runtime;

use std::sync::OnceLock;

use crate::config::Config;
use crate::connection::ConnStatus;
use crate::error::{ConnectionClosed, ConnectionCreateError, ConnectionError};
use crate::runtime::{OpenParams, Runtime};

/// Global runtime singleton (placeholder for future keepalive scheduling).
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new())
}

/// Open a connection by connect to a host or listen for a device connection.
pub async fn open(params: OpenParams) -> Result<ConnHandler, ConnectionCreateError> {
    get_runtime().open(params).await
}

/// Close the connection gracefully.
/// Returns Ok(Res) where Res is the result of the connection task,
/// or Err if the connection task has already finished.
pub async fn close(mut conn: ConnHandler) -> Result<Result<(), ConnectionError>, ConnectionClosed> {
    conn.close().await
}

/// Read a sequence of bytes.
/// Guarantees that this bytes is send by peer in a single time.
pub async fn read(conn: &mut ConnHandler) -> Result<Vec<u8>, ConnectionClosed> {
    conn.read().await
}

/// Write a sequence of bytes.
/// Guarantees that this bytes will be sent as a single Packet,
/// and will be received as a single Packet on the other end.
pub async fn write(conn: &mut ConnHandler, buf: &[u8]) -> Result<(), ConnectionClosed> {
    conn.write(buf).await
}

/// Get the connection status, including the latest rtt, connection state, etc.
pub async fn status(conn: &mut ConnHandler) -> Result<ConnStatus, ConnectionClosed> {
    conn.get_status().await
}

/// Get current runtime configuration.
pub async fn get_config() -> Config {
    get_runtime().get_config().await
}

/// Set current runtime configuration.
/// Note: The configuration item is used for advanced management,
///       for simple usage, you can use the default configuration.
pub async fn set_config(cfg: config::Config) {
    get_runtime().set_config(cfg.clone()).await;
}
