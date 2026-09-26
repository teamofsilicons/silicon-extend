#!/usr/bin/env python3
"""Writes a 640x360 test card PNG (colour bars) without any imaging library."""
import struct, sys, zlib

W, H = 640, 360
BARS = [(255, 255, 255), (255, 214, 0), (0, 200, 255), (0, 200, 80), (230, 0, 200), (230, 30, 30), (20, 40, 230), (0, 0, 0)]
rows = bytearray()
for y in range(H):
    rows.append(0)
    for x in range(W):
        rows += bytes(BARS[x * len(BARS) // W] if y < H * 3 // 4 else ((x * 255) // W,) * 3)

def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)
