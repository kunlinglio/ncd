use crate::error::DeviceError;

/// Whether the device is open or closed.
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceState {
    Open,
    Closed,
}

/// Basic device structure shared by all drivers.
///
/// Each driver embeds this struct to store the device path and state.
/// Drivers manage their own I/O buffers internally so that each
/// driver can size its buffers appropriately for its workload.
///
/// Devices default to [`DeviceState::Open`] — initialisation that
/// can fail should happen in the driver's constructor, not in
/// `open()`.  The `open()` method is idempotent (no-op when
/// already open) and exists primarily for re-opening after an
/// explicit `close()`.
#[derive(Debug)]
pub struct NcdDevice {
    pub device_state: DeviceState,
    pub device_path: String,
}

impl NcdDevice {
    pub fn new(device_path: String) -> Self {
        NcdDevice {
            device_state: DeviceState::Open,
            device_path,
        }
    }
}

/// Operations that every device driver must implement.
pub trait NcdDeviceOperations: std::fmt::Debug + Send {
    /// Human-readable identifier for this device.
    fn device_path(&self) -> &str;

    /// Prepare the device for I/O.  Idempotent — must return `Ok(())`
    /// when the device is already open.
    fn open(&mut self) -> Result<(), DeviceError>;

    /// Release resources acquired by `open()`.  Idempotent — must
    /// return `Ok(())` when the device is already closed.
    fn close(&mut self) -> Result<(), DeviceError>;

    /// Read data from the device into `buf`.
    /// Returns the number of bytes read, or 0 if no data is available.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError>;

    /// Write data to the device.
    /// Returns the number of bytes written.
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError>;

    /// Suggested read-buffer size for the session event loop.
    /// Drivers that deal with large payloads (e.g. camera frames)
    /// should override this.
    fn read_buffer_size(&self) -> usize {
        64 * 1024 // 64 KB default
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal in-memory device used only for testing the trait
    /// mechanics — not a real driver.
    #[derive(Debug)]
    struct TestDevice {
        state: DeviceState,
        path: String,
        buffer: Vec<u8>,
    }

    impl TestDevice {
        fn new(path: &str) -> Self {
            TestDevice {
                state: DeviceState::Open,
                path: path.to_string(),
                buffer: Vec::new(),
            }
        }
    }

    impl NcdDeviceOperations for TestDevice {
        fn device_path(&self) -> &str {
            &self.path
        }

        fn open(&mut self) -> Result<(), DeviceError> {
            self.state = DeviceState::Open;
            Ok(())
        }

        fn close(&mut self) -> Result<(), DeviceError> {
            self.state = DeviceState::Closed;
            Ok(())
        }

        fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
            if self.state != DeviceState::Open {
                return Err(DeviceError::NotOpen(self.path.clone()));
            }
            let n = self.buffer.len().min(buf.len());
            buf[..n].copy_from_slice(&self.buffer[..n]);
            self.buffer.drain(..n);
            Ok(n)
        }

        fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
            if self.state != DeviceState::Open {
                return Err(DeviceError::NotOpen(self.path.clone()));
            }
            self.buffer.extend_from_slice(data);
            Ok(data.len())
        }
    }

    #[test]
    fn write_then_read() {
        let mut dev = TestDevice::new("/dev/test");

        let n = dev.write(b"hello").unwrap();
        assert_eq!(n, 5);

        let mut buf = [0u8; 10];
        let n = dev.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn read_drains_buffer() {
        let mut dev = TestDevice::new("/dev/test");
        dev.write(b"world").unwrap();

        let mut buf = [0u8; 10];
        dev.read(&mut buf).unwrap();
        let n = dev.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn open_is_idempotent() {
        let mut dev = TestDevice::new("/dev/test");
        // Already Open by default — open() should succeed
        assert!(dev.open().is_ok());
        assert_eq!(dev.state, DeviceState::Open);
    }

    #[test]
    fn close_then_reopen() {
        let mut dev = TestDevice::new("/dev/test");
        dev.close().unwrap();
        assert_eq!(dev.state, DeviceState::Closed);
        // read on closed device fails
        let mut buf = [0u8; 10];
        assert!(dev.read(&mut buf).is_err());
        // re-open
        dev.open().unwrap();
        assert!(dev.read(&mut buf).is_ok());
    }

    #[test]
    fn default_read_buffer_size() {
        let dev = TestDevice::new("/dev/test");
        assert_eq!(dev.read_buffer_size(), 64 * 1024);
    }
}
