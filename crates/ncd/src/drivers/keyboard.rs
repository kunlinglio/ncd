use crate::device::{NcdDeviceOperations, NcdDevice, DeviceState};
use crate::error::DeviceError;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use tokio::sync::Notify;

/// KeyboardDriver
///
/// Two modes:
/// - **File-backed** (`file://`): reads lines from a text file — for testing.
/// - **Real keyboard**: captures keystrokes on the local machine and injects
///   keystrokes from the remote side.
///
/// Data direction:
/// - `read()`  → captures keystrokes typed on THIS machine → sends to Linux.
///   Data is raw key-event text (see `vk_to_text`).
/// - `write()` → receives keystrokes from Linux → injects on THIS machine.
#[derive(Debug)]
pub struct KeyboardDriver {
    device: NcdDevice,
    // file-backed
    reader: Option<BufReader<File>>,
    // real keyboard
    notify: Arc<Notify>,
    read_buffer: Arc<Mutex<Vec<u8>>>,
    stop_flag: Arc<AtomicBool>,
    capture_handle: Option<std::thread::JoinHandle<()>>,
}

impl KeyboardDriver {
    pub fn new(device_path: &str, notify: Arc<Notify>) -> Self {
        let mut driver = KeyboardDriver {
            device: NcdDevice::new(device_path.to_string()),
            reader: None,
            notify,
            read_buffer: Arc::new(Mutex::new(Vec::new())),
            stop_flag: Arc::new(AtomicBool::new(false)),
            capture_handle: None,
        };

        if let Some(stripped) = device_path.strip_prefix("file://") {
            match File::open(stripped) {
                Ok(f) => driver.reader = Some(BufReader::new(f)),
                Err(e) => eprintln!("KeyboardDriver: failed to open {device_path}: {e}"),
            }
        } else {
            // Real keyboard: spawn capture thread.
            driver.capture_handle = Some(spawn_capture(
                driver.read_buffer.clone(),
                driver.notify.clone(),
                driver.stop_flag.clone(),
            ));
        }

        driver
    }
}

impl fmt::Display for KeyboardDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyboardDriver({})", self.device.device_path)
    }
}

impl Drop for KeyboardDriver {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        // Wake the message pump so GetMessageW unblocks.
        #[cfg(target_os = "windows")]
        unsafe {
            let tid = CAPTURE_THREAD_ID.load(Ordering::Relaxed);
            if tid != 0 {
                use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
                let _ = PostThreadMessageW(tid, 0, None, None); // WM_NULL → unblock
            }
        }
        if let Some(h) = self.capture_handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(target_os = "windows")]
static CAPTURE_THREAD_ID: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

impl NcdDeviceOperations for KeyboardDriver {
    fn device_path(&self) -> &str {
        &self.device.device_path
    }

    fn read_buffer_size(&self) -> usize {
        4 * 1024
    }

    fn open(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Open {
            return Ok(());
        }
        if let Some(stripped) = self.device.device_path.strip_prefix("file://") {
            let f = File::open(stripped)
                .map_err(|e| DeviceError::Io(format!("open keyboard file: {}", e)))?;
            self.reader = Some(BufReader::new(f));
        } else {
            return Err(DeviceError::UnsupportedOperation {
                device: self.device.device_path.clone(),
                operation: "open",
            });
        }
        self.device.device_state = DeviceState::Open;
        Ok(())
    }

    fn close(&mut self) -> Result<(), DeviceError> {
        if self.device.device_state == DeviceState::Closed {
            return Ok(());
        }
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.capture_handle.take() {
            let _ = h.join();
        }
        self.reader = None;
        self.read_buffer.lock().unwrap().clear();
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.device.device_state = DeviceState::Closed;
        Ok(())
    }

