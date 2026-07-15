"""MJPEG server — stream ncd camera to browser.
Usage:  python3 camera_server.py [/dev/ncd_camera] [port]
Open:   http://<linux-ip>:8080
"""

import os
import struct
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

DEVICE = sys.argv[1] if len(sys.argv) > 1 else "/dev/ncd_camera"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8080

latest_frame = b""
frame_seq = 0            # increments each time reader() delivers a new frame
frame_ready = threading.Event()
lock = threading.Lock()


class MJPEGHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            self._send_page()
        elif self.path == "/stream":
            self._send_stream()

    def _send_page(self):
        html = """<!DOCTYPE html>
<html><head><title>NCD Camera</title></head>
<body style="margin:0;background:#000;display:flex;justify-content:center;">
  <img src="/stream" style="max-width:100vw;max-height:100vh">
</body></html>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(html.encode())

    def _send_stream(self):
        self.send_response(200)
        self.send_header("Content-Type",
                         "multipart/x-mixed-replace; boundary=frame")
        self.send_header("Cache-Control",
                         "no-store, no-cache, must-revalidate, max-age=0")
        self.send_header("Pragma", "no-cache")
        self.send_header("Connection", "close")
        self.send_header("Expires", "0")
        self.end_headers()

        last_seq = -1
        while True:
            frame_ready.wait()                  # block until a new frame arrives
            with lock:
                seq = frame_seq
                frame = latest_frame
            frame_ready.clear()

            if not frame or seq == last_seq:
                continue

            last_seq = seq
            try:
                self.wfile.write(b"--frame\r\n")
                self.wfile.write(b"Content-Type: image/jpeg\r\n")
                self.wfile.write(f"Content-Length: {len(frame)}\r\n".encode())
                self.wfile.write(b"\r\n")
                self.wfile.write(frame)
                self.wfile.write(b"\r\n")
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                break

    def log_message(self, fmt, *args):
        pass  # suppress access logs


def reader():
    global latest_frame, frame_seq
    fd = os.open(DEVICE, os.O_RDONLY)
    while True:
        raw = os.read(fd, 4)
        n = struct.unpack("!I", raw)[0]
        jpg = b""
        while len(jpg) < n:
            chunk = os.read(fd, n - len(jpg))
            if not chunk:
                break
            jpg += chunk
        with lock:
            latest_frame = jpg
            frame_seq += 1
        frame_ready.set()


threading.Thread(target=reader, daemon=True).start()

print(f"http://0.0.0.0:{PORT}")
HTTPServer(("0.0.0.0", PORT), MJPEGHandler).serve_forever()
