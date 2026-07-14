# NCD 应用层 TUI 使用说明

[`ncd_tui.py`](./ncd_tui.py) 是 Linux 端的应用层界面。数据路径仍然是：

```text
Linux TUI <-> /dev/ncd_* <-> driver <-> ncdd <-> 网络 <-> ncd <-> 实际设备 adapter
```

本轮没有修改 `ncdd`、`libncd-runtime`、Linux 内核 driver、NCD 线上协议、`ncd` 主程序、base/camera/keyboard adapter。camera 只增强 Linux TUI 的逐层接收和保存状态；file/instruction 的现有 adapter 因确有启动、落盘和请求安全问题而进行了必要修复。

## 启动与选择连接

```bash
python3 demo/ncd_tui.py
```

首页不会自动打开默认连接。可以输入：

```text
control+camera
control+ncd_keyboard
control+instruction
control+file
1
```

进入子页面时自动 `open`，输入 `back` 时自动 `close`。子页面内仍可使用：

```text
open
close
reopen
back
help
```

运行数据位于 `demo/runs/<UTC 时间>/`，进入每个子页面时都会显示准确目录。

## Keyboard 命令

keyboard 页面的核心用途是：把 Linux TUI 中的输入发送到实际设备当前获得焦点的应用；同时接收实际设备的物理键盘事件并在 Linux 显示、保存。当前回传事件不会自动注入 Linux 桌面。

```text
send hello             输入一段文字到实际设备
key enter              点击一个键；也可用 key a、key left
combo ctrl+c           发送组合键/快捷键
raw                     当前终端输入立即转发；Ctrl-] 退出
show text              简洁显示实际设备回传的按键
show events            显示完整回传事件
show off               隐藏回传显示，但仍继续接收和保存
hold shift             按住一个键
release shift          释放之前按住的键
status                  查看本地 handle、接收进程和事件数
info                    查看方向说明、确认限制和日志路径
```

`raw` 的准确含义是：在 Linux TUI 终端里输入的可打印字符、Enter、Tab、退格和方向键会立即发送到实际设备；实际设备传回的按键仍按 `show` 设置显示。它不是远程 shell，也不是把任意控制组合键原样捕获。Ctrl/Command 组合键应使用 `combo`。按 `Ctrl-]` 退出 raw。

`key`、`hold`、`release` 和 `combo` 会在 TUI 内严格检查按键。`hold key` 这类把帮助占位词当成真实按键的输入会被拒绝，不再把 `press char:key` 发给 adapter，从而避免 `pynput` 异常导致 adapter 整体退出。

keyboard adapter 没有发送 ACK。界面中的 `written to NCD` 只能证明命令已经写入 NCD 字符设备，不能证明目标输入框已经接受。实际设备端必须满足：目标窗口有焦点、`inject=true`，并且操作系统允许 `pynput` 注入。

`status` 和 `info` 已恢复。它们只读取 TUI 进程内的连接状态、receiver 状态、事件计数、显示模式和本地日志路径，不读取日志内容、不执行文件，也不向实际设备发送命令。不存在命令执行风险；需要注意的只是终端截图可能暴露本地绝对路径和远端 IP/端口。

旧命令 `enter/type/tap/press/down/up/listen` 仍作为兼容别名，但主帮助只显示上面含义更明确的命令。

## Camera 命令与连续保存

camera 页面一旦打开就自动连续接收并保存，不需要先输入任何开始命令。主命令只是查看结果或验证下一帧是否还在到达：

```text
status                 查看是否正在接收、保存数量、时间、间隔和错误
latest                 显示最近一张已保存图片的路径
latest copy.jpg        把最近图片额外复制到 copy.jpg
wait                   等待下一张自动到达的完整帧，最多 30 秒
wait copy.jpg          等到下一帧后再额外复制到 copy.jpg
files                  显示图片目录、latest.jpg 和 frames.jsonl
```

