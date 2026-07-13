import os
import struct
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

    def read(self) -> bytes:
        while True:
            with self.lock:
                self.file.seek(0)
                data = self.file.read()
                stat = os.fstat(self.file.fileno())
                signature = (stat.st_mtime_ns, stat.st_size)

            if signature != self.last_signature:
                self.last_signature = signature
                return self._pack_payload(data)

            time.sleep(self.poll_interval)

    def write(self, data: bytes):
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]
            frame_len = 4 + payload_len
            if len(self.input_buffer) < frame_len:
                break

            payload = self.input_buffer[4:frame_len]
            self.input_buffer = self.input_buffer[frame_len:]
            self._write_file(payload)

    def _write_file(self, data: bytes):
        with self.lock:
            self.file.seek(0)
            self.file.truncate(0)
            self.file.write(data)
            self.file.flush()
            stat = os.fstat(self.file.fileno())
            self.last_signature = (stat.st_mtime_ns, stat.st_size)

    def close(self):
        file = getattr(self, "file", None)
        if file is not None:
            file.close()
            self.file = None
