use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::config::Config;
use crate::connection::{ConnHandler, Connection};
use crate::error::ConnectionCreateError;

/// Async runtime for connection management and keepalive etc.
#[allow(dead_code)]
pub struct Runtime {
    config: RwLock<Config>,
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
        }
    }

    pub async fn open(&self, params: OpenParams) -> Result<ConnHandler, ConnectionCreateError> {
        let config_guard = self.config.read().await;
        match params {
            OpenParams::Host {
                listen_addr,
                listen_port,
            } => {
                Connection::listen(
                    listen_addr,
                    listen_port,
                    config_guard.timeout_ms,
                    config_guard.close_timeout_ms,
                    config_guard.keep_alive_factor,
                )
                .await
            }
            OpenParams::Device {
                host_addr,
                host_port,
            } => {
                Connection::connect(
                    host_addr,
                    host_port,
                    Duration::from_millis(config_guard.connect_timeout_ms as u64),
                    config_guard.close_timeout_ms,
                    config_guard.timeout_ms,
                    config_guard.keep_alive_factor,
                )
                .await
            }
        }
    }

    pub async fn set_config(&self, cfg: Config) {
        let mut config_guard = self.config.write().await;
        *config_guard = cfg;
    }

    pub async fn get_config(&self) -> Config {
        let config_guard = self.config.read().await;
        config_guard.clone()
    }
}
