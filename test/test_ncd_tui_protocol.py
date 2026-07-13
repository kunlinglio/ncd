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

    def read_frame(self, *, stop_event=None, timeout=None):
        deadline = None if timeout is None else ncd_tui.time.monotonic() + timeout
        while True:
            if stop_event is not None and stop_event.is_set():
                raise ncd_tui.OperationCancelled()
            if deadline is not None and ncd_tui.time.monotonic() >= deadline:
                raise TimeoutError
            try:
                return self.incoming.get(timeout=0.02)
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
            with self.assertRaises(queue.Empty):
                session.outgoing.get_nowait()
            page.close_connection()

    def test_keyboard_sends_commands_and_receives_existing_adapter_events(self):
        session = QueueSession()
        session.incoming.put({"event": "press", "key_type": "char", "key": "x"})
        with tempfile.TemporaryDirectory() as directory:
            page = ncd_tui.KeyboardPage(make_spec("keyboard"), Path(directory))
            page.session = session
            self.assertTrue(page.open_connection())
            page.send_keyboard_command({"action": "type", "text": "hello"})
            command = session.outgoing.get(timeout=1)
            self.assertEqual(command["action"], "type")
            self.assertEqual(command["text"], "hello")
            deadline = ncd_tui.time.monotonic() + 1
            event_log = page.run_dir / "events.jsonl"
            while not event_log.exists() and ncd_tui.time.monotonic() < deadline:
                ncd_tui.time.sleep(0.01)
            self.assertTrue(event_log.exists())
            self.assertIn('"key": "x"', event_log.read_text(encoding="utf-8"))
            page.close_connection()

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

    def test_file_append_is_confirmed_by_existing_adapter_snapshot(self):
        session = QueueSession()
        state = bytearray(b"initial")
        session.incoming.put(bytes(state))

        def device():
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
            page.close_connection()
        worker.join(timeout=1)

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
