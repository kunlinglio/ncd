#!/usr/bin/env python3
"""NCD serial port driver.

Communicates with ncd via raw stdin/stdout byte streams.
Configuration is passed as CLI arguments.

stdin  → data from remote NCD peer → write to serial device
stdout ← data read from serial device → forward to remote NCD peer
stderr → diagnostic messages
exit 0 = clean shutdown, exit 1 = error
"""

import sys
import argparse
import threading

try:
    import serial
except ImportError:
    print("pyserial is required: pip install pyserial", file=sys.stderr)
    sys.exit(1)


def main():
    parser = argparse.ArgumentParser(description="NCD serial port driver")
    parser.add_argument("--device", default="/dev/ttyUSB0",
                        help="Serial device path")
    parser.add_argument("--baud", type=int, default=115200,
                        help="Baud rate")
    args = parser.parse_args()

    # Open the serial device
    try:
        ser = serial.Serial(port=args.device, baudrate=args.baud, timeout=0.1)
    except Exception as e:
        print(f"Failed to open {args.device}: {e}", file=sys.stderr)
        sys.exit(1)

    print(f"Opened {args.device} at {args.baud} baud", file=sys.stderr)

    # Reader thread: device → stdout
    def read_loop():
        try:
            while True:
                available = max(1, ser.in_waiting)
                data = ser.read(available)
                if data:
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
        except Exception:
            sys.exit(1)

    reader = threading.Thread(target=read_loop, daemon=True)
    reader.start()

    # Main thread: stdin → device
    try:
        while True:
            data = sys.stdin.buffer.read(4096)
            if not data:
                break
            ser.write(data)
    except KeyboardInterrupt:
        pass
    except Exception as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        ser.close()
        print("Serial device closed", file=sys.stderr)


if __name__ == "__main__":
    main()
