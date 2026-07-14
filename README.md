# NCD

NCD (Network Character Device) is a framework for mapping a remote device as a local Linux character device over the network.

## Components
- Kernel driver(`driver/`): implements a standard Linux character device.
- Device daemon (`crates/ncdd/`): a user space daemon process that manages transport, sessions, and the Network Character Device Protocol.
- Host tool (`crates/ncd/`): provides a command-line interface for interacting with the remote device, which is able to run on different platforms.
- Protocol library (`crates/libncd/`): a library that implements the Network Character Device Protocol.
- Runtime (`crates/libncd-runtime`): a library that implements the runtime for the Network Character Device Protocol, which is used by both the device daemon and the host tool. This library requires a tokio runtime.

## Usage
### Device Endpoint
**Only Linux is supported.**
1. Build and install ncdd executable
```bash
cargo install --release --bin ncdd
```

2. Edit the configuration file to specify the remote address and the device name. The configuration file is located at `/etc/ncd/config.toml`. For example:
```toml
[[device]]
name = "ncd01"
remote_ip = "192.168.5.2"
remote_port = 8083
```

3. Start the ncd daemon
*ncdd must be run as root to create the character device*
*You need to make sure the c build tool chain and make are available for building the kernel module.*
```bash
sudo ncdd
```
> If you want to run ncdd in the background automatically, you can use systemd to manage the service.

4. Now you can access the character device at `/dev/`. For example, if the device name is `ncd01`, you can access it at `/dev/ncd01`.

### Host Endpoint
**Linux, macOS, and Windows are supported.**
1. Build and install ncd executable
```bash
cargo install --release --bin ncd
```

2. Configure the map from local device to `address:port`:
```bash
ncd config
```
This will prompt a TUI interface to configure the mapping. After saving, the configuration file will be located at your home directory.

3. After configuration, you can run the ncd host tool to start the host service:
```bash
ncd
```

### Extending ncd
If you want to extend ncd to support more devices, you can write a Python script that implements the device driver interface.

The script should be placed in the `crates/ncd/adapters` directory of the ncd installation path. You can refer to the existing drivers for examples. You also need to update the `crates/ncd/adapters/adapter_list.toml` and `crates/ncd/adapters/pyproject.toml` files to include the new driver information.

After that, you can rebuild and install the ncd host tool to include the new driver. The new driver will be automatically detected and listed when you run `ncd config`.

### ncd demo

We provide a demo method for each device type to showcase ncd's functionality.
**Prerequisite:** start `ncd` on the Host and `sudo ncdd` on the Linux endpoint first.

#### Keyboard
On the **Linux** endpoint, run:
```bash
cat /dev/ncd_keyboard
```
Subsequently, type characters on the host keyborad and press "enter"after which you would be able to see characters sent from the host in its original appearance.

#### Camera
On the Linux endpoint, start the MJPEG server:
```bash
python demo/camera_server.py
```
to start a server on the linux
Then, open "http://localhost:8080" to view the camera live-stream from the host.

#### File
Execute the instructions below to test the file-device:
```bash
cat /dev/ncd_file                    # read the remote file
echo "Hello, linux!" > /dev/ncd_file # append a line
cat /dev/ncd_file                    # verify it was written
```

#### Instruction
On the Linux endpoint, run the demo instruction:
```bash
    ll | python3 demo/instruction.py
```

## License
Distributed under the terms of the Apache 2.0 license.