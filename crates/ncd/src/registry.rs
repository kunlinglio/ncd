#[cfg(target_os = "linux")]
use glob::glob;
#[cfg(target_os = "macos")]
use objc::runtime::Object;
#[cfg(target_os = "macos")]
use objc::{class, msg_send, sel, sel_impl};
use regex::Regex;
use serialport::available_ports;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Camera,
    Keyboard,
    Serial,
    Ssh,
    Unknown,
}

impl std::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub kind: DeviceKind,
    pub path: String,
}

pub struct DevicesRegistry {
    port_to_device: HashMap<u16, DeviceInfo>,
}

impl DevicesRegistry {
    pub fn new() -> Self {
        DevicesRegistry {
            port_to_device: HashMap::new(),
        }
    }

    /// Scan the local host and return every detected device.
    ///
    /// This does NOT assign ports — call [`register`] afterwards with
    /// the subset of devices the user wants to expose.
    pub fn detect_all(&self) -> Vec<DeviceInfo> {
        let mut devices = Vec::new();
        devices.extend(self.detect_cameras());
        devices.extend(self.detect_keyboards());
        devices.extend(self.detect_serials());
        devices.extend(self.detect_ssh());
        devices
    }

    /// Register devices with explicit port assignments.
    /// Previous registrations are cleared.
    pub fn register_with_ports(&mut self, mapping: &[(u16, DeviceInfo)]) {
        self.port_to_device.clear();
        for (port, device) in mapping {
            self.port_to_device.insert(*port, device.clone());
        }
    }

    /// Register a list of devices with sequential port numbers starting
    /// at `start_port`.  Previous registrations are cleared.
    pub fn register(&mut self, devices: &[DeviceInfo], start_port: u16) {
        self.port_to_device.clear();
        for (i, device) in devices.iter().enumerate() {
            self.port_to_device
                .insert(start_port + i as u16, device.clone());
        }
    }

