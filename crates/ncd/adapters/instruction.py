from base import Adapter, Device

import json
import locale
import platform
import queue
import struct
import subprocess


class InstructionAdapter(Adapter):
    @classmethod
    def list_devices(cls) -> list[Device]:
        system = platform.system()
        return [
            Device(
                identifier="system_instruction",
                name=f"{system} Command Executor",
                description=f"Run commands on {system}",
            )
        ]

    def open(self, options: dict[str, str]):
        self.timeout_ms = int(options.get("timeout_ms") or "5000")
        self.cwd = options.get("cwd") or None
        self.allow_shell = options.get("allow_shell", "false").lower() == "true"
        self.encoding = options.get("encoding") or locale.getpreferredencoding(False)

    def read(self) -> bytes:
        response = self.responses.get()
        payload = json.dumps(response).encode("utf-8")
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]

            if len(self.input_buffer) < 4 + payload_len:
                break

            payload = self.input_buffer[4:4 + payload_len]
            self.input_buffer = self.input_buffer[4 + payload_len:]

            request = json.loads(payload.decode("utf-8"))
            request_id = request.get("id")

            try:
                shell = bool(request.get("shell", False))
                if shell and not self.allow_shell:
                    raise RuntimeError("shell execution is disabled")

                timeout = (request.get("timeout_ms") or self.timeout_ms) / 1000
                cwd = request.get("cwd") or self.cwd

                if shell:
                    command = request["command"]
                else:
                    command = request["argv"]

                result = subprocess.run(
                    command,
                    input=(request.get("stdin") or "").encode(self.encoding),
                    capture_output=True,
                    shell=shell,
                    cwd=cwd,
                    timeout=timeout,
                )

                response = {
                    "id": request_id,
                    "ok": result.returncode == 0,
                    "returncode": result.returncode,
                    "stdout": result.stdout.decode(self.encoding, errors="replace"),
                    "stderr": result.stderr.decode(self.encoding, errors="replace"),
                    "system": platform.system(),
                }

            except Exception as error:
                response = {
                    "id": request_id,
                    "ok": False,
                    "returncode": None,
                    "stdout": "",
                    "stderr": str(error),
                    "system": platform.system(),
                }

            self.responses.put(response)

    def close(self):
        return