`wait` 不会向实际设备发送“拍一张”的命令，也不会启动摄像头；摄像头本来就在持续发送。它的用途是做一次明确的实时检查：从输入命令之后，是否又完成了一张新图片。平时只看自动保存结果时无需使用 `wait`。

`status` 适合排障：`WAITING FOR FIRST FRAME` 表示尚未收完整首帧，`RECEIVING` 表示最近仍有帧保存，`STALE` 表示太久没有新帧，`RECEIVER STOPPED/ERROR` 表示接收或保存进程已经结束。`latest` 用于找到/复制图片，`files` 用于找到完整历史和元数据。

camera 页面打开期间会持续读取。每个完整帧都会：

1. 写入独立的 UTC 时间戳 `.jpg`；
2. 原子更新 `latest.jpg`；
3. 把大小、SHA-256、接收/保存时间和帧间隔写入 `frames.jsonl`；
4. 大约每 5 秒显示一次短字节样本，不完整输出 JPEG。

读取一个尚未完成的大帧时，页面也会大约每 5 秒显示 `camera assembling frame bytes=已收/总数`。这可以直接区分“Linux 没收到后续字节”和“后续字节正在到达，但完整 JPEG 还没有组装完成”。

自动测试使用 4 个连续的 128 KiB 帧验证，4 帧都会保存为独立文件。因此如果实际设备有持续上传流量，但 Linux 很久才出现第二张完整图片，应先检查实际 JPEG 大小和设备端配置：

```toml
options = { width = "640", height = "480", fps = "2", jpeg_quality = "60" }
```

当前 driver FIFO 为 4 KiB，`read()` 每次最多排出 4 KiB；高分辨率 JPEG 需要很多轮流控才能组成一个完整帧。driver 和 `ncdd` 只处理字节块，不解析 `[长度][JPEG]`，所以 `dmesg` 本来就不会声明“收到一张图片”。同样，一张较大的 JPEG 会产生很多条 `bytes=...` 转发日志，因此“很多字节块”不能单独证明已经传了多张完整图片。

TUI 的 `status` 现在分别显示：`transport_frames`（已读完多少个完整 camera 业务帧）、`saved_frames`（已成功落盘多少帧），以及 `stream_stage`（正在等下一帧 4 字节长度头，或当前 payload 已收/应收字节）。若 `transport_frames` 和 `saved_frames` 同步增长，接收与保存都正常；若 payload 数字停住，TUI 尚未收到完整后续帧；若完整帧数增长但保存数不增长，则检查校验和磁盘错误。`wait` 在 30 秒内没有完整帧时仍会提示可能断线或帧过大。仅有网卡上传占用或内核 FIFO 日志不能代替应用层完整帧证据。

旧命令 `capture` 和 `path` 分别保留为 `wait` 和 `files` 的兼容别名。

## File 命令与 file_path

file 页面不是远程文件管理器。它只映射实际设备 `ncd` 配置中的一个 `file_path`：实际设备文件变化后会把完整快照传回 Linux；Linux 可以在文件末尾追加文字或一个本地文件的字节。

```text
show                   预览最近快照的前 4096 字节
append hello           追加文字并等待设备回传新快照确认
push local.bin         把 Linux 本地文件的字节追加到实际设备文件
save [OUTPUT]          在 Linux 保存完整最新快照副本
status                 查看接收进程、快照数和最新文件
info                   查看用途、大小、哈希、时间和日志路径
reopen                 重新连接并要求一份新的初始快照
```

`append`/`push` 都是“追加到末尾”，不是覆盖文件，也不能在命令中选择另一个实际设备路径。只有收到更新后的完整快照，并确认快照末尾与发送内容完全一致时，TUI 才显示成功。`show` 只适合快速预览；二进制或大文件应使用 `save` 获取完整副本。

FileAdapter 现在每次追加都会 `flush + fsync`，不再长期持有文件句柄，并使用 inode/mtime/size 加内容哈希检测变化，因此能识别编辑器的原子替换和同大小改写。轮询间隔限制为 10～60000 ms；单个协议帧和映射文件上限为 64 MiB，超限会明确断开而不是无限增长内存。

