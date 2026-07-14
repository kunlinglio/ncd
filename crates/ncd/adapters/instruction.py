from base import Adapter, Device

import platform
import queue
import subprocess
import sys


def _grep_cmd():
    if platform.system() == "Windows":
        return [
            "powershell", "-NoProfile", "-Command",
            "$input | Select-String ncd | ForEach-Object { $_.Line }",
        ]
    return ["grep", "ncd"]


class InstructionAdapter(Adapter):

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="grep_ncd",
                name=f"{platform.system()} Grep NCD",
                description="Filter lines containing ncd",
            )
        ]

    def open(self, options: dict[str, str]):
        self.cmd = _grep_cmd()
        self.responses = queue.Queue()

    def read(self) -> bytes:
        return self.responses.get().encode("utf-8")

    def write(self, data: bytes):
        result = subprocess.run(
            self.cmd,
            input=data,
            capture_output=True,
            timeout=30,
        )
        out = result.stdout.decode("utf-8", errors="replace").replace("\r\n", "\n").strip()
        self.responses.put(out)

    def close(self):
        pass
