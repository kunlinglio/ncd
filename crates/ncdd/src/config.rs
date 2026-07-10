use serde::Deserialize;
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ConfigFile {
    device: Vec<DeviceConfig>, // according to the config format
}

#[derive(Debug, Deserialize)]
pub struct DeviceConfig {
    pub name: String,
    pub remote_ip: IpAddr,
    pub remote_port: u16,
}

/// Find the configuration path.
/// Returns None if the path is not found.
/// the path is /etc/ncd/config.toml
pub fn get_config_path() -> Option<PathBuf> {
    let config_path = PathBuf::from("/etc/ncd/config.toml");
    if config_path.exists() {
        return Some(config_path);
    }
    None
}

// config format:
// [[device]]
// name = "ncd01"
// remote_ip = "192.168.1.100"
// remote_port = 8080

// [[device]]
// name = "ncd02"
// remote_ip = "192.168.1.101"
// remote_port = 8080
pub fn load_config(path: &PathBuf) -> io::Result<Vec<DeviceConfig>> {
    let content = std::fs::read_to_string(path)?;

    let config_file: ConfigFile =
        toml::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(config_file.device)
}
