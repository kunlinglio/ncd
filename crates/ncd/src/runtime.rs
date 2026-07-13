use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use libncd_runtime::{self, ConnHandler, OpenParams, error::ConnectionClosed};

use crate::adapter_loader::adapter::{Adapter, AdapterError};
use crate::config::HostConfig;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("No devices configured")]
    NoDevices,
    #[error("Adapter error: {0}")]
    Adapter(#[from] AdapterError),
    #[error("NCD connection error: {0}")]
    Ncd(#[from] libncd_runtime::error::ConnectionCreateError),
}

/// Run the NCD host with loaded configuration.
pub async fn run(config: HostConfig) -> Result<(), RuntimeError> {
    if config.device.is_empty() {
        return Err(RuntimeError::NoDevices);
    }

    // Spawn a reconnecting actor for each device entry.
    let mut tasks = tokio::task::JoinSet::new();
    for entry in &config.device {
        let name = format!("{}:{}", entry.driver, entry.port);
        eprintln!("[{name}] Listening on port {}...", entry.port);

        let port = entry.port;
        let driver = entry.driver.clone();
        let identifier = entry.device_identifier.clone();
        let device_name = entry.device_name.clone();
        let options = entry.options.clone();

        tasks.spawn(device_actor(
            name,
            port,
            driver,
            identifier,
            device_name,
            options,
        ));
    }

    // Keep running until Ctrl+C or all actors fail.
    let all_done = async {
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(()) => {}
                Err(e) if e.is_panic() => {
                    eprintln!("Actor panicked: {e}");
                }
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nShutting down...");
            tasks.abort_all();
        }
        _ = all_done => {}
    }

    Ok(())
}

async fn device_actor(
    name: String,
    port: u16,
    driver: String,
    identifier: String,
    device_name: String,
    options: std::collections::HashMap<String, String>,
) {
    loop {
        // Accept a new NCD connection.
        let conn = match libncd_runtime::open(OpenParams::Host {
            listen_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_port: port,
        })
        .await
        {
            Ok(conn) => {
                eprintln!("[{name}] Connection accepted");
                conn
            }
            Err(e) => {
                eprintln!("[{name}] Failed to accept: {e}, retrying...");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        // Spawn a fresh adapter process for this connection.
        let mut adapter_options = options.clone();
        adapter_options
            .entry("port".to_string())
            .or_insert_with(|| port.to_string());
        let adapter =
            match Adapter::spawn(&driver, &identifier, &device_name, &adapter_options).await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("[{name}] Failed to spawn adapter: {e}");
                    let _ = libncd_runtime::close(conn).await;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

        // Bridge data until the connection or adapter closes.
        handle_connection(conn, adapter, &name).await;

        eprintln!("[{name}] Peer disconnected, re-listening...");
    }
}

/// Bidirectional bridge between one NCD connection and one adapter process.
async fn handle_connection(mut conn: ConnHandler, mut adapter: Adapter, name: &str) {
    loop {
        // Check if the adapter process has exited.
        if let Some(status) = adapter.try_exit_status() {
            if !status.success() {
                eprintln!("[{name}] Adapter exited with {status}");
            } else {
                eprintln!("[{name}] Adapter exited normally");
            }
            break;
        }

        tokio::select! {
            // NCD connection -> Python adapter stdin
            result = libncd_runtime::read(&mut conn) => {
                match result {
                    Ok(data) => {
                        if let Err(e) = adapter.write(&data).await {
                            eprintln!("[{name}] Write to adapter failed: {e}");
                            break;
                        }
                    }
                    Err(ConnectionClosed::Normal) => {
                        eprintln!("[{name}] NCD peer disconnected");
                        break;
                    }
                    Err(ConnectionClosed::Error(e)) => {
                        eprintln!("[{name}] NCD read error: {e}");
                        break;
                    }
                    Err(e) => {
                        eprintln!("[{name}] NCD connection closed: {e}");
                        break;
                    }
                }
            }
            // Python adapter stdout -> NCD connection
            data = adapter.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(e) = libncd_runtime::write(&mut conn, bytes).await {
                            eprintln!("[{name}] NCD write error: {e}");
                            break;
                        }
                    }
                    None => {
                        // Adapter stdout closed — process exited.
                        eprintln!("[{name}] Adapter stdout closed");
                        break;
                    }
                }
            }
        }
    }

    match libncd_runtime::close(conn).await {
        Ok(Ok(remaining)) => {
            if !remaining.is_empty() {
                eprintln!(
                    "[{name}] {} unread messages delivered on close",
                    remaining.len()
                );
                for data in remaining {
                    if let Err(e) = adapter.write(&data).await {
                        eprintln!("[{name}] Write remaining data to adapter failed: {e}");
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Ok(Err(e)) => eprintln!("[{name}] Close error: {e}"),
        Err(e) => eprintln!("[{name}] Close error: {e}"),
    }

    // Cleanup.
    let _ = adapter.kill().await;
}
