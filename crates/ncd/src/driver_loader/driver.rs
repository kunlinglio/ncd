use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use super::DRIVERS_DIR;

use super::registry::DriverInfo;
use super::registry::get_drivers;

/// Manages a single Python driver child process, spawned via `uv run`.
pub struct Driver {
    child: Child,
    stdin: ChildStdin,
    /// Receives raw bytes from the driver's stdout (via internal reader task).
    data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("Unknown driver: {0}")]
    UnknownDriver(String),
    #[error("Failed to spawn uv run: {0}")]
    Spawn(std::io::Error),
    #[error("Driver I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl Driver {
    pub async fn spawn(name: &str, options: &HashMap<String, String>) -> Result<Self, DriverError> {
        let info = get_drivers()
            .query_driver(name)
            .ok_or_else(|| DriverError::UnknownDriver(name.to_string()))?;
        let script = PathBuf::from(DRIVERS_DIR).join(&info.driver_path);

        let mut cmd = Command::new("uv");
        cmd.arg("run")
            .arg("--project")
            .arg(DRIVERS_DIR)
            .arg("python")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (key, value) in options {
            cmd.arg(format!("--{key}"));
            cmd.arg(value);
        }

        let mut child = cmd.spawn().map_err(DriverError::Spawn)?;
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

    pub async fn write(&mut self, data: &[u8]) -> Result<(), DriverError> {
        self.stdin.write_all(data).await?;
        Ok(())
    }

    /// Returns None if the driver process has exited and the channel is closed.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.data_rx.recv().await
    }

    /// Check if the process has exited. Returns exit status if it has.
    pub fn try_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Kill the process and wait for it to exit.
    pub async fn kill(mut self) -> Result<(), DriverError> {
        let _ = self.child.start_kill();
        self.child.wait().await?;
        Ok(())
    }
}
