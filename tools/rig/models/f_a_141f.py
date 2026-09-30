"""F/A-141F (fictional; CC BY-NC-SA 4.0).

A low-poly model without panel lines on its control surfaces, so the
surfaces follow the geometry:

- ailerons: the outboard ~45 % of each swept wing's trailing edge, a
  0.45 m strip cut along a hinge parallel to the trailing edge.
- stabilators: the tailplanes are separate flat pieces — all-moving, 15°
  pitch + 8° differential, pivot at mid root chord.
- rudders: the aft ~0.4 m of each canted fin (cut in side view; the hinge
  follows the cant).
- no landing gear is modeled (the round shapes under the engines are drop
  tanks); nozzles at both rectangular engine exits.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "f__a-141f_fighter.glb"
FIXUP = ((-14.66, 0.0, -0.04), -pi / 2, 0.016)

# Right wing trailing-edge strip (plan view): hinge a, hinge b, then past
# the trailing edge.
RIGHT_AILERON = [(4.16, -5.94), (5.61, -7.26), (5.21, -7.72), (3.76, -6.40)]
# Right fin, aft strip (side view, y z).
RIGHT_RUDDER = [(-8.45, 0.18), (-9.05, 1.53), (-9.45, 1.53), (-8.95, 0.18)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, s in (("R", 1.0), ("L", -1.0)):
        outline = RIGHT_AILERON if s > 0 else mirror_x(RIGHT_AILERON)
        ail = cut_outline(f"aileron.{name}", outline, depth=(-0.2, 0.12))
        aileron(ail, (4.16 * s, -5.94), (5.61 * s, -7.26))

        x0, x1 = sorted((1.40 * s, 4.30 * s))
        stab = cut_pieces(f"stabilator.{name}", (x0, -9.5, -0.42), (x1, -5.2, -0.24))
        stabilator(stab, (x0, -7.6, -0.33), (x1, -7.6, -0.33), pitch=15.0, roll=8.0)

        x0, x1 = sorted((0.9 * s, 2.0 * s))
        rud = cut_outline(f"rudder.{name}", RIGHT_RUDDER, plane="yz", depth=(x0, x1))
        rudder(rud, (-8.45, 0.18), (-9.05, 1.53))

        nozzle(f"nozzle.{name}", (1.0 * s, -9.05, -0.32), 0.25)
