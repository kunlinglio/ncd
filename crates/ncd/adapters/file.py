import os
import struct
import sys
import threading
import time

from base import Adapter, Device


class FileAdapter(Adapter):

    DEFAULT_FILE_PATH = "~/ncd-share.bin"

    @staticmethod
    def _normalize_path(path: str) -> str:
        path = path.strip()
        if len(path) >= 2 and path[0] == path[-1] and path[0] in ("'", '"'):
            path = path[1:-1]
        return os.path.abspath(os.path.expandvars(os.path.expanduser(path)))

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="file_device",
                name="File Device",
                description="Cursor-based read/write (overwrite mode)",
            )
        ]

    def open(self, options: dict[str, str]):
        path = options.get("file_path") or self.DEFAULT_FILE_PATH
        self.file_path = self._normalize_path(path)
        parent = os.path.dirname(self.file_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        self.file = open(self.file_path, "r+b")
        self.cursor = 0
        self.lock = threading.Lock()

    def read(self) -> bytes:
        with self.lock:
            self.file.seek(self.cursor)
            data = self.file.read(4096)
            if data:
                self.cursor += len(data)
                return data
        time.sleep(0.1)
        return b""

    def write(self, data: bytes):
        with self.lock:
            self.file.seek(self.cursor)
            self.file.write(data)
            self.file.flush()
            self.cursor += len(data)

    def close(self):
        f = getattr(self, "file", None)
        if f:
            f.close()
            self.file = None
