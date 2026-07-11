use std::net::{IpAddr, Ipv4Addr};

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

    // Spawn all device actors
    let mut tasks = tokio::task::JoinSet::new();
    for entry in &config.device {
        let adapter = Adapter::spawn(
            &entry.driver,
            &entry.device_identifier,
            &entry.device_name,
            &entry.options,
        )
        .await?;

        let conn = libncd_runtime::open(OpenParams::Host {
            listen_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_port: entry.port,
        })
        .await?;

        let name = format!("{}:{}", entry.driver, entry.port);
        eprintln!("[{name}] Listening on port {}...", entry.port);

        tasks.spawn(device_actor(conn, adapter, name));
    }

    // Wait for all actors to complete, or Ctrl+C to shutdown
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

async fn device_actor(mut conn: ConnHandler, mut adapter: Adapter, name: String) {
    loop {
        // Check if the adapter process has exited
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
                        // Adapters stdout closed — process exited
                        eprintln!("[{name}] Adapter stdout closed");
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = adapter.kill().await;
    match libncd_runtime::close(conn).await {
        Ok(Ok(remaining)) => {
            if !remaining.is_empty() {
                eprintln!(
                    "[{name}] {remaining_len} unread messages discarded on close",
                    remaining_len = remaining.len()
                );
            }
        }
        Ok(Err(e)) => eprintln!("[{name}] Close error: {e}"),
        Err(e) => eprintln!("[{name}] Close error: {e}"),
    }
}
