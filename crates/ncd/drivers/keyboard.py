#!/usr/bin/env python3
"""NCD keyboard driver.

Simple passthrough: stdin → stdout.

ncd feeds its own terminal input to this driver's stdin;
the driver forwards it to stdout, which ncd sends to the remote peer.

stdin  → remote (local keystrokes forwarded to NCD peer)
stdout ← ncd reads and forwards over NCD

No special permissions, no /dev/tty, no external dependencies.
"""

import sys


def main():
    try:
        while True:
            data = "*" * 100  # for debug
            sys.stdout.write(data)
            sys.stdout.buffer.flush()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
