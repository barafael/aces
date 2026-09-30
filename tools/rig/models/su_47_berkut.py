"""Su-47 Berkut (Carlos.Maciel, CC BY 4.0).

The moving surfaces are separate objects (upper and lower skins as
pairs), so nothing is cut but the canards (both sides are one object);
hinges are measured from the parts' own geometry.

- canards (Object_19, split at the centreline): all-moving, pivot at mid
  root chord.
- forward-swept wing, three trailing-edge surfaces per side: outboard
  ailerons (Object_39+41 / 43+45), mid flaperons (31+33 / 35+37) and
  inboard flaperons (47+49 / 51+53) — elevons, roll-dominant outboard.
- stabilators (Object_55+57 / 61+63): all-moving, pitch 15° + 8°
  differential, pivot at ~45 % of the root chord.
- rudders (Object_67 / 69): hinged on their leading edge.
- static: leading-edge flaps and the weapon-bay door strips.
- no landing gear: the model is gear-up.
- nozzles: read off the rear view.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "su-47_berkut.glb"
FIXUP = ((0.0, -1.03, 0.86), pi, 0.9229)


def _verts(o):
    return [o.matrix_world @ v.co for v in o.data.vertices]


def leading_edge(o, span_axis=0, frac=0.06):
    vs = _verts(o)
    lo = min(v[span_axis] for v in vs)
    hi = max(v[span_axis] for v in vs)
    band = (hi - lo) * frac
    out = []
    for end in (lo, hi):
        near = [v for v in vs if abs(v[span_axis] - end) <= band]
        p = Vector(max(near, key=lambda v: v.y))
        other = 2 if span_axis != 2 else 0
        p[other] = sum(v[other] for v in near) / len(near)
        out.append(p)
    return out


def pivot(o, frac):
    vs = _verts(o)
    ax = [abs(v.x) for v in vs]
    lo, hi = min(ax), max(ax)
    band = (hi - lo) * 0.08
    root = [v for v in vs if abs(v.x) <= lo + band]
    le, te = max(v.y for v in root), min(v.y for v in root)
    y = le - frac * (le - te)
    sx = 1.0 if sum(v.x for v in vs) > 0 else -1.0
    out = []
    for end in (lo, hi):
        near = [v for v in vs if abs(abs(v.x) - end) <= band]
        out.append(Vector((end * sx, y, sum(v.z for v in near) / len(near))))
    return out


def named(*nums):
    return [f"Object_{n}" for n in nums]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    canards = obj("Object_19")
    for side_name, sx in (("R", 1), ("L", -1)):
        m = (lambda pts: pts) if sx > 0 else mirror_x
        c = cut_outline(f"canard.{side_name}", m([(0.0, -1.5), (3.6, -1.5), (3.6, 2.0), (0.0, 2.0)]),
                        objs=[canards], slice_edges=False)
        a, b = pivot(c, 0.5)
        canard(c, a, b)

    surfaces = {
        "R": (("aileron", (43, 45)), ("flaperon.mid", (35, 37)), ("flaperon.inboard", (51, 53))),
        "L": (("aileron", (39, 41)), ("flaperon.mid", (31, 33)), ("flaperon.inboard", (47, 49))),
    }
    for side_name, parts in surfaces.items():
        for name, nums in parts:
            o = take(named(*nums), f"{name}.{side_name}")
            a, b = leading_edge(o)
            if name == "aileron":
                aileron(o, a, b)
            elif name == "flaperon.mid":
                elevon(o, a, b, pitch=10.0, roll=15.0)
            else:
                elevon(o, a, b, pitch=15.0, roll=10.0)

    for side_name, nums in (("R", (61, 63)), ("L", (55, 57))):
        s = take(named(*nums), f"stabilator.{side_name}")
        a, b = pivot(s, 0.45)
        stabilator(s, a, b, pitch=15.0, roll=8.0)

    for side_name, num in (("R", 69), ("L", 67)):
        r = take(named(num), f"rudder.{side_name}")
        a, b = leading_edge(r, 2)
        rudder(r, a, b)

    for sx in (1, -1):
        nozzle(f"nozzle.{'R' if sx > 0 else 'L'}", (0.57 * sx, -9.2, -0.84), 0.5)
