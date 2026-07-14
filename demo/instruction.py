import os
import sys

fd = os.open("/dev/ncd_ins", os.O_RDWR)
data = sys.stdin.read()
os.write(fd, data.encode())
r = os.read(fd, 4096 * 1000)
print("got", len(r), "bytes")
print(r.decode())
os.close(fd)
