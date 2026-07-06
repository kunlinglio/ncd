# NCD

NCD (Network Character Device) is a Linux character device framework that exposes a remote byte stream as a local character device.

## Components
- Kernel driver(`driver/`): implements a standard Linux character device.
- Device daemon (`crates/ncdd/`): a user space daemon process that manages transport, sessions, and the Network Character Device Protocol.
- Host tool (`crates/ncd/`): provides a command-line interface for interacting with the remote device, which is able to run on different platforms.
- Protocol library (`crates/libncd/`): a library that implements the Network Character Device Protocol.

