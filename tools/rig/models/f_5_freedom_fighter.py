"""Northrop F-5 Freedom Fighter (Pan_Ar4ik, CC BY 4.0).

The surfaces are separate objects in the source (Maya polySurfaces), so
nothing is cut; hinges are measured from the parts' own geometry.

- ailerons (polySurface22 / 31): hinged on their leading edge.
- stabilators (polySurface18 + 19 / 27 + 28): all-moving, pivot at ~40 %
  of the root chord.
- rudder (polySurface37): hinged on its leading edge.
- nozzles: centred on the two nacelles' exits (polySurface20 / 29 — the
  engine pair sits ~0.1 m left of the fuselage centreline in the model).
- static: the inboard flaps are part of the wing skin.
- no landing gear: the model is gear-up.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "northrop_f-5_freedom_fighter.glb"
FIXUP = ((0.12, -2.88, 0.12), 0.0, 115.0)


def _named(tag):
    return [o.name for o in meshes() if f" {tag}_" in o.name]


def _verts(o):
    return [o.matrix_world @ v.co for v in o.data.vertices]


def leading_edge(o, span_axis, frac=0.06):
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
    sx = 1.0 if vs[0].x > 0 else -1.0
    out = []
    for end in (lo, hi):
        near = [v for v in vs if abs(abs(v.x) - end) <= band]
        out.append(Vector((end * sx, y, sum(v.z for v in near) / len(near))))
    return out


def exit_of(o, depth=0.4):
    """Centre and radius of a nacelle's aft end."""
    vs = _verts(o)
    y0 = min(v.y for v in vs)
    ring = [v for v in vs if v.y <= y0 + depth]
    lo = Vector(map(min, *ring))
    hi = Vector(map(max, *ring))
    c = (lo + hi) / 2
    return Vector((c.x, y0, c.z)), min(hi.x - lo.x, hi.z - lo.z) / 2


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for tags, side_name in ((("polySurface22",), "R"), (("polySurface31",), "L")):
        a_obj = take(sum((_named(t) for t in tags), []), f"aileron.{side_name}")
        a, b = leading_edge(a_obj, 0)
        aileron(a_obj, a, b)

    for tags, side_name in ((("polySurface18", "polySurface19"), "R"),
                            (("polySurface27", "polySurface28"), "L")):
        s = take(sum((_named(t) for t in tags), []), f"stabilator.{side_name}")
        a, b = pivot(s, 0.40)
        stabilator(s, a, b, pitch=15.0)

    r = take(_named("polySurface37"), "rudder")
    a, b = leading_edge(r, 2)
    rudder(r, a, b)

    for tag, side_name in (("polySurface20", "R"), ("polySurface29", "L")):
        # Centre from the nacelle's aft end; radius from the rear view (the
        # nacelle mesh's aft ring is only the lip).
        c, _ = exit_of(obj(_named(tag)[0]))
        nozzle(f"nozzle.{side_name}", c, 0.22)
