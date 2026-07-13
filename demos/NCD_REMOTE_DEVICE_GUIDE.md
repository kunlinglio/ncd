# NCD 远端实际设备驱动与 Linux ncdd 调用说明

这份文档整理当前四类实际设备端 adapter 的设计：`camera`、`keyboard`、`instruction`、`file`，以及 Linux 端 `ncdd`/字符设备层如何连接、分片、缓冲和调用这些远端设备。

这里的“实际设备端”指运行 `ncd` 的机器，可能是 Windows、macOS 或 Linux；“Linux 远端”指运行 `ncdd` 的机器，它通过 `/dev/<name>` 把实际设备端暴露成 Linux 本地字符设备。

## 当前文件分布

实际设备端 Python adapters：

- `crates/ncd/adapters/base.py`
- `crates/ncd/adapters/camera.py`
- `crates/ncd/adapters/keyboard.py`
- `crates/ncd/adapters/instruction.py`
- `crates/ncd/adapters/file.py`
- `crates/ncd/adapters/adapter_list.toml`
- `crates/ncd/adapters/pyproject.toml`

实际设备端 Rust runtime：

- `crates/ncd/src/runtime.rs`
- `crates/ncd/src/adapter_loader/adapter.rs`
- `crates/ncd/src/adapter_loader/list.rs`
- `crates/ncd/src/adapter_loader/bundle.rs`
- `crates/ncd/src/ui/state.rs`
- `crates/ncd/build.rs`

Linux 远端 daemon / kernel driver：

- `crates/ncdd/src/start.rs`
- `crates/ncdd/src/device.rs`
- `crates/ncdd/src/netlink.rs`
- `driver/src/ncd.c`

Linux 端 demo 处理程序：

- `demos/ncd_device.py`
- `demos/camera_linux.py`
- `demos/keyboard_linux.py`
- `demos/instruction_linux.py`
- `demos/file_linux.py`
- `demos/pyproject.toml`

## Python 包依赖

`crates/ncd/adapters/pyproject.toml` 是实际设备端 adapter bundle 的依赖声明。

当前需要：

- `opencv-python`：摄像头检测、采集、JPEG 编码。
- `pynput`：键盘监听和键盘注入。
- `evdev; platform_system == 'Linux'`：Linux 下 `pynput` 常用底层依赖。
- `python-xlib; platform_system == 'Linux'`：Linux/X11 下 `pynput` 常用依赖。

`demos/pyproject.toml` 默认无强依赖，因为 Linux 端 demo 默认只使用标准库。只有 `camera_linux.py --display` 需要额外显示预览，所以放在可选依赖：

```toml
[project.optional-dependencies]
display = ["opencv-python", "numpy"]
```

## Adapter 生命周期

所有 Python adapter 都继承 `base.Adapter`，必须实现五个函数：

```python
list_devices(cls) -> list[Device]
open(self, options: dict[str, str])
read(self) -> bytes
write(self, data: bytes)
close(self)
```

这些函数和底层运行时的关系如下：

1. `list_devices`
   - TUI/配置阶段调用。
   - 返回可选择的设备列表。
   - 不打开长期设备，只做轻量检测。

2. `open`
   - 用户真正运行 `ncd` 并有 Linux 端连接时调用。
   - 根据 `device_identifier` 和 `options` 打开实际设备。
   - 例如 camera 打开 OpenCV `VideoCapture`，keyboard 启动 `pynput` listener/controller，file 打开本地文件。

3. `read`
   - 在 adapter 的读线程中循环调用。
   - 可以阻塞等待数据。
   - 返回的 bytes 会被写入 adapter stdout，再由 Rust runtime 通过 NCD TCP 协议发给 Linux 端。

4. `write`
   - adapter stdin 收到 Linux 端写入的数据后调用。
   - `base.py` 每次从 stdin 读 4096 字节，所以 adapter 自己不能假设一次 `write()` 就是完整业务消息。
   - 因此 keyboard/instruction/file 都使用 `4 字节长度 + payload` 的 adapter 层帧格式来重组消息。

