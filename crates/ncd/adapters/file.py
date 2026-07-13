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

    def _log(self, message: str):
        print(
            f"[file adapter name={self.device_name!r} id={self.device_identifier!r}] {message}",
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
        self._log(f"open requested options={options}")
        self.file_path = options.get("file_path")
        if not self.file_path:
            raise ValueError("file_path option is required for FileAdapter")

        self.file_path = self._normalize_user_path(self.file_path)
        self.poll_interval = int(options.get("poll_interval_ms") or "200") / 1000
        self.lock = threading.Lock()
        self.last_signature = None
        self.input_buffer = b""

        parent_dir = os.path.dirname(self.file_path)
        if parent_dir:
            os.makedirs(parent_dir, exist_ok=True)
        self.file = open(self.file_path, "a+b")
        self._log(f"opened path={self.file_path} poll_interval={self.poll_interval}s")

    def read(self) -> bytes:
        while True:
            with self.lock:
                self.file.seek(0)
                data = self.file.read()
                stat = os.fstat(self.file.fileno())
                signature = (stat.st_mtime_ns, stat.st_size)

            if signature != self.last_signature:
                self.last_signature = signature
                self._log(f"[actual->linux] read snapshot bytes={len(data)} signature={signature}")
                return self._pack_payload(data)

            time.sleep(self.poll_interval)

    def write(self, data: bytes):
        self._log(f"[linux->actual] write bytes={len(data)}")
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]
            frame_len = 4 + payload_len
            if len(self.input_buffer) < frame_len:
                break

            payload = self.input_buffer[4:frame_len]
            self.input_buffer = self.input_buffer[frame_len:]
            self._append_file(payload)

    def _append_file(self, data: bytes):
        with self.lock:
            self.file.seek(0, os.SEEK_END)
            self.file.write(data)
            self.file.flush()
            stat = os.fstat(self.file.fileno())
            signature = (stat.st_mtime_ns, stat.st_size)
            self._log(f"[linux->actual] file appended bytes={len(data)} signature={signature}")

    def close(self):
        file = getattr(self, "file", None)
        if file is not None:
            self._log("close")
            file.close()
            self.file = None
