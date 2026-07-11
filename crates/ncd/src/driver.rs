use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// Map of driver name → Python source code, embedded at compile time.
static EMBEDDED_DRIVERS: &[(&str, &str)] = &[
    ("serial", include_str!("../drivers/serial.py")),
    ("keyboard", include_str!("../drivers/keyboard.py")),
];

/// Manages a single Python driver child process.
///
/// Uses an internal reader task + mpsc channel for stdout to avoid
/// borrow conflicts with stdin writes in the tokio::select! loop.
pub struct DriverProcess {
    child: Child,
    stdin: ChildStdin,
    /// Receives raw bytes from the driver's stdout (via internal reader task).
    data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("Unknown driver: {0}")]
    UnknownDriver(String),
    #[error("Failed to extract driver file: {0}")]
    Extract(std::io::Error),
    #[error("Failed to spawn python3 process: {0}")]
    Spawn(std::io::Error),
    #[error("Driver I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl DriverProcess {
    /// Spawn `python3 <temp_dir>/<driver_name>.py --key1 val1 --key2 val2 ...`
    ///
    /// Extracts the .py file from embedded resources to temp_dir if not already there.
    /// Returns immediately after spawn — if the process is alive, the driver is "ready".
    /// If the driver fails to open the device, it will exit non-zero on its own.
    pub async fn spawn(
        driver_name: &str,
        config: &HashMap<String, String>,
        temp_dir: &Path,
    ) -> Result<Self, DriverError> {
        // Extract .py file to temp directory
        let source = EMBEDDED_DRIVERS
            .iter()
            .find(|(name, _)| *name == driver_name)
            .map(|(_, src)| *src)
            .ok_or_else(|| DriverError::UnknownDriver(driver_name.to_string()))?;

        let py_path = temp_dir.join(format!("{driver_name}.py"));
        std::fs::write(&py_path, source).map_err(DriverError::Extract)?;

        // Build command: python3 <path> --key1 val1 --key2 val2 ...
        let mut cmd = Command::new("python3");
        cmd.arg(&py_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        for (key, value) in config {
            cmd.arg(format!("--{key}"));
            cmd.arg(value);
        }

        let mut child = cmd.spawn().map_err(DriverError::Spawn)?;

        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");

        // Spawn internal reader task: child stdout → mpsc channel
        let (data_tx, data_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let mut reader = tokio::io::BufReader::new(stdout);
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break, // EOF — process exited
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

    /// Write raw bytes to the driver's stdin (data from remote NCD peer).
    pub async fn write(&mut self, data: &[u8]) -> Result<(), DriverError> {
        self.stdin.write_all(data).await?;
        Ok(())
    }

    /// Receive raw bytes from the driver's stdout (data from the local device).
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
