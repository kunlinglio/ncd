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
from datetime import datetime, timezone
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
        os.fsync(output.fileno())


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def utc_text(value: datetime | None = None) -> str:
    return (value or utc_now()).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def utc_filename(value: datetime | None = None) -> str:
    return (value or utc_now()).strftime("%Y%m%dT%H%M%S_%fZ")


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
        self.prompt = f"{spec.kind}> "
        self._shutdown = False

    def print(self, message: str = "", *, end: str = "\n", file=None) -> None:
        with self.print_lock:
            print(message, end=end, file=file or sys.stdout, flush=True)

    def emptyline(self):
        return None

    def run(self) -> None:
        self.print()
        self.print(f"[{self.spec.kind}] {self.spec.name}")
        self.print(f"Path: {self.spec.path} -> {self.spec.endpoint}")
        self.print(f"Saved data: {self.run_dir}")
        self.print("Opening the selected connection...")
        self.open_connection()
        self.show_commands()
        try:
            self.cmdloop()
        finally:
            self.shutdown()

    def show_commands(self) -> None:
        self.print()
        self.print("Commands:")
        for line in self.command_help.strip().splitlines():
            self.print(f"  {line.strip()}")
        self.print("  status                 show connection and data state")
        self.print("  open | close | reopen  manage only this connection")
        self.print("  back                   close it and return home")
        self.print("  help                   show this help")
        self.print()

    def open_connection(self) -> bool:
        if self.session.is_open:
            self.print(f"[{self.spec.name}] already OPEN")
            return True
        try:
            self.session.open()
            self.on_open()
        except Exception as error:
            try:
                self.on_before_close()
            finally:
                self.session.close()
            self.print(f"[{self.spec.name}] OPEN FAILED: {error}", file=sys.stderr)
            return False
        self.print(
            f"[{self.spec.name}] LOCAL HANDLE OPEN. This page uses only this connection."
        )
        return True

    def close_connection(self, *, announce: bool = True) -> bool:
        if not self.session.is_open:
            if announce:
                self.print(f"[{self.spec.name}] already CLOSED")
            return False
        try:
            self.on_before_close()
        finally:
            self.session.close()
        if announce:
            self.print(f"[{self.spec.name}] CLOSED")
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
        state = "LOCAL_HANDLE_OPEN" if self.session.is_open else "CLOSED"
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

    def receiver_queue_maxsize(self) -> int:
        return 0

    def drop_old_receiver_event_when_full(self) -> bool:
        return False

    def on_open(self) -> None:
        self.receiver_error = None
        if os.name == "posix" and type(self.session) is DeviceSession:
            # The current kernel driver has a blocking read() and no poll
            # callback.  A process is therefore the only application-layer
            # way to cancel camera/keyboard/file receiving without changing
            # ncdd or the driver.  fork preserves the one selected fd.
            context = multiprocessing.get_context("fork")
            self.receiver_stop = context.Event()
            self.receiver_queue = context.Queue(maxsize=self.receiver_queue_maxsize())
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
        self.receiver_queue = queue_module.Queue(maxsize=self.receiver_queue_maxsize())
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
        if self.receiver_queue is None:
            return
        if not self.drop_old_receiver_event_when_full():
            self.receiver_queue.put(value)
            return
        try:
            self.receiver_queue.put_nowait(value)
            return
        except queue_module.Full:
            pass
        try:
            self.receiver_queue.get_nowait()
        except queue_module.Empty:
            pass
        try:
            self.receiver_queue.put_nowait(value)
        except queue_module.Full:
            # Saving must never wait for a status-display queue.
            pass

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
        alive = self.receiver_is_alive()
        result = f"receiver={'RUNNING' if alive else 'STOPPED'}"
        if self.receiver_error:
            result += f" error={self.receiver_error}"
        return result

    def receiver_is_alive(self) -> bool:
        return (
            (self.receiver_process is not None and self.receiver_process.is_alive())
            or (self.receiver_thread is not None and self.receiver_thread.is_alive())
        )


