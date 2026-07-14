import hashlib
import json
import os
import queue
import struct
import subprocess
import sys
import tempfile
import threading
import types
import unittest
import uuid
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
from unittest.mock import patch


TEST_DIR = Path(__file__).resolve().parent
ROOT = TEST_DIR.parent
sys.path.insert(0, str(TEST_DIR))

import ncd_tui  # noqa: E402


class QueueSession:
    """In-memory full-duplex session used to exercise page-level protocols on Windows."""

    def __init__(self):
        self.opened = False
        self.incoming = queue.Queue()
        self.outgoing = queue.Queue()

    @property
    def is_open(self):
        return self.opened

    def open(self):
        if self.opened:
            return False
        self.opened = True
        return True

    def close(self):
        if not self.opened:
            return False
        self.opened = False
        return True

    def read_frame(self, *, stop_event=None, timeout=None, progress=None):
        deadline = None if timeout is None else ncd_tui.time.monotonic() + timeout
        while True:
            if stop_event is not None and stop_event.is_set():
                raise ncd_tui.OperationCancelled()
            if deadline is not None and ncd_tui.time.monotonic() >= deadline:
                raise TimeoutError
            try:
                value = self.incoming.get(timeout=0.02)
                if progress is not None and isinstance(value, bytes):
                    progress(len(value), len(value))
                return value
            except queue.Empty:
                pass

    def read_json(self, *, stop_event=None, timeout=None):
        value = self.read_frame(stop_event=stop_event, timeout=timeout)
        if isinstance(value, dict):
            return value
        return json.loads(value.decode("utf-8"))

    def write_frame(self, payload):
        if not self.opened:
            raise RuntimeError("closed")
        self.outgoing.put(payload)

    def write_json(self, value):
        if not self.opened:
            raise RuntimeError("closed")
        self.outgoing.put(value)


def make_spec(kind: str) -> ncd_tui.ConnectionSpec:
    return ncd_tui.ConnectionSpec(
        name=f"test_{kind}",
        kind=kind,
        path=f"/dev/test_{kind}",
        remote_ip="127.0.0.1",
        remote_port=ncd_tui.DEFAULT_PORT_TYPES and next(
            port for port, value in ncd_tui.DEFAULT_PORT_TYPES.items() if value == kind
        ),
    )


