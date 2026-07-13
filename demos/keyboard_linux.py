import argparse
import json
import sys

from ncd_device import AdapterFrameReader, NcdDevice, pack_adapter_payload


SPECIAL_KEYS = {
    "alt",
    "alt_l",
    "alt_r",
    "backspace",
    "cmd",
    "ctrl",
    "ctrl_l",
    "ctrl_r",
    "delete",
    "down",
    "end",
    "enter",
    "esc",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "home",
    "left",
    "page_down",
    "page_up",
    "right",
    "shift",
    "shift_l",
    "shift_r",
    "space",
    "tab",
    "up",
}


def main():
    parser = argparse.ArgumentParser(description="Linux /dev handler for remote NCD keyboard adapter")
    parser.add_argument("device", help="ncdd character device path, for example /dev/ncd_keyboard")

    sub = parser.add_subparsers(dest="command", required=True)

    type_parser = sub.add_parser("type", help="type text on the remote host")
    type_parser.add_argument("text")

    stdin_parser = sub.add_parser("stdin", help="read Linux stdin and type it on the remote host")
    stdin_parser.add_argument("--enter", action="store_true", help="tap Enter after each input line")

    tap_parser = sub.add_parser("tap", help="tap one key on the remote host")
    tap_parser.add_argument("key")
    tap_parser.add_argument("--key-type", choices=["auto", "char", "special"], default="auto")

    press_parser = sub.add_parser("press", help="press one key on the remote host")
    press_parser.add_argument("key")
    press_parser.add_argument("--key-type", choices=["auto", "char", "special"], default="auto")

    release_parser = sub.add_parser("release", help="release one key on the remote host")
    release_parser.add_argument("key")
    release_parser.add_argument("--key-type", choices=["auto", "char", "special"], default="auto")

    listen_parser = sub.add_parser("listen", help="print remote keyboard events")
    listen_parser.add_argument("--limit", type=int, default=0)

    args = parser.parse_args()
    with NcdDevice(args.device) as device:
        if args.command == "type":
            send_command(device, {"action": "type", "text": args.text})
        elif args.command == "stdin":
            for line in sys.stdin:
                send_command(device, {"action": "type", "text": line.rstrip("\n")})
                if args.enter:
                    send_command(device, key_command("tap", "enter", "special"))
        elif args.command in {"tap", "press", "release"}:
            send_command(device, key_command(args.command, args.key, args.key_type))
        elif args.command == "listen":
            reader = AdapterFrameReader(device)
            count = 0
            while True:
                event = json.loads(reader.read_payload().decode("utf-8"))
                print(json.dumps(event, ensure_ascii=False))
                count += 1
                if args.limit and count >= args.limit:
                    break


def send_command(device: NcdDevice, command: dict):
    payload = json.dumps(command, ensure_ascii=False).encode("utf-8")
    device.write(pack_adapter_payload(payload))


def key_command(action: str, key: str, key_type: str) -> dict:
    if key_type == "auto":
        key_type = "special" if key in SPECIAL_KEYS else "char"
    return {"action": action, "key_type": key_type, "key": key}


if __name__ == "__main__":
    main()
