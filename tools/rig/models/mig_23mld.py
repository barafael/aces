"""MiG-23MLD (Tim Samedov, CC BY 4.0).

Named, separate objects throughout, so every part is taken by name.

- no ailerons (spoilers aren't modeled): the stabilators are tailerons
  (pitch 15°, roll 10°); rudder along its leading edge. Wings stay at the
  modeled sweep.
- main gear (leg, wheel, mudguard) swings forward about a lateral hinge
  at the leg top — an approximation of the real forward-and-inward fold —
  and hides once stowed; the bay doors under the fuselage close behind it.
- nose gear retracts aft, hides once stowed; its two long side doors close.
- the ventral fin, folded to the right while the gear is down, unfolds
  after the gear is up (as on the real aircraft).
"""

from math import pi  # noqa: F401

from rigkit import *  # noqa: F403

SOURCE = "mig-23_mld.glb"
FIXUP = ((-15.82, -2.11, -2.95), pi, 3.811)


def _named(prefixes):
    """Every mesh whose name starts with one of ``prefixes``."""
    return [o.name for o in meshes() if o.name.startswith(tuple(prefixes))]


def _main_gear(nums):
    return _named([f"OBJ{n:04d}_" for n in nums])


def build():
    load(SOURCE)
    fixup(*FIXUP)

    # Stabilators (tailerons): pivot shaft across the root at ~y -6.8.
    for name, parts, sx in (("R", ["stab_fuzcamo_0", "stab_00Bnosser.paa_0"], 1),
                            ("L", ["stab2_fuzcamo_0", "stab2_00Bnosser.paa_0"], -1)):
        stab = take(parts, f"stabilator.{name}")
        stabilator(stab, (0.6 * sx, -6.8, -0.24), (2.4 * sx, -6.8, -0.36), pitch=15.0, roll=10.0)

    rud = take(["rudder_fuzcamo_0", "rudder_nab_0"], "rudder")
    rudder(rud, (0.0, -6.88, 0.44), (0.0, -7.575, 1.617))

    # Main gear: wheel, leg, mudguard and the small fittings on the leg.
    right = _main_gear([131, 147] + list(range(148, 163)) + [163, 168] + list(range(170, 178)))
    left = _main_gear([109, 46] + list(range(110, 125)) + [125, 130] + list(range(134, 141)))
    for name, objs, sx in (("R", right, 1), ("L", left, -1)):
        leg = take([n for n in objs if not n.startswith(("OBJ0141", "OBJ0182"))],
                   f"gear.main.{name}")
        gear(leg, "gear", (0.6 * sx - 0.3, -1.65, -0.95), (0.6 * sx + 0.3, -1.65, -0.95),
             90.0, hide=1.0)
    # Bay doors hanging under the fuselage: swing up flat once the gear is in.
    door_r = take(_named(["OBJ0061_"]), "door.main.R")
    gear(door_r, "door", (0.25, -1.4, -1.09), (0.25, -2.6, -1.09), 58.0, t=(0.8, 1.0))
    door_l = take(_named(["OBJ0043_"]), "door.main.L")
    gear(door_l, "door", (-0.25, -1.4, -1.09), (-0.25, -2.6, -1.09), -58.0, t=(0.8, 1.0))

    # Nose gear: retracts aft into the nose.
    nose = take(_named(["stoykanos_", "nos_", "OBJ0041_", "OBJ0355_"]), "gear.nose")
    gear(nose, "gear", (-0.2, 4.05, -0.8), (0.2, 4.05, -0.8), -90.0, hide=1.0)
    for name, prefix, sx in (("R", "OBJ0063_", 1), ("L", "OBJ0047_", -1)):
        door = take(_named([prefix]), f"door.nose.{name}")
        # Hinge along the top edge; the door swings in under the bay.
        gear(door, "door", (0.21 * sx, 2.4, -0.75), (0.21 * sx, 4.17, -0.75),
             80.0 * sx, t=(0.8, 1.0))

    # Ventral fin: folded to the right with the gear down, unfolds after.
    fin = take(["OBJ0024_nab_0"], "ventral_fin")
    gear(fin, "ventral_fin", (-0.15, -6.6, -1.42), (-0.15, -4.8, -1.42), 90.0, t=(0.85, 1.0))

    nozzle("nozzle", (0.0, -7.76, -0.72), 0.48)
