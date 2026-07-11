from base import Adapter, Device


class DummyAdapter(Adapter):
    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(identifier="dummy1", name="Dummy Device *", description=""),
            Device(identifier="dummy2", name="Dummy Device -", description=""),
        ]

    def open(self, options: dict[str, str]):
        pass

    def read(self) -> bytes:
        char = b"*" if self.device_identifier == "dummy1" else b"-"
        return char * 100

    def write(self, data: bytes):
        pass

    def close(self):
        pass
