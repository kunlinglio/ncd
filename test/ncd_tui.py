import argparse
import cmd
import json
import os
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
from typing import Any


DEFAULT_CONFIG = Path("/etc/ncd/config.toml")
DEFAULT_READ_SIZE = 64 * 1024
DEFAULT_WRITE_CHUNK_SIZE = 64 * 1024
MAX_FRAME_SIZE = 64 * 1024 * 1024
DEFAULT_CAMERA_INTERVAL_MS = 1000
DEFAULT_DEVICES = {
    "camera": "/dev/ncd_camera",
    "keyboard": "/dev/ncd_keyboard",
    "instruction": "/dev/ncd_instruction",
    "file": "/dev/ncd_file",
}


def open_device(path: str, mode: str = "rw") -> int:
    if mode == "r":
        flags = os.O_RDONLY
    elif mode == "w":
        flags = os.O_WRONLY
    elif mode == "rw":
        flags = os.O_RDWR
    else:
        raise ValueError("mode must be r, w, or rw")
    return os.open(path, flags)


def read_exact(fd: int, size: int) -> bytes:
    chunks = []
    remaining = size

    while remaining > 0:
        chunk = os.read(fd, min(DEFAULT_READ_SIZE, remaining))
        if not chunk:
            raise EOFError(f"device closed while reading {size} bytes")
        chunks.append(chunk)
        remaining -= len(chunk)

    return b"".join(chunks)


def read_frame(fd: int) -> bytes:
    header = read_exact(fd, 4)
    payload_len = struct.unpack("!I", header)[0]
    if payload_len > MAX_FRAME_SIZE:
        raise ValueError(f"frame too large: {payload_len} bytes")
    return read_exact(fd, payload_len)


def write_all(fd: int, data: bytes, chunk_size: int = DEFAULT_WRITE_CHUNK_SIZE) -> None:
    view = memoryview(data)
    while view:
        chunk = view[:chunk_size]
        written = os.write(fd, chunk)
        view = view[written:]


def write_frame(fd: int, payload: bytes) -> None:
    write_all(fd, struct.pack("!I", len(payload)) + payload)


def read_json_frame(fd: int) -> Any:
    return json.loads(read_frame(fd).decode("utf-8"))


def write_json_frame(fd: int, value: Any) -> None:
    payload = json.dumps(value, ensure_ascii=False).encode("utf-8")
    write_frame(fd, payload)


def jpeg_dimensions(data: bytes) -> tuple[int, int] | None:
    if len(data) < 4 or data[:2] != b"\xff\xd8":
        return None

    i = 2
    while i + 3 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue

        while i < len(data) and data[i] == 0xFF:
            i += 1
        if i >= len(data):
            return None

        marker = data[i]
        i += 1

        if marker in (0xD9, 0xDA):
            return None
        if marker == 0x01 or 0xD0 <= marker <= 0xD7:
            continue

        if i + 2 > len(data):
            return None
        segment_len = struct.unpack("!H", data[i : i + 2])[0]
        if segment_len < 2 or i + segment_len > len(data):
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
            height = struct.unpack("!H", data[i + 3 : i + 5])[0]
            width = struct.unpack("!H", data[i + 5 : i + 7])[0]
            return width, height

        i += segment_len

    return None
DEFAULT_PORT_TYPES = {
    8000: "file",
    9000: "camera",
    10000: "keyboard",
    11000: "instruction",
}
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


@dataclass
class DeviceSpec:
    kind: str
    path: str
    remote_port: int | None = None


class SharedDevice:
    def __init__(self, path: str):
        self.path = path
        self.fd: int | None = None
        self.open_lock = threading.Lock()
        self.read_lock = threading.Lock()
        self.write_lock = threading.Lock()

    def open(self) -> int:
        with self.open_lock:
            if self.fd is None:
                self.fd = open_device(self.path, "rw")
            return self.fd

    def read_frame(self) -> bytes:
        with self.read_lock:
            return read_frame(self.open())

    def read_json(self):
        with self.read_lock:
            return read_json_frame(self.open())

    def write_frame(self, payload: bytes) -> None:
        with self.write_lock:
            write_frame(self.open(), payload)

    def write_json(self, value) -> None:
        with self.write_lock:
            write_json_frame(self.open(), value)

    def close(self) -> None:
        with self.open_lock:
            if self.fd is not None:
                try:
                    os.close(self.fd)
                finally:
                    self.fd = None


