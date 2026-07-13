from base import Adapter, Device

import json
import queue
import struct


class KeyboardAdapter(Adapter):
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

        self.keyboard = keyboard
        self.events = queue.Queue()
        self.input_buffer = b""
        self.listener = None
        self.controller = None

        listen = options.get("listen", "true").lower() == "true"
        inject = options.get("inject", "true").lower() == "true"
        suppress = options.get("suppress", "false").lower() == "true"

        if inject:
            self.controller = keyboard.Controller()

        def serialize_key(key):
            if isinstance(key, keyboard.KeyCode) and key.char is not None:
                return {"key_type": "char", "key": key.char}

            return {"key_type": "special", "key": key.name}

        def on_press(key):
            self.events.put({
                "event": "press",
                **serialize_key(key),
            })

        def on_release(key):
            self.events.put({
                "event": "release",
                **serialize_key(key),
            })

        if listen:
            self.listener = keyboard.Listener(
                on_press=on_press,
                on_release=on_release,
                suppress=suppress,
            )
            self.listener.start()

    def read(self) -> bytes:
        event = self.events.get()
        payload = json.dumps(event).encode("utf-8")
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        self.input_buffer += data

        while len(self.input_buffer) >= 4:
            payload_len = struct.unpack("!I", self.input_buffer[:4])[0]

            if len(self.input_buffer) < 4 + payload_len:
                break

            payload = self.input_buffer[4:4 + payload_len]
            self.input_buffer = self.input_buffer[4 + payload_len:]

            command = json.loads(payload.decode("utf-8"))
            action = command["action"]

            if self.controller is None:
                raise RuntimeError("keyboard injection is disabled")

            if action == "type":
                self.controller.type(command["text"])
                continue

            key_type = command.get("key_type", "char")
            key_name = command["key"]

            if key_type == "char":
                key = key_name
            else:
                key = getattr(self.keyboard.Key, key_name)

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
            self.listener.stop()
            self.listener = None

        self.controller = None
