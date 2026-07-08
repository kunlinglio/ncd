use std::net::IpAddr;
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use std::vec::Vec;

use crate::config::Config;
use crate::connection::Connection;

/// Async runtime for connection management and keepalive etc.
#[allow(dead_code)]
pub struct Runtime {
    config: RwLock<Config>,
    connections: Vec<Mutex<Connection>>,
}

pub enum OpenParams {
    Host,
    Device { host_addr: IpAddr, host_port: u16 },
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            config: RwLock::new(Default::default()),
            connections: Vec::new(),
        }
    }

    pub async fn open(&self, params: OpenParams) -> Result<Connection, crate::error::Error> {
        let config_guard = self
            .config
            .read()
            .expect("Failed to acquire read lock on config");
        match params {
            OpenParams::Host => {
                let on_addr = config_guard.local_addr;
                let on_port = config_guard.local_port;
                Connection::listen(on_addr, on_port).await
            }
            OpenParams::Device {
                host_addr,
                host_port,
            } => {
                let connect_timeout = Duration::from_millis(config_guard.connect_timeout_ms as u64);
                Connection::connect(host_addr, host_port, connect_timeout).await
            }
        }
    }
}