    /// Drain captured keystrokes from the shared buffer.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }
        // File-backed path.
        if let Some(r) = self.reader.as_mut() {
            let mut s = String::new();
            let n = r
                .read_line(&mut s)
                .map_err(|e| DeviceError::Io(format!("keyboard read: {}", e)))?;
            if n == 0 {
                return Ok(0);
            }
            let bytes = s.as_bytes();
            let copy_n = bytes.len().min(buf.len());
            buf[..copy_n].copy_from_slice(&bytes[..copy_n]);
            return Ok(copy_n);
        }
        // Real keyboard: drain shared buffer.
        let mut guard = self.read_buffer.lock().unwrap();
        if guard.is_empty() {
            return Ok(0);
        }
        let n = guard.len().min(buf.len());
        buf[..n].copy_from_slice(&guard[..n]);
        guard.drain(..n);
        Ok(n)
    }

    /// Inject keystrokes on the local machine (remote-control direction).
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        if self.device.device_state != DeviceState::Open {
            return Err(DeviceError::NotOpen(self.device.device_path.clone()));
        }
        if self.reader.is_some() {
            return Err(DeviceError::UnsupportedOperation {
                device: self.device.device_path.clone(),
                operation: "write",
            });
        }
        platform::inject_keys(data)
    }
}

// ── Capture thread (Windows: WH_KEYBOARD_LL hook) ────────────────

fn spawn_capture(
    buffer: Arc<Mutex<Vec<u8>>>,
    notify: Arc<Notify>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        run_capture_windows(buffer, notify, stop);
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (buffer, notify, stop);
            // TODO: Linux evdev key capture
            // TODO: macOS IOKit HID Manager key capture
        }
    })
}

#[cfg(target_os = "windows")]
fn run_capture_windows(
    buffer: Arc<Mutex<Vec<u8>>>,
    notify: Arc<Notify>,
    stop: Arc<AtomicBool>,
) {
    use std::sync::atomic::AtomicPtr;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    struct Shared {
        buffer: Arc<Mutex<Vec<u8>>>,
        notify: Arc<Notify>,
    }

    static HOOK_STATE: AtomicPtr<Shared> = AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "system" fn hook_proc(
        n_code: i32,
        w_param: WPARAM,
        l_param: LPARAM,
    ) -> LRESULT {
        if n_code >= 0 {
            let p = HOOK_STATE.load(std::sync::atomic::Ordering::Relaxed);
            if !p.is_null() {
                // SAFETY: HOOK_STATE is set before hook is installed and
                // cleared after unhook; the pointer is valid for the hook's
                // lifetime.
                let state = unsafe { &*p };
                let msg = w_param.0 as u32;
                if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    let ks = unsafe { &*(l_param.0 as *const KBDLLHOOKSTRUCT) };
                    let text = vk_to_text(ks.vkCode);
                    if !text.is_empty() {
                        let mut buf = state.buffer.lock().unwrap();
                        buf.extend_from_slice(text.as_bytes());
                        state.notify.notify_one();
                    }
                }
            }
        }
        unsafe { CallNextHookEx(None, n_code, w_param, l_param) }
    }

    // Store shared state for the hook callback.
    CAPTURE_THREAD_ID.store(
        unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
        std::sync::atomic::Ordering::Relaxed,
    );
    let shared = Box::into_raw(Box::new(Shared { buffer, notify }));
    HOOK_STATE.store(shared, std::sync::atomic::Ordering::Relaxed);

    let hmod = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("KeyboardDriver: GetModuleHandleW failed: {e:?}");
            return;
        }
    };
    let hook = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod, 0) } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("KeyboardDriver: SetWindowsHookExW failed: {e:?}");
            return;
        }
    };
    eprintln!("KeyboardDriver: capture active (hook installed)");

    // Message pump.
    let mut msg = MSG::default();
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
    }

    // Cleanup.
    unsafe { let _ = UnhookWindowsHookEx(hook); }
    CAPTURE_THREAD_ID.store(0, std::sync::atomic::Ordering::Relaxed);
    HOOK_STATE.store(std::ptr::null_mut(), std::sync::atomic::Ordering::Relaxed);
    unsafe { drop(Box::from_raw(shared)); }
}

