"""Analysis renders of one model — run inside Blender:

    blender -b --factory-startup -P tools/rig/survey.py -- <model> <outdir>
        [--split] [--focus x0 y0 z0 x1 y1 z1] [--views top,left,...]
        [--color texture|object] [--prefix name] [--raw] [--min-size m]

``<model>`` is a rig script in ``tools/rig/models/`` (without ``.py``);
its ``SOURCE`` and ``FIXUP`` are loaded (``--raw``: the original, *not*
the rig script's cuts). By default the rig script's ``build()`` runs
first, so the renders show its parts; ``--raw`` stops after the fixup.

``--split`` separates every mesh into its connected pieces and colours
them one by one (``--color object``); ``--focus`` frames a box (Blender
airframe coordinates) and labels only the pieces centred inside it
(``--min-size``: whose bounding-box diagonal is at least that many meters). A
table of every labeled object (faces, bounding box) is printed and saved
as ``<outdir>/<prefix>_pieces.txt``. Then run ``tools/rig/annotate.py
<outdir>`` for the meter grid.
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "models"))
import rigkit as rk  # noqa: E402
from mathutils import Vector  # noqa: E402

argv = rk.args()
model, outdir = argv[0], argv[1]
opts = argv[2:]


def opt(name, n=1, default=None):
    if name not in opts:
        return default
    i = opts.index(name)
    return opts[i + 1 : i + 1 + n] if n > 1 else opts[i + 1]


mod = importlib.import_module(model)
if "--raw" in opts:
    rk.load(mod.SOURCE)
    rk.fixup(*mod.FIXUP)
else:
    mod.build()

focus = opt("--focus", 6)
box = None
if focus:
    f = [float(x) for x in focus]
    box = (Vector(f[:3]), Vector(f[3:]))

objs = rk.meshes()
if "--split" in opts:
    pieces = []
    for o in list(objs):
        if "aces" in o:
            pieces.append(o)
            continue
        pieces += rk.split_loose(o, o.name[:10])
    objs = pieces


def center(o):
    lo, hi = rk.bounds([o])
    return (lo + hi) / 2


def size(o):
    lo, hi = rk.bounds([o])
    return (hi - lo).length


min_size = float(opt("--min-size", default="0"))
if box:
    labeled = [o for o in objs if all(box[0][i] <= center(o)[i] <= box[1][i] for i in range(3))
               and size(o) >= min_size]
else:
    labeled = [o for o in objs if "aces" in o] or objs
labeled += [o for o in rk.bpy.data.objects if o.type == "EMPTY" and "aces" in o]
color = opt("--color", default="object" if "--split" in opts else "texture")
rk.palette([o for o in labeled if o.type == "MESH"])
views = (opt("--views") or "top,left,rear,bottom").split(",")
prefix = opt("--prefix", default="inspect")
rk.render(outdir, prefix, views=views, color=color, labels=labeled, focus=box,
          persp=box is None)

lines = []
for o in labeled:
    if o.type != "MESH":
        lines.append(f"{o.name:28} empty at {tuple(round(v, 2) for v in o.location)}")
        continue
    lo, hi = rk.bounds([o])
    lines.append(f"{o.name:28} faces={len(o.data.polygons):6} "
                 f"x[{lo.x:6.2f},{hi.x:6.2f}] y[{lo.y:6.2f},{hi.y:6.2f}] z[{lo.z:6.2f},{hi.z:6.2f}] "
                 f"mat={o.data.materials[0].name if o.data.materials else '-'}")
text = "\n".join(lines)
print(text)
with open(os.path.join(outdir, f"{prefix}_pieces.txt"), "w") as fh:
    fh.write(text + "\n")
