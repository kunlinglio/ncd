from base import Adapter, Device

import json
import locale
import platform
import queue
import struct
import subprocess
import sys


class InstructionAdapter(Adapter):
    def _log(self, message: str):
        print(
            f"[instruction adapter name={self.device_name!r} id={self.device_identifier!r}] {message}",
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
        self.timeout_ms = int(options.get("timeout_ms") or "5000")
        self.cwd = options.get("cwd") or None
        self.allow_shell = options.get("allow_shell", "false").lower() == "true"
        self.encoding = options.get("encoding") or locale.getpreferredencoding(False)
        self.responses = queue.Queue()
        self.input_buffer = b""
        self._log(
            f"open timeout_ms={self.timeout_ms} cwd={self.cwd} "
            f"allow_shell={self.allow_shell} encoding={self.encoding}"
        )

    def read(self) -> bytes:
        response = self.responses.get()
        payload = json.dumps(response).encode("utf-8")
        self._log(
            f"[actual->linux] read response id={response.get('id')} "
            f"returncode={response.get('returncode')} payload={len(payload)} bytes"
        )
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        self._log(f"[linux->actual] write bytes={len(data)}")
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]

            if len(self.input_buffer) < 4 + payload_len:
                break

            payload = self.input_buffer[4:4 + payload_len]
            self.input_buffer = self.input_buffer[4 + payload_len:]

            request = json.loads(payload.decode("utf-8"))
            request_id = request.get("id")
            self._log(f"[linux->actual] execute request id={request_id} request={request}")

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
                f"request finished id={request_id} "
                f"returncode={response.get('returncode')} ok={response.get('ok')}"
            )
            self.responses.put(response)

    def close(self):
        self._log("close")
        return
