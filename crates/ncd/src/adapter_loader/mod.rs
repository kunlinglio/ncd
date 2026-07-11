pub mod adapter;
pub mod list;

/// Directory containing Python adapters and pyproject.toml.
const DRIVERS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/adapters");
