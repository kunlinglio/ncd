from base import Adapter, Device

import json
import queue
import struct
import sys


class KeyboardAdapter(Adapter):
    def _log(self, message: str):
        print(
            f"[keyboard adapter name={self.device_name!r} id={self.device_identifier!r}] {message}",
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

        self._log(f"open requested options={options}")
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
            self._log("keyboard injection enabled")
        else:
            self._log("keyboard injection disabled")

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
            self._log(f"[actual->linux] local key press {event}")
            self.events.put(event)

        def on_release(key, injected=False):
            if injected and not echo_injected:
                return

            event = {
                "event": "release",
                **serialize_key(key),
            }
            self._log(f"[actual->linux] local key release {event}")
            self.events.put(event)

        if listen:
            self.listener = keyboard.Listener(
                on_press=on_press,
                on_release=on_release,
                suppress=suppress,
            )
            self.listener.start()
            self._log(f"keyboard listener started suppress={suppress}")
        else:
            self._log("keyboard listener disabled")

    def read(self) -> bytes:
        event = self.events.get()
        payload = json.dumps(event).encode("utf-8")
        self._log(f"[actual->linux] read event payload={len(payload)} bytes event={event}")
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

            command = json.loads(payload.decode("utf-8"))
            action = command["action"]
            self._log(f"[linux->actual] execute command {command}")

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
            self._log("keyboard listener stopping")
            self.listener.stop()
            self.listener = None

        self.controller = None
        self._log("close")
