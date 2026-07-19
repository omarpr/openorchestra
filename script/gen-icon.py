#!/usr/bin/env python3
"""openOrchestra app icon generator (Pillow). Produces a 1024 master PNG,
a macOS .iconset, and a packed .icns."""
import math
import os
import struct
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

S = 1024
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'assets')
os.makedirs(OUT, exist_ok=True)

def hex2rgb(h):
    h = h.lstrip('#')
    return tuple(int(h[i:i+2], 16) for i in (0, 2, 4))

# palette (design tokens)
ACCENT = hex2rgb('5b9bff')
VIOLET = hex2rgb('a877f5')
TEAL   = hex2rgb('22b8cf')
TOP    = hex2rgb('20283c')
BOT    = hex2rgb('0b0d11')

def vgrad(w, h, top, bot):
    t = np.linspace(0, 1, h)[:, None]
    arr = (np.array(top)[None, None, :] * (1 - t[:, :, None]) +
           np.array(bot)[None, None, :] * t[:, :, None])
    arr = np.repeat(arr, w, axis=1)
    return Image.fromarray(arr.astype('uint8'), 'RGB')

def hgrad3(w, h, c0, c1, c2):
    x = np.linspace(0, 1, w)
    def lerp(a, b, tt):
        return np.array(a)[None, :] * (1 - tt[:, None]) + np.array(b)[None, :] * tt[:, None]
    left = lerp(c0, c1, np.clip(x * 2, 0, 1))
    right = lerp(c1, c2, np.clip((x - 0.5) * 2, 0, 1))
    mix = np.where(x[:, None] < 0.5, left, right)
    row = mix.astype('uint8')[None, :, :]
    return Image.fromarray(np.repeat(row, h, axis=0), 'RGB')

# --- background tile (rounded rect with vertical gradient) ---
tile = vgrad(S, S, TOP, BOT).convert('RGBA')
mask = Image.new('L', (S, S), 0)
ImageDraw.Draw(mask).rounded_rectangle([0, 0, S - 1, S - 1], radius=228, fill=255)
img = Image.new('RGBA', (S, S), (0, 0, 0, 0))
img.paste(tile, (0, 0), mask)

# --- soft radial glow behind the mark ---
glow = Image.new('RGBA', (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(glow)
cx, cy = 512, 440
for r in range(420, 0, -6):
    a = int(46 * (1 - r / 420) ** 1.6)
    gd.ellipse([cx - r, cy - r, cx + r, cy + r], fill=ACCENT + (a,))
glow = glow.filter(ImageFilter.GaussianBlur(18))
img.alpha_composite(glow)
img.putalpha(mask)  # keep within tile

# --- conductor's baton sweep (arc above the bars) ---
arc = Image.new('RGBA', (S, S), (0, 0, 0, 0))
ad = ImageDraw.Draw(arc)
pts = []
p0, p1, p2 = (196, 452), (512, 176), (852, 356)
for i in range(101):
    t = i / 100
    x = (1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * p1[0] + t ** 2 * p2[0]
    y = (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * p1[1] + t ** 2 * p2[1]
    pts.append((x, y))
ad.line(pts, fill=(207, 224, 255, 235), width=22, joint='curve')
# rounded caps + baton tip
ad.ellipse([p0[0] - 11, p0[1] - 11, p0[0] + 11, p0[1] + 11], fill=(207, 224, 255, 235))
ad.ellipse([p2[0] - 30, p2[1] - 30, p2[0] + 30, p2[1] + 30], fill=TEAL + (255,))
img.alpha_composite(arc)

# --- equalizer / orchestra bars (heights swell to a peak) ---
bars = Image.new('RGBA', (S, S), (0, 0, 0, 0))
bd = ImageDraw.Draw(bars)
grad = hgrad3(640, S, ACCENT, VIOLET, TEAL)
gx0 = 192
bottom = 726
heights = [188, 322, 430, 322, 188]
bw, gap = 96, 40
x = gx0
for hgt in heights:
    top = bottom - hgt
    bd.rounded_rectangle([x, top, x + bw, bottom], radius=48, fill=(255, 255, 255, 255))
    x += bw + gap
# clip the horizontal gradient to the bar shapes
bar_mask = bars.split()[3]
bars_grad = Image.new('RGBA', (S, S), (0, 0, 0, 0))
bars_grad.paste(grad, (gx0, 0))
bars_grad.putalpha(bar_mask)
img.alpha_composite(bars_grad)

img.putalpha(mask)
master = os.path.join(OUT, 'openorchestra_1024.png')
img.save(master)
print('wrote', master)

# --- iconset + icns ---
iconset = os.path.join(OUT, 'openOrchestra.iconset')
os.makedirs(iconset, exist_ok=True)
specs = [(16,1),(16,2),(32,1),(32,2),(128,1),(128,2),(256,1),(256,2),(512,1),(512,2)]
for size, scale in specs:
    px = size * scale
    im = img.resize((px, px), Image.LANCZOS)
    name = f'icon_{size}x{size}{"@2x" if scale==2 else ""}.png'
    im.save(os.path.join(iconset, name))

# pack .icns directly (OSType -> PNG), so the app has an icon without iconutil
ICNS_TYPES = {
    'icp4': 16, 'icp5': 32, 'ic07': 128, 'ic08': 256, 'ic09': 512,
    'ic11': 32, 'ic12': 64, 'ic13': 256, 'ic14': 512, 'ic10': 1024,
}
chunks = b''
for ostype, px in ICNS_TYPES.items():
    im = img.resize((px, px), Image.LANCZOS)
    import io
    buf = io.BytesIO(); im.save(buf, format='PNG'); data = buf.getvalue()
    chunks += ostype.encode('ascii') + struct.pack('>I', len(data) + 8) + data
icns = b'icns' + struct.pack('>I', len(chunks) + 8) + chunks
icns_path = os.path.join(OUT, 'openOrchestra.icns')
with open(icns_path, 'wb') as f:
    f.write(icns)
print('wrote', icns_path, len(icns), 'bytes')
