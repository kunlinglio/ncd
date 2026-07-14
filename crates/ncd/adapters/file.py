import hashlib
import os
import struct
import sys
import threading
import time

from base import Adapter, Device


class FileAdapter(Adapter):
    """
    A simple adapter that reads from and writes to a file.
    """

    DEFAULT_FILE_PATH = "~/ncd-share.bin"
    MAX_FRAME_SIZE = 64 * 1024 * 1024
    MIN_POLL_INTERVAL_MS = 10
    MAX_POLL_INTERVAL_MS = 60_000

    @staticmethod
    def _normalize_user_path(path: str) -> str:
        path = path.strip()
        if len(path) >= 2 and path[0] == path[-1] and path[0] in ("'", '"'):
            path = path[1:-1]
        path = os.path.expandvars(os.path.expanduser(path))
        return os.path.abspath(path)

    @staticmethod
    def _pack_payload(payload: bytes) -> bytes:
        return struct.pack("!I", len(payload)) + payload

    def _log(self, direction: str, message: str = ""):
        suffix = f" {message}" if message else ""
        print(
            f"[{self.device_name}:{getattr(self, 'port', '?')} {direction}]{suffix}",
            file=sys.stderr,
            flush=True,
        )

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="Unspecified",
                name="File Device",
                description="Map the file_path option to a framed file device",
            ),
        ]

    def open(self, options: dict[str, str]):
        self.port = options.get("port", "?")
        self._log("connect", "open")
        self.file_path = options.get("file_path") or self.DEFAULT_FILE_PATH
        if not options.get("file_path"):
            self._log(
                "connect",
                f"file_path was empty; using default {self.DEFAULT_FILE_PATH}",
            )

        self.file_path = self._normalize_user_path(self.file_path)
        poll_interval_ms = int(options.get("poll_interval_ms") or "200")
        if not self.MIN_POLL_INTERVAL_MS <= poll_interval_ms <= self.MAX_POLL_INTERVAL_MS:
            raise ValueError(
                f"poll_interval_ms must be between {self.MIN_POLL_INTERVAL_MS} "
                f"and {self.MAX_POLL_INTERVAL_MS}"
            )
        self.poll_interval = poll_interval_ms / 1000
        self.lock = threading.Lock()
        self.last_signature = None
        self.input_buffer = bytearray()
        self.opened = False
        self.next_poll_at = 0.0

        parent_dir = os.path.dirname(self.file_path)
        if parent_dir:
            os.makedirs(parent_dir, exist_ok=True)
        # Keep no long-lived handle. Editors commonly save by replacing the
        # pathname atomically; reopening for each operation observes the new
        # file and also avoids preventing replacement on Windows.
        with open(self.file_path, "a+b"):
            pass
        self.opened = True
        self._log("connect", "opened")

    def read(self) -> bytes:
        while True:
            delay = self.next_poll_at - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            with self.lock:
                with open(self.file_path, "rb") as file:
                    data = file.read(self.MAX_FRAME_SIZE + 1)
                    stat = os.fstat(file.fileno())
                self.next_poll_at = time.monotonic() + self.poll_interval
                if len(data) > self.MAX_FRAME_SIZE:
                    raise ValueError(
                        f"mapped file exceeds {self.MAX_FRAME_SIZE} byte limit"
                    )
                signature = (
                    stat.st_dev,
                    stat.st_ino,
                    stat.st_mtime_ns,
                    stat.st_size,
                    hashlib.blake2b(data, digest_size=16).digest(),
                )

            if signature != self.last_signature:
                self.last_signature = signature
                self._log("device->linux", f"snapshot={len(data)} bytes")
                return self._pack_payload(data)

            # next_poll_at throttles the next scan, including immediately after
            # returning a changed snapshot to the adapter bridge.

    def write(self, data: bytes):
        self._log("linux->device", f"bytes={len(data)}")
        self.input_buffer.extend(data)

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]
            if payload_len > self.MAX_FRAME_SIZE:
                raise ValueError(
                    f"file payload length {payload_len} exceeds "
                    f"{self.MAX_FRAME_SIZE} byte limit"
                )
            frame_len = 4 + payload_len
            if len(self.input_buffer) < frame_len:
                break

            payload = bytes(self.input_buffer[4:frame_len])
            del self.input_buffer[:frame_len]
            self._append_file(payload)

    def _append_file(self, data: bytes):
        with self.lock:
            with open(self.file_path, "ab") as file:
                file.write(data)
                file.flush()
                os.fsync(file.fileno())
            self._log("linux->device", f"append={len(data)} bytes")

    def close(self):
        if getattr(self, "opened", False):
            self._log("close")
            self.opened = False
