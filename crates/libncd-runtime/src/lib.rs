//! Async runtime implementation for the Network Character Device Protocol based on tokio.

mod config;
mod connection;
pub use connection::Connection;
pub mod error;
mod runtime;
mod status;

use std::sync::OnceLock;

use error::Error;

use crate::runtime::{OpenParams, Runtime};
use crate::status::Status;

const DEFAULT_LOCAL_PORT: u16 = 7867;

/// Global runtime singleton (placeholder for future keepalive scheduling).
#[allow(dead_code)]
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[allow(dead_code)]
fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new())
}

pub async fn open(params: OpenParams) -> Result<Connection, Error> {
    get_runtime().open(params).await
}

pub async fn close(conn: Connection) -> Result<(), Error> {
    unimplemented!("Closing a connection is not yet implemented");
}

pub async fn read(conn: &Connection, buf: &mut [u8]) -> Result<usize, Error> {
    unimplemented!("Reading from a connection is not yet implemented");
}

pub async fn write(conn: &Connection, buf: &[u8]) -> Result<usize, Error> {
    unimplemented!("Writing to a connection is not yet implemented");
}

pub async fn status(conn: &Connection) -> Result<Status, Error> {
    unimplemented!("Getting connection status is not yet implemented");
}

pub fn get_config(_conn: &Connection, _cfg: &config::Config) -> Result<(), Error> {
    unimplemented!("Getting connection configuration is not yet implemented");
}

pub fn set_config(_conn: &Connection, _cfg: &config::Config) -> Result<(), Error> {
    unimplemented!("Setting connection configuration is not yet implemented");
}
