"""Lockheed SR-71 "Blackbird" (KOG_THORNS, CC BY 4.0).

Meshes are split by material, not by function; the outboard elevons are
their own objects, everything else is cut. Hinges are measured from the
parts' geometry.

- elevons: inboard (between fuselage and nacelle, cut from object_6.001
  aft of its hinge line) and outboard (object_3.001 / 3.002); pitch 20°,
  roll 15°.
- rudders: the twin canted fins (object_4.001, split at the centreline)
  are all-moving, pivoting about their canted mid-chord line.
- main gear: each three-wheel bogie swings inward about a fore-aft hinge
  at the strut top into the fuselage; the side brace folds away early
  (hide 0.3); the outboard door plate, hinged at its top, swings inward
  (0.6..0.95) and hides at 0.9 (the curved plate cannot fold flush). Legs
  hide at 0.9 (the bogie is deeper than the modeled bay).
- nose gear: leg and twin wheels fold forward into the nose bay; the drag
  brace and a fairing box aft of the leg hide early (0.3); a centreline
  box under the belly between the mains hides early too; the side doors close inward behind it; legs hide
  once stowed.
- nozzles: the two nacelle exits.
- known leftovers (static, thin): a few brace rods just under the wing
  roots and a short stub under the nose bay stay out with the gear up.
"""

from math import pi  # noqa: F401

# Gear boxes stop below the skin (z ≈ -0.8 main, -1.4 nose): the wheel
# wells' walls and the wing skin are loose triangles just above and must
# not move (taking them leaves holes).

from rigkit import *  # noqa: F403

SOURCE = "lockheed_sr-71_blackbird.glb"
FIXUP = ((0.0, -3.09, -0.31), pi / 2, 1.1217)


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
        p.z = sum(v.z for v in near) / len(near)
        out.append(p)
    return out


def fin_pivot(o, frac=0.5):
    """The canted pivot line: mid-chord (``frac``) at root and tip."""
    vs = _verts(o)
    lo = min(v.z for v in vs)
    hi = max(v.z for v in vs)
    band = (hi - lo) * 0.08
    out = []
    for end in (lo, hi):
        near = [v for v in vs if abs(v.z - end) <= band]
        le, te = max(v.y for v in near), min(v.y for v in near)
        x = sum(v.x for v in near) / len(near)
        out.append(Vector((x, le - frac * (le - te), end)))
    return out


def box(sx, x0, x1, *yz):
    """A box on side ``sx`` between |x| = x0..x1 (y and z ranges)."""
    xs = sorted((x0 * sx, x1 * sx))
    (y0, y1), (z0, z1) = yz
    return (xs[0], y0, z0), (xs[1], y1, z1)


def pieces(name, *boxes, whole=True):
    """``cut_pieces`` over several boxes, joined into one object ``name``
    (boxes that catch nothing are skipped). ``whole=False``: a piece whose
    centre lies in a box is enough (for struts reaching up into a well —
    keep such boxes narrow, clear of the well's walls)."""
    got = []
    for i, (lo, hi) in enumerate(boxes):
        try:
            got.append(cut_pieces(f"{name}.{i}", lo, hi, whole=whole))
        except ValueError:
            pass
    if not got:
        raise ValueError(f"pieces({name!r}): nothing")
    return join(got, name) if len(got) > 1 else take([got[0].name], name)


