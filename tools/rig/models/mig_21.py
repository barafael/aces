"""MiG-21 (NETRUNNER_pl, CC BY 4.0).

Low-poly, one skin mesh for fuselage, wings and tail: the ailerons,
stabilators and rudder are cut out of it. The camouflage texture barely
shows panel lines, so the outlines follow the real aircraft's layout:

- ailerons: outboard trailing edge (outboard of the flaps).
- stabilator: all-moving halves (with their steel tip plates) about a
  lateral shaft at mid root chord.
- rudder: the rear of the fin above the braking-parachute fairing.
- main gear (both legs share one mesh; split per side) swings inward
  about a fore-aft hinge at the wing into the fuselage bays; nose gear
  swings forward. Both hide once stowed (the bays are open holes); the
  splayed main bay doors swing out flat under the wing roots and the nose
  bay's side doors swing shut.
- the file's other two aircraft and their smoke trails are dropped (see
  ``_drop_other_aircraft``).
"""

from math import pi  # noqa: F401

import bpy

from rigkit import *  # noqa: F403

SOURCE = "mig-21_fishbed_-_cold_war_era_fighter_-_free.glb"
FIXUP = ((-46.0, -1.99, 31.98), -pi / 2, 1.0)


def _drop_other_aircraft():
    """The file ships three aircraft and smoke trails; the game shows only
    the parked one (the scene root lists it alone), but Blender's importer
    brings in the orphaned nodes too. Keep what lies around the origin."""
    for o in list(meshes()):
        lo, hi = bounds([o])
        c = (lo + hi) / 2
        if abs(c.x) > 5 or abs(c.y) > 8 or abs(c.z) > 3:
            bpy.data.objects.remove(o, do_unlink=True)


RIGHT_AILERON = [(2.02, -2.47), (3.56, -2.47), (3.56, -2.99), (2.02, -2.99)]
RIGHT_STAB = [(0.55, -3.85), (1.92, -5.95), (1.92, -7.05), (0.5, -5.85)]
STAB_TIP = [(1.8, -5.7), (1.97, -5.7), (1.97, -7.1), (1.8, -7.1)]
# Side view (y, z).
RUDDER = [(-5.20, 0.33), (-6.40, 1.62), (-6.88, 1.62), (-5.74, 0.33)]


def build():
    load(SOURCE)
    fixup(*FIXUP)
    _drop_other_aircraft()

    for name, sx in (("R", 1), ("L", -1)):
        mirror = (lambda pts: pts) if sx > 0 else mirror_x
        ail = cut_outline(f"aileron.{name}", mirror(RIGHT_AILERON), depth=(-0.8, -0.35))
        aileron(ail, (2.02 * sx, -2.47), (3.56 * sx, -2.47))
        # A thin slab around the tailplane, so the fuselage beside its root
        # stays; the tip plate (taller) comes separately.
        stab = cut_outline(f"stabilator.{name}", mirror(RIGHT_STAB), depth=(-0.56, -0.38))
        tip = cut_outline(f"stabilator.tip.{name}", mirror(STAB_TIP), depth=(-0.85, -0.1))
        stab = join([stab, tip], f"stabilator.{name}")
        stabilator(stab, (0.6 * sx, -5.1, -0.47), (1.9 * sx, -5.1, -0.47), pitch=15.0)

        lo, hi = sorted((0.4 * sx, 1.7 * sx))
        # The leg's top fittings would swing up through the wing: gone early.
        top = cut_pieces(f"gear.main.top.{name}", (lo, -1.4, -1.0), (hi, -0.2, -0.4))
        gear(top, "gear", (1.3 * sx, -1.2, -0.6), (1.3 * sx, -0.3, -0.6), 90.0 * sx, hide=0.3)
        leg = cut_pieces(f"gear.main.{name}", (lo, -1.4, -2.1), (hi, -0.2, -0.4))
        gear(leg, "gear", (1.3 * sx, -1.2, -0.6), (1.3 * sx, -0.3, -0.6), 90.0 * sx, hide=1.0)

    rud = cut_outline("rudder", RUDDER, plane="yz", depth=(-0.15, 0.15))
    rudder(rud, (-5.20, 0.33), (-6.40, 1.62))

    nose = cut_pieces("gear.nose", (-0.2, 3.5, -2.1), (0.2, 4.3, -0.55))
    gear(nose, "gear", (-0.2, 3.95, -0.65), (0.2, 3.95, -0.65), 90.0, hide=1.0)

    for name, sx in (("R", 1), ("L", -1)):
        # Main bay doors, hanging splayed under the fuselage: swing out flat.
        lo, hi = sorted((0.15 * sx, 0.4 * sx))
        door = cut_pieces(f"door.main.{name}", (lo, -0.55, -1.52), (hi, 0.42, -0.88))
        gear(door, "door", (0.22 * sx, -0.5, -0.90), (0.22 * sx, 0.38, -0.90), -77.0 * sx,
             t=(0.8, 1.0))
        # Nose bay side doors: swing in under the bay.
        lo, hi = sorted((0.14 * sx, 0.3 * sx))
        door = cut_pieces(f"door.nose.{name}", (lo, 3.5, -1.18), (hi, 5.1, -0.9))
        gear(door, "door", (0.22 * sx, 3.54, -0.92), (0.22 * sx, 5.07, -0.92), 90.0 * sx,
             t=(0.8, 1.0))

    nozzle("nozzle", (0.0, -5.95, -0.18), 0.36)
