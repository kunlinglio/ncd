//! Async runtime implementation for the Network Character Device Protocol based on tokio.

mod config;
mod connection;
pub use connection::ConnHandler;
pub mod error;
mod runtime;

use std::sync::OnceLock;

use crate::config::Config;
use crate::connection::ConnStatus;
use crate::error::{ConnectionClosed, ConnectionCreateError, ConnectionError};
use crate::runtime::{OpenParams, Runtime};

/// Global runtime singleton (placeholder for future keepalive scheduling).
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn get_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime::new())
}

/// Open a connection by connect to a host or listen for a device connection.
pub async fn open(params: OpenParams) -> Result<ConnHandler, ConnectionCreateError> {
    get_runtime().open(params).await
}

/// Close the connection gracefully.
/// Returns Ok(Res) where Res is the result of the connection task,
/// or Err if the connection task has already finished.
pub async fn close(mut conn: ConnHandler) -> Result<Result<(), ConnectionError>, ConnectionClosed> {
    conn.close().await
}

/// Read a sequence of bytes.
/// Guarantees that this bytes is send by peer in a single time.
pub async fn read(conn: &mut ConnHandler) -> Result<Vec<u8>, ConnectionClosed> {
    conn.read().await
}

/// Write a sequence of bytes.
/// Guarantees that this bytes will be sent as a single Packet,
/// and will be received as a single Packet on the other end.
pub async fn write(conn: &mut ConnHandler, buf: &[u8]) -> Result<(), ConnectionClosed> {
    conn.write(buf).await
}

/// Get the connection status, including the latest rtt, connection state, etc.
pub async fn status(conn: &mut ConnHandler) -> Result<ConnStatus, ConnectionClosed> {
    conn.get_status().await
}

/// Get current runtime configuration.
pub async fn get_config() -> Config {
    get_runtime().get_config().await
}

