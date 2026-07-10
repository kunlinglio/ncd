use crate::device::{NcdDeviceOperations, NcdDevice, DeviceState};
use crate::error::DeviceError;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::fmt;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use tokio::sync::Notify;

/// CameraDriver
///
/// This driver supports two modes:
/// - File-backed mode: when `device_path` starts with `file://`, the driver treats the rest
///   as a path to a file that contains one complete frame. Each `read()` returns that frame
///   as a single unit. After the frame is consumed, subsequent `read()` calls return 0.
/// - Real capture: opens the camera at the given index via `nokhwa` and spawns a background
///   thread that continuously captures frames into a shared buffer. Each new frame triggers
///   `notify_one()` on the injected `Notify`, waking the session loop.
///
/// The device defaults to [`DeviceState::Open`] — the capture thread (or file-backed
/// frame) is set up in the constructor.
#[derive(Debug)]
pub struct CameraDriver {
    device: NcdDevice,
    // file-backed frame buffer
    frame_buf: Option<Vec<u8>>,
    // real camera capture
    notify: Arc<Notify>,
    shared_frame: Arc<Mutex<Option<Vec<u8>>>>,
    stop_flag: Arc<AtomicBool>,
    capture_handle: Option<std::thread::JoinHandle<()>>,
}

/// Extract a camera index from a device path.
///
/// Supported formats:
/// - `camera://N`  → N
/// - `/dev/videoN` → N
/// - anything else  → 0 (fallback)
fn parse_camera_index(path: &str) -> u32 {
    if let Some(rest) = path.strip_prefix("camera://") {
        return rest.parse::<u32>().unwrap_or(0);
    }
    if let Some(rest) = path.strip_prefix("/dev/video") {
        return rest.parse::<u32>().unwrap_or(0);
    }
    0
}

impl CameraDriver {
    pub fn new(device_path: &str, notify: Arc<Notify>) -> Self {
        let mut driver = CameraDriver {
            device: NcdDevice::new(device_path.to_string()),
            frame_buf: None,
            notify,
            shared_frame: Arc::new(Mutex::new(None)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            capture_handle: None,
        };

        // File-backed mode: preload frame from disk immediately.
        if let Some(stripped) = device_path.strip_prefix("file://") {
            let p = Path::new(stripped);
            if let Err(e) = driver.load_frame_from_file(p) {
                eprintln!("CameraDriver: failed to load frame from {device_path}: {e}");
            } else {
                // Frame loaded — notify session that data is ready.
                driver.notify.notify_one();
            }
        } else {
            // Real camera: spawn background capture thread.
            let cam_index = parse_camera_index(device_path);
            let shared = driver.shared_frame.clone();
            let notify = driver.notify.clone();
            let stop = driver.stop_flag.clone();

            let handle = std::thread::spawn(move || {
                use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType, frame_formats};
                let idx = CameraIndex::Index(cam_index);
                let req = RequestedFormat::with_formats(RequestedFormatType::None, frame_formats());
                let mut cam = match nokhwa::Camera::new(idx, req) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("CameraDriver: failed to open camera: {e:?}");
                        return;
                    }
                };
                if let Err(e) = cam.open_stream() {
                    eprintln!("CameraDriver: failed to open stream: {e:?}");
                    return;
                }
                while !stop.load(Ordering::Relaxed) {
                    match cam.frame() {
                        Ok(frame) => {
                            *shared.lock().unwrap() = Some(frame.buffer().to_vec());
                            notify.notify_one();
                        }
                        Err(e) => {
                            eprintln!("CameraDriver: frame error: {e:?}");
                            break;
                        }
                    }
                }
            });
            driver.capture_handle = Some(handle);
        }

        driver
    }

    fn load_frame_from_file(&mut self, path: &Path) -> Result<(), DeviceError> {
        let mut f = File::open(path)
            .map_err(|e| DeviceError::Io(format!("open frame file: {}", e)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| DeviceError::Io(format!("read frame file: {}", e)))?;
        self.frame_buf = Some(buf);
        Ok(())
    }
}

impl fmt::Display for CameraDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CameraDriver({})", self.device.device_path)
    }
}

impl Drop for CameraDriver {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.capture_handle.take() {
            let _ = h.join();
        }
    }
}

impl NcdDeviceOperations for CameraDriver {
    fn device_path(&self) -> &str {
        &self.device.device_path
    }

    fn read_buffer_size(&self) -> usize {
        4 * 1024 * 1024 // 4 MB — camera frames are large
    }