def build():
    load(SOURCE)
    fixup(*FIXUP)

    # A centreline box with curved plates hangs under the belly between
    # the main gears (gear-related, no well to stow it): gone early.
    belly = pieces("gear.belly", ((-0.45, -5.8, -2.3), (0.45, -4.6, -1.1)), whole=False)
    gear(belly, "gear", (-0.2, -5.2, -1.2), (0.2, -5.2, -1.2), 0.0, hide=0.3)

    inner_src = [obj("object_6.001_object_6.001_0")]
    fins = obj("object_4.001_object_4.001_0")
    for side_name, sx in (("R", 1), ("L", -1)):
        m = (lambda pts: pts) if sx > 0 else mirror_x
        inner = cut_outline(f"elevon.inboard.{side_name}",
                            m([(0.45, -12.6), (3.3, -12.6), (3.3, -15.3), (0.45, -15.3)]),
                            depth=(-1.2, 0.0), objs=inner_src, slice_edges=False)
        # The panel line (top view) is straight; the piece's inner end is
        # cut diagonally by the tail cone, so its leading edge would skew.
        elevon(inner, (0.5 * sx, -12.9), (3.1 * sx, -12.95), pitch=20.0, roll=15.0)

        outer = take([f"object_3.{'001' if sx > 0 else '002'}_object_3.001_0"],
                     f"elevon.outboard.{side_name}")
        a, b = leading_edge(outer)
        elevon(outer, a, b, pitch=20.0, roll=15.0)

        fin = cut_outline(f"rudder.{side_name}", m([(0.0, -14.0), (5.0, -14.0), (5.0, -8.5),
                                                    (0.0, -8.5)]),
                          depth=(0.9, 3.2), objs=[fins], slice_edges=False)
        a, b = fin_pivot(fin)
        rudder(fin, a, b, deg=20.0)

        # Main gear.
        hinge_a = (2.29 * sx, -5.6, -0.89)
        hinge_b = (2.29 * sx, -4.4, -0.89)
        brace = pieces(f"gear.brace.{side_name}",
                       box(sx, 1.1, 2.2, (-6.5, -3.6), (-1.5, -0.9)),
                       # its upper end reaches into the bay
                       box(sx, 1.2, 2.0, (-6.5, -3.6), (-1.0, -0.8)))
        rods = pieces(f"gear.rods.{side_name}", box(sx, 1.3, 1.85, (-6.0, -4.0), (-1.25, -0.95)),
                      whole=False)
        brace = join([brace, rods], brace.name)
        gear(brace, "gear", hinge_a, hinge_b, 90.0 * sx, hide=0.3)
        door = cut_pieces(f"gear.door.{side_name}", *box(sx, 2.4, 3.0, (-6.5, -3.6), (-2.6, -0.85)))
        # The curved plate does not fold flush: it swings most of the way
        # shut, then hides (the bay outline stays).
        gear(door, "gear door", (2.55 * sx, -5.6, -0.9), (2.55 * sx, -4.4, -0.9), 90.0 * sx,
             t=(0.6, 0.95), hide=0.9)
        leg = pieces(f"gear.main.{side_name}",
                     box(sx, 1.6, 3.0, (-6.5, -3.6), (-3.1, -0.95)),
                     # the strut top, up in the bay
                     box(sx, 2.1, 2.45, (-6.5, -3.6), (-1.2, -0.8)))
        stub = pieces(f"gear.stub.{side_name}", box(sx, 2.18, 2.42, (-5.8, -4.2), (-1.35, -0.8)),
                      whole=False)
        leg = join([leg, stub], leg.name)
        gear(leg, "gear", hinge_a, hinge_b, 90.0 * sx, hide=0.9)

    # Nose gear: a fairing box and the drag brace hang aft of the leg (no
    # well for them: gone early), then the side doors, then the leg.
    fairing = pieces("gear.nose.fairing", ((-0.45, 4.3, -2.0), (0.45, 6.4, -1.36)), whole=False)
    gear(fairing, "gear", (-0.2, 6.6, -1.35), (0.2, 6.6, -1.35), 0.0, hide=0.3)
    for side_name, sx in (("R", 1), ("L", -1)):
        door = cut_pieces(f"gear.nose.door.{side_name}", *box(sx, 0.22, 0.8, (5.2, 9.0), (-2.6, -1.28)))
        gear(door, "gear door", (0.45 * sx, 6.0, -1.35), (0.45 * sx, 7.5, -1.35), 90.0 * sx,
             t=(0.8, 1.0))
    leg = pieces("gear.nose", ((-0.7, 5.2, -3.2), (0.7, 9.0, -1.45)),
                 # the strut, up between the well's walls
                 ((-0.17, 5.2, -3.2), (0.17, 9.0, -1.3)))
    gear(leg, "gear", (-0.2, 6.6, -1.35), (0.2, 6.6, -1.35), 90.0, hide=1.0)

    for sx in (1, -1):
        nozzle(f"nozzle.{'R' if sx > 0 else 'L'}", (4.11 * sx, -13.14, -0.48), 0.8)
