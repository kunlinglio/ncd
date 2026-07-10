use crate::device::{NcdDeviceOperations, NcdDevice, DeviceState};
use crate::error::DeviceError;
use std::fmt;
use std::io::{Read, Write};
use std::fs::OpenOptions;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use tokio::sync::Notify;

/// SerialDriver
///
/// Supports two modes:
/// - File-backed: `device_path` starts with `file://` and refers to a regular file.
/// - Real serial: uses the `serialport` crate to open the named port (e.g. COM3, /dev/ttyS0).
///
/// For real serial ports a background thread continuously reads incoming data into a
/// shared buffer and wakes the session via `Notify` — the same pattern used by
/// [`CameraDriver`].  Writes go directly to the port (protected by a mutex so they
/// don't race with the reader).
///
/// The device defaults to [`DeviceState::Open`] — the port or file is opened in the
/// constructor.
#[derive(Debug)]
pub struct SerialDriver {
    device: NcdDevice,
    // file-backed handle (no background thread)
    file: Option<std::fs::File>,
    // real serial port — Arc<Mutex<>> so the background reader and write() can share it
    port: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>>,
    notify: Arc<Notify>,
    // background-reader state
    read_buffer: Arc<Mutex<Vec<u8>>>,
    stop_flag: Arc<AtomicBool>,
    read_handle: Option<std::thread::JoinHandle<()>>,
}

impl SerialDriver {
    pub fn new(device_path: &str, notify: Arc<Notify>) -> Self {
        let mut driver = SerialDriver {
            device: NcdDevice::new(device_path.to_string()),
            file: None,
            port: Arc::new(Mutex::new(None)),
            notify,
            read_buffer: Arc::new(Mutex::new(Vec::new())),
            stop_flag: Arc::new(AtomicBool::new(false)),
            read_handle: None,
        };

        if let Some(stripped) = device_path.strip_prefix("file://") {
            // ── file-backed mode ──────────────────────────────
            match OpenOptions::new().read(true).write(true).create(true).open(stripped) {
                Ok(f) => driver.file = Some(f),
                Err(e) => eprintln!("SerialDriver: failed to open file {stripped}: {e}"),
            }
        } else {
            // ── real serial port ─────────────────────────────
            let port_name = device_path.to_string();
            match serialport::new(&port_name, 115200)
                .timeout(Duration::from_millis(50))
                .open()
            {
                Ok(p) => {
                    *driver.port.lock().unwrap() = Some(p);

                    // Spawn background reader thread.
                    let port = driver.port.clone();
                    let buf = driver.read_buffer.clone();
                    let nfy = driver.notify.clone();
                    let stop = driver.stop_flag.clone();

                    let handle = std::thread::spawn(move || {
                        let mut tmp = [0u8; 4096];
                        while !stop.load(Ordering::Relaxed) {
                            let n = {
                                let mut guard = port.lock().unwrap();
                                match guard.as_mut() {
                                    Some(p) => match p.read(&mut tmp) {
                                        Ok(n) if n > 0 => n,
                                        _ => 0,
                                    },
                                    None => break,
                                }
                            };
                            if n > 0 {
                                let mut guard = buf.lock().unwrap();
                                guard.extend_from_slice(&tmp[..n]);
                                nfy.notify_one();
                            }
                        }
                    });
                    driver.read_handle = Some(handle);
                }
                Err(e) => {
                    eprintln!("SerialDriver: failed to open serial port {port_name}: {e}");
                }
            }
        }

        driver
    }
}

impl fmt::Display for SerialDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SerialDriver({})", self.device.device_path)
    }
}

impl Drop for SerialDriver {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.read_handle.take() {
            let _ = h.join();
        }
    }
}

impl NcdDeviceOperations for SerialDriver {
    fn device_path(&self) -> &str {
        &self.device.device_path
    }

    fn read_buffer_size(&self) -> usize {
        64 * 1024
    }

