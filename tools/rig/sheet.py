#!/usr/bin/env python3
"""Tile renders into one captioned contact sheet:

    python3 tools/rig/sheet.py <out.png> <cols> <image>...

Each tile is captioned with its file name; tiles are scaled to 700 px wide.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

out, cols, paths = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
tiles = [Image.open(p).convert("RGB") for p in paths]
w = 700
tiles = [t.resize((w, int(t.height * w / t.width))) for t in tiles]
h = max(t.height for t in tiles)
rows = (len(tiles) + cols - 1) // cols
sheet = Image.new("RGB", (cols * w, rows * (h + 24)), (255, 255, 255))
draw = ImageDraw.Draw(sheet)
try:
    font = ImageFont.truetype("DejaVuSans.ttf", 16)
except OSError:
    font = ImageFont.load_default()
for i, (t, p) in enumerate(zip(tiles, paths)):
    x, y = (i % cols) * w, (i // cols) * (h + 24)
    draw.text((x + 6, y + 3), Path(p).stem, fill=(0, 0, 0), font=font)
    sheet.paste(t, (x, y + 24))
sheet.save(out)
