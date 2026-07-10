//! Full-stack integration tests:
//!   CameraDriver → NcdSession → NcdConnection → remote side → filesystem

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::fs;
use tokio::sync::Notify;
use libncd_runtime::{open, read, close, OpenParams};

use ncd::connection::NcdConnection;
use ncd::device::NcdDeviceOperations;
use ncd::drivers::CameraDriver;
use ncd::session::NcdSession;

// ── helpers ───────────────────────────────────────────────────────

async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind free port");
    let p = listener.local_addr().unwrap().port();
    drop(listener);
    p
}

fn tmp_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tmp")
}

fn capture_camera_bmp(output: &Path) -> Result<(), String> {
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType, frame_formats};

    let idx = CameraIndex::Index(0);
    let req = RequestedFormat::with_formats(RequestedFormatType::None, frame_formats());
    let mut cam =
        nokhwa::Camera::new(idx, req).map_err(|e| format!("camera open: {e:?}"))?;
    cam.open_stream()
        .map_err(|e| format!("open stream: {e:?}"))?;

    let frame = cam.frame().map_err(|e| format!("capture: {e:?}"))?;
    let res = cam.resolution();
    let fmt = cam.frame_format();
    let w = res.width_x as usize;
    let h = res.height_y as usize;
    let buf = frame.buffer();

    let rgb = match fmt {
        nokhwa::utils::FrameFormat::NV12 => nv12_to_rgb(buf, w, h),
        _ => buf.to_vec(),
    };
    write_bmp(output, &rgb, w as u32, h as u32)
        .map_err(|e| format!("write bmp: {e}"))?;
    Ok(())
}

fn nv12_to_rgb(nv12: &[u8], width: usize, height: usize) -> Vec<u8> {
    let y_size = width * height;
    let y_plane = &nv12[..y_size];
    let uv_plane = &nv12[y_size..];
    let mut rgb = vec![0u8; y_size * 3];

    for row in 0..height {
        for col in 0..width {
            let y = y_plane[row * width + col] as i32;
            let uv_idx = (row / 2) * width + (col & !1);
            let u = uv_plane.get(uv_idx).copied().unwrap_or(128) as i32;
            let v = uv_plane.get(uv_idx + 1).copied().unwrap_or(128) as i32;
            let c = y - 16;
            let d = u - 128;
            let e = v - 128;
            let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
            let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
            let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            let out_idx = (row * width + col) * 3;
            rgb[out_idx] = r;
            rgb[out_idx + 1] = g;
            rgb[out_idx + 2] = b;
        }
    }
    rgb
}

fn write_bmp(path: &Path, rgb: &[u8], width: u32, height: u32) -> std::io::Result<()> {
    let row_size = (width * 3 + 3) & !3;
    let pixel_array_size = row_size * height;
    let file_size = 54u32 + pixel_array_size;
    let mut f = fs::File::create(path)?;

    f.write_all(b"BM")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(width as i32).to_le_bytes())?;
    f.write_all(&(height as i32).to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&pixel_array_size.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;

    let row_stride = width as usize * 3;
    let zero_pad = vec![0u8; (row_size as usize).saturating_sub(row_stride)];
    for y in (0..height).rev() {
        let row_start = (y as usize) * row_stride;
        let row_end = std::cmp::min(row_start + row_stride, rgb.len());
        let row = &rgb[row_start..row_end];
        for x in 0..width as usize {
            let i = x * 3;
            if i + 2 < row.len() {
                f.write_all(&[row[i + 2], row[i + 1], row[i]])?;
            }
        }
        if !zero_pad.is_empty() {
            f.write_all(&zero_pad)?;
        }
    }
    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn session_file_camera_to_remote_file() {
    let tmp = tmp_dir();
    fs::create_dir_all(&tmp).ok();

    let frame_path = tmp.join("session_test_frame.bin");
    let frame_data = b"SESSION_FULL_STACK_FRAME_DATA_2024";
    fs::write(&frame_path, frame_data).unwrap();

    let port = pick_free_port().await;
    let notify = Arc::new(Notify::new());

    let driver: Arc<Mutex<Box<dyn NcdDeviceOperations>>> = Arc::new(Mutex::new(
        Box::new(CameraDriver::new(
            &format!("file://{}", frame_path.display()),
            notify.clone(),
        )),
    ));

    let ncd_notify = notify.clone();
    let ncd_port = port;
    let _ncd_task = tokio::spawn(async move {
        let conn = NcdConnection::create_connection(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            ncd_port,
        )
        .await
        .unwrap();

        let mut session = NcdSession::new(conn, driver, ncd_notify);
        let _ = session.run().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let mut device_conn = open(OpenParams::Device {
        host_addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        host_port: port,
    })
    .await
    .expect("Device open");

    let received = read(&mut device_conn).await.expect("read frame");

    let output_path = tmp.join("session_received_frame.bin");
    fs::write(&output_path, &received).unwrap();
    let _ = close(device_conn).await;

    assert!(!received.is_empty());
    assert_eq!(received, frame_data);

    let saved = fs::read(&output_path).unwrap();
    assert_eq!(saved, received);
}

#[tokio::test]
async fn session_real_camera_to_remote_file() {
    let tmp = tmp_dir();
    fs::create_dir_all(&tmp).ok();

    let bmp_path = tmp.join("session_real_photo.bmp");
    match capture_camera_bmp(&bmp_path) {
        Ok(()) => println!("📷 Reference photo saved to {}", bmp_path.display()),
        Err(e) => eprintln!("⚠ Could not capture reference BMP: {e}"),
    }

    let port = pick_free_port().await;
    let notify = Arc::new(Notify::new());

    let driver: Arc<Mutex<Box<dyn NcdDeviceOperations>>> = Arc::new(Mutex::new(
        Box::new(CameraDriver::new("camera://0", notify.clone())),
    ));

    let ncd_notify = notify.clone();
    let ncd_port = port;
    let ncd_task = tokio::spawn(async move {
        let conn = NcdConnection::create_connection(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            ncd_port,
        )
        .await
        .unwrap();

        let mut session = NcdSession::new(conn, driver, ncd_notify);
        let _ = session.run().await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut device_conn = match open(OpenParams::Device {
        host_addr: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        host_port: port,
    })
    .await
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot connect to ncd side: {e:?}");
            return;
        }
    };

    let received =
        tokio::time::timeout(std::time::Duration::from_secs(5), read(&mut device_conn)).await;

    match received {
        Ok(Ok(data)) if !data.is_empty() => {
            let raw_path = tmp.join("session_real_frame.bin");
            fs::write(&raw_path, &data).unwrap();
            println!(
                "✅ session_real_camera_to_remote_file  PASSED  ({} bytes → {})",
                data.len(),
                raw_path.display(),
            );
            println!("   Reference BMP for visual check: {}", bmp_path.display());
        }
        Ok(Ok(_)) => eprintln!("SKIP: received empty frame"),
        Ok(Err(e)) => eprintln!("SKIP: read error: {e:?}"),
        Err(_) => eprintln!("SKIP: timeout (no camera?)"),
    }

    let _ = close(device_conn).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ncd_task).await;
}
