use std::net::IpAddr;
use std::sync::{Mutex, RwLock};
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
    Host {
        listen_addr: IpAddr,
        listen_port: u16,
    },
    Device {
        host_addr: IpAddr,
        host_port: u16,
    },
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            config: RwLock::new(Default::default()),
            connections: Vec::new(),
        }
    }

    pub async fn open(&self, _params: OpenParams) -> Result<Connection, crate::error::Error> {
        let config_guard = self
            .config
            .read()
            .expect("Failed to acquire read lock on config");
        let _keep_alive_ms =
            (config_guard.timeout_ms as f32 / config_guard.keep_alive_factor) as u32;
        unimplemented!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_runtime_has_default_config() {
        let rt = Runtime::new();
        let cfg = rt.config.read().unwrap();
        assert_eq!(cfg.timeout_ms, 5000);
        assert_eq!(cfg.connect_retry, 3);
    }

    #[test]
    fn new_runtime_starts_with_no_connections() {
        let rt = Runtime::new();
        assert!(rt.connections.is_empty());
    }

    #[test]
    fn config_can_be_updated() {
        let rt = Runtime::new();
        {
            let mut cfg = rt.config.write().unwrap();
            cfg.timeout_ms = 10000;
        }
        assert_eq!(rt.config.read().unwrap().timeout_ms, 10000);
    }
}