5. `close`
   - 释放实际设备资源。
   - 例如 camera release，keyboard stop listener，file close。

`base.Adapter.run()` 内部开了一个读线程：

- 读线程：`adapter.read()` -> stdout。
- 主线程：stdin -> `adapter.write(data)`。

所以每个 adapter 的 `read()` 可以阻塞；只要它最终返回 bytes，runtime 就会继续转发。

## 配置、检测值和 options 的关系

`list_devices()` 检测到的是“当前设备存在，并给出一些当前观察到的信息”。`options` 是用户希望 `open()` 阶段应用的配置。

二者关系：

- 检测值用于展示和选择。
- options 用于运行时请求或覆盖。
- 如果 options 和硬件实际能力冲突，应以实际设备/底层 API 的结果为准。

例如 camera：

- `list_devices()` 可能显示 `640x480`。
- 用户 options 里填 `width=1280 height=720`。
- `open()` 会调用 OpenCV `capture.set(...)` 尝试设置。
- 摄像头或系统后端可能接受，也可能忽略。
- 当前对端不依赖 width/height 元信息，而是按 JPEG 解码，所以即使实际宽高和 options 不一致，解析也不会错。

## Adapter 层数据格式

为了跨过以下不稳定边界：

- Python stdin 每次 4096 字节读取；
- TCP/NCD 协议分片；
- Linux `/dev` 读写分块；
- kernel kfifo 分片；

当前四类设备统一使用 adapter 层帧：

```text
u32be payload_length | payload bytes
```

也就是：

```python
struct.pack("!I", len(payload)) + payload
```

注意：这是 adapter 业务层的帧边界，不是 NCD 协议本身的帧边界。NCD/libncd 下面还会把 Data packet 按自身协议切成 frame；Linux `/dev` 读到的也可能只是业务帧的一部分。因此 Linux demo 里统一用 `AdapterFrameReader` 重组。

四类设备 payload 含义：

| 设备 | payload 类型 | 说明 |
| --- | --- | --- |
| camera | JPEG bytes | OpenCV 将 BGR frame 编码成 `.jpg` |
| keyboard read | JSON bytes | 键盘事件 |
| keyboard write | JSON bytes | 键盘注入命令 |
| instruction write | JSON bytes | 命令请求 |
| instruction read | JSON bytes | 命令响应 |
| file read/write | raw bytes | 文件完整快照或写入内容 |

## Linux 远程调用整体流程

### 1. 实际设备端启动 ncd

实际设备端通过 TUI 选择设备，并保存配置。

配置来自：

- adapter 列表：`crates/ncd/adapters/adapter_list.toml`
- 检测结果：各 adapter 的 `list_devices()`
- 用户填写 options：例如 `file_path`、camera `jpeg_quality` 等

保存后，`ncd run` 会读取 HostConfig，然后对每个设备：

1. `Adapter::spawn(...)`
   - 启动对应 Python adapter 子进程。
   - 传入：
     - driver name
     - device identifier
     - device display name
     - options

2. `libncd_runtime::open(OpenParams::Host { listen_port })`
   - 在实际设备端监听 TCP 端口。
   - 每个 adapter 一个端口。

3. `device_actor(...)`
   - 进入 `tokio::select!` 事件循环：
     - NCD connection -> adapter stdin -> `adapter.write(...)`
     - adapter stdout -> NCD connection -> Linux 端

这个事件循环在 `crates/ncd/src/runtime.rs`。

### 2. Linux 端启动 ncdd

Linux 端配置 `/etc/ncd/config.toml`：

```toml
[[device]]
name = "ncd_file"
remote_ip = "192.168.1.100"
remote_port = 8000

[[device]]
name = "ncd_camera"
remote_ip = "192.168.1.100"
remote_port = 9000

[[device]]
name = "ncd_keyboard"
remote_ip = "192.168.1.100"
remote_port = 11000

[[device]]
name = "ncd_instruction"
remote_ip = "192.168.1.100"
remote_port = 12000
```

