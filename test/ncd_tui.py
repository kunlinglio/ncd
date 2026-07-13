from __future__ import annotations

import argparse
import cmd
import errno
import hashlib
import json
import multiprocessing
import os
import queue as queue_module
import shlex
import struct
import sys
import threading
import time
import tomllib
import uuid
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Callable


DEFAULT_CONFIG = Path("/etc/ncd/config.toml")
DEFAULT_READ_SIZE = 64 * 1024
DEFAULT_WRITE_CHUNK_SIZE = 4096
MAX_FRAME_SIZE = 64 * 1024 * 1024
IO_POLL_INTERVAL = 0.02
SUPPORTED_KINDS = ("camera", "keyboard", "instruction", "file")

DEFAULT_PORT_TYPES = {
    8000: "file",
    9000: "camera",
    10000: "keyboard",
    11000: "instruction",
}

CAMERA_MAGIC = b"NCDC1"
CAMERA_HEADER_SIZE = len(CAMERA_MAGIC) + 8 + 32

TEXT_SPECIAL_KEYS = {
    "space": " ",
    "tab": "\t",
    "enter": "\n",
}
SPECIAL_KEYS = {
    "alt",
    "alt_l",
    "alt_r",
    "backspace",
    "cmd",
    "cmd_l",
    "cmd_r",
    "ctrl",
    "ctrl_l",
    "ctrl_r",
    "delete",
    "down",
    "end",
    "enter",
    "esc",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "home",
    "left",
    "page_down",
    "page_up",
    "right",
    "shift",
    "shift_l",
    "shift_r",
    "space",
    "tab",
    "up",
}


class OperationCancelled(Exception):
    pass


def open_device(path: str, mode: str = "rw", *, nonblocking: bool = False) -> int:
    if mode == "r":
        flags = os.O_RDONLY
    elif mode == "w":
        flags = os.O_WRONLY
    elif mode == "rw":
        flags = os.O_RDWR
    else:
        raise ValueError("mode must be r, w, or rw")

    if nonblocking:
        flags |= getattr(os, "O_NONBLOCK", 0)
    return os.open(path, flags)


def _check_wait(stop_event: threading.Event | None, deadline: float | None) -> None:
    if stop_event is not None and stop_event.is_set():
        raise OperationCancelled()
    if deadline is not None and time.monotonic() >= deadline:
        raise TimeoutError("timed out while waiting for device data")


def read_exact(
    fd: int,
    size: int,
    *,
    stop_event: threading.Event | None = None,
    deadline: float | None = None,
) -> bytes:
    chunks: list[bytes] = []
    remaining = size

    while remaining > 0:
        _check_wait(stop_event, deadline)
        try:
            chunk = os.read(fd, min(DEFAULT_READ_SIZE, remaining))
        except BlockingIOError:
            if stop_event is not None:
                stop_event.wait(IO_POLL_INTERVAL)
            else:
                time.sleep(IO_POLL_INTERVAL)
            continue
        except OSError as error:
            if error.errno in (errno.EAGAIN, errno.EWOULDBLOCK):
                time.sleep(IO_POLL_INTERVAL)
                continue
            raise

        if not chunk:
            raise EOFError(f"device closed while reading {size} bytes")
        chunks.append(chunk)
        remaining -= len(chunk)

    return b"".join(chunks)


def read_frame(
    fd: int,
    *,
    stop_event: threading.Event | None = None,
    timeout: float | None = None,
) -> bytes:
    deadline = None if timeout is None else time.monotonic() + timeout
    header = read_exact(fd, 4, stop_event=stop_event, deadline=deadline)
    payload_len = struct.unpack("!I", header)[0]
    if payload_len > MAX_FRAME_SIZE:
        raise ValueError(f"frame too large: {payload_len} bytes")
    return read_exact(fd, payload_len, stop_event=stop_event, deadline=deadline)


def write_all(fd: int, data: bytes, chunk_size: int = DEFAULT_WRITE_CHUNK_SIZE) -> None:
    view = memoryview(data)
    while view:
        try:
            written = os.write(fd, view[:chunk_size])
        except BlockingIOError:
            time.sleep(IO_POLL_INTERVAL)
            continue
        if written <= 0:
            raise EOFError("device closed while writing")
        view = view[written:]


def write_frame(fd: int, payload: bytes) -> None:
    if len(payload) > MAX_FRAME_SIZE:
        raise ValueError(f"frame too large: {len(payload)} bytes")
    write_all(fd, struct.pack("!I", len(payload)) + payload)


def read_json_frame(
    fd: int,
    *,
    stop_event: threading.Event | None = None,
    timeout: float | None = None,
) -> Any:
    return json.loads(read_frame(fd, stop_event=stop_event, timeout=timeout).decode("utf-8"))


def write_json_frame(fd: int, value: Any) -> None:
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    write_frame(fd, payload)


