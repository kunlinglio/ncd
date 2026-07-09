use dirs;
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

/// Find the default configuration path.
/// Returns None if no valid path is found.
/// The order of candidates is:
/// 1. Current directory (./ncd_config.toml)
/// 2. Config directory (~/.config/ncd/ncd_config.toml)
/// 3. /etc/ncd/ncd_config.toml
pub fn default_config_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("ncd_config.toml"),
        PathBuf::from("/etc/ncd/ncd_config.toml"),
    ];
    if let Some(dir) = dirs::config_dir() {
        candidates.insert(1, dir.join("ncd/ncd_config.toml"));
    }
    for path in candidates {
        if path.exists() {
            return Some(path);
        }
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
