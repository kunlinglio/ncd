use crate::device::{NcdDeviceOperations, NcdDevice, DeviceState};
use crate::error::DeviceError;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use tokio::sync::Notify;

/// SshDriver
///
/// This driver provides a file-backed simulation mode for tests where `device_path` is
/// `file://` pointing to a text file that represents the remote side's line-oriented stream.
/// Each `read()` returns up to the next newline (Enter-terminated).
///
/// A production SSH driver should use a library like `ssh2`/`russh` to open a TCP connection,
/// perform authentication, open a channel and read/write bytes.
///
/// The device defaults to [`DeviceState::Open`] — the connection is established in the
/// constructor.
///
/// TODO: spawn a background thread for real SSH connections so that incoming channel data
/// triggers `notify_one()` and wakes the session loop. The `notify` is stored for future use.
#[derive(Debug)]
pub struct SshDriver {
    device: NcdDevice,
    reader: Option<BufReader<File>>,
    writer_path: Option<String>,
    // stored for future background-SSH-reader thread
    #[allow(dead_code)]
    notify: Arc<Notify>,
}

impl SshDriver {
    pub fn new(device_path: &str, notify: Arc<Notify>) -> Self {
        let mut driver = SshDriver {
            device: NcdDevice::new(device_path.to_string()),
            reader: None,
            writer_path: None,
            notify,
        };

        // Open the connection immediately — devices default to Open.
        if let Some(stripped) = device_path.strip_prefix("file://") {
            match File::open(stripped) {
                Ok(f) => {
                    let mut br = BufReader::new(f);
                    let mut first = String::new();
                    if br.read_line(&mut first).is_ok() {
                        let reader_path = first.trim().to_string();
                        match File::open(&reader_path) {
                            Ok(rf) => driver.reader = Some(BufReader::new(rf)),
                            Err(e) => eprintln!("SshDriver: failed to open reader {reader_path}: {e}"),
                        }
                    }
                    let mut second = String::new();
                    if br.read_line(&mut second).is_ok() && !second.trim().is_empty() {
                        driver.writer_path = Some(second.trim().to_string());
                    }
                }
                Err(e) => eprintln!("SshDriver: failed to open header {stripped}: {e}"),
            }
        } else {
            // Real SSH connection would be established here using `ssh2` or similar.
            eprintln!("SshDriver: real SSH not yet implemented for {device_path}");
        }

        driver
    }
}

impl fmt::Display for SshDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SshDriver({})", self.device.device_path)
    }
}

impl NcdDeviceOperations for SshDriver {
    fn device_path(&self) -> &str {
        &self.device.device_path
    }

    fn read_buffer_size(&self) -> usize {
        64 * 1024 // 64 KB — SSH command output can be moderately large
    }

    /// Idempotent — no-op when already open.
    fn open(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Open {
            return Ok(());
        }

        if let Some(stripped) = self.device.device_path.strip_prefix("file://") {
            let f = File::open(stripped)
                .map_err(|e| DeviceError::Io(format!("open ssh file: {}", e)))?;
            let mut br = BufReader::new(f);
            let mut first = String::new();
            br.read_line(&mut first)
                .map_err(|e| DeviceError::Io(format!("ssh read header: {}", e)))?;
            let reader_path = first.trim().to_string();
            let rf = File::open(&reader_path)
                .map_err(|e| DeviceError::Io(format!("open ssh reader: {}", e)))?;
            self.reader = Some(BufReader::new(rf));
            let mut second = String::new();
            br.read_line(&mut second).ok();
            if !second.trim().is_empty() {
                self.writer_path = Some(second.trim().to_string());
            }
        } else {
            return Err(DeviceError::UnsupportedOperation {
                device: self.device.device_path.clone(),
                operation: "open",
            });
        }

        self.device.device_state = DeviceState::Open;
        Ok(())
    }