def durable_write(path: Path, data: bytes) -> None:
    """Atomically publish a complete file after its contents reach the filesystem."""
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    try:
        with temporary.open("wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def append_jsonl(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as output:
        output.write(json.dumps(value, ensure_ascii=False) + "\n")
        output.flush()


def safe_name(value: str) -> str:
    result = "".join(ch if ch.isalnum() or ch in "-_." else "_" for ch in value)
    return result.strip("._") or "connection"


def jpeg_dimensions(data: bytes) -> tuple[int, int] | None:
    if len(data) < 4 or data[:2] != b"\xff\xd8":
        return None

    offset = 2
    while offset + 3 < len(data):
        if data[offset] != 0xFF:
            offset += 1
            continue
        while offset < len(data) and data[offset] == 0xFF:
            offset += 1
        if offset >= len(data):
            return None

        marker = data[offset]
        offset += 1
        if marker in (0xD9, 0xDA):
            return None
        if marker == 0x01 or 0xD0 <= marker <= 0xD7:
            continue
        if offset + 2 > len(data):
            return None

        segment_len = struct.unpack("!H", data[offset : offset + 2])[0]
        if segment_len < 2 or offset + segment_len > len(data):
            return None
        if marker in {
            0xC0,
            0xC1,
            0xC2,
            0xC3,
            0xC5,
            0xC6,
            0xC7,
            0xC9,
            0xCA,
            0xCB,
            0xCD,
            0xCE,
            0xCF,
        }:
            if segment_len < 7:
                return None
            height = struct.unpack("!H", data[offset + 3 : offset + 5])[0]
            width = struct.unpack("!H", data[offset + 5 : offset + 7])[0]
            return width, height
        offset += segment_len

    return None


def decode_camera_payload(payload: bytes) -> tuple[int | None, bytes, str]:
    if not payload.startswith(CAMERA_MAGIC):
        return None, payload, hashlib.sha256(payload).hexdigest()
    if len(payload) < CAMERA_HEADER_SIZE:
        raise ValueError("truncated camera envelope")

    sequence = struct.unpack("!Q", payload[len(CAMERA_MAGIC) : len(CAMERA_MAGIC) + 8])[0]
    expected = payload[len(CAMERA_MAGIC) + 8 : CAMERA_HEADER_SIZE]
    jpeg = payload[CAMERA_HEADER_SIZE:]
    actual = hashlib.sha256(jpeg).digest()
    if actual != expected:
        raise ValueError(f"camera frame {sequence} failed SHA-256 verification")
    return sequence, jpeg, actual.hex()


@dataclass(frozen=True)
class ConnectionSpec:
    name: str
    kind: str
    path: str
    remote_ip: str | None = None
    remote_port: int | None = None

    @property
    def endpoint(self) -> str:
        if self.remote_ip is None or self.remote_port is None:
            return "<endpoint unknown>"
        return f"{self.remote_ip}:{self.remote_port}"


class DeviceSession:
    def __init__(
        self,
        spec: ConnectionSpec,
        opener: Callable[..., int] = open_device,
    ):
        self.spec = spec
        self.opener = opener
        self.fd: int | None = None
        self.lifecycle_lock = threading.Lock()
        self.read_lock = threading.Lock()
        self.write_lock = threading.Lock()

    @property
    def is_open(self) -> bool:
        with self.lifecycle_lock:
            return self.fd is not None

    def open(self) -> bool:
        with self.lifecycle_lock:
            if self.fd is not None:
                return False
            # Keep the existing blocking character-device contract.  Camera,
            # keyboard and file reads run in a TUI-owned child process, which
            # can be terminated before this fd is closed.
            self.fd = self.opener(self.spec.path, "rw", nonblocking=False)
            return True

    def fileno(self) -> int:
        with self.lifecycle_lock:
            if self.fd is None:
                raise RuntimeError("connection is closed; use 'open' first")
            return self.fd

    def read_frame(
        self,
        *,
        stop_event: threading.Event | None = None,
        timeout: float | None = None,
    ) -> bytes:
        with self.read_lock:
            return read_frame(self.fileno(), stop_event=stop_event, timeout=timeout)

    def read_json(
        self,
        *,
        stop_event: threading.Event | None = None,
        timeout: float | None = None,
    ) -> Any:
        with self.read_lock:
            return read_json_frame(self.fileno(), stop_event=stop_event, timeout=timeout)

    def write_frame(self, payload: bytes) -> None:
        with self.write_lock:
            write_frame(self.fileno(), payload)

    def write_json(self, value: Any) -> None:
        with self.write_lock:
            write_json_frame(self.fileno(), value)

    def close(self) -> bool:
        with self.lifecycle_lock:
            if self.fd is None:
                return False
            fd = self.fd
            self.fd = None
        os.close(fd)
        return True


class ConnectionPage(cmd.Cmd):
    intro = None
    command_help = ""

    def __init__(
        self,
        spec: ConnectionSpec,
        run_dir: Path,
        *,
        opener: Callable[..., int] = open_device,
        print_lock: threading.Lock | None = None,
    ):
        super().__init__()
        self.spec = spec
        self.run_dir = run_dir / spec.kind / safe_name(spec.name)
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.session = DeviceSession(spec, opener)
        self.print_lock = print_lock or threading.Lock()
        self.prompt = f"ncd/{spec.kind}:{spec.name}> "
        self._shutdown = False

    def print(self, message: str = "", *, end: str = "\n", file=None) -> None:
        with self.print_lock:
            print(message, end=end, file=file or sys.stdout, flush=True)

    def emptyline(self):
        return None

    def run(self) -> None:
        self.print()
        self.print(f"[{self.spec.name}] {self.spec.kind}  {self.spec.path}  ->  {self.spec.endpoint}")
        self.print(f"日志目录: {self.run_dir}")
        self.print("进入子页面，正在自动 open ...")
        self.open_connection()
        self.show_commands()
        try:
            self.cmdloop()
        finally:
            self.shutdown()

    def show_commands(self) -> None:
        self.print()
        self.print("本页面命令:")
        for line in self.command_help.strip().splitlines():
            self.print(f"  {line.strip()}")
        self.print("  status              查看当前连接状态")
        self.print("  open / close        在本子页面内重复打开或关闭")
        self.print("  reopen              关闭后重新打开")
        self.print("  back                关闭连接并返回首页")
        self.print()

    def open_connection(self) -> bool:
        if self.session.is_open:
            self.print(f"[{self.spec.name}] 已经 open")
            return True
        try:
            self.session.open()
            self.on_open()
        except Exception as error:
            try:
                self.on_before_close()
            finally:
                self.session.close()
            self.print(f"[{self.spec.name}] open 失败: {error}", file=sys.stderr)
            return False
        self.print(f"[{self.spec.name}] open 成功；当前只与此连接交互")
        return True

    def close_connection(self, *, announce: bool = True) -> bool:
        if not self.session.is_open:
            if announce:
                self.print(f"[{self.spec.name}] 已经 close")
            return False
        try:
            self.on_before_close()
        finally:
            self.session.close()
        if announce:
            self.print(f"[{self.spec.name}] close 完成")
        return True

    def on_open(self) -> None:
        pass

    def on_before_close(self) -> None:
        pass

    def do_open(self, _line):
        """Open this selected connection."""
        self.open_connection()

    def do_close(self, _line):
        """Close this selected connection without leaving its page."""
        self.close_connection()

    def do_reopen(self, _line):
        """Close and open this selected connection."""
        self.close_connection()
        self.open_connection()

    def do_status(self, _line):
        """Show connection state."""
        state = "OPEN" if self.session.is_open else "CLOSED"
        self.print(f"{self.spec.name}: {state}  {self.spec.path} -> {self.spec.endpoint}")

    def do_help(self, _line):
        """Show commands for this page."""
        self.show_commands()

    def do_back(self, _line):
        """Return to the connection-selection homepage."""
        self.shutdown()
        return True

    def do_exit(self, line):
        return self.do_back(line)

    def do_quit(self, line):
        return self.do_back(line)

    def do_EOF(self, line):
        self.print()
        return self.do_back(line)

    def shutdown(self) -> None:
        if self._shutdown:
            return
        self._shutdown = True
        self.close_connection(announce=self.session.is_open)


class ReceiverPage(ConnectionPage):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.receiver_stop: Any | None = None
        self.receiver_thread: threading.Thread | None = None
        self.receiver_process: multiprocessing.Process | None = None
        self.receiver_queue: Any | None = None
        self.receiver_error: str | None = None

    def on_open(self) -> None:
        self.receiver_error = None
        if os.name == "posix" and type(self.session) is DeviceSession:
            # The current kernel driver has a blocking read() and no poll
            # callback.  A process is therefore the only application-layer
            # way to cancel camera/keyboard/file receiving without changing
            # ncdd or the driver.  fork preserves the one selected fd.
            context = multiprocessing.get_context("fork")
            self.receiver_stop = context.Event()
            self.receiver_queue = context.Queue()
            self.receiver_process = context.Process(
                target=self.receiver_loop,
                args=(self.receiver_stop,),
                name=f"ncd-{self.spec.kind}-{safe_name(self.spec.name)}",
                daemon=True,
            )
            self.receiver_process.start()
            return

        # Portable in-memory fallback used by the protocol tests.
        self.receiver_stop = threading.Event()
        self.receiver_queue = queue_module.Queue()
        self.receiver_thread = threading.Thread(
            target=self.receiver_loop,
            args=(self.receiver_stop,),
            name=f"ncd-{self.spec.kind}-{safe_name(self.spec.name)}",
            daemon=True,
        )
        self.receiver_thread.start()

    def receiver_loop(self, stop_event: Any) -> None:
        raise NotImplementedError

    def emit_receiver_event(self, value: dict[str, Any]) -> None:
        if self.receiver_queue is not None:
            self.receiver_queue.put(value)

    def next_receiver_event(self, timeout: float) -> dict[str, Any] | None:
        if self.receiver_queue is None:
            return None
        try:
            return self.receiver_queue.get(timeout=timeout)
        except queue_module.Empty:
            return None

    def drain_receiver_events(self) -> list[dict[str, Any]]:
        values: list[dict[str, Any]] = []
        if self.receiver_queue is None:
            return values
        while True:
            try:
                values.append(self.receiver_queue.get_nowait())
            except queue_module.Empty:
                return values

    def on_before_close(self) -> None:
        if self.receiver_stop is not None:
            self.receiver_stop.set()
        if self.receiver_process is not None:
            self.receiver_process.join(timeout=0.1)
            if self.receiver_process.is_alive():
                self.receiver_process.terminate()
                self.receiver_process.join(timeout=2)
            if self.receiver_process.is_alive():
                self.print("receiver process could not be terminated", file=sys.stderr)
        if self.receiver_thread is not None:
            self.receiver_thread.join(timeout=2)
            if self.receiver_thread.is_alive():
                self.print("receiver thread could not be stopped", file=sys.stderr)
        self.on_receiver_stopped()
        if self.receiver_queue is not None and hasattr(self.receiver_queue, "close"):
            self.receiver_queue.cancel_join_thread()
            self.receiver_queue.close()
        self.receiver_stop = None
        self.receiver_thread = None
        self.receiver_process = None
        self.receiver_queue = None

    def on_receiver_stopped(self) -> None:
        pass

    def receiver_status(self) -> str:
        alive = (
            (self.receiver_process is not None and self.receiver_process.is_alive())
            or (self.receiver_thread is not None and self.receiver_thread.is_alive())
        )
        result = f"receiver={'RUNNING' if alive else 'STOPPED'}"
        if self.receiver_error:
            result += f" error={self.receiver_error}"
        return result


class CameraPage(ReceiverPage):
    command_help = """
capture [OUTPUT.jpg]  等待下一帧；该帧始终会自动保存
latest [OUTPUT.jpg]   查看或复制最近一次成功保存的帧
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.frame_condition = threading.Condition()
        self.frame_count = 0
        self.last_remote_sequence: int | None = None
        self.latest_path: Path | None = None
        self.latest_bytes: bytes | None = None

    def on_open(self) -> None:
        self.last_remote_sequence = None
        super().on_open()

    def receiver_loop(self, stop_event: Any) -> None:
        self.print("[camera] 持续接收已启动：收到的每一帧都会校验并落盘")
        try:
            while not stop_event.is_set():
                payload = self.session.read_frame(stop_event=stop_event)
                try:
                    sequence, jpeg, digest = decode_camera_payload(payload)
                    self._save_frame(sequence, jpeg, digest)
                except Exception as error:
                    corrupt = self.run_dir / f"corrupt_{datetime.now():%Y%m%d_%H%M%S_%f}.bin"
                    durable_write(corrupt, payload)
                    append_jsonl(
                        self.run_dir / "frames.jsonl",
                        {"ok": False, "bytes": len(payload), "error": str(error), "output": str(corrupt)},
                    )
                    self.print(f"[camera] 数据校验失败，原始帧已保存: {error} -> {corrupt}", file=sys.stderr)
        except OperationCancelled:
            pass
        except Exception as error:
            if not stop_event.is_set():
                self.receiver_error = str(error)
                self.print(f"[camera] 接收失败: {error}", file=sys.stderr)
        finally:
            self.print("[camera] 持续接收已停止")

    def _save_frame(self, sequence: int | None, jpeg: bytes, digest: str) -> None:
        gap: str | None = None
        if sequence is not None and self.last_remote_sequence is not None:
            expected = self.last_remote_sequence + 1
            if sequence != expected:
                gap = f"远端帧序号不连续: expected={expected}, received={sequence}"
                self.print(f"[camera] {gap}", file=sys.stderr)
        if sequence is not None:
            self.last_remote_sequence = sequence

        with self.frame_condition:
            local_number = self.frame_count + 1

        sequence_part = f"remote_{sequence:012d}" if sequence is not None else f"local_{local_number:012d}"
        output = self.run_dir / f"frame_{sequence_part}_{datetime.now():%Y%m%d_%H%M%S_%f}.jpg"
        durable_write(output, jpeg)
        durable_write(self.run_dir / "latest.jpg", jpeg)

        record = {
            "ok": True,
            "frame": local_number,
            "remote_sequence": sequence,
            "bytes": len(jpeg),
            "sha256": digest,
            "size": jpeg_dimensions(jpeg),
            "gap": gap,
            "output": str(output),
        }
        append_jsonl(self.run_dir / "frames.jsonl", record)

        with self.frame_condition:
            self.frame_count = local_number
            self.latest_path = output
            self.latest_bytes = jpeg
            self.frame_condition.notify_all()
        self.emit_receiver_event({"type": "camera_frame", **record})
        self.print(
            f"[camera device->linux 已校验并保存] frame={sequence if sequence is not None else local_number} "
            f"bytes={len(jpeg)} sha256={digest[:12]} -> {output}"
        )

    def do_capture(self, line):
        """Wait for the next automatically received frame."""
        if not self.session.is_open:
            self.print("连接已关闭；请先执行 open")
            return
        parts = shlex.split(line)
        output = Path(parts[0]) if parts else None
        self._drain_camera_events()
        event = self._wait_camera_event(timeout=10)
        if event is None:
            self.print("[camera] 10 秒内没有收到新帧", file=sys.stderr)
            return
        latest_path = Path(event["output"])
        latest_bytes = latest_path.read_bytes()
        if output is not None:
            durable_write(output, latest_bytes)
            self.print(f"[camera] 下一帧副本 -> {output}")
        else:
            self.print(f"[camera] 下一帧已自动保存 -> {latest_path}")

    def do_latest(self, line):
        """Show or copy the most recently saved frame."""
        parts = shlex.split(line)
        self._drain_camera_events()
        latest_path = self.latest_path
        latest_bytes = latest_path.read_bytes() if latest_path and latest_path.exists() else None
        if latest_path is None or latest_bytes is None:
            self.print("[camera] 尚未成功保存任何帧")
            return
        if parts:
            output = Path(parts[0])
            durable_write(output, latest_bytes)
            self.print(f"[camera] latest 副本 -> {output}")
        else:
            self.print(str(latest_path))

    def do_status(self, line):
        self._drain_camera_events()
        super().do_status(line)
        self.print(f"{self.receiver_status()} saved_frames={self.frame_count} latest={self.latest_path}")

    def _apply_camera_event(self, event: dict[str, Any]) -> None:
        if event.get("type") != "camera_frame":
            return
        self.frame_count = max(self.frame_count, int(event["frame"]))
        self.latest_path = Path(event["output"])

    def _drain_camera_events(self) -> None:
        for event in self.drain_receiver_events():
            self._apply_camera_event(event)

    def _wait_camera_event(self, timeout: float) -> dict[str, Any] | None:
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            event = self.next_receiver_event(remaining)
            if event is None:
                return None
            self._apply_camera_event(event)
            if event.get("type") == "camera_frame":
                return event

    def on_receiver_stopped(self) -> None:
        self._drain_camera_events()


class KeyboardPage(ReceiverPage):
    command_help = """
type TEXT             向实际设备当前焦点窗口输入文字
tap KEY [TYPE]        按下并释放按键，TYPE 为 char/special/vk
press KEY [TYPE]      按下按键
release KEY [TYPE]    释放按键
mode                  进入键盘直通模式（Ctrl-] 退出）
listen text|events|off 选择设备->Linux 事件的显示方式（始终接收并记录）
"""

    def __init__(self, *args, event_display: str = "text", **kwargs):
        super().__init__(*args, **kwargs)
        self.event_display = event_display
        self.shared_event_display: Any | None = None

    def on_open(self) -> None:
        if os.name == "posix" and type(self.session) is DeviceSession:
            code = {"off": 0, "text": 1, "events": 2}[self.event_display]
            self.shared_event_display = multiprocessing.get_context("fork").Value("i", code)
        else:
            self.shared_event_display = None
        super().on_open()

    def _event_display_mode(self) -> str:
        if self.shared_event_display is None:
            return self.event_display
        return {0: "off", 1: "text", 2: "events"}[self.shared_event_display.value]

    def receiver_loop(self, stop_event: Any) -> None:
        try:
            while not stop_event.is_set():
                event = self.session.read_json(stop_event=stop_event)
                self._handle_keyboard_event(event)
        except OperationCancelled:
            pass
        except Exception as error:
            if not stop_event.is_set():
                self.receiver_error = str(error)
                self.print(f"[keyboard] 接收失败: {error}", file=sys.stderr)

    def _handle_keyboard_event(self, event: dict[str, Any]) -> None:
        append_jsonl(self.run_dir / "events.jsonl", event)
        text = keyboard_event_to_text(event)
        if text:
            path = self.run_dir / "text.txt"
            with path.open("a", encoding="utf-8") as output:
                output.write(text)
                output.flush()
        display = self._event_display_mode()
        if display == "events":
            self.print(f"[keyboard device->linux] {event}")
        elif display == "text" and text:
            self.print(text, end="")

    def send_keyboard_command(self, command: dict[str, Any], *, announce: bool = True) -> None:
        if not self.session.is_open:
            raise RuntimeError("connection is closed; use 'open' first")
        request_id = str(uuid.uuid4())
        request = {"id": request_id, **command}
        self.session.write_json(request)
        append_jsonl(self.run_dir / "commands.jsonl", {"state": "written_to_ncd", **request})
        if announce:
            self.print(
                f"[keyboard linux->device 已写入 NCD] {summarize_keyboard_command(request)} "
                f"id={request_id[:8]}；实际窗口是否接受输入取决于设备端焦点和系统权限"
            )

    def do_type(self, line):
        """Type text in the focused application on the actual device."""
        try:
            self.send_keyboard_command({"action": "type", "text": line})
        except Exception as error:
            self.print(f"[keyboard] {error}", file=sys.stderr)

    def _key_action(self, action: str, line: str) -> None:
        parts = shlex.split(line)
        if not parts:
            self.print(f"usage: {action} KEY [char|special|vk]")
            return
        key = parts[0]
        key_type = parts[1] if len(parts) > 1 else infer_key_type(key)
        try:
            self.send_keyboard_command({"action": action, "key_type": key_type, "key": key})
        except Exception as error:
            self.print(f"[keyboard] {error}", file=sys.stderr)

    def do_tap(self, line):
        self._key_action("tap", line)

    def do_press(self, line):
        self._key_action("press", line)

    def do_release(self, line):
        self._key_action("release", line)

    def do_listen(self, line):
        mode = line.strip().lower()
        if mode not in {"text", "events", "off"}:
            self.print("usage: listen text|events|off")
            return
        self.event_display = mode
        if self.shared_event_display is not None:
            self.shared_event_display.value = {"off": 0, "text": 1, "events": 2}[mode]
        self.print(f"[keyboard] 事件显示={mode}；后台接收和落盘不会停止")

    def do_mode(self, _line):
        """Enter direct keyboard pass-through mode."""
        if os.name == "posix" and sys.stdin.isatty():
            self._raw_mode()
        else:
            self._line_mode()

    def _line_mode(self) -> None:
        self.print("[keyboard] 行模式：输入文字；/enter、/tap KEY、/press KEY、/release KEY、/exit")
        while True:
            try:
                line = input("keyboard> ")
            except (EOFError, KeyboardInterrupt):
                self.print()
                break
            if line == "/exit":
                break
            try:
                if line == "/enter":
                    self.send_keyboard_command({"action": "tap", "key_type": "special", "key": "enter"})
                elif line.startswith("/tap "):
                    self._key_action("tap", line[5:])
                elif line.startswith("/press "):
                    self._key_action("press", line[7:])
                elif line.startswith("/release "):
                    self._key_action("release", line[9:])
                else:
                    self.send_keyboard_command({"action": "type", "text": line})
            except Exception as error:
                self.print(f"[keyboard] {error}", file=sys.stderr)

    def _raw_mode(self) -> None:
        import select
        import termios
        import tty

        fd = sys.stdin.fileno()
        old_settings = termios.tcgetattr(fd)
        self.print("[keyboard] 直通模式；Ctrl-] 退出。")
        try:
            tty.setraw(fd)
            while True:
                ready, _, _ = select.select([sys.stdin], [], [], 0.1)
                if not ready:
                    continue
                ch = sys.stdin.read(1)
                if ch in ("\x1d", "\x03", "\x04"):
                    break
                self._send_raw_input(ch, select)
        finally:
            termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)
            self.print()

    def _send_raw_input(self, ch: str, select_module) -> None:
        special = None
        if ch in ("\r", "\n"):
            special = "enter"
        elif ch == "\t":
            special = "tab"
        elif ch in ("\x7f", "\b"):
            special = "backspace"
        elif ch == "\x1b":
            sequence = ch
            while select_module.select([sys.stdin], [], [], 0.01)[0]:
                sequence += sys.stdin.read(1)
                if len(sequence) >= 6:
                    break
            special = {
                "\x1b": "esc",
                "\x1b[A": "up",
                "\x1b[B": "down",
                "\x1b[C": "right",
                "\x1b[D": "left",
                "\x1b[H": "home",
                "\x1b[F": "end",
                "\x1b[3~": "delete",
                "\x1b[5~": "page_up",
                "\x1b[6~": "page_down",
            }.get(sequence)
        if special is not None:
            self.send_keyboard_command(
                {"action": "tap", "key_type": "special", "key": special},
                announce=False,
            )
            self.print(f"<{special}>", end="")
        elif ch >= " ":
            self.send_keyboard_command({"action": "type", "text": ch}, announce=False)
            self.print(ch, end="")

    def do_status(self, line):
        super().do_status(line)
        self.print(f"{self.receiver_status()} display={self.event_display}")


class InstructionPage(ConnectionPage):
    command_help = """
