import argparse
import json
import sys
import threading
from abc import ABC, abstractmethod
from dataclasses import asdict, dataclass
from typing import BinaryIO, Literal


@dataclass
class Device:
    identifier: str  # Identifier must be unique for each device, this will not be used for display
    name: str  # Display name for the device, can be non-unique
    description: str  # Description for the device, can be empty


class Adapter(ABC):
    """
    Abstract base class for an adapter.
    An adapter class corresponds to a class of devices, an instance corresponds to a specific device.
    """

    device_identifier: str
    device_name: str
    options: dict[str, str]

    @classmethod
    @abstractmethod
    def list_devices(cls) -> list[Device]:
        """List available devices."""
        pass

    @abstractmethod
    def open(self, options: dict[str, str]):
        """Open the device with the given options."""
        pass

    @abstractmethod
    def read(self) -> bytes:
        """Read data from the device."""
        pass

    @abstractmethod
    def write(self, data: bytes):
        """Write data to the device."""
        pass

    @abstractmethod
    def close(self):
        """Close the device."""
        pass

    def __init_subclass__(cls, **kwargs) -> None:
        """
        Magic hook: when an Adapter subclass is defined in a script run as __main__,
        automatically parse CLI args and dispatch to list_devices() or run().
        """
        super().__init_subclass__(**kwargs)
        if cls.__module__ == "__main__":
            main(cls)
            sys.exit(0)

    def adapter_name(self) -> str:
        return self.__class__.__name__

    def __init__(
        self, device_identifier: str, device_name: str, options: dict[str, str]
    ):
        self.device_name = device_name
        self.device_identifier = device_identifier
        self.options = options

    def _read_loop(self, output: BinaryIO):
        """Read data from the device and write it to the output."""
        try:
            while True:
                data = self.read()
                if data:
                    output.write(data)
                    output.flush()
        except Exception as e:
            name = self.adapter_name()
            print(
                f"Adapter {name} Error: {e} \nOn device {self.device_identifier}",
                file=sys.stderr,
            )
            sys.exit(1)

    def _write_loop(self, input: BinaryIO):
        """Read data from the input and write it to the device."""
        try:
            while True:
                data = (
                    input.read1(4096)
                    if hasattr(input, "read1")
                    else input.read(4096)
                )
                print(f"DEBUG _write_loop got {len(data)} bytes", file=sys.stderr, flush=True)  # ← 加这行
                if not data:
                    break

                self.write(data)
        except Exception as e:
            name = self.adapter_name()
            print(
                f"Adapter {name} Error: {e} \nOn device {self.device_identifier}",
                file=sys.stderr,
            )
            sys.exit(1)

    def run(self, input: BinaryIO, output: BinaryIO):
        """Run the adapter, reading from input and writing to output."""

        reader = threading.Thread(target=self._read_loop, args=(output,), daemon=True)
        reader.start()
        self._write_loop(input)
        reader.join()

    def __enter__(
        self,
    ) -> "Adapter":
        self.open(self.options)
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()


def parse_arg() -> tuple[Literal["list", "run"], tuple[str, str, dict[str, str]]]:
    """
    Parse command line arguments.
    Returns (command, (device_identifier, device_name, options))
    """
    parser = argparse.ArgumentParser(description="NCD adapter")
    sub_command = parser.add_subparsers(dest="command", required=True)
    sub_command.add_parser("list")
    run_parser = sub_command.add_parser("run")
    run_parser.add_argument("identifier")
    run_parser.add_argument("name")

    parsed, unknown = parser.parse_known_args()
    options = {}
    i = 0
    while i < len(unknown):
        if unknown[i].startswith("--"):
            key = unknown[i][2:].replace("-", "_")
            i += 1
            options[key] = (
                unknown[i]
                if i < len(unknown) and not unknown[i].startswith("--")
                else ""
            )
        else:
            i += 1

    match parsed.command:
        case "list":
            return "list", ("", "", {})
        case "run":
            return "run", (parsed.identifier, parsed.name, options)
        case _:
            parser.error(f"Unknown command: {parsed.command}")


def main(cls):
    command, (identifier, name, options) = parse_arg()
    match command:
        case "list":
            devices = cls.list_devices()
            devices_json = json.dumps([asdict(device) for device in devices])
            sys.stdout.buffer.write(devices_json.encode("utf-8"))
            sys.stdout.buffer.flush()
        case "run":
            adapter = cls(identifier, name, options)
            with adapter:
                try:
                    adapter.run(sys.stdin.buffer, sys.stdout.buffer)
                except KeyboardInterrupt:
                    pass
                except Exception as e:
                    name = adapter.adapter_name()
                    print(
                        f"Adapter {name} Error: {e} \nOn device {adapter.device_identifier}",
                        file=sys.stderr,
                    )
                    sys.exit(1)
