import argparse
import itertools
from pathlib import Path

from ncd_device import AdapterFrameReader, NcdDevice


def main():
    parser = argparse.ArgumentParser(description="Linux /dev handler for remote NCD camera adapter")
    parser.add_argument("device", help="ncdd character device path, for example /dev/ncd_camera")
    parser.add_argument("--output-dir", default="camera_frames", help="directory for captured JPEG frames")
    parser.add_argument("--latest", default="", help="optional path overwritten with the latest JPEG")
    parser.add_argument("--limit", type=int, default=0, help="stop after N frames, 0 means forever")
    parser.add_argument("--display", action="store_true", help="display frames with OpenCV if installed")
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    latest_path = Path(args.latest) if args.latest else None

    display = None
    if args.display:
        try:
            import cv2
            import numpy as np

            display = (cv2, np)
        except ImportError:
            print("OpenCV/numpy not installed; continuing without display")

    with NcdDevice(args.device) as device:
        frames = AdapterFrameReader(device)
        for frame_no in itertools.count(1):
            jpeg = frames.read_payload()

            frame_path = output_dir / f"frame_{frame_no:06d}.jpg"
            frame_path.write_bytes(jpeg)
            if latest_path is not None:
                latest_path.write_bytes(jpeg)

            print(f"frame {frame_no}: {len(jpeg)} bytes -> {frame_path}")

            if display is not None:
                cv2, np = display
                image = cv2.imdecode(np.frombuffer(jpeg, dtype=np.uint8), cv2.IMREAD_COLOR)
                if image is not None:
                    cv2.imshow("NCD Camera", image)
                    if cv2.waitKey(1) & 0xFF == ord("q"):
                        break

            if args.limit and frame_no >= args.limit:
                break


if __name__ == "__main__":
    main()
