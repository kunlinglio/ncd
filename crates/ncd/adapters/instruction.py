from base import Adapter, Device

import platform
import queue
import struct
import subprocess
import sys


def _grep_command():
    system = platform.system()
    if system == "Windows":
        return ["powershell", "-Command", "$input | Select-String ncd"]
    else:
        return ["grep", "ncd"]


class InstructionAdapter(Adapter):

    @classmethod
    def list_devices(cls) -> list[Device]:
        system = platform.system()
        return [
            Device(
                identifier="grep_ncd",
                name=f"{system} Grep NCD",
                description="Filter lines containing ncd from stdin",
            )
        ]

    def open(self, options: dict[str, str]):
        self.command = _grep_command()
        self.responses = queue.Queue()

    def read(self) -> bytes:
        payload = self.responses.get().encode("utf-8")
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        result = subprocess.run(
            self.command,
            input=data,
            capture_output=True,
            timeout=30,
        )
        output = result.stdout.decode("utf-8", errors="replace").replace("\r\n", "\n")
        self.responses.put(output)

    def close(self):
        pass
