"""Northrop T-38 Talon (helijah, CC BY 4.0).

Two big meshes of loose pieces (upper/lower skins separate). Ailerons and
rudder are cut along the texture's panel lines; the stabilator halves and
all gear parts are whole pieces.

- ailerons: outboard of the (unanimated) flaps.
- stabilator: all-moving halves about a lateral shaft, following the
  tailplane's anhedral.
- rudder: the panel on the lower rear fin.
- main gear (leg, wheel, leg door) swings inward about a fore-aft hinge at
  the wing: the wheel stows flat in the fuselage behind the closed belly
  doors, the leg door ends up flush with the wing's lower skin — no hide.
- nose gear swings forward into the nose bay and hides once stowed (its
  wheel would stick out of the closed bay doors); the door hanging behind
  the leg swings up flat.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "northrop_t-38_talon.glb"
FIXUP = ((0.0, 0.0, -0.04), -pi / 2, 1.0)

# Right wing, plan view: the aileron between the flap and the wingtip pod.
RIGHT_AILERON = [(2.20, -2.27), (3.03, -2.36), (3.03, -2.81), (2.20, -2.88)]
# Side view (y, z): the rudder panel.
RUDDER = [(-5.58, -0.02), (-5.58, 0.87), (-6.08, 0.87), (-6.37, -0.02)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, sx in (("R", 1), ("L", -1)):
        mirror = (lambda pts: pts) if sx > 0 else mirror_x
        ail = cut_outline(f"aileron.{name}", mirror(RIGHT_AILERON), depth=(-0.85, -0.5))
        aileron(ail, (2.20 * sx, -2.27), (3.03 * sx, -2.36))

        lo, hi = sorted((0.55 * sx, 2.15 * sx))
        stab = cut_pieces(f"stabilator.{name}", (lo, -6.1, -0.9), (hi, -4.45, -0.6))
        stabilator(stab, (0.65 * sx, -5.2), (2.0 * sx, -5.2), pitch=15.0)

        lo, hi = sorted((1.45 * sx, 1.75 * sx))
        leg = cut_pieces(f"gear.main.{name}", (lo, -2.1, -2.0), (hi, -1.25, -0.6))
        gear(leg, "gear", (1.60 * sx, -2.0, -0.72), (1.60 * sx, -1.2, -0.72), 90.0 * sx)

    rud = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.15, 0.15))
    rudder(rud, (-5.58, -0.02), (-5.58, 0.87))

    door = cut_pieces("door.nose", (-0.2, 3.75, -1.42), (0.2, 4.02, -1.0))
    gear(door, "door", (-0.2, 4.0, -1.03), (0.2, 4.0, -1.03), -62.0, t=(0.8, 1.0))
    nose = cut_pieces("gear.nose", (-0.2, 3.85, -2.0), (0.2, 4.4, -0.88))
    gear(nose, "gear", (-0.2, 4.09, -0.98), (0.2, 4.09, -0.98), 90.0, hide=1.0)

    for name, sx in (("R", 1), ("L", -1)):
        nozzle(f"nozzle.{name}", (0.26 * sx, -6.97, -0.36), 0.19)