启动：

```bash
sudo ncdd
```

如果之前加载过旧版 kernel module，先卸载旧模块：

```bash
sudo rmmod ncd
sudo ncdd
```

`ncdd` 启动后会：

1. 加载或编译 `ncd.ko`。
2. 通过 netlink 注册 daemon PID。
3. 根据 `/etc/ncd/config.toml` 创建 `/dev/<name>`。
4. 进入主循环，等待 kernel driver 的 open/read/write/close 请求。

### 3. Linux 用户进程 open 设备

例如：

```python
fd = os.open("/dev/ncd_camera", os.O_RDWR)
```

kernel driver 的 `ncd_open()` 会：

1. 检查独占打开。
2. 通过 netlink 发送 `NCD_MSG_OPEN_REQ` 给 `ncdd`。
3. 阻塞等待连接结果。

`ncdd` 收到 `OPEN_REQ` 后：

1. 调用 `Device::open(...)`。
2. 作为 NCD Device 端连接实际设备端的 NCD Host 端口。
3. libncd-runtime 完成 TCP 连接和握手。
4. 将连接结果通过 `NCD_MSG_CONN_RES` 返回 kernel。

如果连接成功，Linux 用户态 `open()` 返回成功；否则返回失败。

### 4. Linux 写入远端设备

Linux 用户态：

```python
os.write(fd, bytes)
```

数据路径：

```text
Linux process
  -> /dev/<name>
  -> driver/src/ncd.c::ncd_write
  -> netlink NCD_MSG_DATA
  -> ncdd start.rs
  -> Device::write
  -> libncd-runtime Data packet
  -> TCP
  -> 实际设备端 ncd runtime
  -> adapter stdin
  -> adapter.write(data)
```

注意：业务命令通常需要先包一层 adapter 长度帧：

```python
struct.pack("!I", len(payload)) + payload
```

`demos/ncd_device.py` 的 `pack_adapter_payload()` 做的就是这件事。

### 5. 远端设备返回数据给 Linux

实际设备端 adapter 的 `read()` 阻塞等待实际设备数据：

```text
adapter.read()
  -> 返回 u32be length + payload
  -> adapter stdout
  -> ncd runtime
  -> libncd-runtime Data packet
  -> TCP
  -> ncdd Device actor
  -> queue_shards / flush_device
  -> netlink NCD_MSG_DATA
  -> kernel driver kfifo
  -> Linux process read(/dev/<name>)
```

Linux 用户态一次 `read()` 不保证读到完整 adapter 业务帧。因此 demo 使用：

```python
AdapterFrameReader.read_payload()
```

它会持续从 `/dev` 读取，直到重组出完整 `payload`。

### 6. Linux close 设备

Linux 用户态关闭 fd：

```python
os.close(fd)
```

kernel driver 的 `ncd_release()` 会：

1. 发送 `NCD_MSG_CLOSE_REQ`。
2. 清空 kfifo 和 pending queue。
3. 释放 open_count。

`ncdd` 收到 close 后关闭 TCP actor；实际设备端 runtime 收到连接关闭后 kill adapter 子进程并调用清理逻辑。

## ncdd / kernel driver 的缓冲和分片修改

camera JPEG 帧通常明显大于 4KB，所以不能假设 Linux `/dev` 一次 read/write 等于一帧。

当前做了这些处理：

### daemon 侧

`crates/ncdd/src/start.rs`：

- `FIFO_SIZE` 从 `4096` 提升到 `64 * 1024`。
- `FIFO_HIGH_WATERMARK = FIFO_SIZE * 80 / 100`。
- `FIFO_LOW_WATERMARK = FIFO_SIZE * 20 / 100`。
- `SHARD_SIZE = FIFO_LOW_WATERMARK`。

