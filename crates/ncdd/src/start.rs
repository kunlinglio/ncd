use crate::config::{self};
use crate::device::Device;
use crate::netlink::NetlinkSocket;
use crate::netlink::{NCD_MSG_CLOSE_REQ, NCD_MSG_DATA, NCD_MSG_OPEN_REQ};
use std::process;
use tokio::sync::mpsc;

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
        eprintln!("Error creating netlink socket.: {}", e);
        process::exit(1);
    });

    // 1.3 register daemon PID
    netlink_socket.register().await.unwrap_or_else(|e| {
        eprintln!("Error registering daemon PID: {}", e);
        process::exit(1);
    });

    // 1.4 create devices
    let mut devices = Vec::new();
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
    }

    // 1.5 create channel for actor tasks to send received TCP data back to main loop
    let (tcp_tx, mut tcp_rx) = mpsc::unbounded_channel::<(u8, Vec<u8>)>();

    /* 2. main loop */
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
                    }
                    _ => {}
                }
            }

            // TCP actor → main loop
            Some((minor, data)) = tcp_rx.recv() => {
                if let Err(e) = netlink_socket.send_data_to_kernel(minor, &data).await {
                    eprintln!("Error sending data to kernel: {}", e);
                }
            }
        }
    }
}