旧命令 `read/cat/write/writefile/appendfile/pull` 仍作为兼容别名。

原来的异常发生在实际设备端 adapter 启动阶段，Linux TUI 无法在连接建立之后补传 `file_path`。现在空路径会安全回退到实际设备用户主目录下：

```text
~/ncd-share.bin
```

仍然建议显式配置实际设备端 `ncd`：

```toml
[[device]]
driver = "file"
device_identifier = "Unspecified"
device_name = "File Device"
port = 8000
options = { file_path = "C:\\Users\\Lenovo\\ncd-share.txt", poll_interval_ms = "200" }
```

实际设备日志来自临时解包目录，修改源码后必须重新构建/安装并重启实际设备端 `ncd`，否则它仍会运行旧的 `FileAdapter` 并继续报告 `file_path option is required`。在项目根目录可执行：

```bash
cargo install --path crates/ncd --force
```

## Instruction：自动检测终端

instruction 页面连接后会进行无副作用探测，并显示：

```text
device->linux: detected terminal=powershell system=Windows executable=powershell.exe
```

或：

```text
device->linux: detected terminal=bash system=Linux executable=/bin/bash
```

随后 `help` 会按照检测结果给出示例：

```text
# PowerShell
run Get-ChildItem
run Get-Process
run $PSVersionTable

# cmd
run dir
run echo 12345
run ver

# bash/zsh/sh
run ls -la
run pwd
run uname -a
```

公共命令：

```text
run COMMAND             在实际设备检测到的终端中执行一次命令
exec PROGRAM [ARGS...]  直接运行实际设备上的真实程序（高级用法）
detect                  重新检测 PowerShell/cmd/bash/zsh/sh
timeout 10000           设置 Linux 等待结果的时间，单位毫秒
status                  查看最后一次结果和本地连接状态
logs                    显示请求、响应、stdout、stderr 日志路径
```

`run` 发送的是明确的 argv，例如 PowerShell 使用 `powershell.exe ... -Command`，bash 使用 `/bin/bash -lc`，因此不依赖 adapter 的 `allow_shell=true`。每个响应按相同 UUID 匹配，并显示 `device->linux`、退出码、stdout 和 stderr。这里不是持续存在的交互式终端；每次 `run` 都是一次独立命令。

InstructionAdapter 会校验 JSON 对象、argv/command、stdin、cwd 和 timeout，拒绝超过 1 MiB 的请求或 stdin。命令输出先写临时文件，stdout/stderr 各最多回传 4 MiB，超出时返回 `*_truncated=true` 并在 TUI 提示。超时命令会被终止并返回 `timed_out=true` 和明确文字，不再把所有失败笼统显示成 `returncode=None`；只有程序根本未能启动等进程创建错误仍可能没有退出码。

最常用的是 `run`。只有明确知道某个名称是真实可执行文件、并且不需要 PowerShell/cmd/bash 语法时才使用 `exec`。`detect` 解决实际设备系统或终端发生变化的问题；`timeout` 只控制一条命令等待多久；`logs` 用于事后查看完整请求和输出。旧命令 `terminal`、`info` 仍是兼容别名，`win/unix/shell` 保留但不出现在主帮助中。

## 关于“实际设备控制远端 Linux”的修改方案

当前 keyboard/instruction 的主动命令方向主要是：

```text
Linux TUI -> 实际设备
```

你同学描述的是：

```text
实际设备控制界面 -> Linux
```

三种功能的范围不同：

| 功能 | 推荐方案 | 预计范围 | 是否需要改 ncdd/driver/NCD |
| --- | --- | --- | --- |
| keyboard | Linux TUI 把已经收到的实际设备按键事件转换为 Linux `uinput` 事件 | 中小 | 通常不需要 |
| instruction | 新增“实际设备控制端请求 → Linux 安全执行服务 → 响应返回” | 中到大 | 数据转发层通常不需要，但应用角色和协议要新增 |
| file | 新增受限 Linux 文件服务与实际设备文件客户端，支持 list/read/write 等明确操作 | 大 | 数据转发层通常不需要，但应用协议、权限和分块传输要新增 |

