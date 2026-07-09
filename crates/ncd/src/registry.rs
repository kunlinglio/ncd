use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
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

    pub fn initialize(&mut self, start_port: u16) {
        let paths = self.detect_devices();
        let ports = self.allocate_ports(paths.len(), start_port);
        self.register_devices(paths, ports);
    }

    // ── 设备检测 ──

    fn detect_devices(&self) -> Vec<String> {
        let mut paths = Vec::new();
        paths.extend(self.detect_cameras());
        paths.extend(self.detect_serial());
        paths.extend(self.detect_ssh());
        if paths.is_empty() {
            paths.push("/dev/test".into());
        }
        paths
    }

    fn detect_cameras(&self) -> Vec<String> {
        #[allow(unused_mut)]
        let mut cams = Vec::new();
        #[cfg(target_os = "linux")]
        {
            for i in 0..8 {
                let path = format!("/dev/video{i}");
                if Path::new(&path).exists() {
                    cams.push(path);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            // macOS 摄像头走 AVFoundation，无设备文件。
            // 通过 system_profiler 可枚举，但开销大；先留空。
        }
        #[cfg(target_os = "windows")]
        {
            // 集成 nokhwa 后可枚举 CameraInfo，届时删除下面这行
            let _ = &mut cams;
        }
        cams
    }

    // ── 串口检测 ──

    fn detect_serial(&self) -> Vec<String> {
        let mut ports = Vec::new();
        #[cfg(target_os = "linux")]
        {
            for pattern in &["/dev/ttyS", "/dev/ttyUSB", "/dev/ttyACM"] {
                for i in 0..8 {
                    let path = format!("{pattern}{i}");
                    if Path::new(&path).exists() {
                        ports.push(path);
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(entries) = std::fs::read_dir("/dev") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with("cu.") || name.starts_with("tty.") {
                        ports.push(format!("/dev/{name}"));
                    }
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            for i in 1..=16 {
                let path = format!("\\\\.\\COM{i}");
                ports.push(path);
            }
        }
        ports
    }

    // ── SSH 检测 ──

    fn detect_ssh(&self) -> Vec<String> {
        let mut hosts = Vec::new();
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let config_path = Path::new(&home).join(".ssh").join("config");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            for line in content.lines() {
                let trimmed = line.trim_start();
                if trimmed.to_lowercase().starts_with("host ") {
                    let host = trimmed[5..].trim();
                    if host != "*" && !host.is_empty() {
                        hosts.push(format!("ssh://{host}"));
                    }
                }
            }
        }
        hosts
    }

    // ── 端口分配 ──

    fn allocate_ports(&self, count: usize, start: u16) -> Vec<u16> {
        let mut ports = Vec::with_capacity(count);
        let mut candidate = start;
        while ports.len() < count {
            if !self.port_to_device.contains_key(&candidate) {
                ports.push(candidate);
            }
            candidate += 1;
        }
        ports
    }

    fn register_devices(&mut self, paths: Vec<String>, ports: Vec<u16>) {
        for (path, port) in paths.into_iter().zip(ports) {
            self.port_to_device.insert(port, DeviceInfo { path });
        }
    }

    pub fn choose_device(&self, port: u16) -> Option<&DeviceInfo> {
        self.port_to_device.get(&port)
    }
}