run PROGRAM [ARGS...]  在实际设备端执行 argv（不经过 shell）
shell COMMAND...       在实际设备端执行 shell 命令（需设备端 allow_shell=true）
timeout MILLISECONDS   设置后续命令超时
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.timeout_ms = 5000

    def _run_request(self, request: dict[str, Any]) -> dict[str, Any]:
        request_id = str(uuid.uuid4())
        value = {"id": request_id, "timeout_ms": self.timeout_ms, **request}
        append_jsonl(self.run_dir / "requests.jsonl", value)
        self.print(f"[instruction linux->device] {summarize_instruction_request(value)}")
        self.session.write_json(value)

        deadline = time.monotonic() + self.timeout_ms / 1000 + 5
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("设备端未在期限内返回 instruction 响应")
            response = self.session.read_json(timeout=remaining)
            if response.get("id") == request_id:
                break
            append_jsonl(self.run_dir / "unmatched_responses.jsonl", response)

        append_jsonl(self.run_dir / "responses.jsonl", response)
        for stream in ("stdout", "stderr"):
            text = response.get(stream) or ""
            if text:
                with (self.run_dir / f"{stream}.log").open("a", encoding="utf-8") as output:
                    output.write(text)
        self.print(
            f"[instruction device->linux 已确认] returncode={response.get('returncode')} "
            f"ok={response.get('ok')} id={request_id[:8]}"
        )
        if response.get("stdout"):
            self.print(response["stdout"], end="")
        if response.get("stderr"):
            self.print(response["stderr"], end="", file=sys.stderr)
        return response

    def do_run(self, line):
        argv = shlex.split(line)
        if not argv:
            self.print("usage: run PROGRAM [ARGS...]")
            return
        try:
            self._run_request({"argv": argv})
        except Exception as error:
            self.print(f"[instruction] {error}", file=sys.stderr)

    def do_shell(self, line):
        if not line.strip():
            self.print("usage: shell COMMAND...")
            return
        try:
            self._run_request({"shell": True, "command": line})
        except Exception as error:
            self.print(f"[instruction] {error}", file=sys.stderr)

    def do_timeout(self, line):
        try:
            value = int(line)
            if value <= 0:
                raise ValueError
        except ValueError:
            self.print("usage: timeout POSITIVE_MILLISECONDS")
            return
        self.timeout_ms = value
        self.print(f"instruction timeout={value}ms")