/// Set current runtime configuration.
/// Note: The configuration item is used for advanced management,
///       for simple usage, you can use the default configuration.
pub async fn set_config(cfg: config::Config) {
    get_runtime().set_config(cfg.clone()).await;
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use crate::connection::ConnState;

    use super::*;

    async fn pick_free_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind to port 0");
        listener.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn runtime_singleton() {
        let rt1 = get_runtime();
        let rt2 = get_runtime();
        assert_eq!(rt1 as *const Runtime, rt2 as *const Runtime);
    }

    async fn gen_conn_handlers() -> (ConnHandler, ConnHandler) {
        let port = pick_free_port().await;
        let host = tokio::spawn(async move {
            let host = open(OpenParams::Host {
                listen_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                listen_port: port,
            })
            .await
            .unwrap();
            host
        });
        let device = tokio::spawn(async move {
            let device = open(OpenParams::Device {
                host_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                host_port: port,
            })
            .await
            .unwrap();
            device
        });
        let (host, device) = tokio::join!(host, device);
        let mut host = host.unwrap();
        let mut device = device.unwrap();
        let host_status = status(&mut host).await.unwrap();
        let device_status = status(&mut device).await.unwrap();
        assert_eq!(host_status.state, ConnState::Connected);
        assert_eq!(device_status.state, ConnState::Connected);
        (host, device)
    }

    #[tokio::test]
    async fn conn_lifecycle() {
        // Create host and device connections
        let (mut host, mut device) = gen_conn_handlers().await;
        // Host sends a message to device
        write(&mut host, b"Hello from host").await.unwrap();
        let msg = read(&mut device).await.unwrap();
        assert_eq!(msg, b"Hello from host");
        // Device sends a message to host
        write(&mut device, b"Hello from device").await.unwrap();
        let msg = read(&mut host).await.unwrap();
        assert_eq!(msg, b"Hello from device");
        // Close connections
        let host_close = close(host).await;
        assert!(matches!(host_close, Ok(Ok(()))));
        let read_res = device.read().await;
        assert!(matches!(
            read_res,
            Err(ConnectionClosed::Error(ConnectionError::PeerClosed))
        ));
    }
    #[tokio::test]
    async fn conn_close_sequence() {
        // host closed first
        let (host, mut device) = gen_conn_handlers().await;
        let host_close = close(host).await;
        assert!(matches!(host_close, Ok(Ok(()))));
        let read_res = device.read().await;
        assert!(matches!(
            read_res,
            Err(ConnectionClosed::Error(ConnectionError::PeerClosed))
        ));
        // device close first
        let (host, mut device) = gen_conn_handlers().await;
        let host_close = close(host).await;
        assert!(matches!(host_close, Ok(Ok(()))));
        let read_res = device.read().await;
        assert!(matches!(
            read_res,
            Err(ConnectionClosed::Error(ConnectionError::PeerClosed))
        ));
        // both closed
        // Test multiple times to avoid random failures due to race conditions in closing
        for _ in 0..10 {
            let (host, device) = gen_conn_handlers().await;
            let host_close = tokio::spawn(async {
                let host_close = close(host).await;
                host_close
            });
            let device_close = tokio::spawn(async {
                let device_close = close(device).await;
                device_close
            });
            match (host_close.await.unwrap(), device_close.await.unwrap()) {
                (Ok(Ok(())), Ok(Err(ConnectionError::PeerClosed))) => {}
                (Ok(Err(ConnectionError::PeerClosed)), Ok(Ok(()))) => {}
                (Ok(Ok(())), Err(ConnectionClosed::Normal)) => {}
                (Err(ConnectionClosed::Normal), Ok(Ok(()))) => {}
                (Ok(Ok(())), Ok(Ok(()))) => {}
                other => panic!("Unexpected close result: {other:?}"),
            }
        }
    }
    #[tokio::test]
    async fn connection_msg_transfer() {
        let port = pick_free_port().await;
        let host_msgs = [
            b"Hello from host".to_vec(),
            b"ncd is awesome".to_vec(),
            b"w is working".to_vec(),
        ];
        let device_msgs = [
            b"Hello from device".to_vec(),
            b"ncd is awesome".to_vec(),
            b"x is working".to_vec(),
        ];
        let host_msgs_host = host_msgs.clone();
        let device_msgs_host = device_msgs.clone();
        let host_future = tokio::spawn(async move {
            let mut host = open(OpenParams::Host {
                listen_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                listen_port: port,
            })
            .await
            .unwrap();
            for msg in host_msgs_host {
                write(&mut host, &msg).await.unwrap();
            }
            for msg in device_msgs_host {
                let received = read(&mut host).await.unwrap();
                assert_eq!(received, msg);
            }
            match close(host).await {
                Ok(Ok(())) | Ok(Err(ConnectionError::PeerClosed)) => {}
                other => other.unwrap().unwrap(),
            }
        });
        let host_msgs_dev = host_msgs.clone();
        let device_msgs_dev = device_msgs.clone();
        let dev_future = tokio::spawn(async move {
            let mut device = open(OpenParams::Device {
                host_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                host_port: port,
            })
            .await
            .unwrap();
            for msg in device_msgs_dev {
                write(&mut device, &msg).await.unwrap();
            }
            for msg in host_msgs_dev {
                let received = read(&mut device).await.unwrap();
                assert_eq!(received, msg);
            }
            match close(device).await {
                Ok(Ok(())) | Ok(Err(ConnectionError::PeerClosed)) => {}
                other => other.unwrap().unwrap(),
            }
        });
        let res = tokio::join!(host_future, dev_future);
        res.0.unwrap();
        res.1.unwrap();
    }

    #[tokio::test]
    async fn connection_big_package_transfer() {
        let port = pick_free_port().await;
        // generate big packages
        let host_msgs: Vec<Vec<u8>> = (0..25)
            .map(|i| {
                let mut msg = vec![0u8; 10 * 1024 * 1024]; // 10MB
                msg[0] = i;
                msg
            })
            .collect();
        let device_msgs: Vec<Vec<u8>> = (0..25)
            .map(|i| {
                let mut msg = vec![0u8; 10 * 1024 * 1024]; // 1MB
                msg[0] = i;
                msg
            })
            .collect();
        let host_msgs_host = host_msgs.clone();
        let device_msgs_host = device_msgs.clone();
        let host_future = tokio::spawn(async move {
            let mut host = open(OpenParams::Host {
                listen_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                listen_port: port,
            })
            .await
            .unwrap();
            for msg in host_msgs_host {
                write(&mut host, &msg).await.unwrap();
            }
            for msg in device_msgs_host {
                let received = read(&mut host).await.unwrap();
                assert_eq!(received, msg);
            }
            match close(host).await {
                Ok(Ok(())) | Ok(Err(ConnectionError::PeerClosed)) => {}
                other => other.unwrap().unwrap(),
            }
        });
        let host_msgs_dev = host_msgs.clone();
        let device_msgs_dev = device_msgs.clone();
        let dev_future = tokio::spawn(async move {
            let mut device = open(OpenParams::Device {
                host_addr: std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                host_port: port,
            })
            .await
            .unwrap();
            for msg in host_msgs_dev {
                let received = read(&mut device).await.unwrap();
                assert_eq!(received, msg);
            }
            for msg in device_msgs_dev {
                write(&mut device, &msg).await.unwrap();
            }
            match close(device).await {
                Ok(Ok(())) | Ok(Err(ConnectionError::PeerClosed)) => {}
                other => other.unwrap().unwrap(),
            }
        });
        let res = tokio::join!(host_future, dev_future);
        res.0.unwrap();
        res.1.unwrap();
    }
}
