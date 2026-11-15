#!/usr/bin/env python3
"""Background for the installer DMG window (Pillow): an arrow from the app
icon to the Applications folder, and a one-line hint under them.

Writes assets/dmg-background.png (1x) and assets/dmg-background@2x.png.
script/package-mac.sh combines the two into one HiDPI TIFF so the window is
sharp on Retina screens.

The geometry here and the icon positions in package-mac.sh describe the same
window. Change one and you must change the other.
"""
import os

import numpy as np
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'assets')

# Window layout, in points. Must match DMG_* in script/package-mac.sh.
W, H = 720, 420
ICON = 160                 # Finder icon size
APP_X, APPS_X, ICON_Y = 200, 520, 190   # icon centres

def hex2rgb(h):
    h = h.lstrip('#')
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))

# The app icon's palette (script/gen-icon.py). The window stays light because
# Finder always draws icon labels in black, which can't be read on a dark one.
ACCENT = hex2rgb('5b9bff')
VIOLET = hex2rgb('a877f5')
TOP = hex2rgb('fbfcfe')
BOT = hex2rgb('e9edf5')
HINT = hex2rgb('5d6577')

def font(size):
    for path in ('/System/Library/Fonts/SFNS.ttf',
                 '/System/Library/Fonts/Helvetica.ttc',
                 '/Library/Fonts/Arial.ttf'):
        if os.path.exists(path):
            return ImageFont.truetype(path, size)
    return ImageFont.load_default()

def render(scale):
    w, h = W * scale, H * scale

    # Soft vertical wash.
    t = np.linspace(0, 1, h)[:, None, None]
    bg = np.array(TOP) * (1 - t) + np.array(BOT) * t
    img = Image.fromarray(np.repeat(bg, w, axis=1).astype('uint8'), 'RGB')

    # Arrow, drawn 4x larger than needed and scaled down so its edges are
    # smooth (ImageDraw polygons aren't anti-aliased).
    ss = 4
    k = scale * ss
    # A curve that bows up over the gap between the icons: a quadratic Bézier
    # from just right of the app icon to just left of Applications.
    left = (APP_X + ICON / 2 + 16) * k
    right = (APPS_X - ICON / 2 - 16) * k
    cy = (ICON_Y + 6) * k
    p0 = np.array([left, cy])
    p1 = np.array([(left + right) / 2, cy - 70 * k])   # control point
    p2 = np.array([right, cy])
    shaft = 5 * k          # half the stroke's thickness
    head_len, head_half = 26 * k, 17 * k

    def bez(t):
        return (1 - t) ** 2 * p0 + 2 * (1 - t) * t * p1 + t ** 2 * p2

    # The head points from where the stroke ends to the tip, so it lines up
    # with the line feeding into it.
    tip = p2
    ts = np.linspace(0, 1, 2000)
    pts = np.array([bez(t) for t in ts])
    joint = pts[np.argmin(np.abs(np.linalg.norm(pts - tip, axis=1) - head_len))]
    tangent = (tip - joint) / np.linalg.norm(tip - joint)
    normal = np.array([-tangent[1], tangent[0]])
    base = tip - tangent * head_len

    mask = Image.new('L', (w * ss, h * ss), 0)
    d = ImageDraw.Draw(mask)
    # Stroke the curve as a dense run of discs (round caps for free), stopping
    # where the head begins so the line doesn't poke through its tip.
    for t in np.linspace(0, 1, 600):
        x, y = bez(t)
        if np.dot(np.array([x, y]) - base, tangent) > 0:
            break
        d.ellipse([x - shaft, y - shaft, x + shaft, y + shaft], fill=255)
    d.polygon([tuple(tip), tuple(base + normal * head_half),
               tuple(base - normal * head_half)], fill=255)
    mask = mask.resize((w, h), Image.LANCZOS)

    # Fill it with the icon's accent→violet sweep across the arrow's length.
    x = np.clip((np.arange(w) - left / ss) / ((right - left) / ss), 0, 1)[None, :, None]
    sweep = np.array(ACCENT) * (1 - x) + np.array(VIOLET) * x
    sweep = Image.fromarray(np.repeat(sweep, h, axis=0).astype('uint8'), 'RGB')
    img.paste(sweep, (0, 0), mask)

    # Hint, centred under the icons and their labels.
    d = ImageDraw.Draw(img)
    text = 'Drag openOrchestra to Applications to install'
    f = font(15 * scale)
    tw = d.textlength(text, font=f)
    d.text(((w - tw) / 2, 340 * scale), text, font=f, fill=HINT)
    return img

if __name__ == '__main__':
    os.makedirs(OUT, exist_ok=True)
    for scale, name in ((1, 'dmg-background.png'), (2, 'dmg-background@2x.png')):
        path = os.path.join(OUT, name)
        # 72 dpi per point at 1x, 144 at 2x: tiffutil keys HiDPI off this.
        render(scale).save(path, dpi=(72 * scale, 72 * scale))
        print('wrote', os.path.normpath(path))
