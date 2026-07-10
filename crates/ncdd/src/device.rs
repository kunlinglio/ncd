use std::net::IpAddr;

use libncd_runtime::OpenParams;
use libncd_runtime::{self, ConnHandler};

use libncd_runtime::error::ConnectionCreateError;
use tokio::sync::mpsc;

/// Commands that the main loop sends to the TCP actor task.
enum WriteCommand {
    Data(Vec<u8>),
    Close,
}

#[derive(Debug)]
pub enum DeviceOperationError {
    NotOpen,        // write_tx == None, open() not called yet
    ConnectionLost, // actor task has exited
    #[allow(dead_code)]
    ConnectionCreateError(ConnectionCreateError),
}

pub struct Device {
    pub minor: u8,
    pub name: String,
    pub remote_ip: IpAddr,
    pub remote_port: u16,
    write_tx: Option<mpsc::UnboundedSender<WriteCommand>>,
}

/// write_tx: main loop -> actor task
/// data_tx: actor task -> main loop
impl Device {
    pub fn new(minor: u8, name: String, remote_ip: IpAddr, remote_port: u16) -> Self {
        Device {
            minor,
            name,
            remote_ip,
            remote_port,
            write_tx: None,
        }
    }

    /// Open a TCP connection to the remote host and spawn an actor task
    /// that owns the ConnHandler.  `data_tx` is used by the actor to send
    /// received data back to the main loop.
    pub async fn open(
        &mut self,
        data_tx: mpsc::UnboundedSender<(u8, Vec<u8>)>,
    ) -> Result<(), DeviceOperationError> {
        let conn = libncd_runtime::open(OpenParams::Device {
            host_addr: self.remote_ip,
            host_port: self.remote_port,
        })
        .await
        .map_err(|e| DeviceOperationError::ConnectionCreateError(e))?;

        let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteCommand>();
        self.write_tx = Some(write_tx);

        let minor = self.minor;
        tokio::spawn(tcp_actor(conn, minor, data_tx, write_rx));

        Ok(())
    }

    /// Send data to the remote peer through the actor task.
    pub fn write(&self, data: Vec<u8>) -> Result<(), DeviceOperationError> {
        match &self.write_tx {
            Some(tx) => tx
                .send(WriteCommand::Data(data))
                .map_err(|_| DeviceOperationError::ConnectionLost),
            None => Err(DeviceOperationError::NotOpen),
        }
    }

    /// Gracefully close the TCP connection through the actor task.
    pub fn close(&mut self) {
        if let Some(tx) = self.write_tx.take() {
            let _ = tx.send(WriteCommand::Close);
        }
    }
}

/// Actor task that owns the ConnHandler exclusively.
/// Handles both reading from and writing to the TCP connection.
async fn tcp_actor(
    mut conn: ConnHandler,
    minor: u8,
    data_tx: mpsc::UnboundedSender<(u8, Vec<u8>)>,
    mut write_rx: mpsc::UnboundedReceiver<WriteCommand>,
) {
    loop {
        tokio::select! {
            // read from TCP → forward to main loop via data_tx
            result = libncd_runtime::read(&mut conn) => {
                match result {
                    Ok(data) => {
                        if data_tx.send((minor, data)).is_err() {
                            break;  // main loop channel closed
                        }
                    }
                    Err(e) => {
                        eprintln!("Device {} read error: {:?}", minor, e);
                        break;
                    }
                }
            }
            // main loop sends command → write to TCP or close
            Some(cmd) = write_rx.recv() => {
                match cmd {
                    WriteCommand::Data(data) => {
                        if libncd_runtime::write(&mut conn, data).await.is_err() {
                            break;
                        }
                    }
                    WriteCommand::Close => {
                        // protocol-level close, conn consumed
                        match libncd_runtime::close(conn).await {
                            Ok(Ok(remaining)) => {
                                for msg in remaining {
                                    let _ = data_tx.send((minor, msg));
                                }
                            }
                            Ok(Err(e)) => eprintln!("Device {} close error: {:?}", minor, e),
                            Err(e) => eprintln!("Device {} close error: {:?}", minor, e),
                        }
                        return;  // conn is consumed, exit actor
                    }
                }
            }
        }
    }
    // If we broke out of the loop (read error), conn is dropped here
}
