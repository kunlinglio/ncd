# ncdd — Network Character Device Daemon

将远程 NCD 服务端的设备映射为 Linux 本地字符设备 `/dev/ncd*`，
用户进程通过 `open/read/write/close` 即可与远端设备双向通信。

## 安装

```bash
cargo install --path crates/ncdd
```

驱动源码随二进制内嵌，运行时自动编译加载，无需单独安装内核模块。
唯一前置：内核头文件和编译工具链。

```bash
sudo apt install linux-headers-$(uname -r) build-essential
```

## 配置

创建 `/etc/ncd/config.toml`，每个远端设备一个 `[[device]]` 节：

```toml
[[device]]
name = "ncd01"
remote_ip = "192.168.1.100"
remote_port = 10000

[[device]]
name = "ncd02"
remote_ip = "192.168.1.101"
remote_port = 10001
```

| 字段          | 含义                             |
| ------------- | -------------------------------- |
| `name`        | 设备节点名，会创建 `/dev/<name>` |
| `remote_ip`   | 远端 ncd 服务端 IP               |
| `remote_port` | 远端 ncd 服务端端口              |

## 使用

```bash
# 安装（一次性）
cargo install --path crates/ncdd

# 启动（需要 root，sudo 需用完整路径）
sudo $(which ncdd)
```

启动流程：

```
ncdd
  ├─ 自动检测/编译/加载 ncd.ko 内核驱动
  ├─ 注册 daemon PID
  ├─ 根据配置文件创建 /dev/ncd01, /dev/ncd02, …
  └─ 进入事件循环，双向转发数据
```

远端必须在对应端口运行 ncd 服务端（`ncd run`），否则 `open()` 会返回连接拒绝。

## 用户侧操作

daemon 启动后，设备节点 `ls /dev/ncd*` 已创建。用户进程通过标准文件 API 使用：

```c
int fd = open("/dev/ncd01", O_RDWR);
write(fd, "hello", 5);
char buf[4096];
int n = read(fd, buf, sizeof(buf));
close(fd);
```

用户感知的只是一个普通文件。数据通过 daemon 透明转发到远端。
