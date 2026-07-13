import os
import struct


DEFAULT_READ_SIZE = 64 * 1024
DEFAULT_WRITE_CHUNK_SIZE = 2048
DEFAULT_MAX_ADAPTER_PAYLOAD = 256 * 1024 * 1024


class NcdDevice:
    """User-space wrapper for ncdd-created `/dev/<name>` character devices."""

    def __init__(
        self,
        path: str,
        read_size: int = DEFAULT_READ_SIZE,
        write_chunk_size: int = DEFAULT_WRITE_CHUNK_SIZE,
    ):
        self.path = path
        self.read_size = read_size
        self.write_chunk_size = write_chunk_size
        self.fd = None

    def __enter__(self):
        self.fd = os.open(self.path, os.O_RDWR)
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()

    def read(self, size: int | None = None) -> bytes:
        if size is None:
            size = self.read_size
        return os.read(self.fd, size)

    def write(self, data: bytes):
        view = memoryview(data)
        while view:
            chunk = view[: self.write_chunk_size]
            written = os.write(self.fd, chunk)
            view = view[written:]

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


class AdapterFrameReader:
    """Reassembles adapter-level `u32be length + payload` streams from /dev."""

    def __init__(
        self,
        device: NcdDevice,
        max_payload_size: int = DEFAULT_MAX_ADAPTER_PAYLOAD,
    ):
        self.device = device
        self.max_payload_size = max_payload_size
        self.buffer = bytearray()

    def read_payload(self) -> bytes:
        while True:
            if len(self.buffer) >= 4:
                payload_len = struct.unpack("!I", self.buffer[:4])[0]
                if payload_len > self.max_payload_size:
                    raise ValueError(f"adapter payload too large: {payload_len} bytes")
                if len(self.buffer) >= 4 + payload_len:
                    payload = bytes(self.buffer[4 : 4 + payload_len])
                    del self.buffer[: 4 + payload_len]
                    return payload

            chunk = self.device.read()
            if not chunk:
                raise EOFError("NCD character device closed")
            self.buffer.extend(chunk)


def pack_adapter_payload(payload: bytes) -> bytes:
    return struct.pack("!I", len(payload)) + payload
