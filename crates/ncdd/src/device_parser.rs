use serde_json::Value;

const MAX_ADAPTER_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeviceKind {
    Camera,
    Keyboard,
    Instruction,
    File,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    RemoteToLinux,
    LinuxToRemote,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::RemoteToLinux => "remote->linux",
            Direction::LinuxToRemote => "linux->remote",
        }
    }
}

impl DeviceKind {
    fn label(self) -> &'static str {
        match self {
            DeviceKind::Camera => "camera",
            DeviceKind::Keyboard => "keyboard",
            DeviceKind::Instruction => "instruction",
            DeviceKind::File => "file",
            DeviceKind::Unknown => "unknown",
        }
    }
}

pub struct DeviceParser {
    label: String,
    kind: DeviceKind,
    remote_to_linux: Vec<u8>,
    linux_to_remote: Vec<u8>,
    remote_frame_count: u64,
    local_frame_count: u64,
}

impl DeviceParser {
    pub fn new(minor: u8, name: &str, remote_port: u16) -> Self {
        let kind = infer_device_kind(name, remote_port);
        Self {
            label: format!(
                "kind={} device={} minor={} port={}",
                kind.label(),
                name,
                minor,
                remote_port
            ),
            kind,
            remote_to_linux: Vec::new(),
            linux_to_remote: Vec::new(),
            remote_frame_count: 0,
            local_frame_count: 0,
        }
    }

    pub fn inspect_remote_to_linux(&mut self, data: &[u8]) {
        if self.kind == DeviceKind::Unknown {
            return;
        }
        let label = self.label.clone();
        let kind = self.kind;
        inspect_stream(
            &label,
            kind,
            Direction::RemoteToLinux,
            &mut self.remote_to_linux,
            &mut self.remote_frame_count,
            data,
        );
    }

    pub fn inspect_linux_to_remote(&mut self, data: &[u8]) {
        if self.kind == DeviceKind::Unknown {
            return;
        }
        let label = self.label.clone();
        let kind = self.kind;
        inspect_stream(
            &label,
            kind,
            Direction::LinuxToRemote,
            &mut self.linux_to_remote,
            &mut self.local_frame_count,
            data,
        );
    }
}

fn infer_device_kind(name: &str, remote_port: u16) -> DeviceKind {
    let lowered = name.to_ascii_lowercase();

    if lowered.contains("camera") || remote_port == 9000 {
        DeviceKind::Camera
    } else if lowered.contains("keyboard") || remote_port == 10000 {
        DeviceKind::Keyboard
    } else if lowered.contains("instruction") || remote_port == 11000 {
        DeviceKind::Instruction
    } else if lowered.contains("file") || remote_port == 8000 {
        DeviceKind::File
    } else {
        DeviceKind::Unknown
    }
}

fn inspect_stream(
    device_label: &str,
    kind: DeviceKind,
    direction: Direction,
    buffer: &mut Vec<u8>,
    frame_count: &mut u64,
    data: &[u8],
) {
    if data.is_empty() {
        return;
    }

    buffer.extend_from_slice(data);

    loop {
        if buffer.len() < 4 {
            return;
        }

        let payload_len = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        if payload_len > MAX_ADAPTER_FRAME_SIZE {
            eprintln!(
                "[ncdd parse][{}][{}] invalid adapter frame length {}; parser buffer cleared",
                direction.label(),
                device_label,
                payload_len
            );
            buffer.clear();
            return;
        }

        let frame_len = 4 + payload_len;
        if buffer.len() < frame_len {
            return;
        }

        let payload = buffer[4..frame_len].to_vec();
        buffer.drain(..frame_len);
        *frame_count += 1;
        log_frame(device_label, kind, direction, *frame_count, &payload);
    }
}