class CameraPage(ReceiverPage):
    command_help = """
status                 show receive/save health, times and interval
latest [OUTPUT.jpg]    show or copy the latest saved image
capture [OUTPUT.jpg]   wait up to 10 seconds for the next image
path                   show image and metadata locations
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.frame_condition = threading.Condition()
        self.frame_count = 0
        self.last_remote_sequence: int | None = None
        self.latest_path: Path | None = None
        self.latest_bytes: bytes | None = None
        self.total_bytes = 0
        self.save_failures = 0
        self.invalid_frames = 0
        self.last_received_at_utc: str | None = None
        self.last_saved_at_utc: str | None = None
        self.last_saved_epoch: float | None = None
        self.last_interval_ms: float | None = None
        self._worker_last_saved_monotonic: float | None = None
        self._worker_last_report_monotonic: float | None = None

    def receiver_queue_maxsize(self) -> int:
        # Files and frames.jsonl are the authoritative history.  The queue is
        # latest-only so high-rate camera data can never block disk saving.
        return 1

    def drop_old_receiver_event_when_full(self) -> bool:
        return True

    def on_open(self) -> None:
        self.last_remote_sequence = None
        self._worker_last_saved_monotonic = None
        self._worker_last_report_monotonic = None
        super().on_open()

    def receiver_loop(self, stop_event: Any) -> None:
        self.print("camera: receiving continuously; every complete frame is saved")
        save_failure = False
        try:
            while not stop_event.is_set():
                payload = self.session.read_frame(stop_event=stop_event)
                try:
                    sequence, jpeg, digest = decode_camera_payload(payload)
                except Exception as error:
                    received_at = utc_text()
                    corrupt = self.run_dir / f"corrupt_{utc_filename()}.bin"
                    try:
                        durable_write(corrupt, payload)
                        record = {
                            "type": "camera_corrupt",
                            "ok": False,
                            "received_at_utc": received_at,
                            "bytes": len(payload),
                            "error": str(error),
                            "output": str(corrupt),
                        }
                        append_jsonl(self.run_dir / "frames.jsonl", record)
                        self.emit_receiver_event(record)
                        self.print(
                            f"camera SAVE WARNING: invalid frame kept as {corrupt}: {error}",
                            file=sys.stderr,
                        )
                    except Exception as save_error:
                        self._report_camera_save_error(save_error, len(payload))
                        save_failure = True
                        raise
                    continue

                try:
                    self._save_frame(sequence, jpeg, digest)
                except Exception as save_error:
                    self._report_camera_save_error(save_error, len(jpeg))
                    save_failure = True
                    raise
        except OperationCancelled:
            pass
        except Exception as error:
            if not stop_event.is_set():
                self.receiver_error = str(error)
                self.emit_receiver_event(
                    {
                        "type": "camera_receiver_error",
                        "error": str(error),
                        "save_error": save_failure,
                    }
                )
                self.print(f"camera RECEIVE/SAVE STOPPED: {error}", file=sys.stderr)
        finally:
            self.print("camera: receiver stopped")

    def _report_camera_save_error(self, error: Exception, byte_count: int) -> None:
        record = {
            "type": "camera_save_error",
            "ok": False,
            "received_at_utc": utc_text(),
            "bytes": byte_count,
            "error": str(error),
        }
        try:
            append_jsonl(self.run_dir / "frames.jsonl", record)
        except Exception:
            pass
        self.emit_receiver_event(record)
        self.print(f"camera SAVE ERROR: {error}", file=sys.stderr)

    def _save_frame(self, sequence: int | None, jpeg: bytes, digest: str) -> None:
        gap: str | None = None
        if sequence is not None and self.last_remote_sequence is not None:
            expected = self.last_remote_sequence + 1
            if sequence != expected:
                gap = f"remote sequence gap: expected={expected}, received={sequence}"
                self.print(f"camera WARNING: {gap}", file=sys.stderr)
        if sequence is not None:
            self.last_remote_sequence = sequence

        with self.frame_condition:
            local_number = self.frame_count + 1

        received_time = utc_now()
        received_at = utc_text(received_time)
        sequence_part = (
            f"remote_{sequence:012d}" if sequence is not None else f"local_{local_number:012d}"
        )
        output = self.run_dir / f"frame_{sequence_part}_{utc_filename(received_time)}.jpg"
        durable_write(output, jpeg)
        durable_write(self.run_dir / "latest.jpg", jpeg)

        saved_time = utc_now()
        saved_at = utc_text(saved_time)
        now_monotonic = time.monotonic()
        interval_ms = None
        if self._worker_last_saved_monotonic is not None:
            interval_ms = (now_monotonic - self._worker_last_saved_monotonic) * 1000
        self._worker_last_saved_monotonic = now_monotonic
        total_bytes = self.total_bytes + len(jpeg)

        record = {
            "type": "camera_frame",
            "ok": True,
            "frame": local_number,
            "remote_sequence": sequence,
            "received_at_utc": received_at,
            "saved_at_utc": saved_at,
            "saved_epoch": saved_time.timestamp(),
            "interval_ms": interval_ms,
            "bytes": len(jpeg),
            "total_bytes": total_bytes,
            "sha256": digest,
            "size": jpeg_dimensions(jpeg),
            "sample_hex": jpeg[:12].hex(),
            "gap": gap,
            "output": str(output),
        }
        append_jsonl(self.run_dir / "frames.jsonl", record)

        with self.frame_condition:
            self.frame_count = local_number
            self.total_bytes = total_bytes
            self.latest_path = output
            self.latest_bytes = jpeg
            self.last_received_at_utc = received_at
            self.last_saved_at_utc = saved_at
            self.last_saved_epoch = saved_time.timestamp()
            self.last_interval_ms = interval_ms
            self.frame_condition.notify_all()
        self.emit_receiver_event(record)

        # Keep a live indication without flooding the terminal with every frame.
        if (
            self._worker_last_report_monotonic is None
            or now_monotonic - self._worker_last_report_monotonic >= 5
        ):
            self._worker_last_report_monotonic = now_monotonic
            interval = "first frame" if interval_ms is None else f"interval={interval_ms:.0f}ms"
            self.print(
                f"device->linux: camera frame={local_number} bytes={len(jpeg)} "
                f"sample={jpeg[:12].hex()}..."
            )
            self.print(f"saved: OK time={saved_at} {interval} path={output}")

    def do_capture(self, line):
        """Wait for the next automatically received frame."""
        if not self.session.is_open:
            self.print("camera: connection is CLOSED; use 'open' first")
            return
        parts = shlex.split(line)
        output = Path(parts[0]) if parts else None
        self._drain_camera_events()
        event = self._wait_camera_event(timeout=10)
        if event is None:
            self.print("camera: no new frame in 10 seconds; peer may be disconnected", file=sys.stderr)
            return
        latest_path = Path(event["output"])
        latest_bytes = latest_path.read_bytes()
        if output is not None:
            durable_write(output, latest_bytes)
            self.print(f"camera: copied next frame -> {output}")
        else:
            self.print(f"camera: next frame was automatically saved -> {latest_path}")

    def do_latest(self, line):
        """Show or copy the most recently saved frame."""
        parts = shlex.split(line)
        self._drain_camera_events()
        latest_path = self.latest_path
        latest_bytes = latest_path.read_bytes() if latest_path and latest_path.exists() else None
        if latest_path is None or latest_bytes is None:
            self.print("camera: no frame has been saved yet")
            return
        if parts:
            output = Path(parts[0])
            durable_write(output, latest_bytes)
            self.print(f"camera: copied latest image -> {output}")
        else:
            self.print(str(latest_path))

    def do_status(self, line):
        self._drain_camera_events()
        super().do_status(line)
        if not self.session.is_open:
            health = "CLOSED"
        elif self.receiver_error:
            health = "ERROR"
        elif not self.receiver_is_alive():
            health = "RECEIVER STOPPED (possible disconnect/error)"
        elif self.last_saved_epoch is None:
            health = "WAITING FOR FIRST FRAME"
        else:
            age = max(0.0, time.time() - self.last_saved_epoch)
            stale_after = max(10.0, ((self.last_interval_ms or 0) / 1000) * 3)
            health = "RECEIVING" if age <= stale_after else "STALE (possible disconnect)"
        interval = "n/a" if self.last_interval_ms is None else f"{self.last_interval_ms:.0f}ms"
        self.print(
            f"camera={health} {self.receiver_status()} saved_frames={self.frame_count} "
            f"save_errors={self.save_failures} invalid_frames={self.invalid_frames} "
            f"total_bytes={self.total_bytes}"
        )
        self.print(
            f"last_saved_utc={self.last_saved_at_utc or 'never'} interval={interval} "
            f"latest={self.latest_path or 'none'}"
        )
        if health.startswith("STALE"):
            self.print("Hint: the current driver API cannot distinguish an idle stream from a dead peer.")
        elif "STOPPED" in health:
            self.print("Hint: the receive worker ended; use 'reopen' and inspect ncdd/device logs.")

    def do_path(self, _line):
        self._drain_camera_events()
        self.print(f"Images:   {self.run_dir}")
        self.print(f"Latest:   {self.run_dir / 'latest.jpg'}")
        self.print(f"Metadata: {self.run_dir / 'frames.jsonl'}")
        self.print("Use 'latest OUTPUT.jpg' to make an extra copy.")

    def _apply_camera_event(self, event: dict[str, Any]) -> None:
        event_type = event.get("type")
        if event_type == "camera_corrupt":
            self.invalid_frames += 1
            return
        if event_type == "camera_save_error":
            self.save_failures += 1
            self.receiver_error = str(event.get("error"))
            return
        if event_type == "camera_receiver_error":
            if event.get("save_error"):
                self.save_failures += 1
            self.receiver_error = str(event.get("error"))
            return
        if event_type != "camera_frame":
            return
        self.frame_count = max(self.frame_count, int(event["frame"]))
        self.latest_path = Path(event["output"])
        self.total_bytes = max(self.total_bytes, int(event.get("total_bytes", 0)))
        self.last_received_at_utc = event.get("received_at_utc")
        self.last_saved_at_utc = event.get("saved_at_utc")
        self.last_saved_epoch = event.get("saved_epoch")
        self.last_interval_ms = event.get("interval_ms")

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
send TEXT              type text in the focused app (also: send+TEXT)
enter                  send the Enter key
key KEY                tap a key, e.g. key left
combo KEYS             e.g. combo ctrl+c, combo shift+a, combo command+s
down KEY | up KEY      hold or release a modifier/key
raw                     direct keyboard mode; Ctrl-] exits
listen text|events|off  change display only; receiving and logging continue
info                    explain delivery and show log files
"""

    def __init__(self, *args, event_display: str = "text", **kwargs):
        super().__init__(*args, **kwargs)
        self.event_display = event_display
        self.shared_event_display: Any | None = None
        self.shared_keyboard_event_count: Any | None = None
        self.keyboard_event_count = 0
        self._keyboard_event_count_base = 0
        self._worker_keyboard_event_count = 0
        self.prompt = "linux->device: "

    def receiver_queue_maxsize(self) -> int:
        return 1

    def drop_old_receiver_event_when_full(self) -> bool:
        return True

    def on_open(self) -> None:
        self._keyboard_event_count_base = self.keyboard_event_count
        if os.name == "posix" and type(self.session) is DeviceSession:
            context = multiprocessing.get_context("fork")
            code = {"off": 0, "text": 1, "events": 2}[self.event_display]
            self.shared_event_display = context.Value("i", code)
            self.shared_keyboard_event_count = context.Value("Q", 0)
        else:
            self.shared_event_display = None
            self.shared_keyboard_event_count = None
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
                self.emit_receiver_event({"type": "keyboard_receiver_error", "error": str(error)})
                self.print(f"keyboard RECEIVE STOPPED: {error}", file=sys.stderr)

    def _handle_keyboard_event(self, event: dict[str, Any]) -> None:
        self._worker_keyboard_event_count += 1
        if self.shared_keyboard_event_count is not None:
            with self.shared_keyboard_event_count.get_lock():
                self.shared_keyboard_event_count.value += 1
        received_at = utc_text()
        append_jsonl(self.run_dir / "events.jsonl", {"received_at_utc": received_at, **event})
        text = keyboard_event_to_text(event)
        if text:
            path = self.run_dir / "text.txt"
            with path.open("a", encoding="utf-8") as output:
                output.write(text)
                output.flush()
                os.fsync(output.fileno())
        self.emit_receiver_event(
            {
                "type": "keyboard_event_received",
                "event": event,
                "event_count": self._worker_keyboard_event_count,
                "received_at_utc": received_at,
            }
        )
        display = self._event_display_mode()
        if display == "events":
            self._print_received(f"device->linux: event {event}")
        elif display == "text" and event.get("event") == "press":
            key_type = event.get("key_type", "unknown")
            key = event.get("key", "")
            if key_type == "char":
                rendered = repr(key)
            elif key_type == "special":
                rendered = f"<{key}>"
            else:
                rendered = f"<{key_type}:{key}>"
            self._print_received(f"device->linux: {rendered}")

    def _print_received(self, message: str) -> None:
        # cmd.Cmd is blocked inside readline while background input arrives.
        # Put the event on its own line and redraw the prompt so readline does
        # not visually swallow the device->Linux text.
        self.print(f"\r\n{message}\r\n{self.prompt}", end="")

    def _drain_keyboard_events(self) -> None:
        for value in self.drain_receiver_events():
            if value.get("type") == "keyboard_event_received":
                self.keyboard_event_count = max(
                    self.keyboard_event_count, int(value.get("event_count", 0))
                )
            elif value.get("type") == "keyboard_receiver_error":
                self.receiver_error = str(value.get("error"))
        if self.shared_keyboard_event_count is not None:
            self.keyboard_event_count = max(
                self.keyboard_event_count,
                self._keyboard_event_count_base + int(self.shared_keyboard_event_count.value),
            )

    def send_keyboard_command(self, command: dict[str, Any], *, announce: bool = True) -> None:
        if not self.session.is_open:
            raise RuntimeError("connection is closed; use 'open' first")
        request_id = str(uuid.uuid4())
        request = {"id": request_id, **command}
        self.session.write_json(request)
        append_jsonl(self.run_dir / "commands.jsonl", {"state": "written_to_ncd", **request})
        if announce:
            self.print(
                f"linux->device: {summarize_keyboard_command(request)} [written to NCD, id={request_id[:8]}]"
            )
            self.print("delivery note: the keyboard adapter has no ACK; focus and OS input permission matter")

    @staticmethod
    def _normalize_key(key: str) -> str:
        return {
            "command": "cmd",
            "meta": "cmd",
            "win": "cmd",
            "control": "ctrl",
            "return": "enter",
            "escape": "esc",
        }.get(key.lower(), key.lower())

    @staticmethod
    def _strip_plus(line: str) -> str:
        return line[1:] if line.startswith("+") else line

    def do_send(self, line):
        """Send text to the actual device's focused application."""
        text = self._strip_plus(line)
        if not text:
            self.print("usage: send TEXT  or  send+TEXT")
            return
        try:
            self.send_keyboard_command({"action": "type", "text": text})
        except Exception as error:
            self.print(f"keyboard: {error}", file=sys.stderr)

    def do_type(self, line):
        """Type text in the focused application on the actual device."""
        self.do_send(line)

    def _key_action(self, action: str, line: str) -> None:
        parts = shlex.split(line)
        if not parts:
            self.print(f"usage: {action} KEY [char|special|vk]")
            return
        key = self._normalize_key(parts[0])
        key_type = parts[1] if len(parts) > 1 else infer_key_type(key)
        try:
            self.send_keyboard_command({"action": action, "key_type": key_type, "key": key})
        except Exception as error:
            self.print(f"keyboard: {error}", file=sys.stderr)

    def do_tap(self, line):
        self._key_action("tap", line)

    def do_key(self, line):
        self._key_action("tap", line)

    def do_enter(self, _line):
        self._key_action("tap", "enter")

    def do_press(self, line):
        self._key_action("press", line)

    def do_down(self, line):
        self._key_action("press", line)

    def do_release(self, line):
        self._key_action("release", line)

    def do_up(self, line):
        self._key_action("release", line)

    def do_combo(self, line):
        value = self._strip_plus(line).strip()
        keys = [self._normalize_key(item.strip()) for item in value.split("+") if item.strip()]
        if len(keys) < 2:
            self.print("usage: combo ctrl+c  (also: combo+ctrl+c)")
            return
        held = keys[:-1]
        pressed: list[str] = []
        sent = False

        def combo_key_type(key: str) -> str:
            return "special" if key in SPECIAL_KEYS else "char"

        try:
            for key in held:
                self.send_keyboard_command(
                    {"action": "press", "key_type": combo_key_type(key), "key": key},
                    announce=False,
                )
                pressed.append(key)
            self.send_keyboard_command(
                {"action": "tap", "key_type": combo_key_type(keys[-1]), "key": keys[-1]},
                announce=False,
            )
            sent = True
        except Exception as error:
            self.print(f"keyboard: {error}", file=sys.stderr)
        finally:
            for key in reversed(pressed):
                try:
                    self.send_keyboard_command(
                        {"action": "release", "key_type": combo_key_type(key), "key": key},
                        announce=False,
                    )
                except Exception as error:
                    self.print(f"keyboard release warning: {error}", file=sys.stderr)
        if sent:
            self.print(f"linux->device: combo {'+'.join(keys)} [written to NCD; no adapter ACK]")

    def do_listen(self, line):
        mode = line.strip().lower()
        if mode not in {"text", "events", "off"}:
            self.print("usage: listen text|events|off")
            return
        self.event_display = mode
        if self.shared_event_display is not None:
            self.shared_event_display.value = {"off": 0, "text": 1, "events": 2}[mode]
        self._drain_keyboard_events()
        self.print(
            f"keyboard: display={mode}; receiving and logging remain active; "
            f"received={self.keyboard_event_count} log={self.run_dir / 'events.jsonl'}"
        )

    def do_mode(self, _line):
        """Enter direct keyboard pass-through mode."""
        if os.name == "posix" and sys.stdin.isatty():
            self._raw_mode()
        else:
            self._line_mode()

    def do_raw(self, line):
        self.do_mode(line)

    def _line_mode(self) -> None:
        self.print("keyboard line mode: text sends directly; /enter, /key KEY, /combo KEYS, /exit")
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
                elif line.startswith("/key "):
                    self._key_action("tap", line[5:])
                elif line.startswith("/combo "):
                    self.do_combo(line[7:])
                elif line.startswith("/tap "):
                    self._key_action("tap", line[5:])
                elif line.startswith("/press "):
                    self._key_action("press", line[7:])
                elif line.startswith("/release "):
                    self._key_action("release", line[9:])
                else:
                    self.send_keyboard_command({"action": "type", "text": line})
            except Exception as error:
                self.print(f"keyboard: {error}", file=sys.stderr)

    def _raw_mode(self) -> None:
        import select
        import termios
        import tty

        fd = sys.stdin.fileno()
        old_settings = termios.tcgetattr(fd)
        self.print("keyboard raw mode; Ctrl-] exits")
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
        self._drain_keyboard_events()
        super().do_status(line)
        self.print(
            f"{self.receiver_status()} display={self.event_display} "
            f"received_events={self.keyboard_event_count} log={self.run_dir / 'events.jsonl'}"
        )
        if self.session.is_open and not self.receiver_error and self.receiver_is_alive():
            self.print("State: OPEN/IDLE. A quiet keyboard cannot be distinguished from a disconnected peer.")

    def do_info(self, _line):
        self._drain_keyboard_events()
        self.print("Send log:     " + str(self.run_dir / "commands.jsonl"))
        self.print("Receive log:  " + str(self.run_dir / "events.jsonl"))
        self.print("Received text:" + " " + str(self.run_dir / "text.txt"))
        self.print("'written to NCD' is not an application ACK; focus the target input box on the device.")
        self.print("Examples: send hello | enter | combo ctrl+c | combo shift+a | combo command+s")

    def on_receiver_stopped(self) -> None:
        self._drain_keyboard_events()


