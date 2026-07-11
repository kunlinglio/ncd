//! Hard coded known drivers.
//!
//! TODO: Maybe move this into drivers directory

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DriverList {
    pub drivers: Vec<DriverInfo>,
}

#[derive(Debug, Clone)]
pub struct DriverInfo {
    pub name: String,
    pub description: String,
    pub default_port: u16,
    /// Relative path from the drivers directory
    pub driver_path: PathBuf,
    /// Option field name -> default value
    pub options: HashMap<String, String>,
}

static DRIVERS: std::sync::OnceLock<DriverList> = std::sync::OnceLock::new();

pub fn get_drivers() -> &'static DriverList {
    DRIVERS.get_or_init(|| DriverList {
        drivers: vec![DriverInfo {
            name: "keyboard".to_string(),
            description: "Terminal keyboard (raw /dev/tty, no permissions)".to_string(),
            default_port: 8081,
            driver_path: PathBuf::from("keyboard.py"),
            options: HashMap::new(),
        }],
    })
}

impl DriverList {
    pub fn query_driver(&self, name: &str) -> Option<&DriverInfo> {
        self.drivers.iter().find(|d| d.name == name)
    }
}
