//! Integration test: direct CameraDriver → NCD protocol → file
//!
//! Captures a frame via nokhwa, passes it through the NCD protocol
//! stack (host/device pair), and verifies the received data.

use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;

use ncd::device::NcdDeviceOperations;
use ncd::drivers::CameraDriver;
use libncd_runtime::{open, read, write, close, OpenParams};

fn capture_with_nokhwa(out: &std::path::Path) -> Result<(), String> {
    let idx = nokhwa::utils::CameraIndex::Index(0);
    let req = nokhwa::utils::RequestedFormat::with_formats(
        nokhwa::utils::RequestedFormatType::None,
        nokhwa::utils::frame_formats(),
    );
    match nokhwa::Camera::new(idx, req) {
        Ok(mut cam) => {
            cam.open_stream().map_err(|e| format!("open_stream: {e:?}"))?;
            let frame = cam.frame().map_err(|e| format!("frame: {e:?}"))?;
            std::fs::write(out, frame.buffer()).map_err(|e| format!("write out: {e}"))?;
            Ok(())
        }
        Err(e) => Err(format!("nokhwa Camera::new error: {e:?}")),
    }
}

async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let p = listener.local_addr().unwrap().port();
    drop(listener);
    p
}

fn tmp_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tmp")
}

#[tokio::test]
async fn integration_camera_transfer() {
    let tmp = tmp_dir();
    fs::create_dir_all(&tmp).expect("create tmp dir");
    let photo_path = tmp.join("real_photo.bin");
    if photo_path.exists() {
        fs::remove_file(&photo_path).ok();
    }

    if let Err(e) = capture_with_nokhwa(&photo_path) {
        eprintln!("SKIP: camera unavailable ({e})");
        return;
    }

    let device_path = format!("file://{}", photo_path.to_string_lossy());
    let mut cam = CameraDriver::new(
        &device_path,
        std::sync::Arc::new(tokio::sync::Notify::new()),
    );
    // Device defaults to Open — frame loaded by new().
    let mut buf = vec![0u8; 10 * 1024 * 1024];
    let n = cam.read(&mut buf).expect("cam read");
    buf.truncate(n);

    let port = pick_free_port().await;
    let tmp2 = tmp.clone();
    let host_task = tokio::spawn(async move {
        let mut host = open(OpenParams::Host {
            listen_addr: IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            listen_port: port,
        })
        .await
        .expect("open host");
        let data = read(&mut host).await.expect("host read");
        let out = tmp2.join("received_photo.bin");
        std::fs::write(&out, &data).expect("write received");
        let _ = close(host).await;
    });

    let device_task = tokio::spawn(async move {
        let mut device = open(OpenParams::Device {
            host_addr: IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            host_port: port,
        })
        .await
        .expect("open device");
        write(&mut device, buf).await.expect("device write");
        let _ = close(device).await;
    });

    let _ = tokio::join!(host_task, device_task);

    let received = tmp.join("received_photo.bin");
    let rdata = std::fs::read(&received).expect("read received");
    assert!(!rdata.is_empty());
}