class InstructionPage(ConnectionPage):
    command_help = """
exec PROGRAM [ARGS...]  run a real executable, e.g. exec whoami
win COMMAND             Windows command, e.g. win dir / win echo 12345
unix COMMAND            Unix command, e.g. unix uname -a
shell COMMAND           advanced; requires device allow_shell=true
timeout MILLISECONDS    set the command timeout
info                    show request/response logs
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.timeout_ms = 5000
        self.prompt = "linux->device: "
        self.last_response: dict[str, Any] | None = None

    def _instruction_reader(self, request_id: str, result_queue: Any) -> None:
        try:
            while True:
                response = self.session.read_json()
                if response.get("id") == request_id:
                    result_queue.put({"response": response})
                    return
                append_jsonl(self.run_dir / "unmatched_responses.jsonl", response)
        except Exception as error:
            result_queue.put({"error": str(error)})

    def _read_matching_response(self, request_id: str, timeout: float) -> dict[str, Any]:
        if os.name == "posix" and type(self.session) is DeviceSession:
            # read() on the current character device is blocking.  A forked
            # one-shot reader gives the TUI a real application-layer timeout
            # without changing ncdd, ncd, the driver, or an adapter.
            context = multiprocessing.get_context("fork")
            result_queue = context.Queue(maxsize=1)
            reader = context.Process(
                target=self._instruction_reader,
                args=(request_id, result_queue),
                name=f"ncd-instruction-{request_id[:8]}",
                daemon=True,
            )
            reader.start()
            try:
                try:
                    result = result_queue.get(timeout=timeout)
                except queue_module.Empty as error:
                    raise TimeoutError(
                        "no response before timeout; the device may be disconnected"
                    ) from error
            finally:
                if reader.is_alive():
                    reader.terminate()
                reader.join(timeout=2)
                result_queue.cancel_join_thread()
                result_queue.close()
            if "error" in result:
                raise RuntimeError(result["error"])
            return result["response"]

        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no response before timeout; the device may be disconnected")
            try:
                response = self.session.read_json(timeout=remaining)
            except TimeoutError as error:
                raise TimeoutError(
                    "no response before timeout; the device may be disconnected"
                ) from error
            if response.get("id") == request_id:
                return response
            append_jsonl(self.run_dir / "unmatched_responses.jsonl", response)

    def _run_request(self, request: dict[str, Any]) -> dict[str, Any]:
        request_id = str(uuid.uuid4())
        value = {"id": request_id, "timeout_ms": self.timeout_ms, **request}
        append_jsonl(self.run_dir / "requests.jsonl", value)
        self.print(f"linux->device: {summarize_instruction_request(value)} id={request_id[:8]}")
        self.session.write_json(value)
        response = self._read_matching_response(request_id, self.timeout_ms / 1000 + 1)

        append_jsonl(self.run_dir / "responses.jsonl", response)
        self.last_response = response
        for stream in ("stdout", "stderr"):
            text = response.get(stream) or ""
            if text:
                with (self.run_dir / f"{stream}.log").open("a", encoding="utf-8") as output:
                    output.write(text)
        self.print(
            f"device->linux: ok={response.get('ok')} exit={response.get('returncode')} "
            f"system={response.get('system', 'unknown')} id={request_id[:8]}"
        )
        if response.get("stdout"):
            self.print("device->linux stdout:")
            self.print(response["stdout"], end="" if response["stdout"].endswith("\n") else "\n")
        if response.get("stderr"):
            self.print("device->linux stderr:", file=sys.stderr)
            self.print(response["stderr"], end="" if response["stderr"].endswith("\n") else "\n", file=sys.stderr)
        error_text = str(response.get("stderr") or "").lower()
        if "shell execution is disabled" in error_text:
            self.print("Hint: use 'win ...' or 'unix ...'; 'shell' needs device option allow_shell=true.")
        missing_program = any(
            marker in error_text
            for marker in ("no such file or directory", "winerror 2", "cannot find the file")
        )
        if response.get("returncode") is None and missing_program:
            requested = (value.get("argv") or ["executable"])[0]
            self.print(
                f"Hint: executable {requested!r} was not found. Use 'win dir' for Windows "
                "shell built-ins, or 'unix ...' on Unix."
            )
        return response

    def do_exec(self, line):
        argv = shlex.split(line)
        if not argv:
            self.print("usage: exec PROGRAM [ARGS...]  (example: exec whoami)")
            return
        if argv[0].upper() == "PROGRAM":
            self.print("PROGRAM is a placeholder, not a command. Try: exec whoami")
            return
        try:
            self._run_request({"argv": argv})
        except Exception as error:
            self.print(f"instruction ERROR: {error}", file=sys.stderr)

    def do_run(self, line):
        self.do_exec(line)

    def do_win(self, line):
        command = line[1:] if line.startswith("+") else line
        if not command.strip():
            self.print("usage: win COMMAND  (example: win dir)")
            return
        try:
            self._run_request({"argv": ["cmd.exe", "/d", "/s", "/c", command]})
        except Exception as error:
            self.print(f"instruction ERROR: {error}", file=sys.stderr)

    def do_unix(self, line):
        command = line[1:] if line.startswith("+") else line
        if not command.strip():
            self.print("usage: unix COMMAND  (example: unix uname -a)")
            return
        try:
            self._run_request({"argv": ["/bin/sh", "-lc", command]})
        except Exception as error:
            self.print(f"instruction ERROR: {error}", file=sys.stderr)

    def do_shell(self, line):
        if not line.strip():
            self.print("usage: shell COMMAND...")
            return
        try:
            self._run_request({"shell": True, "command": line})
        except Exception as error:
            self.print(f"instruction ERROR: {error}", file=sys.stderr)

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

    def do_status(self, line):
        super().do_status(line)
        if self.last_response is None:
            self.print(f"timeout={self.timeout_ms}ms last_response=none")
        else:
            self.print(
                f"timeout={self.timeout_ms}ms last_ok={self.last_response.get('ok')} "
                f"last_exit={self.last_response.get('returncode')}"
            )
        if self.session.is_open:
            self.print("Remote liveness is verified only when a matched response arrives.")

    def do_info(self, _line):
        self.print(f"Requests:  {self.run_dir / 'requests.jsonl'}")
        self.print(f"Responses: {self.run_dir / 'responses.jsonl'}")
        self.print(f"stdout:    {self.run_dir / 'stdout.log'}")
        self.print(f"stderr:    {self.run_dir / 'stderr.log'}")
        self.print("Use 'win' for a Windows device and 'unix' for a Unix-like device.")


class FilePage(ReceiverPage):
    command_help = """
