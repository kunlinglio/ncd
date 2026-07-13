from base import Adapter, Device

import platform
import cv2
import io
import struct

MAX_CAMERA_NUM = 4

def get_os_platform() -> tuple[int, str]:
    system = platform.system()
    if system == "Linux":
        return cv2.CAP_V4L2, "V4L2"
    if system == "Darwin":
        return cv2.CAP_AVFOUNDATION, "AVFoundation"
    if system == "Windows":
        return cv2.CAP_MSMF, "Media Foundation"
    raise RuntimeError(f"Unsupported platform: {system}")

class CameraAdapter(Adapter):
    @classmethod
    def list_devices(cls) -> list[Device]:
        backend, backend_name = get_os_platform()
        devices = []

        for i in range(MAX_CAMERA_NUM):
            capture = cv2.VideoCapture(i, backend)

            try:
                if not capture.isOpened():
                    continue

                ok, frame = capture.read()
                if not ok:
                    continue

                height, width = frame.shape[:2]

                devices.append(Device(
                    identifier=str(i),
                    name=f"Camera {i}",
                    description=f"frame-height: {height}, frame-width: {width}, platform: {backend_name}",
                ))
            finally:
                capture.release()

        return devices
    
    def open(self, options: dict[str, str]):
        os, _ = get_os_platform()
        index = int(self.device_identifier)

        self.capture = cv2.VideoCapture(index, os)

        if not self.capture.isOpened():
            self.capture.release()
            self.capture = None
            raise RuntimeError(f"failed to open camera {index}")

        width = options.get("width")
        height = options.get("height")
        fps = options.get("fps")

        if width:
            self.capture.set(cv2.CAP_PROP_FRAME_WIDTH, int(width))
        if height:
            self.capture.set(cv2.CAP_PROP_FRAME_HEIGHT, int(height))
        if fps:
            self.capture.set(cv2.CAP_PROP_FPS, int(fps))

        self.width = int(self.capture.get(cv2.CAP_PROP_FRAME_WIDTH))
        self.height = int(self.capture.get(cv2.CAP_PROP_FRAME_HEIGHT))
        self.fps = self.capture.get(cv2.CAP_PROP_FPS)

        self.jpeg_quality = int(options.get("jpeg_quality") or "80")

    def read(self) -> bytes:
        if self.capture is None:
            raise RuntimeError("camera is not open")

        ok, frame = self.capture.read()
        if not ok:
            raise RuntimeError("failed to read camera frame")

        ok, encoded = cv2.imencode(
            ".jpg",
            frame,
            [cv2.IMWRITE_JPEG_QUALITY, self.jpeg_quality],
        )

        if not ok:
            raise RuntimeError("failed to encode camera frame")

        payload = encoded.tobytes()
        return struct.pack("!I", len(payload)) + payload

    def write(self, data: bytes):
        raise io.UnsupportedOperation("Can not write to the device: camera is read-only")
    
    def close(self):
        if self.capture is not None:
            self.capture.release()
            self.capture = None
