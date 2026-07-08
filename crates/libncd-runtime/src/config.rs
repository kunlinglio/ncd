use std::net::IpAddr;

use crate::DEFAULT_LOCAL_PORT;

#[derive(Debug)]
pub struct Config {
    pub local_addr: IpAddr,
    pub local_port: u16,
    pub connect_timeout_ms: u32,
    pub connect_retry: u32,
    /// Report disconnected after this many milliseconds of inactivity
    /// Used to request peer to send keepalive packets at a specific interval
    pub timeout_ms: u32,
    /// Keepalive interval factor,
    /// keepalive_interval = timeout_ms / keep_alive_factor
    pub keep_alive_factor: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            local_addr: IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)),
            local_port: DEFAULT_LOCAL_PORT,
            connect_timeout_ms: 3000,
            connect_retry: 3,
            timeout_ms: 5000,
            keep_alive_factor: 2.0,
        }
    }
}
