use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    #[serde(default)]
    pub device: Vec<DeviceEntry>,
}

/// A single device configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// Driver name (e.g. "serial"). Must match a built-in driver.
    pub driver: String,
    /// TCP port on which this host listens for incoming NCD device connections.
    pub port: u16,
    /// Driver-specific configuration key-value pairs (e.g. device_path, baud_rate).
    /// These become CLI arguments: --key value
    #[serde(default)]
    pub options: HashMap<String, String>,
}

impl HostConfig {
    pub fn load() -> Option<Self> {
        let path = config_path();
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, content)
    }
}

pub fn config_path() -> PathBuf {
    PathBuf::from(
        directories::ProjectDirs::from("", "", "ncd")
            .expect("Failed to determine config directory")
            .config_dir(),
    )
    .join("config.toml")
}