    /// Idempotent — no-op when already closed.
    fn close(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Closed {
            return Ok(());
        }
        self.reader = None;
        self.writer_path = None;
        self.device.device_state = DeviceState::Closed;
        Ok(())
    }

    /// Read up to next newline (Enter-terminated unit).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }
        if let Some(r) = self.reader.as_mut() {
            let mut s = String::new();
            let n = r
                .read_line(&mut s)
                .map_err(|e| DeviceError::Io(format!("ssh read: {}", e)))?;
            if n == 0 {
                return Ok(0);
            }
            let bytes = s.as_bytes();
            let copy_n = std::cmp::min(bytes.len(), buf.len());
            buf[..copy_n].copy_from_slice(&bytes[..copy_n]);
            return Ok(copy_n);
        }
        Err(DeviceError::UnsupportedOperation {
            device: self.device.device_path.clone(),
            operation: "read",
        })
    }

    /// Write appends to writer_path if available; otherwise Unsupported.
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }
        if let Some(wp) = &self.writer_path {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(wp)
                .map_err(|e| DeviceError::Io(format!("ssh writer open: {}", e)))?;
            f.write_all(data)
                .map_err(|e| DeviceError::Io(format!("ssh writer write: {}", e)))?;
            Ok(data.len())
        } else {
            Err(DeviceError::UnsupportedOperation {
                device: self.device.device_path.clone(),
                operation: "write",
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn ssh_file_backed_read_write_lines() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let reader_path = format!("{}/src/tmp/ssh_remote.txt", tmp_dir);
        let writer_path = format!("{}/src/tmp/ssh_local_write.txt", tmp_dir);
        // create remote file with a line
        let mut rf = File::create(&reader_path).unwrap();
        writeln!(rf, "remote-line-1").unwrap();
        // create header file: first line is reader path, second line is writer path
        let header = format!("{}\n{}\n", reader_path, writer_path);
        let header_path = format!("{}/src/tmp/ssh_header.txt", tmp_dir);
        let mut hf = File::create(&header_path).unwrap();
        hf.write_all(header.as_bytes()).unwrap();

        let mut d = SshDriver::new(&format!("file://{}", header_path), Arc::new(Notify::new()));
        let mut buf = [0u8; 128];
        let n = d.read(&mut buf).unwrap();
        let s = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(s.contains("remote-line-1"));
        let w = d.write(b"hello\n").unwrap();
        assert_eq!(w, 6);
    }

    #[test]
    fn close_then_reopen() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let reader_path = format!("{}/src/tmp/ssh_reopen_r.txt", tmp_dir);
        let mut rf = File::create(&reader_path).unwrap();
        writeln!(rf, "reopened-line").unwrap();
        let header_path = format!("{}/src/tmp/ssh_reopen_h.txt", tmp_dir);
        let mut hf = File::create(&header_path).unwrap();
        hf.write_all(format!("{}\n", reader_path).as_bytes()).unwrap();

        let mut d = SshDriver::new(&format!("file://{}", header_path), Arc::new(Notify::new()));
        d.close().unwrap();
        assert_eq!(d.device.device_state, DeviceState::Closed);
        d.open().unwrap();
        assert_eq!(d.device.device_state, DeviceState::Open);
        let mut buf = [0u8; 128];
        let n = d.read(&mut buf).unwrap();
        assert!(n > 0);
    }

    #[test]
    fn open_is_idempotent() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let reader_path = format!("{}/src/tmp/ssh_idem_r.txt", tmp_dir);
        File::create(&reader_path).unwrap();
        let header_path = format!("{}/src/tmp/ssh_idem_h.txt", tmp_dir);
        let mut hf = File::create(&header_path).unwrap();
        hf.write_all(format!("{}\n", reader_path).as_bytes()).unwrap();

        let mut d = SshDriver::new(&format!("file://{}", header_path), Arc::new(Notify::new()));
        // Already Open — open() is a no-op
        assert!(d.open().is_ok());
        assert_eq!(d.device.device_state, DeviceState::Open);
    }
}
