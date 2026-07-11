use std::net::{IpAddr, Ipv4Addr};

use libncd_runtime::{self, ConnHandler, OpenParams, error::ConnectionClosed};

use crate::config::HostConfig;
use crate::driver_loader::driver::{Driver, DriverError};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("No devices configured")]
    NoDevices,
    #[error("Driver error: {0}")]
    Driver(#[from] DriverError),
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
        let driver = Driver::spawn(&entry.driver, &entry.options).await?;

        let conn = libncd_runtime::open(OpenParams::Host {
            listen_addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            listen_port: entry.port,
        })
        .await?;

        let name = format!("{}:{}", entry.driver, entry.port);
        eprintln!("[{name}] Listening on port {}...", entry.port);

        tasks.spawn(device_actor(conn, driver, name));
    }

    // Wait for all actors to complete, or Ctrl+C to shut down
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

async fn device_actor(mut conn: ConnHandler, mut driver: Driver, name: String) {
    loop {
        // Check if the driver process has exited
        if let Some(status) = driver.try_exit_status() {
            if !status.success() {
                eprintln!("[{name}] Driver exited with {status}");
            } else {
                eprintln!("[{name}] Driver exited normally");
            }
            break;
        }

        tokio::select! {
            // NCD connection -> Python driver stdin
            result = libncd_runtime::read(&mut conn) => {
                match result {
                    Ok(data) => {
                        if let Err(e) = driver.write(&data).await {
                            eprintln!("[{name}] Write to driver failed: {e}");
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
            // Python driver stdout -> NCD connection
            data = driver.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(e) = libncd_runtime::write(&mut conn, bytes).await {
                            eprintln!("[{name}] NCD write error: {e}");
                            break;
                        }
                    }
                    None => {
                        // Driver stdout closed — process exited
                        eprintln!("[{name}] Driver stdout closed");
                        break;
                    }
                }
            }
        }
    }

    // Cleanup
    let _ = driver.kill().await;
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