从 TCP 收到 NCD Data 后：

1. `queue_shards()` 按 `SHARD_SIZE` 切块。
2. `flush_device()` 在 kernel kfifo 没有达到高水位时发送到 kernel。
3. kernel 报 `NCD_MSG_KFIFO_FULL` 时暂停读取 TCP。
4. kernel 报 `NCD_MSG_KFIFO_AVAILABLE` 时恢复读取并继续 flush pending。

`crates/ncdd/src/netlink.rs`：

- `RECV_BUF_SIZE` 提升到 `64 * 1024 + 1024`。
- 检测 `MSG_TRUNC`，如果 netlink 消息被截断则报错。

### kernel driver 侧

`driver/src/ncd.c`：

- `FIFO_SIZE` 从 `4096` 提升到 `64 * 1024`。
- 增加 `NETLINK_DATA_CHUNK = 64 * 1024 - 1024`。
- `ncd_write()` 对用户态大写入主动切成多个 netlink 消息。
- `ncd_read()` 从 kfifo 中取数据给用户态，不保证业务帧完整。
- driver 内部有 pending queue，在 kfifo 不够时暂存 chunk，并用高/低水位通知 daemon 暂停/恢复。

### demo 侧

`demos/ncd_device.py`：

- 默认 `read_size = 64 * 1024`。
- 默认 `write_chunk_size = 2048`。
- 这样即使 Linux 同事暂时跑旧版 4KB netlink 接收逻辑，demo 写入也不容易因为一次大 `write()` 被截断。
- `AdapterFrameReader` 负责重组 `u32be length + payload`。

## Camera adapter

文件：`crates/ncd/adapters/camera.py`

### 检测

`list_devices()` 使用 OpenCV：

- Linux：`cv2.CAP_V4L2`
- macOS：`cv2.CAP_AVFOUNDATION`
- Windows：`cv2.CAP_MSMF`

当前扫描 `0..MAX_CAMERA_NUM-1`，默认 `MAX_CAMERA_NUM = 4`。

检测流程：

1. `cv2.VideoCapture(i, backend)`
2. `capture.isOpened()`
3. `capture.read()`
4. 从 `frame.shape[:2]` 读取当前宽高。
5. 返回 `Device(identifier=str(i), name=f"Camera {i}", description=...)`

当前不支持热插拔，默认设备列表在运行期间不变。

### open

`open(options)` 做这些事：

1. 根据系统选择 backend。
2. 从 `device_identifier` 得到 camera index。
3. 创建 `cv2.VideoCapture(index, backend)`。
4. 如果 options 里有 `width`、`height`、`fps`，尝试 `capture.set(...)`。
5. 读取实际 `width`、`height`、`fps`。
6. 读取 `jpeg_quality`，默认 `80`。

`jpeg_quality` 是 JPEG 压缩质量，通常 `1..100`：

- 越高：画质越好，帧更大，占用带宽更多。
- 越低：画质更差，帧更小，延迟和带宽压力更低。
- 默认 `80` 是比较稳妥的折中。

### read

`read()` 每次返回一帧：

1. `capture.read()` 采集 OpenCV frame。
2. `cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, jpeg_quality])` 编码成 JPEG。
3. 返回：

```text
u32be jpeg_length | jpeg bytes
```

对端不需要单独知道 OpenCV 原始 BGR 矩阵格式，因为传输的是 JPEG。JPEG 本身携带宽高和图像格式信息，Linux 端按 JPEG 解码即可。

### write

camera 是只读设备，`write()` 直接抛出 `io.UnsupportedOperation`。

### close

释放 OpenCV capture：

```python
self.capture.release()
```

### Linux 调用

保存 JPEG：

```bash
python demos/camera_linux.py /dev/ncd_camera --output-dir frames --limit 10
```

保存最新帧：

```bash
python demos/camera_linux.py /dev/ncd_camera --latest latest.jpg
```

