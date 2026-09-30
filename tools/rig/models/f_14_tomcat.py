"""F-14 Tomcat (Ryan.Qin, CC BY 4.0).

Wings are modeled spread (variable sweep stays fixed; the spoilers that
roll the real aircraft are not separate, so there are no ailerons).
Surfaces and gear are whole skin pieces:

- stabilators: all-moving, 15° pitch + 8° differential (tailerons — the
  F-14's main roll control at speed), pivot at ~40 % root chord.
- rudders: the swept aft panel of each fin.
- main gear folds forward about a lateral hinge under the wing glove;
  nose gear (twin wheels) folds forward. No wells are modeled: both hide
  once up.
- nozzles: both engine exits.
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "f-14_tomcat.glb"
FIXUP = ((0.0, 0.0, 0.0), pi, 9.55)


def build():
    load(SOURCE)
    fixup(*FIXUP)

    for name, s in (("R", 1.0), ("L", -1.0)):
        def xs(a, b):
            return sorted((a * s, b * s))

        x0, x1 = xs(2.0, 5.1)
        stab = cut_pieces(f"stabilator.{name}", (x0, -9.6, -0.76), (x1, -5.3, -0.43))
        stabilator(stab, (2.1 * s, -7.0), (4.6 * s, -7.0), pitch=15.0, roll=8.0)

        x0, x1 = xs(1.30, 1.66)
        rud = cut_pieces(f"rudder.{name}", (x0, -9.25, 0.05), (x1, -7.8, 2.1))
        rudder(rud, (-7.89, 0.15), (-8.87, 2.06))

        x0, x1 = xs(2.0, 3.0)
        leg = cut_pieces(f"gear.main.{name}", (x0, -3.4, -2.55), (x1, -1.3, -0.1))
        gear(leg, "gear", (x0, -2.4, -0.45), (x1, -2.4, -0.45), 90.0, hide=1.0)

        nozzle(f"nozzle.{name}", (1.41 * s, -7.91, -0.58), 0.40)

    nose = cut_pieces("gear.nose", (-0.4, 2.8, -2.55), (0.4, 4.5, -1.15))
    gear(nose, "gear", (-0.3, 3.66, -1.25), (0.3, 3.66, -1.25), 90.0, hide=1.0)
