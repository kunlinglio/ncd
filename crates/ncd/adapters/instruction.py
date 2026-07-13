from base import Adapter, Device

import json
import locale
import platform
import queue
import struct
import subprocess
import sys


class InstructionAdapter(Adapter):
    def _log(self, direction: str, message: str = ""):
        suffix = f" {message}" if message else ""
        print(
            f"[{self.device_name}:{getattr(self, 'port', '?')} {direction}]{suffix}",
            file=sys.stderr,
            flush=True,
        )

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
        self.port = options.get("port", "?")
        self.timeout_ms = int(options.get("timeout_ms") or "5000")
        self.cwd = options.get("cwd") or None
        self.allow_shell = options.get("allow_shell", "false").lower() == "true"
        self.encoding = options.get("encoding") or locale.getpreferredencoding(False)
        self.responses = queue.Queue()
        self.input_buffer = b""
        self._log("connect", "open")

    def read(self) -> bytes:
        response = self.responses.get()
        payload = json.dumps(response).encode("utf-8")
        self._log(
            "device->linux",
            f"returncode={response.get('returncode')} bytes={len(payload)}",
        )
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        self._log("linux->device", f"bytes={len(data)}")
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]

            if len(self.input_buffer) < 4 + payload_len:
                break

            payload = self.input_buffer[4:4 + payload_len]
            self.input_buffer = self.input_buffer[4 + payload_len:]

            request = json.loads(payload.decode("utf-8"))
            request_id = request.get("id")
            self._log("linux->device", self._summarize_request(request))

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

            self._log(
                "device->linux",
                f"finished returncode={response.get('returncode')} ok={response.get('ok')}",
            )
            self.responses.put(response)

    def close(self):
        self._log("close")
        return

    def _summarize_request(self, request: dict) -> str:
        if request.get("shell"):
            command = request.get("command", "")
            if len(command) > 40:
                command = command[:40] + "..."
            return f"shell {command!r}"

        argv = request.get("argv") or []
        if argv:
            return f"argv {argv[0]!r} argc={len(argv)}"
        return "request"