    /// Idempotent — if the device is already open this is a no-op.
    fn open(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Open {
            return Ok(());
        }

        // Re-initialise after a previous close().
        let device_path = self.device.device_path.clone();
        if let Some(stripped) = device_path.strip_prefix("file://") {
            let p = Path::new(stripped);
            self.load_frame_from_file(p)?;
            self.notify.notify_one();
        } else {
            let cam_index = parse_camera_index(&device_path);
            let shared = self.shared_frame.clone();
            let notify = self.notify.clone();
            let stop = self.stop_flag.clone();

            let handle = std::thread::spawn(move || {
                use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType, frame_formats};
                let idx = CameraIndex::Index(cam_index);
                let req = RequestedFormat::with_formats(RequestedFormatType::None, frame_formats());
                let mut cam = match nokhwa::Camera::new(idx, req) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("CameraDriver: failed to open camera: {e:?}");
                        return;
                    }
                };
                if let Err(e) = cam.open_stream() {
                    eprintln!("CameraDriver: failed to open stream: {e:?}");
                    return;
                }
                while !stop.load(Ordering::Relaxed) {
                    match cam.frame() {
                        Ok(frame) => {
                            *shared.lock().unwrap() = Some(frame.buffer().to_vec());
                            notify.notify_one();
                        }
                        Err(e) => {
                            eprintln!("CameraDriver: frame error: {e:?}");
                            break;
                        }
                    }
                }
            });
            self.capture_handle = Some(handle);
        }

        self.device.device_state = DeviceState::Open;
        Ok(())
    }

    /// Idempotent — if already closed this is a no-op.
    fn close(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Closed {
            return Ok(());
        }

        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.capture_handle.take() {
            let _ = h.join();
        }
        self.frame_buf = None;
        self.shared_frame = Arc::new(Mutex::new(None));
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.device.device_state = DeviceState::Closed;
        Ok(())
    }

    /// Return one complete frame at a time (or as much as fits into `buf`).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }

        // 1) Real camera: drain the latest frame from the shared buffer.
        {
            let mut guard = self.shared_frame.lock().unwrap();
            if let Some(frame) = guard.as_mut() {
                let n = frame.len().min(buf.len());
                buf[..n].copy_from_slice(&frame[..n]);
                frame.drain(..n);
                if frame.is_empty() {
                    *guard = None;
                }
                return Ok(n);
            }
        }

        // 2) File-backed mode: return the preloaded frame once
        if let Some(frame) = &self.frame_buf {
            let n = std::cmp::min(frame.len(), buf.len());
            buf[..n].copy_from_slice(&frame[..n]);
            self.frame_buf = None;
            return Ok(n);
        }

        // 3) No data available (not an error)
        Ok(0)
    }

    /// Write to camera is not typically supported.
    fn write(&mut self, _data: &[u8]) -> Result<usize, DeviceError> {
        Err(DeviceError::UnsupportedOperation {
            device: self.device.device_path.clone(),
            operation: "write",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn camera_file_backed_read_returns_frame() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let tmp_path = format!("{}/src/tmp/camera_frame.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let mut f = File::create(&tmp_path).unwrap();
        f.write_all(b"FRAME_DATA").unwrap();

        let mut cam = CameraDriver::new(
            &format!("file://{}", tmp_path),
            Arc::new(Notify::new()),
        );
        let mut buf = [0u8; 32];
        let n = cam.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"FRAME_DATA");
        // second read returns 0 (no new frame)
        let n2 = cam.read(&mut buf).unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn close_then_reopen() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let tmp_path = format!("{}/src/tmp/camera_reopen.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let mut f = File::create(&tmp_path).unwrap();
        f.write_all(b"REOPEN_DATA").unwrap();

        let mut cam = CameraDriver::new(
            &format!("file://{}", tmp_path),
            Arc::new(Notify::new()),
        );
        // Default is Open — first read gets the frame loaded by new()
        let mut buf = [0u8; 32];
        let n = cam.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"REOPEN_DATA");

        // Close
        cam.close().unwrap();
        assert_eq!(cam.device.device_state, DeviceState::Closed);

        // Re-open loads the frame again
        cam.open().unwrap();
        assert_eq!(cam.device.device_state, DeviceState::Open);
        let n = cam.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"REOPEN_DATA");
    }

    #[test]
    fn open_is_idempotent() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let tmp_path = format!("{}/src/tmp/camera_idem.bin", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        File::create(&tmp_path).unwrap().write_all(b"DATA").unwrap();

        let mut cam = CameraDriver::new(
            &format!("file://{}", tmp_path),
            Arc::new(Notify::new()),
        );
        // Already Open by default — open() should be a no-op
        assert!(cam.open().is_ok());
        assert_eq!(cam.device.device_state, DeviceState::Open);
    }
}
