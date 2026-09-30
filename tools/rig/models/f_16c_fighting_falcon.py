"""F-16C Fighting Falcon (Carlos.Maciel, CC BY 4.0).

Every moving surface is its own object in the source, so nothing is cut;
hinges are measured from the parts' own geometry.

- flaperons (Object_20 / _22): roll-dominant elevons hinged on their
  leading edge.
- stabilators (Object_18 / _36): all-moving, pitch 15° + 8° differential
  (tailerons), pivot at ~45 % of the root chord, following the anhedral.
- rudder (Object_40): hinged on its leading edge.
- static: leading-edge flaps (Object_32 / _34) and the split speedbrakes
  (Object_24.._30) — no command drives them yet.
- no landing gear: the model is gear-up.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403


SOURCE = "f16-c_falcon.glb"
FIXUP = ((0.01, -17.85, 1.33), 0.0, 1.648)


def _verts(o):
    return [o.matrix_world @ v.co for v in o.data.vertices]


def leading_edge(o, span_axis, frac=0.06):
    """Hinge points on the leading edge (max y) at both span ends."""
    vs = _verts(o)
    lo = min(v[span_axis] for v in vs)
    hi = max(v[span_axis] for v in vs)
    band = (hi - lo) * frac
    out = []
    for end in (lo, hi):
        near = [v for v in vs if abs(v[span_axis] - end) <= band]
        le = max(near, key=lambda v: v.y)
        mid = sum((v for v in near), Vector()) / len(near)
        p = Vector(le)
        # Mid-thickness at the hinge.
        other = 2 if span_axis != 2 else 0
        p[other] = mid[other]
        out.append(p)
    return out


def pivot(o, frac):
    """Spanwise pivot line at ``frac`` of the root chord from its leading
    edge, at the root and tip (mid-thickness)."""
    vs = _verts(o)
    xs = [abs(v.x) for v in vs]
    lo, hi = min(xs), max(xs)
    band = (hi - lo) * 0.08
    out = []
    root = [v for v in vs if abs(v.x) <= lo + band]
    le = max(v.y for v in root)
    te = min(v.y for v in root)
    y = le - frac * (le - te)
    for end in (lo, hi):
        near = [v for v in vs if abs(abs(v.x) - end) <= band]
        z = sum(v.z for v in near) / len(near)
        out.append(Vector((end if near[0].x > 0 else -end, y, z)))
    return out


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for src, side_name in (("Object_20", "R"), ("Object_22", "L")):
        f = take([src], f"flaperon.{side_name}")
        a, b = leading_edge(f, 0)
        elevon(f, a, b, pitch=5.0, roll=20.0)

    for src, side_name in (("Object_18", "R"), ("Object_36", "L")):
        s = take([src], f"stabilator.{side_name}")
        a, b = pivot(s, 0.45)
        stabilator(s, a, b, pitch=15.0, roll=8.0)

    r = take(["Object_40"], "rudder")
    a, b = leading_edge(r, 2)
    rudder(r, a, b)

    nozzle("nozzle", (0.0, -6.92, -1.01), 0.45)