显示预览，需要安装可选依赖：

```bash
python demos/camera_linux.py /dev/ncd_camera --display
```

## Keyboard adapter

文件：`crates/ncd/adapters/keyboard.py`

### 检测

当前返回一个逻辑设备：

```text
System Keyboard
```

原因：当前需求是系统级键盘输入/输出，而不是绑定某个物理键盘。对多数 OS 来说，“枚举每个物理键盘”和“暴露一个系统键盘输入输出设备”在效果上并不等价，但当前功能目标更接近后者：

- read：监听系统全局键盘事件。
- write：向系统注入键盘输入。

### open

`open(options)` 使用 `pynput.keyboard`：

options：

- `listen=true|false`：是否监听键盘事件。
- `inject=true|false`：是否允许注入键盘输入。
- `suppress=true|false`：监听时是否吞掉本地事件。

执行逻辑：

1. 创建 `queue.Queue()` 保存事件。
2. 如果 `inject=true`，创建 `keyboard.Controller()`。
3. 如果 `listen=true`，创建并启动 `keyboard.Listener(...)`。
4. listener 的 `on_press` / `on_release` 将事件序列化后放入 queue。

### read

`read()` 阻塞等待 queue：

```python
event = self.events.get()
```

返回：

```text
u32be json_length | json bytes
```

事件 JSON 形如：

```json
{"event": "press", "key_type": "char", "key": "a"}
```

或：

```json
{"event": "release", "key_type": "special", "key": "enter"}
```

### write

`write(data)` 支持多个被分片的命令。它先累积 stdin 数据，然后按 `u32be length + JSON` 解帧。

支持命令：

```json
{"action": "type", "text": "Hello"}
```

```json
{"action": "tap", "key_type": "special", "key": "enter"}
```

```json
{"action": "press", "key_type": "special", "key": "shift"}
```

```json
{"action": "release", "key_type": "special", "key": "shift"}
```

大小写和特殊字符推荐优先使用 `type`，例如：

```json
{"action": "type", "text": "Aa!@#"}
```

`pynput` 会根据系统输入法/键盘布局处理多数字符输入。需要组合键时再用 `press`/`release`。

### close

停止 listener，并清空 controller。

### 跨平台注意事项

- Windows：通常可以监听和注入，但可能受安全软件或权限影响。
- macOS：需要在系统设置里给进程辅助功能/输入监控权限。
- Linux X11：通常可用，依赖 `python-xlib`/`evdev`。
- Linux Wayland：全局监听和注入通常受限制，可能不可用或需要 compositor 特殊支持。

### Linux 调用

输入文本到实际设备端：

```bash
python demos/keyboard_linux.py /dev/ncd_keyboard type "Hello from Linux"
```

输入 stdin：

```bash
echo "hello" | python demos/keyboard_linux.py /dev/ncd_keyboard stdin --enter
```

敲 Enter：

```bash
python demos/keyboard_linux.py /dev/ncd_keyboard tap enter --key-type special
```

监听远端键盘事件：

```bash
python demos/keyboard_linux.py /dev/ncd_keyboard listen --limit 20
```

## Instruction adapter

文件：`crates/ncd/adapters/instruction.py`

这个 adapter 的语义类似远程过程调用/远程命令执行：Linux 端传入命令和参数，实际设备端执行，返回 stdout/stderr/returncode。

### 检测

返回一个系统命令执行器：

```text
Windows Command Executor
Linux Command Executor
Darwin Command Executor
```

名称取决于 `platform.system()`。

### open

初始化：

- response queue。
- stdin 输入缓冲。
- 默认超时 `timeout_ms`。
- 默认工作目录 `cwd`。
- 是否允许 shell：`allow_shell`，默认 `false`。
- 输出编码：默认 `locale.getpreferredencoding(False)`。

### write

`write(data)` 按 `u32be length + JSON` 解帧。

