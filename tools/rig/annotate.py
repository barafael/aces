#!/usr/bin/env python3
"""Draw a meter grid and part labels onto ``rigkit.render()`` output.

    python3 tools/rig/annotate.py <dir>        # every <view>.png with a .json

Grid: a thin line every meter, a labeled line every 2 m, in the view's own
airframe axes (Blender frame: +X right wing, +Y nose, +Z up) — so a point
read off the picture can go straight into a rig script. Writes
``<name>_grid.png`` next to each render.
"""

import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

AXES = "xyz"


def font(size):
    for name in ("DejaVuSans.ttf", "/usr/share/fonts/TTF/DejaVuSans.ttf",
                 "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            pass
    return ImageFont.load_default()


def annotate(png: Path):
    meta = json.loads(png.with_suffix(".json").read_text())
    img = Image.open(png).convert("RGB")
    draw = ImageDraw.Draw(img, "RGBA")
    w, h = meta["width"], meta["height"]
    scale = meta["ortho_scale"]  # meters across the wider image side
    px_per_m = max(w, h) / scale
    right, up, center = meta["right"], meta["up"], meta["center"]
    # Which airframe axis runs along the image's x / y, and its sign.
    ax_r = max(range(3), key=lambda i: abs(right[i]))
    ax_u = max(range(3), key=lambda i: abs(up[i]))
    sr, su = math.copysign(1, right[ax_r]), math.copysign(1, up[ax_u])
    small, big = font(14), font(18)

    def to_px(coord_r, coord_u):
        x = w / 2 + (coord_r - center[ax_r]) * sr * px_per_m
        y = h / 2 - (coord_u - center[ax_u]) * su * px_per_m
        return x, y

    half_r, half_u = w / 2 / px_per_m, h / 2 / px_per_m
    for m in range(math.floor(center[ax_r] - half_r), math.ceil(center[ax_r] + half_r) + 1):
        x, _ = to_px(m, 0)
        major = m % 2 == 0
        draw.line([(x, 0), (x, h)], fill=(0, 90, 200, 110 if major else 45), width=1)
        if major:
            draw.text((x + 2, h - 18), f"{AXES[ax_r]}={m}", fill=(0, 40, 120, 255), font=small,
                      stroke_width=2, stroke_fill=(255, 255, 255, 255))
    for m in range(math.floor(center[ax_u] - half_u), math.ceil(center[ax_u] + half_u) + 1):
        _, y = to_px(0, m)
        major = m % 2 == 0
        draw.line([(0, y), (w, y)], fill=(0, 90, 200, 110 if major else 45), width=1)
        if major:
            draw.text((2, y + 1), f"{AXES[ax_u]}={m}", fill=(0, 40, 120, 255), font=small,
                      stroke_width=2, stroke_fill=(255, 255, 255, 255))
    for label in meta["labels"]:
        x, y = label["u"] * w, (1 - label["v"]) * h
        draw.ellipse([x - 4, y - 4, x + 4, y + 4], fill=(220, 0, 0, 255))
        draw.text((x + 6, y - 9), label["name"], fill=(0, 0, 0, 255), font=big,
                  stroke_width=3, stroke_fill=(255, 255, 255, 255))
    draw.text((8, 6), f"{meta['view']}  (image right = {'+' if sr > 0 else '-'}{AXES[ax_r]}, "
              f"up = {'+' if su > 0 else '-'}{AXES[ax_u]})", fill=(0, 0, 0, 255), font=big)
    img.save(png.with_name(png.stem + "_grid.png"))


if __name__ == "__main__":
    for d in sys.argv[1:]:
        for png in sorted(Path(d).glob("*.png")):
            if png.with_suffix(".json").exists() and not png.stem.endswith("_grid"):
                annotate(png)
