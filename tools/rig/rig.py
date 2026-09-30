"""Build one rigged model — run inside Blender:

    blender -b --factory-startup -P tools/rig/rig.py -- <model> [--check <outdir>] [--no-export]

Runs ``tools/rig/models/<model>.py``'s ``build()`` and exports
``aces-client/assets/models/<its OUTPUT or SOURCE>``. ``--check`` also
renders verification poses into ``<outdir>`` (textured, parts labeled):
neutral, full pull, full right roll, full right yaw, gear half-way and
gear up — each as ``<model>_<pose>_<view>.png``.

``tools/rig/build.sh`` rebuilds every model.
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "models"))
import rigkit as rk  # noqa: E402

argv = rk.args()
model = argv[0]
mod = importlib.import_module(model)
mod.build()

if "--check" in argv:
    outdir = argv[argv.index("--check") + 1]
    poses = {
        "neutral": {},
        "pull": {"pitch": 1.0},
        "roll": {"roll": 1.0},
        "yaw": {"yaw": 1.0},
        "gearhalf": {"gear": 0.5},
        "gearup": {"gear": 1.0},
    }
    for name, pose in poses.items():
        rk.pose(**pose)
        views = ("left", "rear", "bottom") if name.startswith("gear") else ("top", "left", "rear")
        rk.render(outdir, f"{model}_{name}", views=views, color="texture", persp=True)
    rk.pose()

if "--no-export" not in argv:
    rk.export(getattr(mod, "OUTPUT", mod.SOURCE))
