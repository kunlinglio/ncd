use crate::config::{self};
use crate::device::{Device, DeviceOperationError};
use crate::netlink::NetlinkSocket;
use crate::netlink::{
    NCD_MSG_CLOSE_REQ, NCD_MSG_DATA, NCD_MSG_KFIFO_AVAILABLE, NCD_MSG_KFIFO_FULL, NCD_MSG_OPEN_REQ,
};
use std::collections::VecDeque;
use std::process;
use tokio::sync::mpsc;

/// 20% of FIFO_SIZE — maximum bytes sent to kernel at once
const FIFO_SIZE: usize = 4096;
const FIFO_HIGH_WATERMARK: usize = FIFO_SIZE * 80 / 100;
const FIFO_LOW_WATERMARK: usize = FIFO_SIZE * 20 / 100;
const SHARD_SIZE: usize = FIFO_LOW_WATERMARK;

pub async fn run() -> ! {
    /* 1. initialize */

    // 1.1 load configuration
    let config_path = config::get_config_path().unwrap_or_else(|| {
        eprintln!("Error: no configuration file found.");
        process::exit(1);
    });
    let device_configs = config::load_config(&config_path).unwrap_or_else(|e| {
        eprintln!("Error loading configuration: {}", e);
        process::exit(1);
    });

    // 1.2 create netlink socket
    let netlink_socket = NetlinkSocket::new().unwrap_or_else(|e| {
        eprintln!("Error creating netlink socket: {}", e);
        process::exit(1);
    });

    // 1.3 register daemon PID
    netlink_socket.register().await.unwrap_or_else(|e| {
        eprintln!("Error registering daemon PID: {}", e);
        process::exit(1);
    });

    // 1.4 create devices
    let mut devices = Vec::new();
    let mut paused = Vec::new(); // kfifo back-pressure per device
    let mut inflight = Vec::new(); // bytes sent since the last low-watermark notification
    let mut read_paused = Vec::new(); // whether the TCP actor is currently paused
    let mut pending = Vec::new(); // queued chunks while paused
    for (minor, cfg) in device_configs.iter().enumerate() {
        if let Err(e) = netlink_socket
            .create_device(minor as u8, cfg.name.as_str())
            .await
        {
            eprintln!("Error creating device {}: {}", cfg.name, e);
            process::exit(1);
        }
        devices.push(Device::new(
            minor as u8,
            cfg.name.clone(),
            cfg.remote_ip,
            cfg.remote_port,
        ));
        paused.push(false);
        inflight.push(0usize);
        read_paused.push(false);
        pending.push(VecDeque::<Vec<u8>>::new());
    }

    // 1.5 create channel for actor tasks to send received TCP data back to main loop
    let (tcp_tx, mut tcp_rx) = mpsc::unbounded_channel::<(u8, Vec<u8>)>();

    /* 2. main loop */
    println!("Daemon started");
    loop {
        tokio::select! {
            // kernel → daemon
            msg = netlink_socket.recv_from_kernel() => {
                let (msg_type, payload) = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("Error receiving from kernel: {}", e);
                        continue;
                    }
                };
                match msg_type {
                    NCD_MSG_OPEN_REQ => {
                        let minor = payload[0] as usize;
                        paused[minor] = false;
                        inflight[minor] = 0;
                        read_paused[minor] = false;
                        pending[minor].clear();
                        match devices[minor].open(tcp_tx.clone()).await {
                            Ok(()) => {
                                println!("Device {} connected", devices[minor].name);
                                let _ = netlink_socket
                                    .send_conn_result_to_kernel(minor as u8, true)
                                    .await;
                            }
                            Err(e) => {
                                eprintln!("Device open failed: {:?}", e);
                                let _ = netlink_socket
                                    .send_conn_result_to_kernel(minor as u8, false)
                                    .await;
                            }
                        }
                    }
                    NCD_MSG_DATA => {
                        let minor = payload[0] as usize;
                        if let Err(e) = devices[minor].write(payload[1..].to_vec()) {
                            eprintln!("Device {} write error: {:?}", devices[minor].name, e);
                        }
                    }
                    NCD_MSG_CLOSE_REQ => {
                        let minor = payload[0] as usize;
                        println!("Device {} closing", devices[minor].name);
                        devices[minor].close();
                        paused[minor] = false;
                        inflight[minor] = 0;
                        read_paused[minor] = false;
                        pending[minor].clear();
                    }
                    NCD_MSG_KFIFO_FULL => {
                        let minor = payload[0] as usize;
                        println!("Device {} kfifo full — pausing", devices[minor].name);
                        paused[minor] = true;
                        sync_device_reading(
                            &devices[minor],
                            &mut read_paused[minor],
                            true,
                            pending[minor].is_empty(),
                        );
                    }
                    NCD_MSG_KFIFO_AVAILABLE => {
                        let minor = payload[0] as usize;
                        println!("Device {} kfifo available — resuming, flushing {} chunks",
                                 devices[minor].name, pending[minor].len());
                        paused[minor] = false;
                        inflight[minor] = 0;
                        flush_device(
                            &netlink_socket,
                            minor,
                            &paused,
                            &mut inflight,
                            &mut pending,
                        )
                        .await;
                        sync_device_reading(
                            &devices[minor],
                            &mut read_paused[minor],
                            false,
                            pending[minor].is_empty(),
                        );
                    }
                    _ => {}
                }
            }

            // TCP actor → main loop  (shard then send, or buffer if paused)
            Some((minor, data)) = tcp_rx.recv() => {
                let minor = minor as usize;
                queue_shards(&mut pending[minor], data);
                flush_device(
                    &netlink_socket,
                    minor,
                    &paused,
                    &mut inflight,
                    &mut pending,
                )
                .await;
                sync_device_reading(
                    &devices[minor],
                    &mut read_paused[minor],
                    paused[minor],
                    pending[minor].is_empty(),
                );
            }
        }
    }
}

