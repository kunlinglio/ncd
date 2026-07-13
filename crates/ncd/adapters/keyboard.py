from base import Adapter, Device

import json
import queue
import struct
import sys


class KeyboardAdapter(Adapter):
    def _log(self, direction: str, message: str = ""):
        suffix = f" {message}" if message else ""
        print(
            f"[{self.device_name}:{getattr(self, 'port', '?')} {direction}]{suffix}",
            file=sys.stderr,
            flush=True,
        )

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="system_keyboard",
                name="System Keyboard",
                description="Global keyboard input/output",
            )
        ]

    def open(self, options: dict[str, str]):
        from pynput import keyboard

        self.port = options.get("port", "?")
        self._log("connect", "open")
        self.keyboard = keyboard
        self.events = queue.Queue()
        self.input_buffer = b""
        self.listener = None
        self.controller = None

        listen = options.get("listen", "true").lower() == "true"
        inject = options.get("inject", "true").lower() == "true"
        suppress = options.get("suppress", "false").lower() == "true"
        echo_injected = options.get("echo_injected", "false").lower() == "true"

        if inject:
            self.controller = keyboard.Controller()
            self._log("connect", "keyboard injection enabled")
        else:
            self._log("connect", "keyboard injection disabled")

        def serialize_key(key):
            if isinstance(key, keyboard.KeyCode):
                if key.char is not None:
                    return {"key_type": "char", "key": key.char}

                vk = getattr(key, "vk", None)
                if vk is not None:
                    return {"key_type": "vk", "key": str(vk)}

            name = getattr(key, "name", None)
            if name is not None:
                return {"key_type": "special", "key": name}

            return {"key_type": "unknown", "key": str(key)}

        def on_press(key, injected=False):
            if injected and not echo_injected:
                return

            event = {
                "event": "press",
                **serialize_key(key),
            }
            self._log("device->linux", f"press {event.get('key')!r}")
            self.events.put(event)

        def on_release(key, injected=False):
            if injected and not echo_injected:
                return

            event = {
                "event": "release",
                **serialize_key(key),
            }
            self._log("device->linux", f"release {event.get('key')!r}")
            self.events.put(event)

        if listen:
            self.listener = keyboard.Listener(
                on_press=on_press,
                on_release=on_release,
                suppress=suppress,
            )
            self.listener.start()
            self._log("connect", f"keyboard listener started suppress={suppress}")
        else:
            self._log("connect", "keyboard listener disabled")

    def read(self) -> bytes:
        event = self.events.get()
        payload = json.dumps(event).encode("utf-8")
        self._log("device->linux", f"event bytes={len(payload)}")
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

            command = json.loads(payload.decode("utf-8"))
            action = command["action"]
            self._log("linux->device", self._summarize_command(command))

            if self.controller is None:
                raise RuntimeError("keyboard injection is disabled")

            if action == "type":
                self.controller.type(command["text"])
                continue

            key_type = command.get("key_type", "char")
            key_name = command["key"]

            if key_type == "char":
                key = key_name
            elif key_type == "special":
                key = getattr(self.keyboard.Key, key_name)
            elif key_type == "vk":
                key = self.keyboard.KeyCode.from_vk(int(key_name))
            else:
                raise RuntimeError(f"unknown keyboard key_type: {key_type}")

            if action == "press":
                self.controller.press(key)
            elif action == "release":
                self.controller.release(key)
            elif action == "tap":
                self.controller.press(key)
                self.controller.release(key)
            else:
                raise RuntimeError(f"unknown keyboard action: {action}")

    def close(self):
        if self.listener is not None:
            self._log("close", "keyboard listener stopping")
            self.listener.stop()
            self.listener = None

        self.controller = None
        self._log("close")

    def _summarize_command(self, command: dict) -> str:
        action = command.get("action")
        if action == "type":
            text = command.get("text", "")
            if len(text) > 40:
                text = text[:40] + "..."
            return f"type {text!r}"

        key = command.get("key")
        key_type = command.get("key_type", "char")
        return f"{action} {key_type}:{key}"
