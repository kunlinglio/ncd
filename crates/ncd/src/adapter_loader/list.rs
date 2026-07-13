use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::adapter;
use super::bundle;

#[derive(Debug, Clone, Deserialize)]
pub struct AdapterList {
    pub adapters: Vec<AdapterItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdapterItem {
    pub name: String,
    #[allow(dead_code)]
    pub description: String,
    pub default_port: u16,
    /// Relative path from the adapters directory.
    pub path: PathBuf,
    /// Adapter-level default options, inherited by all its devices.
    #[serde(default)]
    pub options: HashMap<String, String>,
}

/// A discovered device from a Python adapter's `list` command,
/// enriched with adapter-level defaults.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub identifier: String,
    pub name: String,
    pub description: String,
    /// Options inherited from the parent adapter (key -> default value).
    pub options: HashMap<String, String>,
    /// Which adapter this device belongs to.
    pub adapter_name: String,
    /// Base port from the adapter definition.
    pub default_port: u16,
}

pub fn get_adapters() -> AdapterList {
    let path = bundle::drivers_dir().join("adapter_list.toml");
    let content = std::fs::read_to_string(&path).expect("Failed to read adapter_list.toml");
    toml::from_str(&content).expect("Failed to parse adapter_list.toml")
}

/// Discover all devices from all adapters by calling each adapter's `list` command.
/// Each device inherits the adapter's default options.
pub fn get_all_devices() -> Vec<DeviceInfo> {
    let adapters = get_adapters();
    let mut devices: Vec<DeviceInfo> = Vec::new();

    for adapter in &adapters.adapters {
        let script_path = bundle::drivers_dir().join(&adapter.path);
        match adapter::list_devices(&script_path) {
            Ok(raw_devices) => {
                for (i, raw) in raw_devices.into_iter().enumerate() {
                    devices.push(DeviceInfo {
                        identifier: raw.identifier,
                        name: raw.name,
                        description: raw.description,
                        options: adapter.options.clone(),
                        adapter_name: adapter.name.clone(),
                        default_port: adapter.default_port + i as u16,
                    });
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to list devices for adapter '{}': {e}",
                    adapter.name
                );
            }
        }
    }

    devices
}
