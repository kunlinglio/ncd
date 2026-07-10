# ncd — Network Character Device

将本地摄像头、串口、键盘、SSH 等设备通过 TCP 暴露给远程 Linux 主机
（或任意支持 NCD 协议的远程端），实现设备的远程访问与控制。

## 安装

```
cargo install --path crates/ncd
```

之后在任意终端输入 `ncd` 即可使用。

## 使用

### 列出设备

```
ncd list
```

输出本机检测到的所有可用设备：

```
Detected devices:

  camera    ←  camera://0
  camera    ←  camera://1
  serial    ←  COM3
  keyboard  ←  input://HID Keyboard Device
  keyboard  ←  input://PS/2 标准键盘
```

### 选择设备并启动监听

```
ncd run
```

打开终端交互界面（TUI），用方向键和空格选择要暴露的设备：

```
Select devices to expose
  ↑↓: move  Space: toggle  Enter: confirm  Esc: quit

  ◼  camera    ←  camera://0
  ◻  camera    ←  camera://1
  ◼  serial    ←  COM3
  ◻  keyboard  ←  input://HID Keyboard Device

  2 device(s) selected  →  ports 10000-10001
```

确认后自动分配端口并开始监听：

```
Exposing:
  port 10000   camera    ←  camera://0
  port 10001   serial    ←  COM3

  →  <windows-ip>:10000   (camera://0)
  →  <windows-ip>:10001   (COM3)

Listening … (press Ctrl+C to stop)
```

如果某个端口被占用，会自动跳过并选择下一个空闲端口。

## 端口分配

端口从 `10000` 开始**顺序分配**，用户选择的设备按列表顺序依次获得端口号。如果端口被占用则自动跳过，不会分配给设备。

## 各设备类型产生的数据

Linux 端连接不同端口后，收发数据的格式取决于设备类型：

### 摄像头 (`camera`)

| 方向 | 内容 |
|------|------|
| **read**（设备 → Linux） | 原始图像帧字节流（未经编码，格式由摄像头决定，通常为 NV12 / YUYV / MJPEG）。每个 `read()` 返回一帧数据的全部或部分 |
| **write**（Linux → 设备） | 不支持 |

Linux 端收到的每一块数据就是摄像头采集的原始帧。帧之间**有消息边界**（NCD 协议保证），可以逐帧保存或解码。帧格式需要在 Linux 端自行解析和转换（可参考 `tests/integration_session.rs` 中的 NV12→RGB 示例）。

### 串口 (`serial`)

| 方向 | 内容 |
|------|------|
| **read**（设备 → Linux） | 串口接收到的原始字节流。数据是连续的，没有帧边界 |
| **write**（Linux → 设备） | 发送到串口的原始字节。波特率固定为 115200 |

双向透明传输，与直接插在 Linux 上的串口设备行为一致。

### 键盘 (`keyboard`)

| 方向 | 内容 |
|------|------|
| **read**（设备 → Linux） | 暂未实现（返回空）。将来会捕获 本机的按键 |
| **write**（Linux → 设备） | **注入按键到本机**。发送 ASCII 文本（`0x20-0x7E` 为可打印字符，`0x0D` 为回车，`0x08` 为退格，`0x09` 为 Tab，`0x1B` 为 Esc），每个字符被映射为 Windows 键盘事件 |

利用 `write` 方向可以实现 **Linux 远程操控本机键盘输入**：

```
Linux 发送 "notepad\n" → 本机自动打开记事本并输入 "notepad" 后回车
```

### SSH (`ssh`)

| 方向 | 内容 |
|------|------|
| **read**（设备 → Linux） | 暂未实现（仅有文件测试模式）。将来会转发 SSH 服务器的 stdout |
| **write**（Linux → 设备） | 暂未实现。将来会转发命令到 SSH 服务器的 stdin |

## Linux 端连接

在 Linux 上，使用 ncdd daemon 或直接用 `libncd-runtime` 连接：

```rust
// 连接摄像头（假设本机 IP 为 192.168.1.100）
let mut dev = open(OpenParams::Device {
    host_addr: "192.168.1.100".parse().unwrap(),
    host_port: 10000,   // 摄像头端口
}).await?;

// 读取一帧
let frame = read(&mut dev).await?;
std::fs::write("/tmp/frame.raw", &frame)?;

// 关闭
close(dev).await?;
```

也可以手动 TCP 连接（NCD 协议自动处理握手），发送和接收 NCD 数据包。

## 架构

```
┌─ Local (ncd) ─────────────────────────────┐
│                                            │
│  main.rs  →  app.rs                        │
│               ├─ CLI 解析 (parse)           │
│               ├─ TUI 选择 (select_devices)  │
│               ├─ 端口探测 (find_free_ports) │
│               └─ 启动会话 (cmd_run)         │
│                     │                       │
│  ┌──────────────────┼──────────────────┐   │
│  │ registry.rs      │ session.rs       │   │
│  │ 设备检测          │ 双向数据转发      │   │
│  │ port→device 映射  │   Path A: net→dev│   │
│  └──────────────────┼──────────────────┘   │
│                     │                       │
│  ┌──────────────────┼──────────────────┐   │
│  │ connection.rs    │ drivers/          │   │
│  │ NCD 协议         │ camera/serial/    │   │
│  │ TCP 连接管理      │ keyboard/ssh      │   │
│  └──────────────────┴──────────────────┘   │
│                                            │
└────────────────────────────────────────────┘
         │  TCP (NCD protocol)
         ▼
┌─ Linux (ncdd / libncd-runtime) ─┐
│   open(Device {host, port})      │
│   read() / write()               │
│   close()                        │
└──────────────────────────────────┘
```

## 目录结构

```
crates/ncd/
├── src/
│   ├── main.rs         入口
│   ├── app.rs          CLI + TUI + 命令
│   ├── lib.rs          公共 API
│   ├── connection.rs   NCD 连接封装
│   ├── session.rs      双向数据转发
│   ├── device.rs       设备 trait + 基类
│   ├── registry.rs     设备检测 + 端口映射
│   ├── error.rs        错误类型
│   └── drivers/
│       ├── camera.rs   摄像头驱动
│       ├── serial.rs   串口驱动
│       ├── keyboard.rs 键盘驱动
│       └── ssh.rs      SSH 驱动（骨架）
└── tests/
    ├── integration_camera.rs
    └── integration_session.rs
```
