//! Network Character Device Protocol implementation.

pub mod config;
pub mod connection;
pub mod error;
pub mod protocol;
pub mod status;

use connection::Connection;
use status::Status;

pub async fn open() -> Result<Connection, String> {
    Ok(Connection::new())
}

pub async fn close(conn: Connection) -> Result<(), String> {
    println!("Closing connection: {:?}", conn);
    Ok(())
}

pub async fn read(conn: &Connection, buf: &mut [u8]) -> Result<usize, String> {
    println!("Reading from connection: {:?}", conn);
    println!("Buffer: {:?}", buf);
    Ok(0)
}

pub async fn write(conn: &Connection, buf: &[u8]) -> Result<usize, String> {
    println!("Writing to connection: {:?}", conn);
    println!("Buffer: {:?}", buf);
    Ok(buf.len())
}

pub async fn status(conn: &Connection) -> Result<Status, String> {
    println!("Getting status of connection: {:?}", conn);
    Ok(Status::new())
}

pub fn get_config(conn: &Connection, cfg: &config::Config) -> Result<(), String> {
    println!("Configuring connection: {:?}", conn);
    println!("Config: {:?}", cfg);
    Ok(())
}

pub fn set_config(conn: &Connection, cfg: &config::Config) -> Result<(), String> {
    println!("Setting config for connection: {:?}", conn);
    println!("Config: {:?}", cfg);
    Ok(())
}
