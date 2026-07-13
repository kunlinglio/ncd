import argparse
import json
import sys
import uuid

from ncd_device import AdapterFrameReader, NcdDevice, pack_adapter_payload


def main():
    parser = argparse.ArgumentParser(description="Linux /dev handler for remote NCD instruction adapter")
    parser.add_argument("device", help="ncdd character device path, for example /dev/ncd_instruction")
    parser.add_argument("--shell", action="store_true", help="send command as a shell string")
    parser.add_argument("--timeout-ms", type=int, default=5000)
    parser.add_argument("--cwd", default="")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="command argv, or shell text with --shell")
    args = parser.parse_args()

    if not args.command:
        parser.error("missing command")

    request_id = str(uuid.uuid4())
    request = {
        "id": request_id,
        "timeout_ms": args.timeout_ms,
    }
    if args.cwd:
        request["cwd"] = args.cwd

    if args.shell:
        request["shell"] = True
        request["command"] = " ".join(args.command)
    else:
        request["argv"] = args.command

    with NcdDevice(args.device) as device:
        reader = AdapterFrameReader(device)
        device.write(pack_adapter_payload(json.dumps(request).encode("utf-8")))

        while True:
            response = json.loads(reader.read_payload().decode("utf-8"))
            if response.get("id") == request_id:
                break

        stdout = response.get("stdout", "")
        stderr = response.get("stderr", "")
        if stdout:
            print(stdout, end="")
        if stderr:
            print(stderr, end="", file=sys.stderr)

        returncode = response.get("returncode")
        if returncode is None:
            sys.exit(1)
        sys.exit(returncode)


if __name__ == "__main__":
    main()
