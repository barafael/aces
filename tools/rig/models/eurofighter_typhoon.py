"""Eurofighter Typhoon (robnewman76, CC BY 4.0) — a game prop.

Named objects; one untextured-looking (black) material, so there are no
panel lines: surfaces are cut by geometry.

- canards: the model's own LeftCanard / RightCanard, pivoting spanwise
  about the CanardPivot stub (anhedral followed by ``hinge``).
- flaperons: inboard + outboard on each wing's trailing edge (elevon
  preset), cut from MainFuse along a hinge line ~0.75 m ahead of the
  trailing edge (the real chord split is not modeled).
- rudder: the lower ~75 % of the fin's trailing edge, cut from Tail.
- no landing gear is modeled (nothing to retract).
- the "Stabilon" objects are small spine fittings, not surfaces — static.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "eurofighter_typhoon_game_prop.glb"
FIXUP = ((0.0, -2.23, -0.27), pi, 0.0986)

WING_DEPTH = (-1.75, -1.33)
# Right wing hinge line and trailing edge, split inboard / outboard.
INBOARD = [(1.36, -4.24), (3.30, -4.19), (3.30, -5.2), (1.36, -5.2)]
OUTBOARD = [(3.30, -4.19), (5.28, -4.14), (5.28, -5.2), (3.30, -5.2)]
# Fin (side view y, z): trailing-edge rudder.
RUDDER = [(-6.05, -0.55), (-7.08, 1.65), (-7.9, 1.65), (-6.9, -0.55)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    wing_src = [obj("MainFuse_Lo_lambert1_0")]
    for side_name, sx in (("R", 1), ("L", -1)):
        m = (lambda pts: pts) if sx > 0 else mirror_x
        for name, pts in (("inboard", INBOARD), ("outboard", OUTBOARD)):
            flap = cut_outline(f"flaperon.{name}.{side_name}", m(pts), depth=WING_DEPTH,
                               objs=wing_src)
            (a, b) = m([pts[0], pts[1]])
            if name == "inboard":
                elevon(flap, a, b, pitch=15.0, roll=15.0)
            else:
                elevon(flap, a, b, pitch=10.0, roll=20.0)

        canard_obj = take([f"{'Right' if sx > 0 else 'Left'}Canard_Lo_lambert1_0"],
                          f"canard.{side_name}")
        canard(canard_obj, (0.50 * sx, 4.95), (1.80 * sx, 4.95))

    fin = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.3, 0.3),
                      objs=[obj("Tail_Lo_lambert1_0")])
    rudder(fin, (-6.05, -0.50), (-7.08, 1.65))

    for sx in (1, -1):
        nozzle(f"nozzle.{'R' if sx > 0 else 'L'}", (0.48 * sx, -6.90, -1.30), 0.38)
