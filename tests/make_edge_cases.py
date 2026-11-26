#!/usr/bin/env python3
"""Writes edge-case test images (PNG variants, BMP variants, 16-bit, alpha, tiny sizes)."""
import os
import struct
import sys
import zlib

import numpy as np
from PIL import Image

out = sys.argv[1] if len(sys.argv) > 1 else "/tmp/komla-edge"
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(1234)


def png_raw(path, arr, color_type, depth, interlace=False):
    """Minimal PNG writer for 16-bit and arbitrary colour types (no filtering)."""
    h, w = arr.shape[:2]
    if depth == 16:
        rows = arr.astype(">u2").reshape(h, -1).view(np.uint8)
    elif depth < 8:
        per = 8 // depth
        v = arr.astype(np.uint8).reshape(h, -1)
        pad = (-v.shape[1]) % per
        v = np.pad(v, ((0, 0), (0, pad)))
        v = v.reshape(h, -1, per)
        rows = np.zeros((h, v.shape[1]), np.uint8)
        for k in range(per):
            rows |= (v[:, :, k] << (8 - depth * (k + 1))).astype(np.uint8)
    else:
        rows = arr.astype(np.uint8).reshape(h, -1)
    raw = b"".join(b"\x00" + rows[y].tobytes() for y in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", w, h, depth, color_type, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def smooth(h, w, c, maxv):
    y, x = np.mgrid[0:h, 0:w]
    base = np.stack([(np.sin(x / 17.0 + k) + np.cos(y / 23.0 - k)) for k in range(c)], -1)
    noise = rng.normal(0, 0.05, (h, w, c))
    return np.clip((base + 2) / 4 + noise, 0, 1) * maxv


# tiny and degenerate sizes
Image.new("RGB", (1, 1), (10, 200, 30)).save(f"{out}/one_rgb.png")
Image.new("RGBA", (1, 1), (10, 200, 30, 77)).save(f"{out}/one_rgba.png")
Image.fromarray(smooth(1, 300, 3, 255).astype(np.uint8)).save(f"{out}/row.png")
Image.fromarray(smooth(300, 1, 3, 255).astype(np.uint8)).save(f"{out}/column.png")
Image.fromarray(smooth(2, 2, 3, 255).astype(np.uint8)).save(f"{out}/two_by_two.png")

# 8-bit variants
img = smooth(120, 160, 3, 255).astype(np.uint8)
Image.fromarray(img).save(f"{out}/rgb8.png")
Image.fromarray(img[:, :, 0]).save(f"{out}/gray8.png")
la = np.dstack([img[:, :, 0], img[:, :, 1]])
Image.fromarray(la, "LA").save(f"{out}/gray_alpha8.png")
rgba = np.dstack([img, (rng.random((120, 160)) * 255).astype(np.uint8)])
rgba[:20, :20, 3] = 0
Image.fromarray(rgba, "RGBA").save(f"{out}/rgba8.png")
Image.fromarray(img).convert("P", palette=Image.ADAPTIVE, colors=200).save(f"{out}/palette200.png")
p = Image.fromarray(img).convert("P", palette=Image.ADAPTIVE, colors=16)
p.info["transparency"] = 3
p.save(f"{out}/palette_trns.png", transparency=3)
for bits in (1, 2, 4):
    g = (smooth(64, 96, 1, (1 << bits) - 1)[:, :, 0].round()).astype(np.uint8)
    png_raw(f"{out}/gray{bits}bit.png", g, 0, bits)
# 256 vs 257 colours
vals = np.arange(256 * 3).reshape(256, 3) % 251
a = np.tile(vals[:, None, :], (1, 4, 1)).astype(np.uint8)
Image.fromarray(a.reshape(64, 64, 3) if a.size == 64 * 64 * 3 else a).save(f"{out}/colors256.png")
b = np.concatenate([a, np.array([[[250, 1, 2]] * 4], np.uint8)], 0)
Image.fromarray(b).save(f"{out}/colors257.png")
# incompressible noise and a flat large image (exercises stripes)
Image.fromarray(rng.integers(0, 256, (200, 300, 3), dtype=np.uint8)).save(f"{out}/noise.png")
Image.new("RGB", (2500, 2000), (240, 240, 240)).save(f"{out}/flat_5mp.png")
Image.fromarray(smooth(1100, 2000, 3, 255).astype(np.uint8)).save(f"{out}/smooth_2_2mp.png")

# 16-bit
png_raw(f"{out}/gray16.png", smooth(100, 130, 1, 65535).round()[:, :, 0], 0, 16)
png_raw(f"{out}/rgb16.png", smooth(100, 130, 3, 65535).round(), 2, 16)
png_raw(f"{out}/rgba16.png", np.dstack([smooth(80, 90, 3, 65535), np.full((80, 90, 1), 40000.0)]).round(), 6, 16)
png_raw(f"{out}/gray16_few.png", (smooth(50, 60, 1, 3).round()[:, :, 0] * 21845), 0, 16)

# BMP variants
Image.fromarray(img).save(f"{out}/bmp24.bmp")
Image.fromarray(img[:, :, 0]).save(f"{out}/bmp8gray.bmp")
Image.fromarray(img).convert("P", palette=Image.ADAPTIVE, colors=16).save(f"{out}/bmp4pal.bmp")
Image.fromarray(img).convert("1").save(f"{out}/bmp1.bmp")
Image.fromarray(rgba, "RGBA").save(f"{out}/bmp32.bmp")

# JPEG variants: lossless recompression must give back every file byte for byte
photo = np.clip(smooth(240, 320, 3, 255) + rng.normal(0, 6, (240, 320, 3)), 0, 255).astype(np.uint8)
pim = Image.fromarray(photo)
pim.save(f"{out}/jpeg_420.jpg", quality=85)
pim.save(f"{out}/jpeg_444_q95.jpg", quality=95, subsampling=0)
pim.save(f"{out}/jpeg_422_optimized.jpg", quality=80, subsampling=1, optimize=True)
pim.save(f"{out}/jpeg_progressive.jpg", quality=88, progressive=True)
pim.save(f"{out}/jpeg_restart.jpg", quality=75, restart_marker_blocks=3)
pim.save(f"{out}/jpeg_progressive_restart.jpg", quality=70, progressive=True, restart_marker_rows=1)
pim.convert("L").save(f"{out}/jpeg_gray.jpg", quality=90)
pim.crop((5, 3, 58, 40)).save(f"{out}/jpeg_odd_size.jpg", quality=92, subsampling=2)
Image.new("RGB", (1, 1), (200, 30, 90)).save(f"{out}/jpeg_1x1.jpg")
exif = Image.Exif()
exif[0x010F] = "Komla test camera"
exif[0x0110] = "Model 1"
pim.save(f"{out}/jpeg_metadata.jpg", quality=90, exif=exif.tobytes(), icc_profile=bytes(range(256)) * 4, comment=b"edge case")
with open(f"{out}/jpeg_420.jpg", "rb") as f:
    jpg = f.read()
with open(f"{out}/jpeg_trailer.jpg", "wb") as f:
    f.write(jpg + b"bytes after the end of the image")
with open(f"{out}/jpeg_truncated.jpg", "wb") as f:
    f.write(jpg[: len(jpg) * 2 // 3])
print("written to", out)