read                   display the latest snapshot (truncated when large)
write TEXT             append text and verify the returned snapshot (also: write+TEXT)
save [OUTPUT]          copy the complete latest snapshot
writefile PATH         append a local Linux file
info                   show snapshot details and saved paths
reopen                 request a fresh initial snapshot
"""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.snapshot_count = 0
        self.latest_snapshot: Path | None = None
        self.latest_snapshot_record: dict[str, Any] | None = None
        self.prompt = "file> "

    def receiver_queue_maxsize(self) -> int:
        return 1

    def drop_old_receiver_event_when_full(self) -> bool:
        return True

    def on_open(self) -> None:
        previous = self.snapshot_count
        self.latest_snapshot = None
        super().on_open()
        # The existing adapter always emits its current file once on open.
        # Waiting here removes the race between that initial snapshot and an
        # immediately entered append command.
        try:
            self._wait_snapshot(after=previous, timeout=5)
        except TimeoutError as error:
            raise RuntimeError(
                "no initial file snapshot. The device-side FileAdapter likely did not start. "
                "Set [device.options] file_path for the file device (port 8000); "
                "see test/README.md."
            ) from error

    def receiver_loop(self, stop_event: Any) -> None:
        try:
            while not stop_event.is_set():
                data = self.session.read_frame(stop_event=stop_event)
                received_time = utc_now()
                self.snapshot_count += 1
                digest = hashlib.sha256(data).hexdigest()
                output = self.run_dir / (
                    f"snapshot_{self.snapshot_count:06d}_{utc_filename(received_time)}.bin"
                )
                durable_write(output, data)
                record = {
                    "type": "file_snapshot",
                    "snapshot": self.snapshot_count,
                    "received_at_utc": utc_text(received_time),
                    "saved_at_utc": utc_text(),
                    "bytes": len(data),
                    "sha256": digest,
                    "sample_hex": data[:32].hex(),
                    "output": str(output),
                }
                append_jsonl(self.run_dir / "snapshots.jsonl", record)
                self.emit_receiver_event(record)
                self.print(
                    f"device->linux: file snapshot={self.snapshot_count} bytes={len(data)} "
                    f"sample={data[:16].hex()}..."
                )
                self.print(f"saved: OK time={record['saved_at_utc']} path={output}; use 'read' to view")
        except OperationCancelled:
            pass
        except Exception as error:
            if not stop_event.is_set():
                self.receiver_error = str(error)
                self.emit_receiver_event({"type": "file_receiver_error", "error": str(error)})
                self.print(f"file RECEIVE STOPPED: {error}", file=sys.stderr)

    def _apply_snapshot_event(self, event: dict[str, Any]) -> None:
        if event.get("type") == "file_receiver_error":
            self.receiver_error = str(event.get("error"))
            return
        if event.get("type") != "file_snapshot":
            return
        self.snapshot_count = max(self.snapshot_count, int(event["snapshot"]))
        self.latest_snapshot = Path(event["output"])
        self.latest_snapshot_record = event

    def _drain_snapshots(self) -> None:
        for event in self.drain_receiver_events():
            self._apply_snapshot_event(event)

    def _wait_snapshot(self, *, after: int, timeout: float = 10) -> Path:
        deadline = time.monotonic() + timeout
        while True:
            self._drain_snapshots()
            latest_number = (
                int(self.latest_snapshot_record["snapshot"])
                if self.latest_snapshot_record is not None
                else -1
            )
            if self.latest_snapshot is not None and latest_number > after:
                return self.latest_snapshot
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"no new device file snapshot within {timeout:g} seconds")
            event = self.next_receiver_event(remaining)
            if event is None:
                raise TimeoutError(f"no new device file snapshot within {timeout:g} seconds")
            self._apply_snapshot_event(event)

    def _latest_data(self) -> bytes:
        self._drain_snapshots()
        if self.latest_snapshot is None:
            self._wait_snapshot(after=-1)
        if self.latest_snapshot is None:
            raise RuntimeError("no device file snapshot has been received")
        return self.latest_snapshot.read_bytes()

    def _append(self, payload: bytes) -> None:
        if not payload:
            self.print("file: empty payload was not sent")
            return
        self._drain_snapshots()
        before = self.snapshot_count
        self.session.write_frame(payload)
        self.print(f"linux->device: file bytes={len(payload)} [written to NCD; waiting for snapshot]")
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
            raise RuntimeError("a new snapshot arrived, but its suffix does not match the sent payload")
        self.print(
            f"device->linux: write confirmed by returned snapshot bytes={len(payload)} "
            f"sha256={record['sha256'][:12]}"
        )

    def do_cat(self, _line):
        try:
            data = self._latest_data()
            limit = 4096
            preview = data[:limit].decode("utf-8", errors="replace")
            self.print(f"device->linux: latest file ({len(data)} bytes)")
            self.print(preview, end="")
            if preview and not preview.endswith("\n"):
                self.print()
            if len(data) > limit:
                self.print(
                    f"... truncated {len(data) - limit} bytes; use 'save OUTPUT' for the complete file"
                )
        except Exception as error:
            self.print(f"file: {error}", file=sys.stderr)

    def do_read(self, line):
        self.do_cat(line)

    def do_pull(self, line):
        parts = shlex.split(line)
        try:
            data = self._latest_data()
            output = Path(parts[0]) if parts else self.run_dir / "latest_copy.bin"
            durable_write(output, data)
            self.print(f"file: complete snapshot copy -> {output}")
        except Exception as error:
            self.print(f"file: {error}", file=sys.stderr)

    def do_save(self, line):
        self.do_pull(line)

    def do_append(self, line):
        try:
            self._append(line.encode("utf-8"))
        except Exception as error:
            self.print(f"file: {error}", file=sys.stderr)

    def do_write(self, line):
        text = line[1:] if line.startswith("+") else line
        if not text:
            self.print("usage: write TEXT  or  write+TEXT")
            return
        self.do_append(text)

    def do_appendfile(self, line):
        parts = shlex.split(line)
        if len(parts) != 1:
            self.print("usage: appendfile PATH")
            return
        try:
            self._append(Path(parts[0]).read_bytes())
        except Exception as error:
            self.print(f"file: {error}", file=sys.stderr)

    def do_writefile(self, line):
        self.do_appendfile(line)

    def do_stat(self, _line):
        try:
            data = self._latest_data()
            self.print(f"remote file: bytes={len(data)} sha256={hashlib.sha256(data).hexdigest()}")
        except Exception as error:
            self.print(f"file: {error}", file=sys.stderr)

    def do_info(self, _line):
        self._drain_snapshots()
        if self.latest_snapshot_record is None:
            self.print("Latest snapshot: none")
        else:
            record = self.latest_snapshot_record
            self.print(
                f"Latest snapshot: #{record.get('snapshot')} bytes={record.get('bytes')} "
                f"received_utc={record.get('received_at_utc')} sha256={record.get('sha256')}"
            )
            self.print(f"Saved file:      {record.get('output')}")
        self.print(f"Snapshot history: {self.run_dir / 'snapshots.jsonl'}")
        self.print(f"Write history:    {self.run_dir / 'appends.jsonl'}")
        if self.session.is_open and not self.receiver_error and self.receiver_is_alive():
            self.print("State: OPEN/IDLE. No file change is indistinguishable from a dead peer.")

    def do_status(self, line):
        self._drain_snapshots()
        super().do_status(line)
        self.print(
            f"{self.receiver_status()} snapshots={self.snapshot_count} latest={self.latest_snapshot}"
        )
        if self.session.is_open and not self.receiver_error and self.receiver_is_alive():
            self.print("State: OPEN/IDLE. Use 'reopen' if you need a fresh initial snapshot check.")

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
        self.print("NCD TUI - select one connection")
        self.print("No default connection is opened on the home page.")
        self.print(f"Saved data: {self.run_dir}")
        self.print()
        if not self.connections:
            self.print("No connections were found in the ncdd config or command-line options.")
        else:
            for index, spec in enumerate(self.connections, start=1):
                supported = "" if spec.kind in PAGE_TYPES else " [unsupported type]"
                self.print(
                    f"  [{index}] {spec.name:<20} type={spec.kind:<11} "
                    f"{spec.path} -> {spec.endpoint}{supported}"
                )
        self.print()
        self.print("Enter control+DEVICE, a list number, or open DEVICE.")
        self.print("Examples: control+camera | control+ncd_keyboard | 1")
        self.print("Other commands: connections | logdir | quit")
        self.print()

    def _find_connection(self, selector: str) -> ConnectionSpec:
        selector = selector.strip()
        if not selector:
            raise ValueError("usage: control+DEVICE  or  open INDEX|NAME|TYPE|/dev/PATH")
        if selector.isdigit():
            index = int(selector)
            if 1 <= index <= len(self.connections):
                return self.connections[index - 1]
            raise ValueError(f"connection number out of range: {index}")

        normalized = selector.casefold()

        def aliases(spec: ConnectionSpec) -> set[str]:
            name = spec.name.casefold()
            short_name = name[4:] if name.startswith("ncd_") else name
            return {name, short_name, spec.path.casefold(), spec.kind.casefold()}

        matches = [spec for spec in self.connections if normalized in aliases(spec)]
        if not matches:
            raise ValueError(f"connection not found: {selector}")
        if len(matches) > 1:
            choices = ", ".join(spec.name for spec in matches)
            raise ValueError(f"selector is ambiguous ({choices}); use the list number or exact name")
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
            self.print(
                f"{spec.name} is not recognized as camera/keyboard/instruction/file",
                file=sys.stderr,
            )
            return

        kwargs: dict[str, Any] = {
            "opener": self.opener,
            "print_lock": self.print_lock,
        }
        if page_type is KeyboardPage:
            kwargs["event_display"] = self.keyboard_event_display
        page = page_type(spec, self.run_dir, **kwargs)
        page.run()
        self.print("Back at home. No application-layer connection is open.")
        self.print_homepage()

    def do_control(self, line):
        """Enter a connection page with control+DEVICE."""
        selector = line[1:] if line.startswith("+") else line
        return self.do_open(selector)

    def default(self, line):
        if line.strip().isdigit():
            return self.do_open(line)
        if line.casefold().startswith("control+"):
            return self.do_open(line.split("+", 1)[1])
        self.print(f"Unknown home command: {line!r}. Use control+DEVICE, a number, or help.")

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
    stamp = utc_filename()
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
        print(f"Failed to read ncdd config {config_path}: {error}", file=sys.stderr)
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