class FilePage(ReceiverPage):
    command_help = """
cat                   显示最近一次设备端文件快照
pull [OUTPUT]         保存最近一次设备端文件快照
append TEXT           追加文本，并等待设备端回传的新快照确认
appendfile PATH       追加 Linux 本地文件，并等待设备端回传确认
stat                  显示最近快照的大小和 SHA-256
reopen                强制设备端重新发送当前文件快照
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.snapshot_count = 0
        self.latest_snapshot: Path | None = None

    def on_open(self) -> None:
        previous = self.snapshot_count
        self.latest_snapshot = None
        super().on_open()
        # The existing adapter always emits its current file once on open.
        # Waiting here removes the race between that initial snapshot and an
        # immediately entered append command.
        self._wait_snapshot(after=previous, timeout=10)

    def receiver_loop(self, stop_event: Any) -> None:
        try:
            while not stop_event.is_set():
                data = self.session.read_frame(stop_event=stop_event)
                self.snapshot_count += 1
                digest = hashlib.sha256(data).hexdigest()
                output = self.run_dir / (
                    f"snapshot_{self.snapshot_count:06d}_{datetime.now():%Y%m%d_%H%M%S_%f}.bin"
                )
                durable_write(output, data)
                record = {
                    "type": "file_snapshot",
                    "snapshot": self.snapshot_count,
                    "bytes": len(data),
                    "sha256": digest,
                    "output": str(output),
                }
                append_jsonl(self.run_dir / "snapshots.jsonl", record)
                self.emit_receiver_event(record)
                self.print(
                    f"[file device->linux 已保存] snapshot={self.snapshot_count} "
                    f"bytes={len(data)} sha256={digest[:12]} -> {output}"
                )
        except OperationCancelled:
            pass
        except Exception as error:
            if not stop_event.is_set():
                self.receiver_error = str(error)
                self.print(f"[file] 接收失败: {error}", file=sys.stderr)

    def _apply_snapshot_event(self, event: dict[str, Any]) -> None:
        if event.get("type") != "file_snapshot":
            return
        self.snapshot_count = max(self.snapshot_count, int(event["snapshot"]))
        self.latest_snapshot = Path(event["output"])

    def _drain_snapshots(self) -> None:
        for event in self.drain_receiver_events():
            self._apply_snapshot_event(event)

    def _wait_snapshot(self, *, after: int, timeout: float = 10) -> Path:
        deadline = time.monotonic() + timeout
        while True:
            self._drain_snapshots()
            if self.latest_snapshot is not None and self.snapshot_count > after:
                return self.latest_snapshot
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("10 秒内没有收到设备端的新文件快照")
            event = self.next_receiver_event(remaining)
            if event is None:
                raise TimeoutError("10 秒内没有收到设备端的新文件快照")
            self._apply_snapshot_event(event)

    def _latest_data(self) -> bytes:
        self._drain_snapshots()
        if self.latest_snapshot is None:
            self._wait_snapshot(after=-1)
        if self.latest_snapshot is None:
            raise RuntimeError("尚未收到设备端文件快照")
        return self.latest_snapshot.read_bytes()

    def _append(self, payload: bytes) -> None:
        if not payload:
            self.print("[file] 空 payload，不执行追加")
            return
        self._drain_snapshots()
        before = self.snapshot_count
        self.session.write_frame(payload)
        snapshot = self._wait_snapshot(after=before)
        data = snapshot.read_bytes()
        confirmed = data.endswith(payload)
        record = {
            "confirmed_by_snapshot": confirmed,
            "bytes": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
            "snapshot": str(snapshot),
        }
        append_jsonl(self.run_dir / "appends.jsonl", record)
        if not confirmed:
            raise RuntimeError("设备端回传了新快照，但末尾内容与追加 payload 不一致")
        self.print(
            f"[file linux->device 已由回传快照确认] bytes={len(payload)} "
            f"sha256={record['sha256'][:12]}"
        )

    def do_cat(self, _line):
        try:
            data = self._latest_data()
            self.print(data.decode("utf-8", errors="replace"), end="")
            if data and not data.endswith(b"\n"):
                self.print()
        except Exception as error:
            self.print(f"[file] {error}", file=sys.stderr)

    def do_pull(self, line):
        parts = shlex.split(line)
        try:
            data = self._latest_data()
            output = Path(parts[0]) if parts else self.run_dir / "latest_copy.bin"
            durable_write(output, data)
            self.print(f"[file] snapshot copy -> {output}")
        except Exception as error:
            self.print(f"[file] {error}", file=sys.stderr)

    def do_append(self, line):
        try:
            self._append(line.encode("utf-8"))
        except Exception as error:
            self.print(f"[file] {error}", file=sys.stderr)

    def do_appendfile(self, line):
        parts = shlex.split(line)
        if len(parts) != 1:
            self.print("usage: appendfile PATH")
            return
        try:
            self._append(Path(parts[0]).read_bytes())
        except Exception as error:
            self.print(f"[file] {error}", file=sys.stderr)

    def do_stat(self, _line):
        try:
            data = self._latest_data()
            self.print(f"remote file: bytes={len(data)} sha256={hashlib.sha256(data).hexdigest()}")
        except Exception as error:
            self.print(f"[file] {error}", file=sys.stderr)

    def do_status(self, line):
        self._drain_snapshots()
        super().do_status(line)
        self.print(
            f"{self.receiver_status()} snapshots={self.snapshot_count} latest={self.latest_snapshot}"
        )

    def on_receiver_stopped(self) -> None:
        self._drain_snapshots()


PAGE_TYPES = {
    "camera": CameraPage,
    "keyboard": KeyboardPage,
    "instruction": InstructionPage,
    "file": FilePage,
}


class NcdTui(cmd.Cmd):
    intro = None
    prompt = "ncd/home> "

    def __init__(
        self,
        connections: list[ConnectionSpec],
        run_dir: Path,
        *,
        opener: Callable[..., int] = open_device,
        keyboard_event_display: str = "text",
    ):
        super().__init__()
        self.connections = connections
        self.run_dir = run_dir
        self.opener = opener
        self.keyboard_event_display = keyboard_event_display
        self.print_lock = threading.Lock()
        self._quitting = False
        self.print_homepage()

    def print(self, message: str = "", *, end: str = "\n", file=None) -> None:
        with self.print_lock:
            print(message, end=end, file=file or sys.stdout, flush=True)

    def emptyline(self):
        return None

    def print_homepage(self) -> None:
        self.print()
        self.print("NCD Linux 应用层 TUI — 连接选择首页")
        self.print("ncdd 仅负责底层连接调度与转发；本页不会自动 open 任何默认连接。")
        self.print(f"本次运行日志: {self.run_dir}")
        self.print()
        if not self.connections:
            self.print("没有从 ncdd 配置或命令行参数中发现连接。")
        else:
            for index, spec in enumerate(self.connections, start=1):
                supported = "" if spec.kind in PAGE_TYPES else " [不支持的类型]"
                self.print(
                    f"  [{index}] {spec.name:<20} type={spec.kind:<11} "
                    f"{spec.path} -> {spec.endpoint}{supported}"
                )
        self.print()
        self.print("输入编号，或 open 编号/名称 进入子页面；connections 刷新列表显示；quit 退出。")
        self.print()

    def _find_connection(self, selector: str) -> ConnectionSpec:
        selector = selector.strip()
        if not selector:
            raise ValueError("usage: open INDEX|NAME|/dev/PATH")
        if selector.isdigit():
            index = int(selector)
            if 1 <= index <= len(self.connections):
                return self.connections[index - 1]
            raise ValueError(f"连接编号超出范围: {index}")

        matches = [spec for spec in self.connections if selector in {spec.name, spec.path}]
        if not matches:
            raise ValueError(f"找不到连接: {selector}")
        if len(matches) > 1:
            raise ValueError(f"连接名称不唯一，请使用编号: {selector}")
        return matches[0]

    def do_open(self, line):
        """Select a connection and enter its page."""
        try:
            spec = self._find_connection(line)
        except ValueError as error:
            self.print(str(error), file=sys.stderr)
            return
        page_type = PAGE_TYPES.get(spec.kind)
        if page_type is None:
            self.print(f"连接 {spec.name} 无法识别为 camera/keyboard/instruction/file", file=sys.stderr)
            return

        kwargs: dict[str, Any] = {
            "opener": self.opener,
            "print_lock": self.print_lock,
        }
        if page_type is KeyboardPage:
            kwargs["event_display"] = self.keyboard_event_display
        page = page_type(spec, self.run_dir, **kwargs)
        page.run()
        self.print("已返回连接选择首页；当前没有 open 的应用层连接。")
        self.print_homepage()

    def default(self, line):
        if line.strip().isdigit():
            return self.do_open(line)
        self.print(f"未知首页命令: {line!r}；请输入连接编号、open、connections 或 quit")

    def do_connections(self, _line):
        """Show selectable connections."""
        self.print_homepage()

    def do_logdir(self, _line):
        self.print(str(self.run_dir))

    def do_help(self, _line):
        self.print_homepage()

    def do_quit(self, _line):
        self._quitting = True
        return True

    def do_exit(self, line):
        return self.do_quit(line)

    def do_EOF(self, line):
        self.print()
        return self.do_quit(line)


def infer_device_kind(name: str, port: int) -> str:
    lowered = name.lower()
    for kind in SUPPORTED_KINDS:
        if kind in lowered:
            return kind
    return DEFAULT_PORT_TYPES.get(port, "unknown")


def load_connections(config_path: Path) -> list[ConnectionSpec]:
    """Load every configured connection in file order; never invent defaults."""
    if not config_path.exists():
        return []

    data = tomllib.loads(config_path.read_text(encoding="utf-8"))
    connections: list[ConnectionSpec] = []
    for entry in data.get("device", []):
        name = str(entry["name"])
        port = int(entry["remote_port"])
        connections.append(
            ConnectionSpec(
                name=name,
                kind=infer_device_kind(name, port),
                path=f"/dev/{name}",
                remote_ip=str(entry.get("remote_ip")) if entry.get("remote_ip") is not None else None,
                remote_port=port,
            )
        )
    return connections


def add_overrides(connections: list[ConnectionSpec], args: argparse.Namespace) -> None:
    for kind in SUPPORTED_KINDS:
        path = getattr(args, kind)
        if path:
            connections.append(
                ConnectionSpec(
                    name=f"manual_{kind}",
                    kind=kind,
                    path=path,
                )
            )


def infer_key_type(key: str) -> str:
    if key.isdigit():
        return "vk"
    if key in SPECIAL_KEYS:
        return "special"
    return "char"


def keyboard_event_to_text(event: dict[str, Any]) -> str:
    if event.get("event") != "press":
        return ""
    key_type = event.get("key_type")
    key = event.get("key", "")
    if key_type == "char":
        return str(key)
    if key_type == "special":
        if key == "backspace":
            return "\b \b"
        return TEXT_SPECIAL_KEYS.get(str(key), "")
    return ""


def summarize_keyboard_command(command: dict[str, Any]) -> str:
    action = command.get("action")
    if action == "type":
        text = str(command.get("text", ""))
        if len(text) > 60:
            text = text[:60] + "..."
        return f"type {text!r}"
    return f"{action} {command.get('key_type', 'char')}:{command.get('key')}"


def summarize_instruction_request(request: dict[str, Any]) -> str:
    if request.get("shell"):
        command = str(request.get("command", ""))
        if len(command) > 60:
            command = command[:60] + "..."
        return f"shell {command!r}"
    argv = request.get("argv") or []
    if argv:
        return f"argv {argv[0]!r} argc={len(argv)}"
    return "request"


def default_run_dir() -> Path:
    stamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    return Path(__file__).resolve().parent / "runs" / stamp


def main() -> int:
    parser = argparse.ArgumentParser(description="NCD Linux application-layer connection TUI")
    parser.add_argument("--config", default=str(DEFAULT_CONFIG), help="ncdd config path")
    parser.add_argument("--log-dir", default=None, help="directory for images and operation logs")
    parser.add_argument("--camera", default=None, help="add a selectable camera device path")
    parser.add_argument("--keyboard", default=None, help="add a selectable keyboard device path")
    parser.add_argument("--instruction", default=None, help="add a selectable instruction device path")
    parser.add_argument("--file", default=None, help="add a selectable file device path")
    parser.add_argument(
        "--keyboard-events",
        action="store_true",
        help="show raw keyboard events instead of reconstructed text",
    )
    # Kept only so old launch scripts do not fail. Camera receiving is now mandatory while its page is open.
    parser.add_argument("--no-auto-camera", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--no-auto-keyboard", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--camera-interval-ms", type=int, help=argparse.SUPPRESS)
    args = parser.parse_args()

    config_path = Path(args.config)
    try:
        connections = load_connections(config_path)
    except Exception as error:
        print(f"读取 ncdd 配置失败 {config_path}: {error}", file=sys.stderr)
        return 2
    add_overrides(connections, args)

    run_dir = Path(args.log_dir) if args.log_dir else default_run_dir()
    run_dir.mkdir(parents=True, exist_ok=True)
    tui = NcdTui(
        connections,
        run_dir,
        keyboard_event_display="events" if args.keyboard_events else "text",
    )
    try:
        tui.cmdloop()
    except KeyboardInterrupt:
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