    fn open(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Open {
            return Ok(());
        }
        // Re-open path matches the constructor logic.
        if let Some(stripped) = self.device.device_path.strip_prefix("file://") {
            let f = OpenOptions::new().read(true).write(true).create(true).open(stripped)
                .map_err(|e| DeviceError::Io(format!("open serial file: {}", e)))?;
            self.file = Some(f);
        } else {
            let p = serialport::new(&self.device.device_path, 115200)
                .timeout(Duration::from_millis(50))
                .open()
                .map_err(|e| DeviceError::Io(format!("open serial port: {}", e)))?;
            *self.port.lock().unwrap() = Some(p);

            let port = self.port.clone();
            let buf = self.read_buffer.clone();
            let nfy = self.notify.clone();
            let stop = self.stop_flag.clone();
            let handle = std::thread::spawn(move || {
                let mut tmp = [0u8; 4096];
                while !stop.load(Ordering::Relaxed) {
                    let n = {
                        let mut guard = port.lock().unwrap();
                        match guard.as_mut() {
                            Some(p) => match p.read(&mut tmp) {
                                Ok(n) if n > 0 => n,
                                _ => 0,
                            },
                            None => break,
                        }
                    };
                    if n > 0 {
                        let mut guard = buf.lock().unwrap();
                        guard.extend_from_slice(&tmp[..n]);
                        nfy.notify_one();
                    }
                }
            });
            self.read_handle = Some(handle);
        }

        self.device.device_state = DeviceState::Open;
        Ok(())
    }

    fn close(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Closed {
            return Ok(());
        }
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.read_handle.take() {
            let _ = h.join();
        }
        self.file = None;
        *self.port.lock().unwrap() = None;
        self.read_buffer.lock().unwrap().clear();
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.device.device_state = DeviceState::Closed;
        Ok(())
    }

    /// Drain data accumulated by the background reader thread.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }

        // File-backed: direct read.
        if let Some(f) = self.file.as_mut() {
            return match f.read(buf) {
                Ok(n) => Ok(n),
                Err(e) => Err(DeviceError::Io(format!("serial file read: {}", e))),
            };
        }

        // Real serial: drain shared buffer.
        let mut guard = self.read_buffer.lock().unwrap();
        if guard.is_empty() {
            return Ok(0);
        }
        let n = guard.len().min(buf.len());
        buf[..n].copy_from_slice(&guard[..n]);
        guard.drain(..n);
        Ok(n)
    }

    /// Write data directly to the serial port (or file).
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }

        if let Some(f) = self.file.as_mut() {
            return match f.write(data) {
                Ok(n) => Ok(n),
                Err(e) => Err(DeviceError::Io(format!("serial file write: {}", e))),
            };
        }

        let mut guard = self.port.lock().unwrap();
        match guard.as_mut() {
            Some(p) => match p.write(data) {
                Ok(n) => Ok(n),
                Err(e) => Err(DeviceError::Io(format!("serial write: {}", e))),
            },
            None => Err(DeviceError::UnsupportedOperation {
                device: self.device.device_path.clone(),
                operation: "write",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn serial_file_backed_read_write() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/serial_pipe.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(&path).unwrap();
        f.write_all(b"abc").unwrap();
        drop(f);

        let mut s = SerialDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        let mut buf = [0u8; 10];
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"abc");
        let w = s.write(b"xyz").unwrap();
        assert_eq!(w, 3);
    }

    #[test]
    fn close_then_reopen() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/serial_reopen.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        std::fs::write(&path, b"data").unwrap();

        let mut s = SerialDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        s.close().unwrap();
        assert_eq!(s.device.device_state, DeviceState::Closed);
        s.open().unwrap();
        assert_eq!(s.device.device_state, DeviceState::Open);
        let mut buf = [0u8; 10];
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
    }

    #[test]
    fn open_is_idempotent() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/serial_idem.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        std::fs::write(&path, b"x").unwrap();

        let mut s = SerialDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        assert!(s.open().is_ok());
        assert_eq!(s.device.device_state, DeviceState::Open);
    }
}
