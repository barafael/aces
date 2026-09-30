"""MiG-19 (Chenchanchong, CC BY 4.0).

One main mesh; surfaces are cut along the panel lines of the texture:

- ailerons: outboard trailing edge (tapered, inboard of the tip), 20°.
- stabilators: the low-set all-moving tailplane halves, 15° pitch, pivot
  at mid root chord (no differential on the MiG-19).
- rudder: the fin's aft panel below the tip.
- modeled gear-up: no landing gear; nozzles at both jet pipes.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "mikoyan-gurevich_mig-19.glb"
FIXUP = ((0.0, -0.63, -1.44), 0.0, 0.45)

RIGHT_AILERON = [(2.61, -0.86), (4.34, -2.74), (4.36, -3.08), (2.61, -1.55)]
RIGHT_STABILATOR = [(0.78, -2.40), (2.50, -4.35), (2.50, -6.05), (0.78, -4.40)]
RUDDER = [(-4.39, 0.09), (-5.73, 1.44), (-6.28, 1.52), (-4.92, 0.09)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, s in (("R", 1.0), ("L", -1.0)):
        mirror = (lambda p: p) if s > 0 else mirror_x
        ail = cut_outline(f"aileron.{name}", mirror(RIGHT_AILERON), depth=(-1.1, -0.6))
        aileron(ail, (2.61 * s, -0.86), (4.34 * s, -2.74))
        stab = cut_outline(f"stabilator.{name}", mirror(RIGHT_STABILATOR), depth=(-0.48, -0.18))
        # Inner skins and the tip body along the outline's edges.
        x0, x1 = sorted((0.70 * s, 2.6 * s))
        rest = cut_pieces(f"stabilator.{name}.rest", (x0, -6.2, -0.46), (x1, -2.3, -0.2))
        stab = join([stab, rest], f"stabilator.{name}")
        x0, x1 = sorted((0.78 * s, 2.2 * s))
        stabilator(stab, (x0, -3.5, -0.32), (x1, -3.5, -0.32), pitch=15.0)
        nozzle(f"nozzle.{name}", (0.34 * s, -4.5, -0.58), 0.28)

    rud = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.15, 0.15))
    rudder(rud, (-4.39, 0.09), (-5.73, 1.44))