非 shell 请求：

```json
{
  "id": "request-id",
  "argv": ["hostname"],
  "timeout_ms": 5000,
  "cwd": ""
}
```

shell 请求：

```json
{
  "id": "request-id",
  "shell": true,
  "command": "echo hello",
  "timeout_ms": 5000
}
```

如果 `shell=true` 但 adapter options 中 `allow_shell=false`，会拒绝执行。

执行使用：

```python
subprocess.run(...)
```

并捕获 stdout/stderr。

### read

`read()` 阻塞等待 response queue。

响应 JSON：

```json
{
  "id": "request-id",
  "ok": true,
  "returncode": 0,
  "stdout": "...",
  "stderr": "",
  "system": "Windows"
}
```

然后返回：

```text
u32be json_length | json bytes
```

### close

清空输入缓冲。

### 安全建议

`instruction` 是高风险能力。建议：

- 默认保持 `allow_shell=false`。
- 优先使用 `argv`，不要使用 shell 字符串。
- 如果必须启用 shell，确保网络和调用方可信。
- 对生产环境应增加命令白名单、路径限制或认证层。

### Linux 调用

非 shell：

```bash
python demos/instruction_linux.py /dev/ncd_instruction hostname
```

指定超时：

```bash
python demos/instruction_linux.py /dev/ncd_instruction --timeout-ms 10000 python --version
```

shell：

```bash
python demos/instruction_linux.py /dev/ncd_instruction --shell "echo hello"
```

前提是实际设备端 options 中 `allow_shell = "true"`。

## File adapter

文件：`crates/ncd/adapters/file.py`

这个 adapter 将实际设备端的某个文件暴露为远端可读写设备。

### 检测

当前返回一个逻辑设备：

```text
File Device
```

具体文件路径由用户在 TUI 的 `file_path` option 中填写。

### file_path 跨平台处理

TUI 保存配置时会对 `file_path` 做轻量清洗：

- 去掉首尾空白。
- 去掉成对的单引号或双引号。

adapter `open()` 里还会兜底处理：

- 去掉首尾空白。
- 去掉成对的 `'...'` 或 `"..."`
- 展开 `~`
- 展开环境变量。
- 转为 `os.path.abspath(...)`。

因此这些都可以：

```text
C:\Users\Lenovo\Desktop\新建 文本文档.txt
"C:\Users\Lenovo\Desktop\新建 文本文档.txt"
'/tmp/a b.txt'
~/test.txt
$HOME/test.txt
```

注意：Windows 环境变量常见写法是 `%USERPROFILE%`，Linux/macOS 常见写法是 `$HOME`。`os.path.expandvars` 会按当前实际设备端系统规则展开。

### open

`open(options)`：

1. 读取 `file_path`。
2. 清洗和规范化路径。
3. 读取 `poll_interval_ms`，默认 `200ms`。
4. 创建 parent directory。
5. 用 `a+b` 打开文件，不存在则创建。
6. 初始化 `last_signature` 和输入缓冲。

### read

`read()` 轮询文件：

1. `seek(0)`。
2. 读取整个文件。
3. 通过 `(mtime_ns, size)` 判断是否变化。
4. 文件变化时返回：

```text
u32be file_length | file bytes
```

空文件也会返回长度为 0 的 payload。

### write

`write(data)` 会按 `u32be length + payload` 解帧。每个完整 payload 都会覆盖写入远端文件：

1. `seek(0)`
2. `truncate(0)`
3. `write(payload)`
4. `flush()`
5. 更新 `last_signature`，避免自己写入后立刻 echo。

### close

关闭文件句柄。

### Linux 调用

读取远端文件快照：

```bash
python demos/file_linux.py /dev/ncd_file read --output remote_snapshot.bin
```

持续 watch：

```bash
python demos/file_linux.py /dev/ncd_file watch --output remote_snapshot.bin
```

写本地文件到远端暴露文件：

