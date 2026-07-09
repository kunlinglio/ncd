use crate::error::DeviceError;

// whether the device is open or closed
#[derive(Debug, PartialEq, Eq)]
pub enum DeviceState {
    Open,
    Closed,
}

// basic device structure
#[derive(Debug)]
pub struct NcdDevice {
    pub device_state: DeviceState,
    pub device_path: String,
    buffer: Vec<u8>,
}

impl NcdDevice {
    pub fn new(device_path: String) -> Self {
        NcdDevice {
            device_state: DeviceState::Closed,
            device_path,
            buffer: Vec::new(),
        }
    }
}

// define operations for the device

pub trait NcdDeviceOperations: std::fmt::Debug + Send {
    fn device_path(&self) -> &str;
    fn open(&mut self) -> Result<(), DeviceError>;
    fn close(&mut self) -> Result<(), DeviceError>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError>;
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError>;
}

// implement the operations for the device
impl NcdDeviceOperations for NcdDevice {
    fn device_path(&self) -> &str {
        &self.device_path
    }

    fn open(&mut self) -> Result<(), DeviceError> {
        self.device_state = DeviceState::Open;
        Ok(())
    }

    fn close(&mut self) -> Result<(), DeviceError> {
        self.device_state = DeviceState::Closed;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let n = self.buffer.len().min(buf.len());
        buf[..n].copy_from_slice(&self.buffer[..n]);
        self.buffer.drain(..n);
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        self.buffer.extend_from_slice(data);
        Ok(data.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let mut dev = NcdDevice::new("/dev/test".into());

        // 写入
        let n = dev.write(b"hello").unwrap();
        assert_eq!(n, 5);

        // 读出来
        let mut buf = [0u8; 10];
        let n = dev.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");
    }

    #[test]
    fn read_drains_buffer() {
        let mut dev = NcdDevice::new("/dev/test".into());
        dev.write(b"world").unwrap();

        let mut buf = [0u8; 10];
        dev.read(&mut buf).unwrap(); // 第一次有数据
        let n = dev.read(&mut buf).unwrap(); // 第二次：buffer 已空
        assert_eq!(n, 0); // 读不到东西
    }
}
