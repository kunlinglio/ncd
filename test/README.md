# NCD Linux 应用层 TUI

[`ncd_tui.py`](./ncd_tui.py) 只实现 Linux 应用层交互，不修改或替代内核驱动、`ncdd`、`ncd` 和现有 adapters。

数据路径保持不变：

```text
TUI <-> /dev/ncd_* <-> 内核驱动 <-> ncdd <-> NCD 网络协议 <-> ncd <-> adapter
```

## 配置和启动

TUI 按原顺序读取 `/etc/ncd/config.toml` 中的全部 `[[device]]`，不会创建或自动打开默认连接。类型优先从连接名中的 `camera`、`keyboard`、`instruction`、`file` 推断，也兼容默认端口 9000、10000、11000、8000。

```toml
[[device]]
name = "ncd_camera"
remote_ip = "192.168.1.100"
remote_port = 9000

[[device]]
name = "ncd_keyboard"
remote_ip = "192.168.1.100"
remote_port = 10000
```

按项目原有方式启动实际设备端 `ncd` 和 Linux 端 `ncdd`，然后运行：

```bash
python3 test/ncd_tui.py
```

如果字符设备权限不足，使用 `sudo python3 test/ncd_tui.py`。

## 首页和连接生命周期

首页只显示连接，不 open 任何字符设备：

```text
ncd/home> 1
# 或
ncd/home> open ncd_camera
```

进入子页面后只 open 被选择的 `/dev/<name>`。所有子页面支持：

```text
status
close
open
reopen
back
```

- 进入子页面自动 open。
- `close`/`open` 可以在当前子页面内重复执行。
- `back` 自动 close 并返回首页。
- 离开子页面后没有默认连接继续运行。

现有驱动的 `read()` 是阻塞式的。为避免修改驱动，camera、keyboard、file 的后台读取由 TUI 自己创建的 Linux `fork` 子进程完成；close 时先终止读取子进程，再关闭唯一的字符设备 fd。因此阻塞读取不会妨碍返回首页或再次 open。

## Camera

camera 子页面 open 期间始终持续读取现有 adapter 的 `4 字节长度 + JPEG` 帧，不提供停止接收的 stream 命令。每一帧都会：

1. 完整读出协议帧；
2. 计算本地 SHA-256；
3. 写入临时文件并 `fsync`；
4. 原子发布为最终 `.jpg` 和 `latest.jpg`；
5. 写入 `frames.jsonl`。

```text
capture [OUTPUT.jpg]
latest [OUTPUT.jpg]
```

保存目录：

```text
test/runs/<时间>/camera/<连接名>/
```

## Keyboard

keyboard 页面同时支持现有 adapter 的两个方向：

```text
type hello
tap enter
press ctrl
release ctrl
mode
listen text
listen events
```

设备端物理按键事件会持续写入 `events.jsonl`；Linux 端输入命令按原有 JSON framing 写入字符设备。

现有 keyboard adapter 没有命令 ACK，因此 TUI 只会准确显示“已写入 NCD”，不会宣称目标应用已经收到。目标输入框还必须有焦点，设备端操作系统也必须允许 pynput 注入输入。

## Instruction

```text
run uname -a
shell echo hello
timeout 10000
```

连接在整个子页面期间保持 open。请求使用 UUID，只有收到现有 instruction adapter 返回的相同 ID、returncode、stdout 和 stderr 后才显示响应。`shell` 仍遵守 adapter 原有的 `allow_shell` 配置。

## File

```text
cat
pull [OUTPUT]
append TEXT
appendfile PATH
stat
reopen
```

现有 file adapter 会在 open 时和文件发生变化后发送完整快照。TUI 始终接收并保存这些快照：

- `cat`、`pull`、`stat` 使用最近快照；
- `reopen` 可强制 adapter 重新发送当前文件；
- `append`/`appendfile` 使用原有 framing 写入，然后等待 adapter 回传的新快照；只有新快照末尾与 payload 一致时才显示“由回传快照确认”。

该确认表示 payload 已被实际 adapter 写入并能被它重新读出。由于原 adapter 只有 `flush()`、没有应用层 ACK 或 `fsync()`，TUI 不会把它描述为远端持久化 ACK。

## 测试

```bash
python3 -m unittest discover -s test -p 'test_*.py' -v
```

测试覆盖：无默认连接、camera 完整落盘、keyboard 双向现有协议、instruction ID 响应、file 回传快照确认、真实 file/instruction adapter 子进程，以及同一页面内重复 open/close。
