#!/usr/bin/env python3
"""NCD driver: <driver description>

Template for implementing new NCD device drivers.

Protocol:
  stdin  → raw bytes from remote NCD peer → write to device
  stdout ← raw bytes read from device → forwarded to remote NCD peer
  stderr → diagnostic/log messages
  exit 0 = clean shutdown, exit 1 = error

Configuration is passed via CLI arguments (argparse).
"""

import sys
import argparse
import threading


def main():
    parser = argparse.ArgumentParser(description="NCD <driver name> driver")
    # TODO: Define driver-specific CLI arguments
    # parser.add_argument("--example", default="default_value", help="...")
    args = parser.parse_args()

    # 1. Open the device
    #    On failure: print error to stderr, sys.exit(1)
    # device = open_device(args)

    # 2. Reader thread: device → stdout
    def read_loop():
        try:
            while True:
                # TODO: Read from device
                # data = device.read()
                # if data:
                #     sys.stdout.buffer.write(data)
                #     sys.stdout.buffer.flush()
                pass
        except Exception:
            sys.exit(1)

    reader = threading.Thread(target=read_loop, daemon=True)
    reader.start()

    # 3. Main thread: stdin → device
    try:
        while True:
            data = sys.stdin.buffer.read(4096)
            if not data:
                break
            # TODO: Write to device
            # device.write(data)
    except KeyboardInterrupt:
        pass
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        # TODO: Close device
        pass


if __name__ == "__main__":
    main()
