"""Su-25 (tnikita, CC BY 4.0).

The airframe is split into a left and right half mesh; surfaces are cut
along the texture's panel lines, gear parts are whole pieces.

- ailerons: outboard trailing edge up to the wingtip pods (the pods' split
  airbrakes stay closed); elevators: the tailplane's full-span trailing
  edge; rudder: upper and lower sections (split like the real one).
- main gear: legs, wheels and fittings fold forward (the wheel sits
  outboard of the nacelle, so it hides once up); the belly bay doors swing
  outward shut (0.75 → 1). Nose gear folds forward and hides; its two side
  doors fold up flush (0.75 → 1).
- nozzles: both nacelle exits.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "su-25.glb"
FIXUP = ((0.0, -2.23, -0.41), pi, 0.048)

RIGHT_AILERON = [(4.24, -1.62), (6.26, -1.62), (6.26, -2.10), (4.24, -2.10)]
RIGHT_ELEVATOR = [(0.30, -5.82), (2.22, -5.82), (2.22, -6.40), (0.30, -6.40)]
UPPER_RUDDER = [(-5.44, 1.03), (-5.48, 1.47), (-6.20, 1.47), (-6.20, 1.03)]
LOWER_RUDDER = [(-5.30, 0.04), (-5.30, 0.99), (-6.25, 0.99), (-6.25, 0.04)]


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, s in (("R", 1.0), ("L", -1.0)):
        mirror = (lambda p: p) if s > 0 else mirror_x
        ail = cut_outline(f"aileron.{name}", mirror(RIGHT_AILERON), depth=(-0.85, -0.15))
        aileron(ail, (4.24 * s, -1.64), (6.26 * s, -1.64))
        elev = cut_outline(f"elevator.{name}", mirror(RIGHT_ELEVATOR), depth=(-0.8, 0.4))
        elevator(elev, (0.30 * s, -5.83), (2.22 * s, -5.83))

        # Main gear: leg, wheel and fittings fold forward into the nacelle
        # side (the wheel would stay outboard: hidden once up).
        x0, x1 = sorted((0.58 * s, 1.52 * s))
        leg = cut_pieces(f"gear.main.{name}", (x0, -0.85, -2.35), (x1, 0.45, -0.88))
        gear(leg, "gear", (x0, 0.25, -1.0), (x1, 0.25, -1.0), 90.0, hide=1.0)
        # Bay doors hanging from the belly: swing outward shut.
        x0, x1 = sorted((0.35 * s, 0.84 * s))
        door = cut_pieces(f"gear.door.{name}", (x0, -0.4, -2.12), (x1, 0.6, -1.3))
        gear(door, "gear door", (0.38 * s, -0.37, -1.35), (0.38 * s, 0.56, -1.35), -61.0 * s,
             t=(0.75, 1.0))

        nozzle(f"nozzle.{name}", (0.77 * s, -3.68, -0.95), 0.32)

    upper = cut_outline("rudder.upper", UPPER_RUDDER, plane="yz", depth=(-0.2, 0.2))
    rudder(upper, (-5.44, 1.03), (-5.48, 1.47))
    lower = cut_outline("rudder.lower", LOWER_RUDDER, plane="yz", depth=(-0.2, 0.2))
    rudder(lower, (-5.30, 0.04), (-5.30, 0.99))

    # Nose gear folds forward; its side doors close behind it.
    nose = cut_pieces("gear.nose", (-0.2, 2.6, -2.3), (0.2, 3.6, -0.7))
    gear(nose, "gear", (-0.2, 3.3, -1.1), (0.2, 3.3, -1.1), 90.0, hide=1.0)
    for name, s, lo, hi in (("L", -1.0, (-0.6, 2.1, -1.6), (-0.2, 3.12, -1.08)),
                            ("R", 1.0, (0.2, 3.07, -1.55), (0.58, 3.6, -1.07))):
        door = cut_pieces(f"gear.nose.door.{name}", lo, hi)
        a = (0.24 * s, lo[1], -1.12)
        b = (0.24 * s, hi[1], -1.12)
        gear(door, "gear door", a, b, 51.5 * -s, t=(0.75, 1.0))
