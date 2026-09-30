"""JAS 39 Gripen (helijah, CC BY 4.0).

Two big meshes of loose pieces. Canards are whole pieces (upper and lower
skins); elevons and rudder are cut along the texture's panel lines; the
gear legs, wheels and their fittings are whole pieces.

- canards: all-moving about a lateral shaft at mid root chord.
- elevons: inboard and outboard per wing, both pitch + roll.
- rudder: cut along its panel line, below the fin-tip fairing.
- main gear swings forward into the fuselage bays; the curved doors
  hanging out beside the legs fold against the fuselage side and hide
  once stowed (no closed position modeled). Nose gear
  swings forward and its two side doors swing shut. Both legs hide once
  stowed (the wheels would still show below the bay openings); the nose
  gear's retraction jack hides early.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "jas39_gripen.glb"
FIXUP = ((0.0, 0.04, 0.0), -pi / 2, 1.0)

HINGE_Y = -3.88
# Right wing, plan view: inboard and outboard elevon.
INNER_ELEVON = [(0.72, HINGE_Y), (2.24, HINGE_Y), (2.24, -4.56), (0.72, -4.62)]
OUTER_ELEVON = [(2.24, HINGE_Y), (3.63, HINGE_Y), (3.63, -4.46), (2.24, -4.56)]
# Side view (y, z).
RUDDER = [(-5.54, 0.31), (-5.66, 1.61), (-6.14, 1.66), (-6.22, 0.27)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, sx in (("R", 1), ("L", -1)):
        mirror = (lambda pts: pts) if sx > 0 else mirror_x
        for part_name, outline, (x0, x1) in (("elevon.inner", INNER_ELEVON, (0.72, 2.24)),
                                             ("elevon.outer", OUTER_ELEVON, (2.24, 3.63))):
            e = cut_outline(f"{part_name}.{name}", mirror(outline), depth=(-0.6, -0.2))
            elevon(e, (x0 * sx, HINGE_Y), (x1 * sx, HINGE_Y), pitch=20.0, roll=20.0)

        lo, hi = sorted((0.8 * sx, 2.2 * sx))
        can = cut_pieces(f"canard.{name}", (lo, -0.25, -0.5), (hi, 2.5, -0.05))
        canard(can, (0.85 * sx, 1.2), (2.0 * sx, 1.2), deg=15.0)

        # Main gear: leg, wheel and fittings swing forward about the leg top.
        lo, hi = sorted((0.5 * sx, 1.32 * sx))
        leg = cut_pieces(f"gear.main.{name}", (lo, -2.5, -2.32), (hi, -1.5, -0.55))
        gear(leg, "gear", (0.81 * sx - 0.2, -2.05, -1.06), (0.81 * sx + 0.2, -2.05, -1.06),
             90.0, hide=1.0)
        # Curved leg-bay door hanging out from the fuselage side (hinged at
        # its inner top edge): folds down against the side while the gear
        # retracts, gone once stowed (the model has no closed position).
        lo, hi = sorted((0.8 * sx, 1.25 * sx))
        door = cut_pieces(f"door.main.{name}", (lo, -2.2, -1.05), (hi, -0.6, -0.7))
        gear(door, "door", (0.82 * sx, -2.1, -0.72), (0.82 * sx, -0.7, -0.72),
             54.0 * sx, hide=1.0)

    rud = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.2, 0.2))
    rudder(rud, (-5.54, 0.31), (-5.66, 1.61))

    nose = cut_pieces("gear.nose", (-0.25, 2.6, -2.35), (0.25, 3.35, -0.85))
    gear(nose, "gear", (-0.2, 2.93, -0.94), (0.2, 2.93, -0.94), 90.0, hide=1.0)
    # The retraction jack behind the leg can't follow: gone early.
    jack = cut_pieces("gear.nose.jack", (-0.05, 2.1, -1.3), (0.05, 2.85, -0.8))
    gear(jack, "gear", (-0.2, 2.93, -0.94), (0.2, 2.93, -0.94), 90.0, hide=0.3)
    # Nose bay side doors, hanging from the bay edges: swing in to close.
    for name, sx in (("R", 1), ("L", -1)):
        lo, hi = sorted((0.14 * sx, 0.24 * sx))
        door = cut_pieces(f"door.nose.{name}", (lo, 1.6, -1.33), (hi, 2.9, -1.09))
        gear(door, "door", (0.19 * sx, 1.6, -1.11), (0.19 * sx, 2.9, -1.11), 90.0 * sx,
             t=(0.8, 1.0))

    nozzle("nozzle", (0.0, -7.05, -0.31), 0.44)
