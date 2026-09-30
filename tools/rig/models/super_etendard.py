"""Dassault Super Étendard (helijah, CC BY 4.0).

Loose pieces (upper/lower skins separate). Ailerons, tailplane and rudder
are cut along the texture's panel lines; gear parts are whole pieces.

- ailerons: between the flaps (not animated) and the wing fold.
- tailplane: rigged as an all-moving stabilator (the texture also shows
  elevator panels; they move with it).
- rudder: above the tailplane.
- main gear (leg, wheel, leg doors) swings inward about a fore-aft hinge
  at the wing towards the fuselage bay; it hides once stowed (the flat
  wheel would still show below the wing root, the leg doors end askew).
- nose gear (with its leg-mounted front door) swings aft into the bay; the
  bay doors, closed on the ground, open for it and close behind it.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "super-etendard.glb"
FIXUP = ((0.0, 0.07, 0.0), -pi / 2, 1.0)

RIGHT_AILERON = [(2.80, -2.93), (3.86, -3.62), (3.86, -4.09), (2.80, -3.29)]
RIGHT_STAB = [(0.2, -4.1), (1.72, -5.78), (1.74, -7.12), (0.2, -6.32)]
# Side view (y, z).
RUDDER = [(-5.68, 0.88), (-6.67, 2.23), (-7.14, 2.24), (-6.23, 0.88)]
# Plan view: one nose bay door (right; mirrored for the left).
NOSE_DOOR = [(0.0, 1.97), (0.27, 1.97), (0.27, 3.11), (0.0, 3.11)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, sx in (("R", 1), ("L", -1)):
        mirror = (lambda pts: pts) if sx > 0 else mirror_x
        ail = cut_outline(f"aileron.{name}", mirror(RIGHT_AILERON), depth=(-0.8, -0.4))
        aileron(ail, (2.80 * sx, -2.93), (3.86 * sx, -3.62))
        stab = cut_outline(f"stabilator.{name}", mirror(RIGHT_STAB), depth=(0.6, 0.86))
        stabilator(stab, (0.25 * sx, -5.6), (1.7 * sx, -5.6), pitch=15.0)

        lo, hi = sorted((1.5 * sx, 2.05 * sx))
        leg = cut_pieces(f"gear.main.{name}", (lo, -2.5, -1.95), (hi, -0.9, -0.5))
        gear(leg, "gear", (1.63 * sx, -1.9, -0.58), (1.63 * sx, -1.0, -0.58), 90.0 * sx,
             hide=1.0)

        # Nose bay door: open for the gear, closed again behind it.
        door = cut_outline(f"door.nose.{name}", mirror(NOSE_DOOR), depth=(-0.95, -0.78))
        part(door, "door", (0.26 * sx, 1.97, -0.87), (0.26 * sx, 3.11, -0.87),
             gear=[(0.0, 0.15, -80.0 * sx), (0.85, 1.0, 80.0 * sx)])

    rud = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.2, 0.2))
    rudder(rud, (-5.68, 0.88), (-6.67, 2.23))

    nose = cut_pieces("gear.nose", (-0.25, 3.1, -2.3), (0.25, 4.0, -0.86))
    strut = cut_pieces("gear.nose.strut", (-0.07, 3.6, -1.7), (0.07, 3.8, -0.5))
    nose = join([nose, strut], "gear.nose")
    gear(nose, "gear", (-0.2, 3.70, -0.55), (0.2, 3.70, -0.55), -90.0, t=(0.15, 0.85))

    nozzle("nozzle", (0.0, -6.15, -0.31), 0.34)