// ── VK → text ─────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn vk_to_text(vk_code: u32) -> String {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let code = vk_code as u16;
    match code {
        0x08 => "[Backspace]".into(),
        0x09 => "[Tab]".into(),
        0x0D => "\n".into(),
        0x1B => "[Esc]".into(),
        0x20 => " ".into(),
        0x21 => "[PgUp]".into(),
        0x22 => "[PgDn]".into(),
        0x23 => "[End]".into(),
        0x24 => "[Home]".into(),
        0x25 => "[Left]".into(),
        0x26 => "[Up]".into(),
        0x27 => "[Right]".into(),
        0x28 => "[Down]".into(),
        0x2D => "[Ins]".into(),
        0x2E => "[Del]".into(),
        0x70..=0x87 => format!("[F{}]", code - 0x6F),
        _ => {
            let scan = unsafe { MapVirtualKeyW(code as u32, MAPVK_VK_TO_VSC) };
            let mut buf = [0u16; 2];
            let ret = unsafe {
                ToUnicode(code as u32, scan, None, &mut buf, 0)
            };
            if ret > 0 {
                String::from_utf16_lossy(&buf[..ret as usize])
            } else {
                String::new()
            }
        }
    }
}

// ── Platform stubs ────────────────────────────────────────────────

mod platform {
    use crate::error::DeviceError;

    #[cfg(target_os = "windows")]
    pub fn inject_keys(data: &[u8]) -> Result<usize, DeviceError> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let mut inputs: Vec<INPUT> = Vec::with_capacity(data.len() * 2);
        for &byte in data {
            let vk: u16 = match byte {
                0x0D | 0x0A => 0x0D,
                0x08 => 0x08,
                0x09 => 0x09,
                0x1B => 0x1B,
                b if b.is_ascii_graphic() || b == b' ' => {
                    let scan = unsafe { VkKeyScanW(b as u16) };
                    if scan == -1 {
                        continue;
                    }
                    (scan as u16) & 0xFF
                }
                _ => continue,
            };

            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: KEYBD_EVENT_FLAGS(0),
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(vk),
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }

        if inputs.is_empty() {
            return Ok(0);
        }
        let count = inputs.len() / 2;
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent == 0 {
            return Err(DeviceError::Io(
                "SendInput failed — no window has keyboard focus?".into(),
            ));
        }
        Ok(count)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn inject_keys(_data: &[u8]) -> Result<usize, DeviceError> {
        let _ = _data;
        Err(DeviceError::UnsupportedOperation {
            device: "keyboard".into(),
            operation: "inject",
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;

    #[test]
    fn keyboard_file_backed_reads_lines() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/keyboard_input.txt", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let mut f = File::create(&path).unwrap();
        writeln!(f, "hello").unwrap();
        writeln!(f, "world").unwrap();

        let mut k = KeyboardDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        let mut buf = [0u8; 64];
        let n = k.read(&mut buf).unwrap();
        assert!(n > 0);
        let s = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(s.trim_end().starts_with("hello"));
    }

    #[test]
    fn close_then_reopen() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/keyboard_reopen.txt", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        let mut f = File::create(&path).unwrap();
        writeln!(f, "reopened").unwrap();

        let mut k = KeyboardDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        k.close().unwrap();
        assert_eq!(k.device.device_state, DeviceState::Closed);
        k.open().unwrap();
        assert_eq!(k.device.device_state, DeviceState::Open);
        let mut buf = [0u8; 64];
        let n = k.read(&mut buf).unwrap();
        assert!(n > 0);
    }

    #[test]
    fn open_is_idempotent() {
        let tmp_dir = env!("CARGO_MANIFEST_DIR");
        let path = format!("{}/src/tmp/keyboard_idem.txt", tmp_dir);
        fs::create_dir_all(format!("{}/src/tmp", tmp_dir)).ok();
        File::create(&path).unwrap();

        let mut k = KeyboardDriver::new(&format!("file://{}", path), Arc::new(Notify::new()));
        assert!(k.open().is_ok());
        assert_eq!(k.device.device_state, DeviceState::Open);
    }
}
