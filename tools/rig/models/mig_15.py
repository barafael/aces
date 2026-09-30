"""MiG-15 (Vermishel, CC BY 4.0).

One mesh of many loose pieces. Surfaces are cut along the panel lines of
the texture (outlines read off ``survey.py`` renders); the gear legs,
wheels, leg doors and retraction struts are whole pieces.

- ailerons: outboard trailing edge; elevators: whole tailplane trailing
  edge; rudder: two pieces, below and above the tailplane.
- main gear retracts inward into the wing roots; the nose gear forward
  into the nose. Both hide once stowed (no gear bays modeled).
"""

from math import pi

from rigkit import *  # noqa: F403

SOURCE = "mig-15.glb"
FIXUP = ((0.0, -1.84, -1.66), pi, 0.6445)

RIGHT_AILERON = [(3.15, -0.64), (4.60, -1.43), (4.55, -1.76), (3.15, -0.97)]
RIGHT_ELEVATOR = [(0.12, -4.09), (1.62, -4.90), (1.58, -5.07), (0.12, -4.44)]
# Side view (y, z): the rudder is split by the tailplane.
LOWER_RUDDER = [(-2.98, -0.06), (-3.84, 0.87), (-4.41, 0.87), (-3.49, -0.10)]
UPPER_RUDDER = [(-4.02, 1.06), (-4.72, 1.82), (-5.05, 1.75), (-5.05, 1.06)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for side_name, sx in (("R", 1), ("L", -1)):
        mirror = (lambda pts: pts) if sx > 0 else mirror_x
        ail = cut_outline(f"aileron.{side_name}", mirror(RIGHT_AILERON), depth=(-0.88, -0.45))
        aileron(ail, (3.15 * sx, -0.64), (4.60 * sx, -1.43))
        elev = cut_outline(f"elevator.{side_name}", mirror(RIGHT_ELEVATOR), depth=(0.8, 1.1))
        elevator(elev, (0.12 * sx, -4.09), (1.58 * sx, -4.90))

        # Main gear: the retraction strut and the leg's top fittings (every
        # piece up under the wing) fold away — they would swing through the
        # wing, so they are gone early; leg, wheel and leg door swing
        # inward about a fore-aft hinge inside the wing.
        lo_x, hi_x = sorted((1.2 * sx, 2.3 * sx))
        strut = cut_pieces(f"gear.strut.{side_name}", (lo_x, 0.3, -1.0), (hi_x, 1.95, -0.5))
        gear(strut, "gear", (1.98 * sx, 0.6, -0.68), (1.98 * sx, 1.2, -0.68), 90.0 * sx,
             hide=0.25)
        lo_x, hi_x = sorted((1.2 * sx, 2.3 * sx))
        leg = cut_pieces(f"gear.main.{side_name}", (lo_x, 0.3, -1.95), (hi_x, 1.95, -0.5))
        gear(leg, "gear", (1.98 * sx, 0.6, -0.68), (1.98 * sx, 1.2, -0.68), 90.0 * sx,
             hide=1.0)

    lower = cut_outline("rudder.lower", LOWER_RUDDER, plane="yz", depth=(-0.15, 0.15))
    rudder(lower, (-2.98, -0.06), (-3.84, 0.87))
    upper = cut_outline("rudder.upper", UPPER_RUDDER, plane="yz", depth=(-0.15, 0.15))
    rudder(upper, (-4.02, 1.06), (-4.68, 1.77))

    nose = cut_pieces("gear.nose", (-0.3, 3.45, -1.95), (0.3, 4.45, -1.15))
    gear(nose, "gear", (-0.2, 4.10, -1.24), (0.2, 4.10, -1.24), 90.0, hide=1.0)

    # Single engine: the tailpipe exits under the rudder.
    nozzle("nozzle", (0.0, -3.12, -0.56), 0.30)
