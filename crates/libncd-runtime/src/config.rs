use std::net::IpAddr;

use crate::DEFAULT_LOCAL_PORT;

#[derive(Debug)]
pub struct Config {
    /// TODO: This shouldn't be in global config
    pub connect_timeout_ms: u32,
    pub connect_retry: u32,
    pub close_timeout_ms: u32,
    /// Report disconnected after this many milliseconds of inactivity
    /// Used to request peer to send keepalive packets at a specific interval
    pub timeout_ms: u32,
    /// Keepalive interval factor,
    /// keepalive_interval = timeout_ms / keep_alive_factor
    pub keep_alive_factor: f32,
    /// Max buffer size
    pub max_buffer_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            connect_timeout_ms: 3000,
            connect_retry: 3,
            close_timeout_ms: 5000,
            timeout_ms: 5000,
            keep_alive_factor: 2.0,
            max_buffer_size: 10 * 1024 * 1024, // 10 MB
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let cfg = Config::default();
        assert_eq!(cfg.connect_timeout_ms, 3000);
        assert_eq!(cfg.connect_retry, 3);
        assert_eq!(cfg.close_timeout_ms, 5000);
        assert_eq!(cfg.timeout_ms, 5000);
        assert!((cfg.keep_alive_factor - 2.0).abs() < f32::EPSILON);
        assert_eq!(cfg.max_buffer_size, 10 * 1024 * 1024);
    }

    #[test]
    fn override_values() {
        let cfg = Config {
            connect_timeout_ms: 1000,
            connect_retry: 5,
            close_timeout_ms: 2000,
            timeout_ms: 3000,
            keep_alive_factor: 3.0,
            max_buffer_size: 4096,
        };
        assert_eq!(cfg.connect_timeout_ms, 1000);
        assert_eq!(cfg.connect_retry, 5);
        assert!((cfg.keep_alive_factor - 3.0).abs() < f32::EPSILON);
    }
}
