"""Generate the minimal cross-platform app artwork without image dependencies."""

import struct
import zlib
from pathlib import Path


def chunk(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))


def make_png(size: int) -> bytes:
    scale = size / 256
    rows = []
    for y in range(size):
        row = bytearray([0])
        for x in range(size):
            inside = 18 * scale <= x < 238 * scale and 18 * scale <= y < 238 * scale
            triangle = 92 * scale <= x <= 172 * scale and abs(y - 128 * scale) <= (x - 92 * scale) * 0.66
            color = (255, 255, 255, 255) if triangle else (36, 87, 220, 255) if inside else (0, 0, 0, 0)
            row.extend(color)
        rows.append(bytes(row))
    image = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">2I5B", size, size, 8, 6, 0, 0, 0))
    return image + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")


icons = Path(__file__).resolve().parents[1] / "src-tauri" / "icons"
icons.mkdir(exist_ok=True)
(icons / "icon.png").write_bytes(make_png(1024))
png = make_png(256)
ico_header = struct.pack("<HHH", 0, 1, 1)
ico_entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22)
(icons / "icon.ico").write_bytes(ico_header + ico_entry + png)