class TuiProtocolTests(unittest.TestCase):
    def test_read_frame_reports_complete_header_before_payload_progress(self):
        read_fd, write_fd = os.pipe()
        payload = b"framed-camera-payload"
        progress = []
        try:
            os.write(write_fd, struct.pack("!I", len(payload)) + payload)
            result = ncd_tui.read_frame(
                read_fd,
                progress=lambda received, total: progress.append((received, total)),
            )
        finally:
            os.close(read_fd)
            os.close(write_fd)
        self.assertEqual(result, payload)
        self.assertEqual(progress[0], (0, len(payload)))
        self.assertEqual(progress[-1], (len(payload), len(payload)))

    def test_config_preserves_all_connections_and_has_no_defaults(self):
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.toml"
            self.assertEqual(ncd_tui.load_connections(config), [])
            config.write_text(
                """
[[device]]
name = "front_camera"
remote_ip = "10.0.0.2"
remote_port = 9000

[[device]]
name = "rear_camera"
remote_ip = "10.0.0.3"
remote_port = 9000

[[device]]
name = "keys"
remote_ip = "10.0.0.4"
remote_port = 10000
""",
                encoding="utf-8",
            )
            connections = ncd_tui.load_connections(config)
            self.assertEqual([item.name for item in connections], ["front_camera", "rear_camera", "keys"])
            self.assertEqual([item.kind for item in connections], ["camera", "camera", "keyboard"])

    def test_primary_page_commands_are_distinct_and_legacy_aliases_remain(self):
        self.assertIn("wait [OUTPUT.jpg]", ncd_tui.CameraPage.command_help)
        self.assertIn("files", ncd_tui.CameraPage.command_help)
        self.assertNotIn("capture [", ncd_tui.CameraPage.command_help)
        self.assertTrue(callable(ncd_tui.CameraPage.do_capture))
        self.assertTrue(callable(ncd_tui.CameraPage.do_path))

        keyboard_help = ncd_tui.KeyboardPage.command_help
        for command in ("send TEXT", "key NAME", "combo KEYS", "show text|events|off", "status", "info"):
            self.assertIn(command, keyboard_help)
        self.assertNotIn("listen text", keyboard_help)
        self.assertTrue(callable(ncd_tui.KeyboardPage.do_listen))

        instruction_help = ncd_tui.InstructionPage.command_help
        self.assertIn("detect", instruction_help)
        self.assertIn("logs", instruction_help)
        self.assertTrue(callable(ncd_tui.InstructionPage.do_terminal))
        self.assertTrue(callable(ncd_tui.InstructionPage.do_info))

        file_help = ncd_tui.FilePage.command_help
        for command in ("show", "append TEXT", "push PATH", "save [OUTPUT]"):
            self.assertIn(command, file_help)
        self.assertNotIn("write TEXT", file_help)
        self.assertTrue(callable(ncd_tui.FilePage.do_read))
        self.assertTrue(callable(ncd_tui.FilePage.do_write))

    def test_camera_receiver_verifies_and_durably_saves_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.CameraPage(make_spec("camera"), Path(directory))
            session = QueueSession()
            page.session = session
            jpeg = b"\xff\xd8test-camera-frame\xff\xd9"
            session.incoming.put(jpeg)
            self.assertTrue(page.open_connection())
            with page.frame_condition:
                received = page.frame_condition.wait_for(lambda: page.frame_count == 1, timeout=2)
            self.assertTrue(received)
            self.assertEqual(page.latest_bytes, jpeg)
            self.assertEqual((page.run_dir / "latest.jpg").read_bytes(), jpeg)
            records = [
                json.loads(line)
                for line in (page.run_dir / "frames.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            self.assertTrue(records[-1]["saved_at_utc"].endswith("Z"))
            self.assertIn("interval_ms", records[-1])
            with self.assertRaises(queue.Empty):
                session.outgoing.get_nowait()
            page.close_connection()

    def test_camera_receiver_saves_every_large_frame_continuously(self):
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.CameraPage(make_spec("camera"), Path(directory))
            session = QueueSession()
            page.session = session
            frames = [
                b"\xff\xd8" + bytes([index]) * (128 * 1024) + b"\xff\xd9"
                for index in range(1, 5)
            ]
            for frame in frames:
                session.incoming.put(frame)
            self.assertTrue(page.open_connection())
            with page.frame_condition:
                received = page.frame_condition.wait_for(lambda: page.frame_count == 4, timeout=5)
            self.assertTrue(received)
            self.assertEqual((page.run_dir / "latest.jpg").read_bytes(), frames[-1])
            records = [
                json.loads(line)
                for line in (page.run_dir / "frames.jsonl").read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(len([record for record in records if record.get("ok")]), 4)
            self.assertEqual(len(list(page.run_dir.glob("frame_*.jpg"))), 4)
            self.assertEqual(page.transport_frame_count, 4)
            self.assertEqual(
                [record["transport_frame"] for record in records if record.get("ok")],
                [1, 2, 3, 4],
            )
            messages = []
            page.print = lambda message="", **_kwargs: messages.append(message)
            page.do_status("")
            self.assertTrue(any("transport_frames=4" in message for message in messages))
            page.close_connection()

    def test_keyboard_sends_commands_and_receives_existing_adapter_events(self):
        session = QueueSession()
        session.incoming.put({"event": "press", "key_type": "char", "key": "x"})
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.KeyboardPage(make_spec("keyboard"), Path(directory))
            page.session = session
            messages = []
            page.print = lambda message="", **_kwargs: messages.append(message)
            self.assertTrue(page.open_connection())
            before = session.outgoing.qsize()
            page.do_status("")
            page.do_info("")
            self.assertEqual(session.outgoing.qsize(), before)
            self.assertTrue(any("no remote command" in message for message in messages))
            page.do_send("+hello")
            command = session.outgoing.get(timeout=1)
            self.assertEqual(command["action"], "type")
            self.assertEqual(command["text"], "hello")
            deadline = ncd_tui.time.monotonic() + 1
            event_log = page.run_dir / "events.jsonl"
            while not event_log.exists() and ncd_tui.time.monotonic() < deadline:
                ncd_tui.time.sleep(0.01)
            self.assertTrue(event_log.exists())
            self.assertIn('"key": "x"', event_log.read_text(encoding="utf-8"))
            while not any("device->linux:" in message for message in messages) and ncd_tui.time.monotonic() < deadline:
                ncd_tui.time.sleep(0.01)
            self.assertTrue(any("device->linux: 'x'" in message for message in messages))
            page._drain_keyboard_events()
            self.assertEqual(page.keyboard_event_count, 1)

            page.do_combo("ctrl+shift+a")
            combo = [session.outgoing.get(timeout=1) for _ in range(5)]
            self.assertEqual(
                [(item["action"], item["key"]) for item in combo],
                [
                    ("press", "ctrl"),
                    ("press", "shift"),
                    ("tap", "a"),
                    ("release", "shift"),
                    ("release", "ctrl"),
                ],
            )

            before = session.outgoing.qsize()
            page._key_action("press", "key")
            self.assertEqual(session.outgoing.qsize(), before)
            self.assertTrue(any("not one character" in message for message in messages))
            page.close_connection()

    def test_home_accepts_control_plus_kind_and_short_name(self):
        connections = [
            ncd_tui.ConnectionSpec("ncd_camera", "camera", "/dev/ncd_camera"),
            ncd_tui.ConnectionSpec("ncd_keyboard", "keyboard", "/dev/ncd_keyboard"),
        ]
        with tempfile.TemporaryDirectory() as directory:
            tui = ncd_tui.NcdTui(connections, Path(directory))
            self.assertEqual(tui._find_connection("camera").name, "ncd_camera")
            self.assertEqual(tui._find_connection("ncd_keyboard").kind, "keyboard")
            self.assertEqual(tui._find_connection("keyboard").path, "/dev/ncd_keyboard")
            self.assertEqual(tui.parseline("control+ncd_keyboard")[:2], ("control", "+ncd_keyboard"))

    def test_instruction_request_matches_response_id(self):
        session = QueueSession()

        def device():
            request = session.outgoing.get(timeout=2)
            session.incoming.put(
                {
                    "id": request["id"],
                    "ok": True,
                    "returncode": 0,
                    "stdout": "device-ok\n",
                    "stderr": "",
                }
            )

        worker = threading.Thread(target=device, daemon=True)
        worker.start()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.InstructionPage(make_spec("instruction"), Path(directory))
            page.session = session
            self.assertTrue(page.open_connection())
            response = page._run_request({"argv": ["test-command"]})
            self.assertEqual(response["stdout"], "device-ok\n")
            page.close_connection()
        worker.join(timeout=1)

    def test_instruction_win_uses_argv_without_shell_permission(self):
        session = QueueSession()
        captured = []

        def device():
            request = session.outgoing.get(timeout=2)
            captured.append(request)
            session.incoming.put(
                {
                    "id": request["id"],
                    "ok": True,
                    "returncode": 0,
                    "stdout": "ok\n",
                    "stderr": "",
                    "system": "Windows",
                }
            )

        worker = threading.Thread(target=device, daemon=True)
        worker.start()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.InstructionPage(make_spec("instruction"), Path(directory))
            page.session = session
            self.assertTrue(page.open_connection())
            page.do_win("echo 12345")
            self.assertEqual(
                captured[0]["argv"], ["cmd.exe", "/d", "/s", "/c", "echo 12345"]
            )
            self.assertNotIn("shell", captured[0])
            page.close_connection()
        worker.join(timeout=1)

    def test_instruction_detects_powershell_and_returns_terminal_output(self):
        session = QueueSession()
        captured = []

        def device():
            probe = session.outgoing.get(timeout=2)
            captured.append(probe)
            session.incoming.put(
                {
                    "id": probe["id"],
                    "ok": True,
                    "returncode": 0,
                    "stdout": "5.1\n",
                    "stderr": "",
                    "system": "Windows",
                }
            )
            command = session.outgoing.get(timeout=2)
            captured.append(command)
            session.incoming.put(
                {
                    "id": command["id"],
                    "ok": True,
                    "returncode": 0,
                    "stdout": "terminal-output\n",
                    "stderr": "",
                    "system": "Windows",
                }
            )

        worker = threading.Thread(target=device, daemon=True)
        worker.start()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.InstructionPage(make_spec("instruction"), Path(directory))
            page.session = session
            messages = []
            page.print = lambda message="", **_kwargs: messages.append(message)
            session.open()
            self.assertEqual(page.detect_terminal(), "powershell")
            page.do_run("Get-ChildItem")
            self.assertEqual(captured[1]["argv"][-1], "Get-ChildItem")
            self.assertTrue(any("terminal-output" in message for message in messages))
            session.close()
        worker.join(timeout=1)

    def test_instruction_detects_bash_and_uses_bash_command_syntax(self):
        session = QueueSession()
        captured = []

        def respond(request, **values):
            session.incoming.put(
                {
                    "id": request["id"],
                    "ok": values.get("ok", True),
                    "returncode": values.get("returncode", 0),
                    "stdout": values.get("stdout", ""),
                    "stderr": values.get("stderr", ""),
                    "system": "Linux",
                }
            )

        def device():
            powershell_probe = session.outgoing.get(timeout=2)
            respond(powershell_probe, ok=False, returncode=None, stderr="not found")
            shell_probe = session.outgoing.get(timeout=2)
            respond(shell_probe, stdout="/bin/bash\n")
            command = session.outgoing.get(timeout=2)
            captured.append(command)
            respond(command, stdout="bash-output\n")

        worker = threading.Thread(target=device, daemon=True)
        worker.start()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.InstructionPage(make_spec("instruction"), Path(directory))
            page.session = session
            session.open()
            self.assertEqual(page.detect_terminal(), "bash")
            page.do_run("printf hello")
            self.assertEqual(captured[0]["argv"], ["/bin/bash", "-lc", "printf hello"])
            session.close()
        worker.join(timeout=1)

    def test_instruction_reader_has_real_application_timeout(self):
        session = QueueSession()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.InstructionPage(make_spec("instruction"), Path(directory))
            page.session = session
            session.open()
            with self.assertRaisesRegex(TimeoutError, "may be disconnected"):
                page._read_matching_response("missing", 0.05)
            session.close()

    def test_file_append_is_confirmed_by_existing_adapter_snapshot(self):
        session = QueueSession()
        state = bytearray(b"initial")
        session.incoming.put(bytes(state))

        def device():
            for _ in range(2):
                payload = session.outgoing.get(timeout=2)
                state.extend(payload)
                session.incoming.put(bytes(state))

        worker = threading.Thread(target=device, daemon=True)
        worker.start()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.FilePage(make_spec("file"), Path(directory))
            page.session = session
            self.assertTrue(page.open_connection())
            initial = page._wait_snapshot(after=-1)
            self.assertEqual(initial.read_bytes(), b"initial")
            page._append(b"payload")
            self.assertEqual(page._latest_data(), b"initialpayload")
            page.do_append("+more")
            page.close_connection()
        worker.join(timeout=1)

    def test_file_open_explains_missing_remote_file_path(self):
        session = QueueSession()
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.FilePage(make_spec("file"), Path(directory))
            page.session = session
            messages = []
            page.print = lambda message="", **_kwargs: messages.append(message)
            with patch.object(page, "_wait_snapshot", side_effect=TimeoutError):
                self.assertFalse(page.open_connection())
            self.assertTrue(any("file_path" in message for message in messages))

    def test_session_can_open_close_and_open_again_inside_page_lifetime(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fake-device"
            path.touch()
            spec = ncd_tui.ConnectionSpec("reopen", "instruction", str(path))
            session = ncd_tui.DeviceSession(spec)
            self.assertTrue(session.open())
            self.assertTrue(session.close())
            self.assertTrue(session.open())
            self.assertTrue(session.close())


class ActualFileAdapterTests(unittest.TestCase):
    def test_existing_file_adapter_append_and_snapshot_round_trip(self):
        adapter_path = ROOT / "crates" / "ncd" / "adapters" / "file.py"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "actual-device.bin"
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(adapter_path),
                    "run",
                    "Unspecified",
                    "File Device",
                    "--file_path",
                    str(target),
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                self.assertIsNotNone(process.stdin)
                self.assertIsNotNone(process.stdout)
                payload = b"adapter-round-trip"

                # Existing adapter sends an initial snapshot immediately.
                header = process.stdout.read(4)
                self.assertEqual(process.stdout.read(struct.unpack("!I", header)[0]), b"")

                process.stdin.write(struct.pack("!I", len(payload)) + payload)
                process.stdin.flush()

                header = process.stdout.read(4)
                self.assertEqual(len(header), 4)
                length = struct.unpack("!I", header)[0]
                response = process.stdout.read(length)
                self.assertEqual(response, payload)
                self.assertEqual(target.read_bytes(), payload)

                replacement = Path(directory) / "replacement.bin"
                replacement_payload = b"saved-by-atomic-replacement"
                replacement.write_bytes(replacement_payload)
                deadline = ncd_tui.time.monotonic() + 2
                while True:
                    try:
                        os.replace(replacement, target)
                        break
                    except PermissionError:
                        if ncd_tui.time.monotonic() >= deadline:
                            raise
                        ncd_tui.time.sleep(0.02)
                header = process.stdout.read(4)
                self.assertEqual(len(header), 4)
                response = process.stdout.read(struct.unpack("!I", header)[0])
                self.assertEqual(response, replacement_payload)
            finally:
                process.terminate()
                process.wait(timeout=5)
                process.stdin.close()
                process.stdout.close()
                process.stderr.close()

    def test_oversized_file_frame_stops_adapter_instead_of_growing_forever(self):
        adapter_path = ROOT / "crates" / "ncd" / "adapters" / "file.py"
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "bounded.bin"
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(adapter_path),
                    "run",
                    "Unspecified",
                    "File Device",
                    "--file_path",
                    str(target),
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                header = process.stdout.read(4)
                self.assertEqual(process.stdout.read(struct.unpack("!I", header)[0]), b"")
                process.stdin.write(struct.pack("!I", 64 * 1024 * 1024 + 1))
                process.stdin.flush()
                self.assertNotEqual(process.wait(timeout=5), 0)
                error = process.stderr.read().decode("utf-8", errors="replace")
                self.assertIn("exceeds", error)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
                process.stdin.close()
                process.stdout.close()
                process.stderr.close()

    def test_file_adapter_uses_safe_home_default_when_path_is_empty(self):
        adapter_path = ROOT / "crates" / "ncd" / "adapters" / "file.py"
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            environment = os.environ.copy()
            environment["HOME"] = str(home)
            environment["USERPROFILE"] = str(home)
            process = subprocess.Popen(
                [
                    sys.executable,
                    str(adapter_path),
                    "run",
                    "Unspecified",
                    "File Device",
                    "--file_path",
                    "",
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=environment,
            )
            try:
                self.assertIsNotNone(process.stdin)
                self.assertIsNotNone(process.stdout)
                header = process.stdout.read(4)
                self.assertEqual(process.stdout.read(struct.unpack("!I", header)[0]), b"")
                payload = b"default-file-path"
                process.stdin.write(struct.pack("!I", len(payload)) + payload)
                process.stdin.flush()
                header = process.stdout.read(4)
                response = process.stdout.read(struct.unpack("!I", header)[0])
                self.assertEqual(response, payload)
                self.assertEqual((home / "ncd-share.bin").read_bytes(), payload)
            finally:
                process.terminate()
                process.wait(timeout=5)
                process.stdin.close()
                process.stdout.close()
                process.stderr.close()


class ActualInstructionAdapterTests(unittest.TestCase):
    def test_instruction_adapter_executes_and_returns_matching_id(self):
        adapter_path = ROOT / "crates" / "ncd" / "adapters" / "instruction.py"
        process = subprocess.Popen(
            [
                sys.executable,
                str(adapter_path),
                "run",
                "system_instruction",
                "Instruction Device",
                "--timeout_ms",
                "5000",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            invalid_id = str(uuid.uuid4())
            invalid = json.dumps(
                {"id": invalid_id, "argv": "not-a-list", "timeout_ms": 5000}
            ).encode("utf-8")
            process.stdin.write(struct.pack("!I", len(invalid)) + invalid)
            process.stdin.flush()
            header = process.stdout.read(4)
            invalid_response = json.loads(
                process.stdout.read(struct.unpack("!I", header)[0]).decode("utf-8")
            )
            self.assertEqual(invalid_response["id"], invalid_id)
            self.assertFalse(invalid_response["ok"])
            self.assertIn("argv", invalid_response["stderr"])
            self.assertIsNone(process.poll())

            request_id = str(uuid.uuid4())
            request = json.dumps(
                {
                    "id": request_id,
                    "argv": [sys.executable, "-c", "print('instruction-round-trip')"],
                    "timeout_ms": 5000,
                }
            ).encode("utf-8")
            process.stdin.write(struct.pack("!I", len(request)) + request)
            process.stdin.flush()
            header = process.stdout.read(4)
            self.assertEqual(len(header), 4)
            response = json.loads(process.stdout.read(struct.unpack("!I", header)[0]).decode("utf-8"))
            self.assertEqual(response["id"], request_id)
            self.assertTrue(response["ok"])
            self.assertEqual(response["returncode"], 0)
            self.assertIn("instruction-round-trip", response["stdout"])
        finally:
            process.terminate()
            process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()

    def test_instruction_timeout_is_explicit_and_adapter_remains_alive(self):
        adapter_path = ROOT / "crates" / "ncd" / "adapters" / "instruction.py"
        process = subprocess.Popen(
            [
                sys.executable,
                str(adapter_path),
                "run",
                "system_instruction",
                "Instruction Device",
                "--timeout_ms",
                "5000",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            request_id = str(uuid.uuid4())
            request = json.dumps(
                {
                    "id": request_id,
                    "argv": [sys.executable, "-c", "import time; time.sleep(2)"],
                    "timeout_ms": 50,
                }
            ).encode("utf-8")
            process.stdin.write(struct.pack("!I", len(request)) + request)
            process.stdin.flush()
            header = process.stdout.read(4)
            response = json.loads(
                process.stdout.read(struct.unpack("!I", header)[0]).decode("utf-8")
            )
            self.assertEqual(response["id"], request_id)
            self.assertFalse(response["ok"])
            self.assertTrue(response["timed_out"])
            self.assertIsNotNone(response["returncode"])
            self.assertIn("timed out", response["stderr"])
            self.assertIsNone(process.poll())
        finally:
            process.terminate()
            process.wait(timeout=5)
            process.stdin.close()
            process.stdout.close()
            process.stderr.close()

    def test_instruction_output_is_bounded_and_marked(self):
        adapters = ROOT / "crates" / "ncd" / "adapters"
        sys.path.insert(0, str(adapters))
        spec = spec_from_file_location(
            "ncd_instruction_adapter_limit_test", adapters / "instruction.py"
        )
        module = module_from_spec(spec)
        spec.loader.exec_module(module)
        adapter = module.InstructionAdapter(
            "system_instruction",
            "Instruction Device",
            {"timeout_ms": "5000"},
        )
        adapter.open(adapter.options)
        adapter.MAX_OUTPUT_BYTES = 64
        try:
            response = adapter._execute_request(
                {
                    "argv": [
                        sys.executable,
                        "-c",
                        "import sys; sys.stdout.write('x' * 1024)",
                    ]
                },
                "bounded",
            )
            self.assertTrue(response["ok"])
            self.assertTrue(response["stdout_truncated"])
            self.assertIn("output truncated", response["stdout"])
        finally:
            adapter.close()


class ActualKeyboardAdapterTests(unittest.TestCase):
    def test_existing_keyboard_adapter_injects_and_returns_events(self):
        adapters = ROOT / "crates" / "ncd" / "adapters"
        sys.path.insert(0, str(adapters))
        spec = spec_from_file_location("ncd_keyboard_adapter_test", adapters / "keyboard.py")
        module = module_from_spec(spec)
        spec.loader.exec_module(module)

        calls = []

        class Controller:
            def type(self, text):
                calls.append(("type", text))

            def press(self, key):
                calls.append(("press", key))

            def release(self, key):
                calls.append(("release", key))

        class Listener:
            def __init__(self, **_kwargs):
                pass

            def start(self):
                pass

            def stop(self):
                pass

        class KeyCode:
            @staticmethod
            def from_vk(value):
                return ("vk", value)

        fake_keyboard = types.SimpleNamespace(
            Controller=Controller,
            Listener=Listener,
            Key=types.SimpleNamespace(enter="enter"),
            KeyCode=KeyCode,
        )
        fake_pynput = types.ModuleType("pynput")
        fake_pynput.keyboard = fake_keyboard

        with patch.dict(sys.modules, {"pynput": fake_pynput}):
            adapter = module.KeyboardAdapter(
                "system_keyboard",
                "Keyboard Device",
                {"listen": "true", "inject": "true"},
            )
            adapter.open(adapter.options)
            try:
                request_id = str(uuid.uuid4())
                command = json.dumps(
                    {"id": request_id, "action": "type", "text": "ab"}
                ).encode("utf-8")
                adapter.write(struct.pack("!I", len(command)) + command)
                self.assertEqual(calls, [("type", "ab")])

                adapter.events.put({"event": "press", "key_type": "char", "key": "z"})
                event_payload = adapter.read()
                event = json.loads(event_payload[4:].decode("utf-8"))
                self.assertEqual(event, {"event": "press", "key_type": "char", "key": "z"})
            finally:
                adapter.close()


if __name__ == "__main__":
    unittest.main()
