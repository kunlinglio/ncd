use tokio::select;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::connection::NcdConnection;
use crate::device::NcdDeviceOperations;
use crate::error::NcdError;

pub struct NcdSession {
    connection: NcdConnection,
    device: Box<dyn NcdDeviceOperations>,
    ready_rx: Receiver<()>,
    /// 保持 sender 存活，设备通过 `notifier()` 获取 clone 发出就绪信号
    _ready_tx: Sender<()>,
}

impl NcdSession {
    pub fn new(connection: NcdConnection, device: Box<dyn NcdDeviceOperations>) -> Self {
        // 使用有界 channel，避免无限内存增长；容量选择 16
        let (tx, rx) = mpsc::channel(16);
        NcdSession { connection, device, ready_rx: rx, _ready_tx: tx }
    }

    /// 设备端获取通知发送端，帧/事件就绪时 send(()) 唤醒 select! 读分支。
    pub fn notifier(&self) -> Sender<()> {
        self._ready_tx.clone()
    }

    pub async fn run(&mut self) -> Result<(), NcdError> {
        let mut buf = [0u8; 65536];

        loop {
            select! {
                data = self.connection.read_connection() => {
                    let data = data?;
                    if !data.is_empty() {
                        if let Err(e) = self.device.write(&data) {
                            let msg = format!("Error: {e}\n");
                            self.connection.write_connection(msg.as_bytes()).await?;
                        }
                    }
                }

                _ = self.ready_rx.recv() => {
                    // 一旦设备发信号，尽可能把设备产生的数据全部读出并转发到 connection
                    loop {
                        match self.device.read(&mut buf) {
                            Ok(n) if n > 0 => {
                                self.connection.write_connection(&buf[..n]).await?;
                            }
                            Ok(_) => break, // 没有更多数据
                            Err(e) => {
                                let msg = format!("Error: {e}\n");
                                self.connection.write_connection(msg.as_bytes()).await?;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}
