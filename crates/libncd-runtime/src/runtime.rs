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
                    config_guard.max_buffer_size,
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
                    config_guard.max_buffer_size,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn new_runtime_has_default_config() {
        let rt = Runtime::new();
        let cfg = rt.get_config().await;
        assert_eq!(cfg.timeout_ms, 5000);
    }

    #[tokio::test]
    async fn set_and_get_config() {
        let rt = Runtime::new();
        let mut cfg = rt.get_config().await;
        cfg.timeout_ms = 9999;
        rt.set_config(cfg).await;
        assert_eq!(rt.get_config().await.timeout_ms, 9999);
    }

    /// Runtime::open(Device) opens a connection to a manually‑run host.
    /// (Host open can't be tested without a connecting client on a known port.)
    #[tokio::test]
    async fn open_device_via_runtime() {
        use crate::connection::{ConnRole, Connection};
        use std::net::SocketAddr;

        let rt = Runtime::new();
        let addr = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Manual host — bind first to get a known port, then accept + handshake
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(addr, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let host = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let (mut conn, _handler) =
                Connection::new(stream, peer, 5000, 2000, 2.0, ConnRole::Host, 100);
            conn.handshake().await.unwrap();
            conn
        });

        // Device via Runtime — verifies config → connect() path works
        let mut handler = rt
            .open(OpenParams::Device {
                host_addr: addr,
                host_port: port,
            })
            .await
            .unwrap();

        // Handler is functional: can get status from background task
        let status = handler.get_status().await;
        assert!(status.is_ok(), "status failed: {:?}", status.err());

        let _host_conn = host.await.unwrap();
        drop(handler);
    }
}
