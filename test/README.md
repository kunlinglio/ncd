# NCD Linux TUI 使用说明

这个目录提供 Linux 端应用层测试程序：[ncd_tui.py](./ncd_tui.py)。

它不直接连 TCP，也不替代 `ncdd`。它只读写 `/dev/ncd_*` 字符设备；内核驱动和 `ncdd` 仍然负责与远端实际设备建立连接和转发数据。

## 1. 配置 `/etc/ncd/config.toml`

Linux 端需要在 `/etc/ncd/config.toml` 中配置远端实际设备端的 IP 和端口。每个设备一个 `[[device]]`。

推荐设备名包含设备类型，方便 TUI 自动识别：

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
remote_port = 10000

[[device]]
name = "ncd_instruction"
remote_ip = "192.168.1.100"
remote_port = 11000
```

说明：

- `name`：Linux 端创建的字符设备名，例如 `ncd_camera` 会创建 `/dev/ncd_camera`。
- `remote_ip`：实际设备端运行 `ncd` 的机器 IP。
- `remote_port`：实际设备端对应设备监听端口。

如果设备名不包含 `camera` / `keyboard` / `instruction` / `file`，TUI 会按默认端口推断：

- `8000`：file
- `9000`：camera
- `10000`：keyboard
- `11000`：instruction

## 2. 启动顺序

实际设备端先启动 `ncd`，选择并暴露设备。

Linux 端启动 `ncdd`，让它加载内核驱动并创建 `/dev/ncd_*`：

```bash
sudo ./ncdd
```

确认设备已经创建：

```bash
ls -l /dev/ncd_*
```

然后启动 TUI：

```bash
python3 test/ncd_tui.py
```

如果没有 `/dev/ncd_*` 读写权限，使用：

```bash
sudo python3 test/ncd_tui.py
```

## 3. TUI 启动后的默认行为

启动后首页会直接显示设备路径、日志目录和常用命令。

默认会自动执行：

- camera：持续读取摄像头 JPEG 帧，并保存到 `test/runs/<时间>/camera/`。
- keyboard：持续监听实际设备端键盘输入，并在 Linux 终端显示文本。

日志和结果会保存在：

```text
test/runs/<时间>/
```

## 4. 常用命令

### Keyboard

进入键盘直通模式：

```text
keyboard mode
```

预期结果：

- Linux 终端里输入的字符会写入远端实际设备端键盘。
- 远端实际设备端收到后，会在当前焦点应用中打字。
- 按 `Ctrl-]` 退出键盘直通模式。

查看原始键盘事件：

```text
keyboard listen start events
```

### Camera

camera 默认会自动接收并保存图片。

停止自动接收：

```text
camera stream stop
```

重新开始自动接收，每 1 秒保存一帧：

```text
camera stream start 1000
```

手动读取一帧：

```text
camera capture
```

预期结果：

- 终端提示收到 JPEG 图像、字节数和保存路径。
- 图片保存到 `test/runs/<时间>/camera/`。
- `latest.jpg` 始终是最近一帧。

### Instruction

执行远端命令：

```text
instruction shell uname -a
```

或者：

```text
instruction run whoami
```

预期结果：

- Linux 端发送命令到实际设备端。
- 实际设备端执行命令。
- Linux 终端显示返回码、stdout 和 stderr。
- 结果保存到 `test/runs/<时间>/instruction/`。

### File

从头读取远端暴露文件：

```text
file cat
```

拉取并保存远端文件快照：

```text
file pull
```

追加文本到远端暴露文件：

```text
file append hello
```

追加本地文件内容到远端暴露文件：

```text
file append-file ./local.txt
```

预期结果：

- `file cat` 在终端显示远端文件内容。
- `file pull` 保存快照到 `test/runs/<时间>/file/`。
- `file append ...` 将内容追加到实际设备端暴露的文件。

## 5. 其他命令

查看后台任务：

```text
status
```

查看当前设备映射：

```text
devices
```

查看日志目录：

```text
logdir
```

退出：

```text
quit
```

## 6. 常见问题

如果 TUI 打开 `/dev/ncd_*` 失败，先检查：

```bash
ls -l /dev/ncd_*
```

如果设备不存在，说明 `ncdd` 没有正常启动或配置文件有误。

如果提示权限不足，用 `sudo` 启动 TUI。

如果 camera 没有图片，确认实际设备端已经选择 camera，并且对应端口和 Linux 配置一致。

如果 keyboard 没有输入效果，确认实际设备端当前系统允许程序模拟键盘输入，并且焦点在可输入窗口。
