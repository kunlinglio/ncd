use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use super::DRIVERS_DIR;

use super::list::get_adapters;

/// Raw device info deserialized from a Python adapter's `list` JSON output.
#[derive(Debug, Clone, Deserialize)]
pub struct RawDevice {
    pub identifier: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Manages a single Python adapter child process, spawned via `uv run`.
pub struct Adapter {
    child: Child,
    stdin: ChildStdin,
    /// Receives raw bytes from the adapter's stdout (via internal reader task).
    data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("Unknown driver: {0}")]
    UnknownDriver(String),
    #[error("Failed to spawn uv run: {0}")]
    Spawn(std::io::Error),
    #[error("Adapter I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to list devices for adapter '{0}': exit code {1}")]
    ListFailed(String, i32),
    #[error("Failed to parse device JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Synchronously spawn python adapter to list devices
pub fn list_devices(adapter_dir: &str, script_path: &Path) -> Result<Vec<RawDevice>, AdapterError> {
    let output = std::process::Command::new("uv")
        .arg("run")
        .arg("--project")
        .arg(adapter_dir)
        .arg("python")
        .arg(script_path)
        .arg("list")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(AdapterError::Spawn)?;

    if !output.status.success() {
        return Err(AdapterError::ListFailed(
            script_path.display().to_string(),
            output.status.code().unwrap_or(-1),
        ));
    }

    let devices: Vec<RawDevice> = serde_json::from_slice(&output.stdout)?;
    Ok(devices)
}

impl Adapter {
    pub async fn spawn(
        name: &str,
        device_identifier: &str,
        device_name: &str,
        options: &HashMap<String, String>,
    ) -> Result<Self, AdapterError> {
        let list = get_adapters();
        let info = list
            .adapters
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| AdapterError::UnknownDriver(name.to_string()))?;
        let script = PathBuf::from(DRIVERS_DIR).join(&info.path);

        let mut cmd = Command::new("uv");
        cmd.arg("run")
            .arg("--project")
            .arg(DRIVERS_DIR)
            .arg("python")
            .arg(&script)
            .arg("run")
            .arg(device_identifier)
            .arg(device_name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in options {
            cmd.arg(format!("--{key}"));
            cmd.arg(value);
        }

        let mut child = cmd.spawn().map_err(AdapterError::Spawn)?;
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");

        // Spawn internal reader task to avoid pipe read un-cancellable problem
        let (data_tx, data_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let mut reader = tokio::io::BufReader::new(stdout);
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if data_tx.send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            data_rx,
        })
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<(), AdapterError> {
        self.stdin.write_all(data).await?;
        Ok(())
    }

    /// Returns None if the adapter process has exited and the channel is closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.data_rx.recv().await
    }

    /// Check if the process has exited. Returns exit status if it has.
    pub fn try_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Kill the process and wait for it to exit.
    pub async fn kill(mut self) -> Result<(), AdapterError> {
        let _ = self.child.start_kill();
        self.child.wait().await?;
        Ok(())
    }
}
