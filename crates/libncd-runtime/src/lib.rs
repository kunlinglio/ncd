//! Async runtime for the Network Character Device Protocol.
//!
//! Built on tokio, sits on top of the synchronous [`libncd`] protocol layer.
//! Provides TCP connection management, transparent frame fragmentation/
//! reassembly, and automatic control-packet handling (Ping→Pong, Close).

mod config;
mod connection;
pub mod error;
mod runtime;
mod status;

use std::sync::OnceLock;

pub use connection::Connection;

use runtime::Runtime;
use status::Status;

use error::Error;

/// Global runtime singleton (placeholder for future keepalive scheduling).
#[allow(dead_code)]
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

#[allow(dead_code)]
fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime {})
}

/// Open a connection to an NCD daemon at `addr` (e.g. "127.0.0.1:9876").
///
/// Performs the TCP connect and NCD ControlHello handshake before returning.
pub async fn open(addr: &str) -> Result<Connection, Error> {
    Connection::connect(addr).await
}

/// Gracefully close the connection. Sends ControlClose and shuts down the TCP
/// stream. Consumes the `Connection` so it cannot be used afterwards.
pub async fn close(conn: Connection) -> Result<(), Error> {
    conn.shutdown().await
}

/// Read data from the connection into `buf`.
///
/// Returns the number of bytes copied. Handles control packets (Ping→Pong,
/// Close, KeepAlive) transparently. Blocks until data is available or the
/// connection is closed.
pub async fn read(conn: &Connection, buf: &mut [u8]) -> Result<usize, Error> {
    conn.read_data(buf).await
}

/// Write `buf` to the connection as a Data packet.
///
/// Returns the number of bytes accepted (always `buf.len()` on success).
/// The data may be fragmented into multiple frames automatically.
pub async fn write(conn: &Connection, buf: &[u8]) -> Result<usize, Error> {
    conn.write_data(buf).await
}

/// Query the current connection status.
pub async fn status(conn: &Connection) -> Result<Status, Error> {
    Ok(Status::new(conn.state(), conn.peer_addr()))
}

/// Get the current configuration (placeholder — returns Ok for now).
pub fn get_config(_conn: &Connection, _cfg: &config::Config) -> Result<(), Error> {
    Ok(())
}

/// Set the connection configuration (placeholder — returns Ok for now).
pub fn set_config(_conn: &Connection, _cfg: &config::Config) -> Result<(), Error> {
    Ok(())
}
