import argparse
from pathlib import Path

from ncd_device import AdapterFrameReader, NcdDevice, pack_adapter_payload


def main():
    parser = argparse.ArgumentParser(description="Linux /dev handler for remote NCD file adapter")
    parser.add_argument("device", help="ncdd character device path, for example /dev/ncd_file")

    sub = parser.add_subparsers(dest="command", required=True)

    read_parser = sub.add_parser("read", help="read one remote file snapshot")
    read_parser.add_argument("--output", required=True, help="local output path")

    watch_parser = sub.add_parser("watch", help="write remote file snapshots whenever they arrive")
    watch_parser.add_argument("--output", required=True, help="local output path")

    write_parser = sub.add_parser("write", help="write local file bytes to the remote exposed file")
    write_parser.add_argument("--input", required=True, help="local input path")

    args = parser.parse_args()

    with NcdDevice(args.device) as device:
        if args.command == "read":
            reader = AdapterFrameReader(device)
            data = reader.read_payload()
            Path(args.output).write_bytes(data)
            print(f"wrote {len(data)} bytes to {args.output}")
        elif args.command == "watch":
            output = Path(args.output)
            reader = AdapterFrameReader(device)
            counter = 0
            while True:
                data = reader.read_payload()
                output.write_bytes(data)
                counter += 1
                print(f"snapshot {counter}: wrote {len(data)} bytes to {output}")
        elif args.command == "write":
            data = Path(args.input).read_bytes()
            device.write(pack_adapter_payload(data))
            print(f"sent {len(data)} bytes")


if __name__ == "__main__":
    main()
