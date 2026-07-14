from base import Adapter, Device

import sys
from queue import Queue, Empty

from pynput import keyboard


SPECIAL = {
    "space":   b" ",
    "enter":   b"\n",
    "tab":     b"\t",
    "backspace": b"\x08",
    "esc":     b"<esc>",
    "f1":  b"<f1>",  "f2":  b"<f2>",  "f3":  b"<f3>",  "f4":  b"<f4>",
    "f5":  b"<f5>",  "f6":  b"<f6>",  "f7":  b"<f7>",  "f8":  b"<f8>",
    "f9":  b"<f9>",  "f10": b"<f10>", "f11": b"<f11>", "f12": b"<f12>",
    "up": b"<up>", "down": b"<down>", "left": b"<left>", "right": b"<right>",
    "delete": b"<del>", "home": b"<home>", "end": b"<end>",
    "page_up": b"<pgup>", "page_down": b"<pgdn>",
}

CTRL = {chr(i): chr(i + 0x40) for i in range(1, 27)}  # \x01→A, ..., \x1A→Z


class KeyboardAdapter(Adapter):

    @classmethod
    def list_devices(cls) -> list[Device]:
        return [
            Device(
                identifier="system_keyboard",
                name="System Keyboard",
                description="Keystroke capture with modifier support",
            )
        ]

    def open(self, options: dict[str, str]):
        self.events = Queue()
        self._modifiers = set()

        def on_press(key):
            if hasattr(key, "char") and key.char is not None:
                ch = key.char
                if ch in CTRL:
                    self.events.put(b"^" + CTRL[ch].encode())
                else:
                    self.events.put(ch.encode("utf-8"))
            else:
                name = getattr(key, "name", None)
                if name in SPECIAL:
                    self.events.put(SPECIAL[name])

        def on_release(key):
            pass  # 只发按下的字符，不关心释放

        self.listener = keyboard.Listener(
            on_press=on_press,
            on_release=on_release,
        )
        self.listener.start()

    def read(self) -> bytes:
        try:
            return self.events.get(timeout=1)
        except Empty:
            return b""

    def write(self, data: bytes):
        pass

    def close(self):
        if self.listener:
            self.listener.stop()
