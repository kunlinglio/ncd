#!/usr/bin/env python3
"""
End-to-end test for the ncd device: open / read / write / release.

Requires:
  - ncd.ko driver loaded (insmod / modprobe ncd)
  - ncdd daemon running with a device configured (e.g. ncd00)
  - A TCP server running on the IP:port the daemon expects

Usage: sudo python3 test_e2e.py
"""

import os
import sys
import time

DEV_PATH = "/dev/ncd00"


def main():
    if not os.path.exists(DEV_PATH):
        print(f"[-] {DEV_PATH} not found. Is the daemon running?")
        sys.exit(1)

    # 1. Open device
    print(f"[*] Opening {DEV_PATH} ...")
    fd = os.open(DEV_PATH, os.O_RDWR)
    print(f"[+] Opened, fd={fd}")

    # Give the daemon time to connect TCP
    time.sleep(0.3)

    # 2. Write data
    test_msg = b"Hello ncd!"
    print(f"[*] Writing: {test_msg!r}")
    written = os.write(fd, test_msg)
    print(f"[+] Wrote {written} bytes")

    # 3. Read until newline (or timeout after 100 iterations)
    print("[*] Reading until newline...")
    buf = b""
    for _ in range(100):
        chunk = os.read(fd, 4096)
        buf += chunk
        if b"\n" in buf:
            break
    print(f"[+] Read {len(buf)} bytes: {buf!r}")

    # 4. Release device
    print(f"[*] Closing {DEV_PATH} ...")
    os.close(fd)
    print("[+] Released")


if __name__ == "__main__":
    main()