fn queue_shards(queue: &mut VecDeque<Vec<u8>>, data: Vec<u8>) {
    for chunk in data.chunks(SHARD_SIZE) {
        queue.push_back(chunk.to_vec());
    }
}

/// Flush pending chunks to the kernel.
/// If the kernel is paused, stop flushing.
/// If the inflight chunk size exceeds the high watermark, stop flushing.
/// Otherwise, send the next chunk.
/// If the send operation fails, push the chunk back to the front of the queue.
async fn flush_device(
    nl: &NetlinkSocket,
    minor: usize,
    paused: &[bool],
    inflight: &mut [usize],
    pending: &mut [VecDeque<Vec<u8>>],
) {
    if paused[minor] {
        return;
    }

    while let Some(chunk_len) = pending[minor].front().map(|chunk| chunk.len()) {
        if inflight[minor] + chunk_len > FIFO_HIGH_WATERMARK {
            break;
        }

        let chunk = pending[minor].pop_front().expect("front chunk exists");
        if let Err(e) = nl.send_data_to_kernel(minor as u8, &chunk).await {
            eprintln!("Error sending data to kernel for minor {}: {}", minor, e);
            pending[minor].push_front(chunk);
            break;
        }
        inflight[minor] += chunk_len;
    }
}

/// Synchronize device reading state with kernel back-pressure.
/// If the kernel is paused or the pending queue is not empty, pause the device.
/// Otherwise, resume the device.
fn sync_device_reading(
    device: &Device,
    read_paused: &mut bool,
    kfifo_paused: bool,
    pending_empty: bool,
) {
    let should_pause = kfifo_paused || !pending_empty;
    if *read_paused == should_pause {
        return;
    }

    let result = if should_pause {
        device.pause_reading()
    } else {
        device.resume_reading()
    };

    match result {
        Ok(()) => *read_paused = should_pause,
        Err(DeviceOperationError::NotOpen) => {}
        Err(e) => eprintln!(
            "Device {} back-pressure command error: {:?}",
            device.name, e
        ),
    }
}
