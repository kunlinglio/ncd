pub mod connection;
pub mod device;
pub mod error;
pub mod registry;
pub mod session;
use crate::device::NcdDeviceOperations;
use tokio;

enum ConnectMode {
    Active,
    Passive,
}
fn run_active() {
    println!("Running in active mode");
}

use std::net::{IpAddr, Ipv4Addr};

async fn run_passive() {
    let conn = connection::NcdConnection::create_connection(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        9876,
    )
    .await
    .unwrap();

    let device = Box::new(device::NcdDevice::new("/dev/test".into()));

    let mut session = session::NcdSession::new(conn, device);
    if let Err(e) = session.run().await {
        eprintln!("Session error: {e}");
    }
}

#[tokio::main]
async fn main() {
    let mode = ConnectMode::Passive;
    match mode {
        ConnectMode::Active => run_active(),
        ConnectMode::Passive => run_passive().await,
    }
}