### 反向 keyboard

现有 KeyboardAdapter 已经把实际设备 press/release 事件传到 Linux。可在 Linux keyboard 页面增加显式命令，例如：

```text
inject-linux on
inject-linux off
```

开启后把事件映射为 Linux evdev key code，通过 `/dev/uinput` 注入当前 Linux 桌面。还要实现：权限/udev 规则、字符与物理键码映射、断线时释放所有按下键、紧急停止热键、禁止默认开启和清晰的安全提示。这个方案主要修改 `demo/ncd_tui.py`、依赖与测试；当前 adapter、`ncd`、`ncdd`、driver 和 NCD 协议通常都不用改。

### 反向 instruction

当前 InstructionAdapter 是“实际设备命令执行器”，只会接收 Linux 请求并执行实际设备命令。反向后需要一个新服务，不能直接交换界面文字：

```text
实际设备控制 UI
  -> 带 UUID 的 Linux 命令请求
  -> NCD 双向通道
  -> Linux instruction service
  -> subprocess/PTY 执行
  -> stdout/stderr/exit 响应
  -> 实际设备 UI 显示
```

建议新增独立类型/端口，例如 `linux_instruction`，保留当前 `instruction` 的方向不变。需要新增实际设备控制 UI 或本地 IPC adapter、Linux 执行页面/后台服务、请求协议、超时、取消、输出流和测试。必须加入命令白名单或沙箱、运行用户限制、审计和明确授权。若只做“一次性命令—响应”，属于中等改动；若要完整交互式终端，还要 PTY、窗口大小、信号和会话管理，属于大改动。

### 反向 file

当前 FileAdapter 只映射实际设备上的一个文件，不能反过来管理 Linux 文件。建议新增独立 `linux_file` 服务，协议至少定义：

```text
list(relative_path)
stat(relative_path)
read(relative_path, offset, length)
write(relative_path, offset/mode, bytes)
mkdir / rename / delete（按需开放）
```

每个请求要有 UUID、状态码、文件版本/mtime、分块序号、长度和 SHA-256。Linux 端必须把访问限制在配置好的根目录内，规范化路径并阻止 `..`、符号链接逃逸和绝对路径；写入采用临时文件与原子替换。实际设备还需要文件浏览/上传/下载 UI。只支持一个固定 Linux 文件的 read/append 可以缩小为中等改动；做通用文件管理器则是大改动。

### 推荐实施顺序

1. 先实现反向 keyboard，验证现有 actual-device→Linux 数据方向和 `uinput` 权限模型。
2. 再做一次性反向 instruction，先限制白名单命令，不立即做 PTY。
3. 最后做固定根目录的反向 file，先 list/read/upload/download，再考虑删除和重命名。
4. 三项功能使用新的“Linux target”类型或明确的 direction 字段，不改变现有命令含义。
5. 如果要求实际设备在 Linux TUI 未打开时也能随时控制 Linux，还需要 Linux 常驻服务主动保持这些字符设备 open；这会扩大生命周期和服务管理范围。

综合判断：完成 keyboard、一次性 instruction、受限 file 三项反向功能属于一个新的应用层子系统，整体是中到大型改动，但大部分情况下可以复用当前双向字节通道，不必改 `libncd`、`ncdd` 和内核 driver。真正大的工作集中在两端应用角色、实际设备控制 UI、Linux 执行/文件服务、权限安全和端到端测试。

## 测试

```bash
python3 -m unittest discover -s test -p 'test_*.py' -v
```

测试覆盖主页选择、keyboard 双向事件和非法按键拦截、camera 长度头/接收进度、连续大帧保存、file 显式/默认路径、原子替换、超长帧拦截及回传确认、instruction 非法请求、真实超时、输出截断、PowerShell/bash 自动检测和输出返回、真实 adapters 的子进程回环，以及页面内重复 open/close。