```bash
python demos/file_linux.py /dev/ncd_file write --input local.bin
```

## Linux demos 的通用封装

文件：`demos/ncd_device.py`

### NcdDevice

封装 `/dev/<name>`：

- `os.open(path, os.O_RDWR)`
- `os.read(fd, read_size)`
- `os.write(fd, chunk)`
- `os.close(fd)`

默认：

```python
DEFAULT_READ_SIZE = 64 * 1024
DEFAULT_WRITE_CHUNK_SIZE = 2048
```

写入时主动切成 2048 字节，是为了兼容旧版 ncdd/kernel netlink 缓冲较小的情况。

### AdapterFrameReader

用于重组 adapter 业务帧：

```text
u32be payload_length | payload
```

它内部维护 `bytearray` 缓冲。只要 `/dev` 还能读到 chunk，就会继续累积，直到完整 payload 出现。

## 为什么不再发送额外 metadata

camera 已经统一传 JPEG，JPEG 自身包含宽高和压缩格式信息；Linux 端只需要按 JPEG 解码。

keyboard/instruction/file 的 payload 是 JSON 或 raw bytes，payload 的解释由设备类型决定。

因此当前没有额外 metadata 帧，避免自定义多阶段协议。边界靠 `4 字节长度`，类型靠 device/adapter 语义。

## 当前默认端口

来自 `crates/ncd/adapters/adapter_list.toml`：

| adapter | 默认端口 |
| --- | --- |
| file | 8000 |
| camera | 9000 |
| keyboard | 11000 |
| instruction | 12000 |

Linux `/etc/ncd/config.toml` 里的 `remote_port` 必须对应实际设备端 TUI 保存的端口。

## 常见问题

### camera 没帧或帧很慢

检查：

- 实际设备端是否能用 OpenCV 打开摄像头。
- Windows 是否允许应用访问摄像头。
- macOS 是否给了摄像头权限。
- Linux 是否有 `/dev/video*` 权限。
- `jpeg_quality` 是否过高导致带宽大。

### Linux read 一次不是完整图片

这是正常现象。`/dev` 是字节流，read 边界不等于业务帧边界。必须用 `AdapterFrameReader` 或自己按 `u32be length` 重组。

### file 写大文件被截断

当前 file adapter 和 demo 已经改成长度帧；demo 也会把 `/dev` 写入切成小块。需要确保 Linux 端 ncdd 和 kernel driver 使用当前版本。如果之前加载过旧模块，先：

```bash
sudo rmmod ncd
sudo ncdd
```

### keyboard 在 Linux Wayland 下不可用

这是系统安全模型限制，不是 adapter 逻辑错误。优先在 X11 环境测试。

### instruction shell 被拒绝

默认 `allow_shell=false`。要执行 shell 字符串，需要在实际设备端 adapter options 中启用：

```toml
allow_shell = "true"
```

更推荐使用 `argv`，不要使用 shell。

## 上传给同事测试前建议

实际设备端：

1. 运行 TUI，确认四类设备都能检测到。
2. file 设备粘贴路径时可以带引号，保存时会自动处理。
3. 运行 `ncd run`。

Linux 端：

1. 写好 `/etc/ncd/config.toml`。
2. 如果加载过旧模块，执行 `sudo rmmod ncd`。
3. 启动 `sudo ncdd`。
4. 确认 `/dev/ncd_file`、`/dev/ncd_camera`、`/dev/ncd_keyboard`、`/dev/ncd_instruction` 出现。
5. 用 demos 测试四类设备。

建议测试命令：

```bash
python demos/camera_linux.py /dev/ncd_camera --output-dir frames --limit 5
python demos/file_linux.py /dev/ncd_file read --output remote.bin
python demos/file_linux.py /dev/ncd_file write --input local.bin
python demos/keyboard_linux.py /dev/ncd_keyboard type "hello from linux"
python demos/instruction_linux.py /dev/ncd_instruction hostname
```

