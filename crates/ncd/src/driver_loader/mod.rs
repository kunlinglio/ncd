pub mod driver;
pub mod registry;

/// Directory containing Python drivers and pyproject.toml.
const DRIVERS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/drivers");
