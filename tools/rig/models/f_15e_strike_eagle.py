"""F-15E Strike Eagle (CC BY-NC-SA 4.0).

Control surfaces are separate skin pieces in this model, so everything is
taken with ``cut_pieces``:

- ailerons (outboard trailing edge), differential all-moving stabilators
  (15° pitch + 8° roll, pivot at ~45 % root chord), rudders (the lower aft
  panel of each fin). The trailing-edge flaps stay put.
- main gear: legs, wheels and bay fittings fold forward about a lateral
  hinge under the intake; the inboard bay doors close outward behind them
  (0.8 → 1). Nose gear (leg, drag brace, wheel) folds forward. No wheel
  wells are modeled, so the gear hides once stowed.
- nozzles: both afterburner exits. The LANTIRN pods stay.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "f-15e_strike_eagle_-_fighter_jet_-_free.glb"
FIXUP = ((0.0, -0.98, 4.51), pi / 2, 0.1)


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, s in (("R", 1.0), ("L", -1.0)):
        def box_x(a, b):
            return sorted((a * s, b * s))

        # Ailerons: outboard trailing edge (separate skin pieces).
        x0, x1 = box_x(3.90, 5.64)
        ail = cut_pieces(f"aileron.{name}", (x0, -5.72, -0.8), (x1, -4.30, -0.55))
        aileron(ail, (3.93 * s, -4.38), (5.61 * s, -5.09))

        # All-moving stabilators, differential (tailerons), pivot at ~45 %
        # root chord.
        x0, x1 = box_x(1.76, 4.36)
        stab = cut_pieces(f"stabilator.{name}", (x0, -9.8, -1.2), (x1, -6.3, -0.98))
        stabilator(stab, (1.85 * s, -7.7, -1.1), (4.0 * s, -7.7, -1.1), pitch=15.0, roll=8.0)

        # Rudders: the lower aft panel of each fin.
        x0, x1 = box_x(1.55, 1.95)
        rud = cut_pieces(f"rudder.{name}", (x0, -8.5, -0.45), (x1, -7.5, 1.1))
        rudder(rud, (-7.58, -0.41), (-7.92, 1.06))

        # Main gear: legs swing forward into the fuselage.
        x0, x1 = box_x(0.85, 1.9)
        leg = cut_pieces(f"gear.main.{name}", (x0, -3.0, -3.45), (x1, -1.2, -1.6))
        gear(leg, "gear", (x0, -2.2, -1.75), (x1, -2.2, -1.75), 90.0, hide=1.0)
        # Inboard bay door: closes outward behind the gear.
        x0, x1 = box_x(0.70, 0.90)
        door = cut_pieces(f"gear.door.{name}", (x0, -2.45, -2.4), (x1, -1.4, -1.8))
        a, b = (0.80 * s, -2.37, -1.85), (0.80 * s, -1.47, -1.85)
        gear(door, "gear door", a, b, -90.0 * s, t=(0.8, 1.0))

    # Nose gear (leg, drag brace, wheel) folds forward.
    nose = cut_pieces("gear.nose", (-0.35, 2.7, -3.5), (0.35, 3.97, -1.40))
    gear(nose, "gear", (-0.2, 3.35, -1.45), (0.2, 3.35, -1.45), 90.0, hide=1.0)

    for name, s in (("R", 1.0), ("L", -1.0)):
        nozzle(f"nozzle.{name}", (0.67 * s, -8.18, -1.01), 0.5)