    /// Detect camera devices.
    ///
    /// Linux: enumerates `/sys/class/video4linux/*` via sysfs.
    /// macOS: uses AVFoundation `AVCaptureDevice` enumeration.
    /// Windows: uses SetupDi with `GUID_DEVCLASS_IMAGE`.
    fn detect_cameras(&self) -> Vec<DeviceInfo> {
        let mut cameras = Vec::new();
        #[cfg(target_os = "linux")]
        {
            // Enumerate v4l devices via sysfs.  We assign camera://N paths
            // so CameraDriver can extract the index for nokhwa.
            if let Ok(entries) = glob("/sys/class/video4linux/*") {
                for (idx, _entry) in entries.flatten().enumerate() {
                    cameras.push(DeviceInfo {
                        kind: DeviceKind::Camera,
                        path: format!("camera://{idx}"),
                    });
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            use objc::rc::autoreleasepool;

            // Enumerate cameras via AVFoundation inside an
            // autorelease pool.  The NSArray returned by
            // devicesWithMediaType: is autoreleased; without a pool
            // it could be deallocated before we finish iterating.
            //
            // Note: AVMediaTypeVideo == @"vide" in Apple's ObjC
            // runtime (Apple uses shortened 4-char identifiers for
            // media types).
            autoreleasepool(|| {
                let media_type: *mut Object = unsafe {
                    msg_send![class!(NSString), stringWithUTF8String: "vide\0".as_ptr() as *const u8]
                };
                let devices: *mut Object =
                    unsafe { msg_send![class!(AVCaptureDevice), devicesWithMediaType: media_type] };

                // If the media-type query returns nil there are
                // simply no video capture devices.  Do NOT fall
                // back to [AVCaptureDevice devices] — that returns
                // ALL capture devices (microphones, audio inputs,
                // …) which would break the camera://N index
                // mapping.
                if devices.is_null() {
                    return;
                }

                let count: usize = unsafe { msg_send![devices, count] };
                for idx in 0..count {
                    let device: *mut Object = unsafe { msg_send![devices, objectAtIndex: idx] };
                    if device.is_null() {
                        continue;
                    }
                    // The nokhwa backend uses the same
                    // AVCaptureDevice list, so array index maps
                    // directly to nokhwa's CameraIndex::Index(N).
                    cameras.push(DeviceInfo {
                        kind: DeviceKind::Camera,
                        path: format!("camera://{idx}"),
                    });
                }
            });
        }

        #[cfg(target_os = "windows")]
        {
            // Use nokhwa's own enumeration (MediaFoundation) instead of
            // SetupDi GUID_DEVCLASS_IMAGE.  The two backends disagree on
            // modern UVC cameras — SetupDi often misses them.  Using
            // nokhwa guarantees the same indices as CameraDriver.
            for idx in 0u32..8 {
                use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
                let cam_idx = CameraIndex::Index(idx);
                let req = RequestedFormat::with_formats(
                    RequestedFormatType::None,
                    nokhwa::utils::frame_formats(),
                );
                match nokhwa::Camera::new(cam_idx, req) {
                    Ok(cam) => {
                        // Retrieve the human-readable name if available.
                        let _ = cam.info();
                        drop(cam);
                        cameras.push(DeviceInfo {
                            kind: DeviceKind::Camera,
                            path: format!("camera://{idx}"),
                        });
                    }
                    Err(_) => break, // no more cameras
                }
            }
        }

        cameras
    }

    /// Detect serial devices on the current host.
    fn detect_serials(&self) -> Vec<DeviceInfo> {
        let mut ports = Vec::new();

        for port in available_ports().unwrap_or_default() {
            ports.push(DeviceInfo {
                kind: DeviceKind::Serial,
                path: port.port_name,
            });
        }

        ports
    }

    /// Detect keyboards on the current host.
    fn detect_keyboards(&self) -> Vec<DeviceInfo> {
        let mut keyboards = Vec::new();

        #[cfg(target_os = "linux")]
        {
            // ── ioctl request codes computed from the Linux _IOC macro ──
            // _IOC(dir,type,nr,size) = (dir<<30)|(type<<8)|(nr<<0)|(size<<16)
            const IOC_READ: u64 = 2;
            let eviocgname = |len: usize| -> libc::c_ulong {
                ((IOC_READ << 30) | ((b'E' as u64) << 8) | 0x06u64 | ((len as u64) << 16))
                    as libc::c_ulong
            };
            // EVIOCGBIT(ev, len): read capability bits for event type `ev`
            let eviocgbit = |ev: u8, len: usize| -> libc::c_ulong {
                ((IOC_READ << 30)
                    | ((b'E' as u64) << 8)
                    | ((0x20 + ev as u64) << 0)
                    | ((len as u64) << 16)) as libc::c_ulong
            };

            // Enumerate /dev/input/event* and use evdev ioctls to
            // identify keyboards by their capability bits — not by
            // fragile name heuristics.
            if let Ok(entries) = glob("/dev/input/event*") {
                for entry in entries.flatten() {
                    let Ok(f) = std::fs::File::open(&entry) else {
                        continue;
                    };
                    use std::os::unix::io::AsRawFd;
                    let fd = f.as_raw_fd();

                    // Step 1 — check which event types this device supports.
                    // EV_KEY = 1: the device can generate key/button events.
                    let mut ev_bits = [0u8; 32]; // EV_MAX ≤ 0x1f
                    let ret = unsafe {
                        libc::ioctl(fd, eviocgbit(0, ev_bits.len()), ev_bits.as_mut_ptr())
                    };
                    if ret < 0 {
                        continue;
                    }
                    // EV_KEY is bit 1 in the ev-bitmask.
                    if ev_bits[0] & (1u8 << 1) == 0 {
                        continue; // not a key/button device
                    }

                    // Step 2 — the device supports EV_KEY.  Check which
                    // specific keys it can produce.  A real keyboard has
                    // letter keys (KEY_A = 30) and an enter key
                    // (KEY_ENTER = 28); power buttons, consumer-control
                    // remotes, etc. don't.
                    let mut key_bits = [0u8; 96]; // KEY_MAX ≤ 0x2ff
                    let ret = unsafe {
                        libc::ioctl(fd, eviocgbit(1, key_bits.len()), key_bits.as_mut_ptr())
                    };
                    if ret < 0 {
                        continue;
                    }
                    // KEY_A = 30  → byte 3 (30/8), bit 6 (30%8)
                    // KEY_ENTER = 28 → byte 3 (28/8), bit 4 (28%8)
                    let has_key_a = key_bits[3] & (1u8 << 6) != 0;
                    let has_enter = key_bits[3] & (1u8 << 4) != 0;
                    if !has_key_a || !has_enter {
                        continue;
                    }

                    // Step 3 — it's a keyboard.  Get the human-readable
                    // device name via EVIOCGNAME for the device path.
                    let mut name_buf = [0u8; 256];
                    let ret = unsafe {
                        libc::ioctl(fd, eviocgname(name_buf.len()), name_buf.as_mut_ptr())
                    };
                    use std::ffi::CStr;
                    let display = if ret >= 0 {
                        CStr::from_bytes_until_nul(&name_buf)
                            .map(|c| c.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| "keyboard".into())
                    } else {
                        "keyboard".into()
                    };
                    keyboards.push(DeviceInfo {
                        kind: DeviceKind::Keyboard,
                        path: format!("input://{display}"),
                    });
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            use windows::Win32::Devices::DeviceAndDriverInstallation::*;
            use windows::core::GUID;

            unsafe {
                let class_guid: GUID = GUID_DEVCLASS_KEYBOARD;
                let hdev = SetupDiGetClassDevsW(Some(&class_guid), None, None, DIGCF_PRESENT)
                    .unwrap_or_default();
                if hdev.0 != 0 {
                    let mut index = 0u32;
                    loop {
                        let mut devinfo = SP_DEVINFO_DATA::default();
                        devinfo.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
                        let success = SetupDiEnumDeviceInfo(hdev, index, &mut devinfo).as_bool();
                        if !success {
                            break;
                        }

                        // Try friendly name first, fall back to device
                        // description; fall back to an index-based name
                        // so no device is silently dropped.
                        let name = DevicesRegistry::read_setupdi_name(&hdev, &mut devinfo);
                        let path = name
                            .map(|n| format!("keyboard://{n}"))
                            .unwrap_or_else(|| format!("keyboard://{index}"));
                        keyboards.push(DeviceInfo {
                            kind: DeviceKind::Keyboard,
                            path,
                        });
                        index += 1;
                    }
                    let _ = SetupDiDestroyDeviceInfoList(hdev);
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            // Detect keyboards via system_profiler SPHIDDataType, which
            // reports all HID devices (USB, Bluetooth, built-in).
            //
            // TODO: Replace with IOKit HID Manager
            //   IOHIDManagerCreate → IOHIDManagerSetDeviceMatching
            //   (Usage Page 0x01 / Usage 0x06) → IOHIDManagerCopyDevices
            // This eliminates the subprocess overhead and gives
            // structured access to device properties.
            if let Ok(content) = std::process::Command::new("system_profiler")
                .arg("SPHIDDataType")
                .output()
            {
                if content.status.success() {
                    let text = String::from_utf8_lossy(&content.stdout);
                    for line in text.lines() {
                        let trimmed = line.trim();
                        let lower = trimmed.to_lowercase();
                        // Skip section headers like "Keyboards:" — they are
                        // category labels, not devices.
                        if lower == "keyboards:" || lower == "keyboard:" || lower == "keyboards" {
                            continue;
                        }
                        if lower.contains("keyboard") {
                            keyboards.push(DeviceInfo {
                                kind: DeviceKind::Keyboard,
                                path: format!("macos-keyboard://{trimmed}"),
                            });
                        }
                    }
                }
            }
        }

        keyboards
    }

    /// Detect SSH hosts from the current user's `~/.ssh/config` file.
    ///
    /// TODO: SSH device detection should be replaced with explicit
    ///   configuration (ncd config file).  Auto-detection from
    ///   `~/.ssh/config` is incomplete — it misses `Include`
    ///   directives, `/etc/ssh/ssh_config`, wildcard `Host *`
    ///   entries, and `~/.ssh/config.d/` directories.  Moreover,
    ///   SSH connections require credentials, port numbers, and
    ///   usernames that cannot be inferred from the host alias alone.
    fn detect_ssh(&self) -> Vec<DeviceInfo> {
        let mut hosts = Vec::new();
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let config_path = Path::new(&home).join(".ssh").join("config");
        let host_re = Regex::new(r"(?i)^host\s+(.+)").unwrap();

        if let Ok(content) = std::fs::read_to_string(&config_path) {
            for line in content.lines() {
                if let Some(caps) = host_re.captures(line.trim_start()) {
                    let host = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
                    if host != "*" && !host.is_empty() {
                        hosts.push(DeviceInfo {
                            kind: DeviceKind::Ssh,
                            path: format!("ssh://{host}"),
                        });
                    }
                }
            }
        }

        hosts
    }

    // ── Windows SetupDi helpers ───────────────────────────────────

    /// Read a human-readable name from a SetupDi device-info entry.
    /// Tries `SPDRP_FRIENDLYNAME` first, then `SPDRP_DEVICEDESC`.
    #[cfg(target_os = "windows")]
    unsafe fn read_setupdi_name(
        hdev: &windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
        devinfo: &mut windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVINFO_DATA,
    ) -> Option<String> {
        use windows::Win32::Devices::DeviceAndDriverInstallation::*;

        let props = [SPDRP_FRIENDLYNAME, SPDRP_DEVICEDESC];
        for &prop in &props {
            let mut required_size: u32 = 0;
            unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    *hdev,
                    devinfo,
                    prop,
                    None,
                    None,
                    Some(&mut required_size),
                );
            }
            if required_size < 2 {
                continue;
            }

            let byte_size = required_size as usize;
            let mut buf: Vec<u8> = vec![0u8; byte_size];
            let ok = unsafe {
                SetupDiGetDeviceRegistryPropertyW(
                    *hdev,
                    devinfo,
                    prop,
                    None,
                    Some(&mut buf),
                    Some(&mut required_size),
                )
            }
            .as_bool();
            if !ok {
                continue;
            }

            let used = std::cmp::min(required_size as usize, buf.len());
            let u16_len = used / 2;
            let mut name_utf16 = Vec::with_capacity(u16_len);
            for chunk in buf[..u16_len * 2].chunks(2) {
                name_utf16.push(u16::from_le_bytes([chunk[0], chunk[1]]));
            }
            let s = String::from_utf16_lossy(&name_utf16)
                .trim_end_matches('\0')
                .to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
        None
    }

    // ── Public getters ────────────────────────────────────────────

    /// Return a cloned list of devices matching the requested kind.
    pub fn get_devices_by_kind(&self, kind: DeviceKind) -> Vec<DeviceInfo> {
        self.port_to_device
            .values()
            .filter(|info| info.kind == kind)
            .cloned()
            .collect()
    }

    /// Return devices whose path is one of the supplied values.
    pub fn get_devices_by_path(&self, paths: Vec<String>) -> Vec<DeviceInfo> {
        self.port_to_device
            .values()
            .filter(|info| paths.iter().any(|path| path == &info.path))
            .cloned()
            .collect()
    }

    /// Return devices registered on the supplied ports.
    pub fn get_devices_by_port(&self, ports: Vec<u16>) -> Vec<DeviceInfo> {
        ports
            .into_iter()
            .filter_map(|port| self.port_to_device.get(&port).cloned())
            .collect()
    }

    /// Return every registered device and its assigned port.
    pub fn get_all_devices(&self) -> Vec<(u16, DeviceInfo)> {
        self.port_to_device
            .iter()
            .map(|(port, device)| (*port, device.clone()))
            .collect()
    }

    /// Return the registered device for a single port.
    pub fn get_single_device_by_port(&self, port: u16) -> Option<DeviceInfo> {
        self.port_to_device.get(&port).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_print_detected_devices() {
        let registry = DevicesRegistry::new();

        let cameras = registry.detect_cameras();
        let keyboards = registry.detect_keyboards();
        let serials = registry.detect_serials();
        let ssh_hosts = registry.detect_ssh();

        println!("Detected cameras:");
        for camera in &cameras {
            println!("  {:?}", camera);
        }

        println!("Detected keyboards:");
        for kb in &keyboards {
            println!("  {:?}", kb);
        }

        println!("Detected serials:");
        for serial in &serials {
            println!("  {:?}", serial);
        }

        println!("Detected ssh hosts:");
        for host in &ssh_hosts {
            println!("  {:?}", host);
        }

        assert!(cameras.iter().all(|d| !d.path.is_empty()));
        assert!(keyboards.iter().all(|d| !d.path.is_empty()));
        assert!(serials.iter().all(|d| !d.path.is_empty()));
        assert!(ssh_hosts.iter().all(|d| !d.path.is_empty()));
    }

    #[test]
    fn test_initialize_and_lookup_helpers() {
        let mut registry = DevicesRegistry::new();
        let detected = registry.detect_all();
        // Register all detected devices with sequential ports.
        if !detected.is_empty() {
            registry.register(&detected, 10000);
        }

        let all_devices = registry.get_all_devices();
        assert_eq!(all_devices.len(), registry.port_to_device.len());

        if let Some((port, device)) = all_devices.first() {
            let by_kind = registry.get_devices_by_kind(device.kind);
            assert!(by_kind.iter().any(|d| d.path == device.path));

            let by_path = registry.get_devices_by_path(vec![device.path.clone()]);
            assert!(by_path.iter().any(|d| d.path == device.path));

            let by_port = registry.get_devices_by_port(vec![*port]);
            assert!(by_port.iter().any(|d| d.path == device.path));

            assert_eq!(
                registry.get_single_device_by_port(*port).unwrap().path,
                device.path
            );
        }
    }
}
