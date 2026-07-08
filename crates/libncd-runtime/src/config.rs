#[derive(Debug, Clone)]
pub struct Config {
    /// TODO: This shouldn't be in global config
    pub connect_timeout_ms: u32,
    pub close_timeout_ms: u32,
    /// Report disconnected after this many milliseconds of inactivity
    /// Used to request peer to send keepalive packets at a specific interval
    pub timeout_ms: u32,
    /// Keepalive interval factor,
    /// keepalive_interval = timeout_ms / keep_alive_factor
    pub keep_alive_factor: f32,
    /// Max buffer size: This is only used between ConnHandler and Connection, to limit the buffer size for reading/writing data.
    /// Cannot affect internal buffer size of Connection.
    pub max_buffer_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            connect_timeout_ms: 3000,
            close_timeout_ms: 5000,
            timeout_ms: 5000,
            keep_alive_factor: 2.0,
            max_buffer_size: 1 * 1024, // 1 KB
        }
    }
}
