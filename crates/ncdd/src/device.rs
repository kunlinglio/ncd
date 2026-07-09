use std::io;
use std::net::IpAddr;

use libncd_runtime::OpenParams;
use libncd_runtime::{self, ConnHandler};

use libncd_runtime::error::ConnectionClosed;
use libncd_runtime::error::ConnectionCreateError;
use libncd_runtime::error::ConnectionError;

pub struct Device {
    pub minor: u8,
    pub name: String,
    pub remote_ip: IpAddr,
    pub remote_port: u16,
    conn: Option<ConnHandler>, // tcp connection(init after open)
}

#[derive(Debug)]
pub enum DeviceOperationError {
    NotConnected,
    ConnectionClosed(ConnectionClosed),
    ConnectionCreateError(ConnectionCreateError),
}

impl Device {
    pub fn new(minor: u8, name: String, remote_ip: IpAddr, remote_port: u16) -> Self {
        Device {
            minor,
            name,
            remote_ip,
            remote_port,
            conn: None,
        }
    }

    pub async fn open(&mut self) -> Result<(), DeviceOperationError> {
        let conn = libncd_runtime::open(OpenParams::Device {
            host_addr: self.remote_ip,
            host_port: self.remote_port,
        })
        .await
        .map_err(|e| DeviceOperationError::ConnectionCreateError(e))?;
        self.conn = Some(conn);
        Ok(())
    }

    pub async fn close(
        self,
    ) -> Result<Result<Vec<Vec<u8>>, ConnectionError>, DeviceOperationError> {
        if let Some(conn) = self.conn {
            libncd_runtime::close(conn)
                .await
                .map_err(|e| DeviceOperationError::ConnectionClosed(e))
        } else {
            Err(DeviceOperationError::NotConnected)
        }
    }

    pub async fn read(&mut self) -> Result<Vec<u8>, DeviceOperationError> {
        if let Some(conn) = &mut self.conn {
            libncd_runtime::read(conn)
                .await
                .map_err(|e| DeviceOperationError::ConnectionClosed(e))
        } else {
            Err(DeviceOperationError::NotConnected)
        }
    }

    pub async fn write(&mut self, data: Vec<u8>) -> Result<(), DeviceOperationError> {
        if let Some(conn) = &mut self.conn {
            libncd_runtime::write(conn, data)
                .await
                .map_err(|e| DeviceOperationError::ConnectionClosed(e))
        } else {
            Err(DeviceOperationError::NotConnected)
        }
    }
}
