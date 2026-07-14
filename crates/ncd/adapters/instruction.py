from base import Adapter, Device

import codecs
import json
import locale
import platform
import queue
import struct
import subprocess
import sys
import tempfile


class InstructionAdapter(Adapter):
    MAX_REQUEST_FRAME_SIZE = 1024 * 1024
    MAX_STDIN_BYTES = 1024 * 1024
    MAX_OUTPUT_BYTES = 4 * 1024 * 1024
    MAX_TIMEOUT_MS = 60 * 60 * 1000
    MAX_ARG_COUNT = 256
    MAX_ARG_CHARS = 64 * 1024

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
        if not 1 <= self.timeout_ms <= self.MAX_TIMEOUT_MS:
            raise ValueError(
                f"timeout_ms must be between 1 and {self.MAX_TIMEOUT_MS}"
            )
        self.cwd = options.get("cwd") or None
        allow_shell = options.get("allow_shell", "false").lower()
        if allow_shell not in ("true", "false"):
            raise ValueError("allow_shell must be true or false")
        self.allow_shell = allow_shell == "true"
        self.encoding = options.get("encoding") or locale.getpreferredencoding(False)
        codecs.lookup(self.encoding)
        self.responses = queue.Queue(maxsize=16)
        self.input_buffer = bytearray()
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
        self.input_buffer.extend(data)

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]
            if payload_len > self.MAX_REQUEST_FRAME_SIZE:
                raise ValueError(
                    f"instruction request length {payload_len} exceeds "
                    f"{self.MAX_REQUEST_FRAME_SIZE} byte limit"
                )

            if len(self.input_buffer) < 4 + payload_len:
                break

            payload = bytes(self.input_buffer[4:4 + payload_len])
            del self.input_buffer[:4 + payload_len]
            request_id = None

            try:
                request = json.loads(payload.decode("utf-8"))
                if not isinstance(request, dict):
                    raise ValueError("instruction request must be a JSON object")
                request_id = request.get("id")
                self._log("linux->device", self._summarize_request(request))
                response = self._execute_request(request, request_id)
            except Exception as error:
                response = {
                    "id": request_id,
                    "ok": False,
                    "returncode": None,
                    "stdout": "",
                    "stderr": str(error),
                    "system": platform.system(),
                    "timed_out": False,
                    "stdout_truncated": False,
                    "stderr_truncated": False,
                }

            self._log(
                "device->linux",
                f"finished returncode={response.get('returncode')} ok={response.get('ok')}",
            )
            self.responses.put(response)

    def close(self):
        self._log("close")
        return

    def _execute_request(self, request: dict, request_id):
        shell, command, stdin, cwd, timeout_ms = self._validate_request(request)
        timed_out = False

        # Store child output in temporary files so an unexpectedly verbose
        # command cannot exhaust adapter memory before the response limit is
        # applied.
        with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
            process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=stdout_file,
                stderr=stderr_file,
                shell=shell,
                cwd=cwd,
            )
            try:
                process.communicate(input=stdin, timeout=timeout_ms / 1000)
            except subprocess.TimeoutExpired:
                timed_out = True
                process.kill()
                process.communicate()

            stdout, stdout_truncated = self._read_output(stdout_file)
            stderr, stderr_truncated = self._read_output(stderr_file)

        if timed_out:
            timeout_message = f"command timed out after {timeout_ms} ms"
            stderr = f"{stderr.rstrip()}\n{timeout_message}".lstrip()

        return {
            "id": request_id,
            "ok": process.returncode == 0 and not timed_out,
            "returncode": process.returncode,
            "stdout": stdout,
            "stderr": stderr,
            "system": platform.system(),
            "timed_out": timed_out,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
        }

    def _validate_request(self, request: dict):
        shell = request.get("shell", False)
        if not isinstance(shell, bool):
            raise ValueError("shell must be a boolean")
        if shell and not self.allow_shell:
            raise RuntimeError("shell execution is disabled")

        raw_timeout = request.get("timeout_ms", self.timeout_ms)
        if isinstance(raw_timeout, bool) or not isinstance(raw_timeout, int):
            raise ValueError("timeout_ms must be an integer")
        if not 1 <= raw_timeout <= self.MAX_TIMEOUT_MS:
            raise ValueError(
                f"timeout_ms must be between 1 and {self.MAX_TIMEOUT_MS}"
            )

        cwd = request.get("cwd", self.cwd)
        if cwd == "":
            cwd = None
        if cwd is not None and not isinstance(cwd, str):
            raise ValueError("cwd must be a string")

        stdin_text = request.get("stdin", "")
        if not isinstance(stdin_text, str):
            raise ValueError("stdin must be a string")
        stdin = stdin_text.encode(self.encoding)
        if len(stdin) > self.MAX_STDIN_BYTES:
            raise ValueError(
                f"stdin exceeds {self.MAX_STDIN_BYTES} byte limit"
            )

        if shell:
            command = request.get("command")
            if not isinstance(command, str) or not command.strip():
                raise ValueError("command must be a non-empty string")
            if len(command) > self.MAX_ARG_CHARS:
                raise ValueError("command is too long")
        else:
            command = request.get("argv")
            if not isinstance(command, list) or not command:
                raise ValueError("argv must be a non-empty string list")
            if len(command) > self.MAX_ARG_COUNT:
                raise ValueError(f"argv cannot exceed {self.MAX_ARG_COUNT} entries")
            if any(not isinstance(item, str) for item in command):
                raise ValueError("argv must contain only strings")
            if not command[0]:
                raise ValueError("argv[0] must name a program")
            if any(len(item) > self.MAX_ARG_CHARS for item in command):
                raise ValueError("an argv entry is too long")

        return shell, command, stdin, cwd, raw_timeout

    def _read_output(self, stream):
        stream.flush()
        stream.seek(0)
        data = stream.read(self.MAX_OUTPUT_BYTES + 1)
        truncated = len(data) > self.MAX_OUTPUT_BYTES
        text = data[:self.MAX_OUTPUT_BYTES].decode(self.encoding, errors="replace")
        if truncated:
            text += f"\n[output truncated at {self.MAX_OUTPUT_BYTES} bytes]\n"
        return text, truncated

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