fn log_frame(
    device_label: &str,
    kind: DeviceKind,
    direction: Direction,
    count: u64,
    payload: &[u8],
) {
    match kind {
        DeviceKind::Camera => log_camera(device_label, direction, count, payload),
        DeviceKind::Keyboard => {
            log_json_device("keyboard", device_label, direction, count, payload)
        }
        DeviceKind::Instruction => {
            log_json_device("instruction", device_label, direction, count, payload)
        }
        DeviceKind::File => {
            eprintln!(
                "[ncdd parse][file][{}][{}] frame={} payload={} bytes",
                direction.label(),
                device_label,
                count,
                payload.len()
            );
        }
        DeviceKind::Unknown => {}
    }
}

fn log_camera(device_label: &str, direction: Direction, count: u64, payload: &[u8]) {
    let dimensions = jpeg_dimensions(payload)
        .map(|(width, height)| format!("{width}x{height}"))
        .unwrap_or_else(|| "unknown-size".to_string());

    eprintln!(
        "[ncdd parse][camera][{}][{}] frame={} jpeg={} bytes size={}",
        direction.label(),
        device_label,
        count,
        payload.len(),
        dimensions
    );
}

fn log_json_device(
    kind_label: &str,
    device_label: &str,
    direction: Direction,
    count: u64,
    payload: &[u8],
) {
    match serde_json::from_slice::<Value>(payload) {
        Ok(value) => {
            if kind_label == "keyboard" {
                log_keyboard_json(device_label, direction, count, payload.len(), &value);
            } else {
                log_instruction_json(device_label, direction, count, payload.len(), &value);
            }
        }
        Err(error) => {
            eprintln!(
                "[ncdd parse][{}][{}][{}] frame={} payload={} bytes json-error={}",
                kind_label,
                direction.label(),
                device_label,
                count,
                payload.len(),
                error
            );
        }
    }
}

fn log_keyboard_json(
    device_label: &str,
    direction: Direction,
    count: u64,
    payload_len: usize,
    value: &Value,
) {
    let event = value.get("event").and_then(Value::as_str);
    let action = value.get("action").and_then(Value::as_str);
    let key_type = value.get("key_type").and_then(Value::as_str);
    let key = value.get("key").and_then(Value::as_str);
    let text_len = value
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.chars().count());

    eprintln!(
        "[ncdd parse][keyboard][{}][{}] frame={} payload={} bytes event={:?} action={:?} key_type={:?} key={:?} text_len={:?}",
        direction.label(),
        device_label,
        count,
        payload_len,
        event,
        action,
        key_type,
        key,
        text_len
    );
}

fn log_instruction_json(
    device_label: &str,
    direction: Direction,
    count: u64,
    payload_len: usize,
    value: &Value,
) {
    let id = value.get("id").and_then(Value::as_str);
    let shell = value.get("shell").and_then(Value::as_bool);
    let command = value.get("command").and_then(Value::as_str);
    let argv_len = value.get("argv").and_then(Value::as_array).map(Vec::len);
    let ok = value.get("ok").and_then(Value::as_bool);
    let returncode = value.get("returncode").and_then(Value::as_i64);
    let stdout_len = value
        .get("stdout")
        .and_then(Value::as_str)
        .map(|text| text.len());
    let stderr_len = value
        .get("stderr")
        .and_then(Value::as_str)
        .map(|text| text.len());

    eprintln!(
        "[ncdd parse][instruction][{}][{}] frame={} payload={} bytes id={:?} shell={:?} command={:?} argv_len={:?} ok={:?} returncode={:?} stdout={} stderr={}",
        direction.label(),
        device_label,
        count,
        payload_len,
        id,
        shell,
        command,
        argv_len,
        ok,
        returncode,
        stdout_len.unwrap_or(0),
        stderr_len.unwrap_or(0)
    );
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u16, u16)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }

    let mut i = 2;
    while i + 3 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }

        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }

        let marker = data[i];
        i += 1;

        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }

        if i + 2 > data.len() {
            return None;
        }
        let segment_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if segment_len < 2 || i + segment_len > data.len() {
            return None;
        }

        if is_sof_marker(marker) {
            if segment_len < 7 {
                return None;
            }
            let height = u16::from_be_bytes([data[i + 3], data[i + 4]]);
            let width = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            return Some((width, height));
        }

        i += segment_len;
    }

    None
}

fn is_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}
