import os

from base import Adapter, Device


class FileAdapter(Adapter):
    """
    A simple adapter that reads from and writes to a file.
    """

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="Unspecified",
                name="File Device",
                description="Map a host file to a device",
            ),
        ]

    def open(self, options: dict[str, str]):
        self.file_path = options.get("file_path")
        if not self.file_path:
            raise ValueError("file_path option is required for FileAdapter")
        if not os.path.exists(self.file_path):
            os.makedirs(os.path.dirname(self.file_path), exist_ok=True)
            self.file = open(self.file_path, "w+b")
        else:
            self.file = open(self.file_path, "r+b")

    def read(self) -> bytes:
        self.file.seek(0)
        # TODO: This will cause dead loop.
        # TODO: This is not thread safe.
        return self.file.read()

    def write(self, data: bytes):
        self.file.seek(0)
        self.file.write(data)
        self.file.flush()

    def close(self):
        self.file.close()
