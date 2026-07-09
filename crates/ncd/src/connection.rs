use std::net::IpAddr;

use crate::error::NcdError;
use libncd_runtime::ConnHandler;
use libncd_runtime::error::ConnectionClosed as ConnClosed;
use libncd_runtime::{close, open, read, status, write};
use libncd_runtime::{ConnStatus, OpenParams};

use std::fmt;

pub struct NcdConnection {
    connection: ConnHandler,
}

impl fmt::Debug for NcdConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NcdConnection").finish()
    }
}

impl NcdConnection {
    pub async fn create_connection(ip: IpAddr, port: u16) -> Result<NcdConnection, NcdError> {
        let params = OpenParams::Host { listen_addr: ip, listen_port: port };
        match open(params).await {
            Ok(conn) => Ok(NcdConnection { connection: conn }),
            Err(e) => Err(NcdError::CreateConnectionError(e)),
        }
    }

    pub async fn get_connection_status(&mut self) -> Result<ConnStatus, NcdError> {
        match status(&mut self.connection).await {
            Ok(status) => Ok(status),
            Err(e) => Err(NcdError::InnerConnectionError(e)),
        }
    }

    pub async fn close_connection(self) -> Result<(), NcdError> {
        match close(self.connection).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(NcdError::CloseConnectionError(ConnClosed::Error(e))),
            Err(e) => Err(NcdError::CloseConnectionError(e)),
        }
    }

    /// 读取对端发来的一个完整数据包
    pub async fn read_connection(&mut self) -> Result<Vec<u8>, NcdError> {
        match read(&mut self.connection).await {
            Ok(data) => Ok(data),
            Err(e) => Err(NcdError::InnerConnectionError(e)),
        }
    }

    /// 发送一个完整数据包给对端
    pub async fn write_connection(&mut self, data: &[u8]) -> Result<(), NcdError> {
        match write(&mut self.connection, data).await {
            Ok(()) => Ok(()),
            Err(e) => Err(NcdError::InnerConnectionError(e)),
        }
    }

}
