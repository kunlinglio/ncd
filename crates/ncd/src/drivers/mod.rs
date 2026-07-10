//! Drivers module: camera, keyboard, serial, ssh
//!
//! Each driver implements the `NcdDeviceOperations` trait defined in `device.rs`.

pub mod camera;
pub mod keyboard;
pub mod serial;
pub mod ssh;

pub use camera::CameraDriver;
pub use keyboard::KeyboardDriver;
pub use serial::SerialDriver;
pub use ssh::SshDriver;