class BackgroundTask:
    def __init__(self, name: str, target):
        self.name = name
        self.stop_event = threading.Event()
        self.thread = threading.Thread(
            target=target,
            args=(self.stop_event,),
            name=name,
            daemon=True,
        )

    def start(self):
        self.thread.start()

    def stop(self):
        self.stop_event.set()


class NcdTui(cmd.Cmd):
    intro = None
    prompt = "ncd> "

    def __init__(
        self,
        devices: dict[str, DeviceSpec],
        run_dir: Path,
        *,
        auto_camera: bool = True,
        auto_keyboard: bool = True,
        camera_interval_ms: int = DEFAULT_CAMERA_INTERVAL_MS,
        keyboard_text_only: bool = True,
    ):
        super().__init__()
        self.devices = devices
        self.run_dir = run_dir
        self.handles: dict[str, SharedDevice] = {}
        self.tasks: dict[str, BackgroundTask] = {}
        self.print_lock = threading.Lock()
        self.camera_count = 0
        self.file_count = 0

        for subdir in ("camera", "keyboard", "instruction", "file"):
            (self.run_dir / subdir).mkdir(parents=True, exist_ok=True)

        self.print_homepage(
            auto_camera=auto_camera,
            auto_keyboard=auto_keyboard,
            camera_interval_ms=camera_interval_ms,
            keyboard_text_only=keyboard_text_only,
        )

        if auto_keyboard:
            self.start_keyboard_listener(keyboard_text_only)
        if auto_camera:
            self.start_camera_stream(camera_interval_ms)

    def get_spec(self, kind: str) -> DeviceSpec:
        if kind not in self.devices:
            raise RuntimeError(f"no {kind} device configured")
        return self.devices[kind]

    def get_handle(self, kind: str) -> SharedDevice:
        spec = self.get_spec(kind)
        if kind not in self.handles:
            self.handles[kind] = SharedDevice(spec.path)
        return self.handles[kind]

    def print(self, message: str = "", *, end: str = "\n", file=None):
        with self.print_lock:
            print(message, end=end, file=file or sys.stdout, flush=True)

    def append_text(self, relative: str, text: str):
        path = self.run_dir / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as output:
            output.write(text)

    def append_jsonl(self, relative: str, value):
        self.append_text(relative, json.dumps(value, ensure_ascii=False) + "\n")

    def print_devices(self):
        self.print("devices:")
        for kind in ("camera", "keyboard", "instruction", "file"):
            spec = self.devices.get(kind)
            if spec is None:
                self.print(f"  {kind:<12} <missing>")
            else:
                port = f":{spec.remote_port}" if spec.remote_port is not None else ""
                self.print(f"  {kind:<12} {spec.path}{port}")

    def print_homepage(
        self,
        *,
        auto_camera: bool,
        auto_keyboard: bool,
        camera_interval_ms: int,
        keyboard_text_only: bool,
    ):
        self.print("NCD Linux application TUI")
        self.print("This program reads/writes /dev/ncd_*; kernel driver and ncdd still handle forwarding.")
        self.print(f"logs: {self.run_dir}")
        self.print()
        self.print_devices()
        self.print()
        self.print("automatic behavior:")
        self.print(
            f"  camera      {'ON ' if auto_camera else 'OFF'} "
            f"receive JPEG frames every {camera_interval_ms}ms and save them under camera/"
        )
        self.print(
            f"  keyboard    {'ON ' if auto_keyboard else 'OFF'} "
            f"listen for device-side key input and print {'text' if keyboard_text_only else 'raw events'}"
        )
        self.print()
        self.print("common operations:")
        self.print("  keyboard mode                  direct keyboard passthrough; typed keys go to the device")
        self.print("  Ctrl-]                         leave keyboard raw mode")
        self.print("  keyboard listen start events   show raw key press/release events")
        self.print("  camera stream stop             stop automatic camera receiving")
        self.print("  camera stream start 1000       receive and save one frame per second")
        self.print("  instruction shell uname -a     run a command on the device side")
        self.print("  file cat                       read the exposed remote file from the beginning")
        self.print("  file append hello              append text to the exposed remote file")
        self.print("  status                         show background camera/keyboard tasks")
        self.print("  quit                           exit")
        self.print()

    def do_devices(self, _line):
        """Show current /dev mappings."""
        self.print_devices()

    def do_logdir(self, _line):
        """Show where this session writes images and logs."""
        self.print(str(self.run_dir))

    def do_use(self, line):
        """Override a device path: use keyboard /dev/ncd_keyboard"""
        parts = shlex.split(line)
        if len(parts) != 2:
            self.print("usage: use camera|keyboard|instruction|file /dev/name")
            return

        kind, path = parts
        if kind not in DEFAULT_DEVICES:
            self.print(f"unknown device kind: {kind}")
            return

        old = self.handles.pop(kind, None)
        if old is not None:
            old.close()
        self.devices[kind] = DeviceSpec(kind=kind, path=path)
        self.print(f"{kind} -> {path}")

    def do_camera(self, line):
        """Camera commands: camera capture [OUTPUT.jpg] | camera stream start [interval_ms] | camera stream stop"""
        parts = shlex.split(line)
        if not parts:
            self.print(self.do_camera.__doc__)
            return

        try:
            if parts[0] == "capture":
                output = Path(parts[1]) if len(parts) > 1 else None
                self.capture_camera(output)
                return

            if parts[:2] == ["stream", "start"]:
                interval_ms = int(parts[2]) if len(parts) > 2 else DEFAULT_CAMERA_INTERVAL_MS
                self.start_camera_stream(interval_ms)
                return

            if parts[:2] == ["stream", "stop"]:
                self.stop_task("camera")
                return

            self.print("usage: camera capture [OUTPUT.jpg] | camera stream start [interval_ms] | camera stream stop")
        except Exception as error:
            self.print(f"[camera] error: {error}", file=sys.stderr)

    def capture_camera(self, output: Path | None = None):
        spec = self.get_spec("camera")
        jpeg = self.get_handle("camera").read_frame()
        self.camera_count += 1

        if output is None:
            output = self.run_dir / "camera" / f"frame_{self.camera_count:06d}.jpg"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(jpeg)
        (self.run_dir / "camera" / "latest.jpg").write_bytes(jpeg)

        size = jpeg_dimensions(jpeg)
        self.append_jsonl(
            "camera/frames.jsonl",
            {
                "frame": self.camera_count,
                "device": spec.path,
                "bytes": len(jpeg),
                "size": size,
                "output": str(output),
            },
        )
        self.print(f"[camera {spec.path} device->linux] {len(jpeg)} bytes size={size or 'unknown'} -> {output}")

    def start_camera_stream(self, interval_ms: int):
        if self.task_alive("camera"):
            self.print("[camera] stream already running")
            return

        def run(stop_event: threading.Event):
            self.print(f"[camera] stream started interval={interval_ms}ms")
            while not stop_event.is_set():
                try:
                    self.capture_camera()
                except Exception as error:
                    if not stop_event.is_set():
                        self.print(f"[camera] stream error: {error}", file=sys.stderr)
                    break
                stop_event.wait(interval_ms / 1000)
            self.print("[camera] stream stopped")

        task = BackgroundTask("camera", run)
        self.tasks["camera"] = task
        task.start()

    def do_keyboard(self, line):
        """Keyboard commands: keyboard mode | listen start [text|events] | listen stop | type TEXT | tap/press/release KEY [char|special|vk]"""
        parts = shlex.split(line)
        if not parts:
            self.print(self.do_keyboard.__doc__)
            return

        try:
            if parts[0] == "mode":
                self.keyboard_mode()
                return

            if parts[:2] == ["listen", "start"]:
                text_only = not (len(parts) > 2 and parts[2] == "events")
                self.start_keyboard_listener(text_only)
                return

            if parts[:2] == ["listen", "stop"]:
                self.stop_task("keyboard")
                return

            if parts[0] == "type":
                text = " ".join(parts[1:])
                self.send_keyboard_command({"action": "type", "text": text})
                return

            if parts[0] in {"tap", "press", "release"}:
                if len(parts) < 2:
                    self.print(f"usage: keyboard {parts[0]} KEY [char|special|vk]")
                    return
                key = parts[1]
                key_type = parts[2] if len(parts) > 2 else infer_key_type(key)
                self.send_keyboard_command({"action": parts[0], "key_type": key_type, "key": key})
                return

            self.print(
                "usage: keyboard mode | listen start [text|events] | listen stop | "
                "type TEXT | tap/press/release KEY [char|special|vk]"
            )
        except Exception as error:
            self.print(f"[keyboard] error: {error}", file=sys.stderr)

    def do_keymode(self, _line):
        """Shortcut for: keyboard mode"""
        self.keyboard_mode()

    def start_keyboard_listener(self, text_only: bool):
        if self.task_alive("keyboard"):
            self.print("[keyboard] listener already running")
            return

        def run(stop_event: threading.Event):
            spec = self.get_spec("keyboard")
            self.print(f"[keyboard] listener started text_only={text_only}")
            while not stop_event.is_set():
                try:
                    event = self.get_handle("keyboard").read_json()
                except Exception as error:
                    if not stop_event.is_set():
                        self.print(f"[keyboard] listener error: {error}", file=sys.stderr)
                    break

                self.append_jsonl("keyboard/events.jsonl", event)
                text = keyboard_event_to_text(event)
                if text:
                    self.append_text("keyboard/text.txt", text)

                if text_only:
                    if text:
                        self.print(text, end="")
                else:
                    self.print(f"[keyboard {spec.path} device->linux] {event}")
            self.print("[keyboard] listener stopped")

        task = BackgroundTask("keyboard", run)
        self.tasks["keyboard"] = task
        task.start()

    def keyboard_mode(self):
        if not self.task_alive("keyboard"):
            self.start_keyboard_listener(True)

        if os.name == "posix" and sys.stdin.isatty():
            self.keyboard_raw_mode()
            return

        self.keyboard_line_mode()

    def keyboard_line_mode(self):
        self.print(
            "[keyboard] line mode. Type text to send it to the device. "
            "Use /enter, /tap KEY, /press KEY, /release KEY, /exit."
        )

        while True:
            try:
                line = input("keyboard> ")
            except (EOFError, KeyboardInterrupt):
                self.print()
                break

            if line == "/exit":
                break
            if line == "/enter":
                self.send_keyboard_special("enter")
                continue
            if line.startswith("/tap "):
                self.send_keyboard_special(line[5:].strip())
                continue
            if line.startswith("/press "):
                self.send_keyboard_special(line[7:].strip(), action="press")
                continue
            if line.startswith("/release "):
                self.send_keyboard_special(line[9:].strip(), action="release")
                continue

            self.send_keyboard_command({"action": "type", "text": line})

        self.print("[keyboard] left keyboard mode")

    def keyboard_raw_mode(self):
        import select
        import termios
        import tty

        fd = sys.stdin.fileno()
        old_settings = termios.tcgetattr(fd)

        self.print(
            "[keyboard] raw mode. Every key you type is sent to the device. "
            "Press Ctrl-] to leave this mode."
        )

        try:
            tty.setraw(fd)
            while True:
                ready, _, _ = select.select([sys.stdin], [], [], 0.1)
                if not ready:
                    continue

                ch = sys.stdin.read(1)
                if ch in ("\x1d", "\x03", "\x04"):
                    break

                self.send_keyboard_raw_input(ch, select_module=select)
        finally:
            termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)
            self.print()
            self.print("[keyboard] left keyboard mode")

    def send_keyboard_raw_input(self, ch: str, *, select_module):
        if ch in ("\r", "\n"):
            self.send_keyboard_special("enter", announce=False)
            self.print()
            return
        if ch == "\t":
            self.send_keyboard_special("tab", announce=False)
            self.print("\t", end="")
            return
        if ch in ("\x7f", "\b"):
            self.send_keyboard_special("backspace", announce=False)
            self.print("\b \b", end="")
            return
        if ch == "\x1b":
            sequence = ch
            while select_module.select([sys.stdin], [], [], 0.01)[0]:
                sequence += sys.stdin.read(1)
                if len(sequence) >= 6:
                    break

            key = {
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

            if key is None and sequence == "\x1b":
                key = "esc"
            if key is not None:
                self.send_keyboard_special(key, announce=False)
                self.print(f"<{key}>", end="")
            return

        if ch >= " ":
            self.send_keyboard_command({"action": "type", "text": ch}, announce=False)
            self.print(ch, end="")

    def send_keyboard_special(self, key: str, *, action: str = "tap", announce: bool = True):
        if not key:
            return
        self.send_keyboard_command(
            {"action": action, "key_type": infer_key_type(key), "key": key},
            announce=announce,
        )

    def send_keyboard_command(self, command: dict, *, announce: bool = True):
        spec = self.get_spec("keyboard")
        self.get_handle("keyboard").write_json(command)
        self.append_jsonl("keyboard/commands.jsonl", command)
        if announce:
            self.print(f"[keyboard {spec.path} linux->device] {summarize_keyboard_command(command)}")

    def do_instruction(self, line):
        """Instruction commands: instruction run ARGS... | instruction shell COMMAND..."""
        parts = shlex.split(line)
        if not parts:
            self.print(self.do_instruction.__doc__)
            return

        try:
            if parts[0] == "run":
                argv = parts[1:]
                if not argv:
                    self.print("usage: instruction run ARGS...")
                    return
                self.run_instruction({"argv": argv})
                return

            if parts[0] == "shell":
                command = " ".join(parts[1:])
                if not command:
                    self.print("usage: instruction shell COMMAND...")
                    return
                self.run_instruction({"shell": True, "command": command})
                return

            self.print("usage: instruction run ARGS... | instruction shell COMMAND...")
        except Exception as error:
            self.print(f"[instruction] error: {error}", file=sys.stderr)

    def run_instruction(self, request: dict):
        spec = self.get_spec("instruction")
        request = {
            "id": str(uuid.uuid4()),
            "timeout_ms": 5000,
            **request,
        }

        fd = open_device(spec.path, "rw")
        try:
            self.print(f"[instruction {spec.path} linux->device] {summarize_instruction_request(request)}")
            self.append_jsonl("instruction/requests.jsonl", request)
            write_json_frame(fd, request)

            while True:
                response = read_json_frame(fd)
                if response.get("id") == request["id"]:
                    break

            self.append_jsonl("instruction/responses.jsonl", response)
            self.append_text("instruction/stdout.log", response.get("stdout") or "")
            self.append_text("instruction/stderr.log", response.get("stderr") or "")

            self.print(
                f"[instruction {spec.path} device->linux] "
                f"returncode={response.get('returncode')} ok={response.get('ok')}"
            )
            if response.get("stdout"):
                self.print(response["stdout"], end="")
            if response.get("stderr"):
                self.print(response["stderr"], end="", file=sys.stderr)
        finally:
            os.close(fd)

    def do_file(self, line):
        """File commands: file cat | file pull [OUTPUT] | file append TEXT | file append-file PATH"""
        parts = shlex.split(line)
        if not parts:
            self.print(self.do_file.__doc__)
            return

        try:
            if parts[0] == "cat":
                data = self.read_file_snapshot()
                self.print(data.decode("utf-8", errors="replace"), end="")
                return

            if parts[0] == "pull":
                output = Path(parts[1]) if len(parts) > 1 else None
                self.read_file_snapshot(output)
                return

            if parts[0] == "append":
                text = " ".join(parts[1:])
                self.append_file_payload(text.encode("utf-8"))
                return

            if parts[0] == "append-file":
                if len(parts) < 2:
                    self.print("usage: file append-file PATH")
                    return
                self.append_file_payload(Path(parts[1]).read_bytes())
                return

            self.print("usage: file cat | file pull [OUTPUT] | file append TEXT | file append-file PATH")
        except Exception as error:
            self.print(f"[file] error: {error}", file=sys.stderr)

    def read_file_snapshot(self, output: Path | None = None) -> bytes:
        spec = self.get_spec("file")
        fd = open_device(spec.path, "r")
        try:
            data = read_frame(fd)
        finally:
            os.close(fd)

        self.file_count += 1
        if output is None:
            output = self.run_dir / "file" / f"snapshot_{self.file_count:06d}.bin"
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(data)
        self.append_jsonl(
            "file/snapshots.jsonl",
            {"snapshot": self.file_count, "device": spec.path, "bytes": len(data), "output": str(output)},
        )
        self.print(f"[file {spec.path} device->linux] snapshot={len(data)} bytes -> {output}")
        return data

    def append_file_payload(self, payload: bytes):
        spec = self.get_spec("file")
        fd = open_device(spec.path, "w")
        try:
            write_frame(fd, payload)
        finally:
            os.close(fd)

        self.append_text("file/appends.log", payload.decode("utf-8", errors="replace"))
        self.append_jsonl("file/appends.jsonl", {"device": spec.path, "bytes": len(payload)})
        self.print(f"[file {spec.path} linux->device] append={len(payload)} bytes")

    def do_status(self, _line):
        """Show background tasks."""
        if not self.tasks:
            self.print("no background tasks")
            return
        for name, task in self.tasks.items():
            self.print(f"{name}: running={task.thread.is_alive()}")

    def task_alive(self, name: str) -> bool:
        task = self.tasks.get(name)
        return task is not None and task.thread.is_alive()

    def stop_task(self, name: str):
        task = self.tasks.pop(name, None)
        if task is None:
            self.print(f"[{name}] not running")
            return
        task.stop()
        handle = self.handles.pop(name, None)
        if handle is not None:
            handle.close()
        task.thread.join(timeout=1)

    def do_quit(self, _line):
        """Exit."""
        self.close()
        return True

    def do_exit(self, line):
        """Exit."""
        return self.do_quit(line)

    def do_EOF(self, line):
        self.print()
        return self.do_quit(line)

    def close(self):
        for name in list(self.tasks):
            self.stop_task(name)
        for handle in list(self.handles.values()):
            handle.close()
        self.handles.clear()


def infer_device_kind(name: str, port: int) -> str | None:
    lowered = name.lower()
    for kind in ("camera", "keyboard", "instruction", "file"):
        if kind in lowered:
            return kind
    return DEFAULT_PORT_TYPES.get(port)


def load_devices(config_path: Path) -> dict[str, DeviceSpec]:
    devices = {
        kind: DeviceSpec(kind=kind, path=path)
        for kind, path in DEFAULT_DEVICES.items()
    }

    if not config_path.exists():
        return devices

    data = tomllib.loads(config_path.read_text(encoding="utf-8"))
    for entry in data.get("device", []):
        name = str(entry["name"])
        port = int(entry["remote_port"])
        kind = infer_device_kind(name, port)
        if kind is not None:
            devices[kind] = DeviceSpec(kind=kind, path=f"/dev/{name}", remote_port=port)

    return devices


def infer_key_type(key: str) -> str:
    if key.isdigit():
        return "vk"
    if key in SPECIAL_KEYS:
        return "special"
    return "char"


def keyboard_event_to_text(event: dict) -> str:
    if event.get("event") != "press":
        return ""

    key_type = event.get("key_type")
    key = event.get("key", "")
    if key_type == "char":
        return key
    if key_type == "special":
        if key == "backspace":
            return "\b \b"
        return TEXT_SPECIAL_KEYS.get(key, "")
    return ""


def summarize_keyboard_command(command: dict) -> str:
    action = command.get("action")
    if action == "type":
        text = command.get("text", "")
        if len(text) > 60:
            text = text[:60] + "..."
        return f"type {text!r}"
    return f"{action} {command.get('key_type', 'char')}:{command.get('key')}"


def summarize_instruction_request(request: dict) -> str:
    if request.get("shell"):
        command = request.get("command", "")
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
    parser = argparse.ArgumentParser(description="Interactive terminal UI for NCD Linux devices")
    parser.add_argument("--config", default=str(DEFAULT_CONFIG), help="ncdd config path")
    parser.add_argument("--log-dir", default=None, help="directory for images and text logs")
    parser.add_argument("--camera", default=None, help="override camera device path")
    parser.add_argument("--keyboard", default=None, help="override keyboard device path")
    parser.add_argument("--instruction", default=None, help="override instruction device path")
    parser.add_argument("--file", default=None, help="override file device path")
    parser.add_argument("--no-auto-camera", action="store_true", help="do not start camera receiving on launch")
    parser.add_argument("--no-auto-keyboard", action="store_true", help="do not start keyboard listening on launch")
    parser.add_argument(
        "--camera-interval-ms",
        type=int,
        default=DEFAULT_CAMERA_INTERVAL_MS,
        help="camera auto-receive interval in milliseconds",
    )
    parser.add_argument(
        "--keyboard-events",
        action="store_true",
        help="print raw keyboard events instead of text while listening",
    )
    args = parser.parse_args()

    devices = load_devices(Path(args.config))
    for kind in ("camera", "keyboard", "instruction", "file"):
        override = getattr(args, kind)
        if override:
            devices[kind] = DeviceSpec(kind=kind, path=override)

    run_dir = Path(args.log_dir) if args.log_dir else default_run_dir()
    run_dir.mkdir(parents=True, exist_ok=True)

    tui = NcdTui(
        devices,
        run_dir,
        auto_camera=not args.no_auto_camera,
        auto_keyboard=not args.no_auto_keyboard,
        camera_interval_ms=args.camera_interval_ms,
        keyboard_text_only=not args.keyboard_events,
    )
    try:
        tui.cmdloop()
    finally:
        tui.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
