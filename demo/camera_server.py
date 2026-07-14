"""MJPEG server — stream ncd camera to browser.
Usage:  python3 camera_server.py [/dev/ncd_camera] [port]
Open:   http://<linux-ip>:8080
"""
import os
import struct
import sys
import threading
from http.server import HTTPServer, BaseHTTPRequestHandler

DEVICE = sys.argv[1] if len(sys.argv) > 1 else "/dev/ncd_camera"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8080

latest_frame = b""
lock = threading.Lock()


class MJPEGHandler(BaseHTTPRequestHandler):

    def do_GET(self):
        if self.path == "/":
            self._send_page()
        elif self.path == "/stream":
            self._send_stream()

    def _send_page(self):
        html = f"""<!DOCTYPE html>
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
        self.end_headers()
        while True:
            with lock:
                frame = latest_frame
            if frame:
                self.wfile.write(b"--frame\r\n")
                self.wfile.write(b"Content-Type: image/jpeg\r\n\r\n")
                self.wfile.write(frame)
                self.wfile.write(b"\r\n")
                # flush each frame so the browser renders in real time
                try:
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    break

    def log_message(self, fmt, *args):
        pass  # suppress access logs


def reader():
    global latest_frame
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


threading.Thread(target=reader, daemon=True).start()

print(f"http://0.0.0.0:{PORT}")
HTTPServer(("0.0.0.0", PORT), MJPEGHandler).serve_forever()
