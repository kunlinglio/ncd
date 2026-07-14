from base import Adapter, Device

import platform
import queue
import shlex
import subprocess
import sys


def _make_cmd(command_str):
    if platform.system() == "Windows":
        return [
            "powershell", "-NoProfile", "-Command",
            f"$input | {command_str}",
        ]
    return shlex.split(command_str)


class InstructionAdapter(Adapter):

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="instruction",
                name="Instruction",
                description="Run a command on stdin, configure via options",
            )
        ]

    def open(self, options: dict[str, str]):
        command_str = options.get("command", "grep ncd")
        self.cmd = _make_cmd(command_str)
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
        if not out:
            out = "(no matches)"
        self.responses.put(out)

    def close(self):
        pass
