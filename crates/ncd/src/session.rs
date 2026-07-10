use std::sync::{Arc, Mutex};
use tokio::select;
use tokio::sync::Notify;

use crate::connection::NcdConnection;
use crate::device::NcdDeviceOperations;
use crate::error::NcdError;

pub struct NcdSession {
    connection: NcdConnection,
    device: Arc<Mutex<Box<dyn NcdDeviceOperations>>>,
    ready_notify: Arc<Notify>,
    verbose: bool,
}

impl NcdSession {
    pub fn new(
        connection: NcdConnection,
        device: Arc<Mutex<Box<dyn NcdDeviceOperations>>>,
        ready_notify: Arc<Notify>,
    ) -> Self {
        NcdSession { connection, device, ready_notify, verbose: false }
    }

    /// Enable verbose mode — every data chunk is logged with direction
    /// and preview on stderr.
    pub fn set_verbose(&mut self, v: bool) {
        self.verbose = v;
    }

    pub async fn run(&mut self) -> Result<(), NcdError> {
        let buf_size = self.device.lock().unwrap().read_buffer_size();
        let mut buf = vec![0u8; buf_size];

        // Devices default to Open — no open() call needed.

        // Kick-start the notify branch so that the first read happens even for
        // drivers that don't have a background thread yet.  Drivers with
        // a capture thread (CameraDriver) will re-notify on every frame.
        self.ready_notify.notify_one();

        let result = self.event_loop(&mut buf).await;

        // Devices are closed externally or via Drop — no close() call needed.
        result
    }

    async fn event_loop(&mut self, buf: &mut [u8]) -> Result<(), NcdError> {
        loop {
            let notify = self.ready_notify.notified();
            select! {
                // ── Path A: remote → device ──────────────────────────
                data = self.connection.read_connection() => {
                    let data = data?;
                    if !data.is_empty() {
                        if self.verbose { log_data("→ host", &data); }
                        let write_err = {
                            self.device.lock().unwrap().write(&data).err()
                        };
                        if let Some(e) = write_err {
                            let msg = format!("Error: {e}\n");
                            let _ = self.connection.write_connection(msg.into_bytes()).await;
                        }
                    }
                }

                // ── Path B: device → remote ──────────────────────────
                _ = notify => {
                    let read_res = {
                        self.device.lock().unwrap().read(buf)
                    };
                    match read_res {
                        Ok(n) if n > 0 => {
                            let chunk = buf[..n].to_vec();
                            if self.verbose { log_data("host →", &chunk); }
                            self.connection.write_connection(chunk).await?;
                            self.ready_notify.notify_one();
                        }
                        Ok(_) => {}
                        Err(e) => {
                            let msg = format!("Error: {e}\n");
                            let _ = self.connection.write_connection(msg.into_bytes()).await;
                        }
                    }
                }
            }
        }
    }
}

/// Print a data chunk with direction and a content preview.
fn log_data(dir: &str, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    // Short printable text → show inline.
    if data.len() <= 200
        && data.iter().all(|&b| b.is_ascii_graphic() || b == b'\n' || b == b'\r' || b == b'\t' || b == b' ')
    {
        let text = String::from_utf8_lossy(data)
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        eprintln!("  {dir}  {text}");
    } else {
        let preview = data.len().min(32);
        let hex: String = data[..preview]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        if data.len() > preview {
            eprintln!("  {dir}  {len} B  {hex} …", len = data.len());
        } else {
            eprintln!("  {dir}  {len} B  {hex}", len = data.len());
        }
    }
}
