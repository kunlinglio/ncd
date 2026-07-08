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

pub async fn open(params: OpenParams) -> Result<ConnHandler, ConnectionCreateError> {
    get_runtime().open(params).await
}

pub async fn close(mut conn: ConnHandler) -> Result<Result<(), ConnectionError>, ConnectionClosed> {
    conn.close().await
}

pub async fn read(conn: &mut ConnHandler) -> Result<Vec<u8>, ConnectionClosed> {
    conn.read().await
}

pub async fn write(conn: &mut ConnHandler, buf: &[u8]) -> Result<(), ConnectionClosed> {
    conn.write(buf).await
}

pub async fn status(conn: &mut ConnHandler) -> Result<ConnStatus, ConnectionClosed> {
    conn.get_status().await
}

pub async fn get_config() -> Config {
    get_runtime().get_config().await
}

pub async fn set_config(cfg: config::Config) {
    get_runtime().set_config(cfg.clone()).await;
}
