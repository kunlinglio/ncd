#!/usr/bin/env python3
"""NCD keyboard driver.

Captures local keyboard events and forwards them to stdout.
Each event is one line: +KEY for press, -KEY for release.

Examples:
  +a          (key 'a' pressed)
  -a          (key 'a' released)
  +Key.enter  (Enter pressed)
  -Key.shift  (Shift released)

stdin is reserved for future use (e.g. LED control, key injection).
"""

import sys
import argparse
import threading
from queue import Queue

try:
    from pynput import keyboard
except ImportError:
    print("pynput is required: pip install pynput", file=sys.stderr)
    sys.exit(1)


def main():
    parser = argparse.ArgumentParser(description="NCD keyboard driver")
    parser.add_argument("--capture", choices=("all", "chars"), default="all",
                        help="Capture all keys or only character keys (default: all)")
    args = parser.parse_args()

    event_queue: Queue = Queue()
    running = True

    def on_press(key):
        if args.capture == "chars":
            try:
                event_queue.put(f"+{key.char}")
            except AttributeError:
                pass  # skip special keys in chars mode
        else:
            try:
                event_queue.put(f"+{key.char}")
            except AttributeError:
                event_queue.put(f"+{key}")

    def on_release(key):
        if args.capture == "chars":
            try:
                event_queue.put(f"-{key.char}")
            except AttributeError:
                pass
        else:
            try:
                event_queue.put(f"-{key.char}")
            except AttributeError:
                event_queue.put(f"-{key}")

    # Start pynput listener (runs its own thread internally)
    listener = keyboard.Listener(on_press=on_press, on_release=on_release)
    listener.start()

    mode = "all keys" if args.capture == "all" else "character keys only"
    print(f"Keyboard capture started ({mode})", file=sys.stderr)

    # Writer thread: event queue → stdout
    def writer_loop():
        while running:
            try:
                event = event_queue.get(timeout=0.1)
                sys.stdout.write(event + "\n")
                sys.stdout.flush()
            except Exception:
                pass

    writer = threading.Thread(target=writer_loop, daemon=True)
    writer.start()

    # Main thread: read stdin (reserved for future LED/key-injection control)
    try:
        while True:
            data = sys.stdin.buffer.read(4096)
            if not data:
                break
            # Future: parse commands from remote peer
    except KeyboardInterrupt:
        pass
    finally:
        running = False
        listener.stop()
        print("Keyboard capture stopped", file=sys.stderr)


if __name__ == "__main__":
    main()
