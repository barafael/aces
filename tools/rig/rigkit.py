"""Rigging toolkit for the aircraft models — runs inside Blender.

A rig script (``tools/rig/models/<model>.py``) turns one downloaded Sketchfab
model (``art/models/<model>.glb``, kept untouched) into the game's rigged
model (``aces-client/assets/models/<model>.glb``):

1. ``load()`` imports the original and flattens it: every mesh in world
   space, identity transforms, no empties.
2. ``fixup()`` bakes the scale / orientation / centering into the meshes,
   so the export is in *airframe space*: meters, nose along -Z (glTF), the
   origin at the centre of the airframe's bounding box.
3. The script cuts the moving parts out (``take_objects``,
   ``split_loose``, ``cut``), puts each part's pivot on its hinge line
   (``part``), and marks the engine nozzles (``nozzle``).
4. ``export()`` writes the glb.

Coordinates in rig scripts are Blender's, in the airframe frame after
``fixup()``: **+X right wing, +Y nose, +Z up**, meters. (The exporter turns
them into glTF's Y-up, nose -Z; ``part()`` converts the hinge axis itself.)

Every moving part carries a glTF ``extras`` entry ``aces`` (a JSON string)
that the game reads (``aces-client/src/flight/rig.rs``)::

    {"role": "aileron",            # what it is (for humans / debugging)
     "axis": [x, y, z],            # hinge axis, glTF airframe space, unit
     "mix":  [pitch, roll, yaw],   # degrees of rotation per unit of the
                                   # elevator / aileron / rudder command
     "gear": [[t0, t1, deg], ...], # degrees at full retraction, reached
                                   # between gear progress t0..t1 (0 = down,
                                   # 1 = up), summed over the segments
     "hide": 0.3}                  # hidden from gear progress 0.3 on
                                   # (parts that can't stow cleanly)

A positive angle rotates by the right-hand rule about ``axis``. Command
conventions (see the flight model): elevator > 0 pulls (trailing edges of
elevators go *up*), aileron > 0 rolls right (right aileron up, left down),
rudder > 0 yaws right (rudder trailing edge to the right).

Engine nozzles are empties with ``{"role": "nozzle", "radius": r}`` at the
centre of the nozzle exit, exhaust along +Z (glTF airframe space, aft).
"""

import json
import math
import os
import sys

import bpy
import bmesh
from mathutils import Matrix, Vector

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SOURCE_DIR = os.path.join(REPO, "art", "models")
OUTPUT_DIR = os.path.join(REPO, "aces-client", "assets", "models")

# glTF (Y up, nose -Z after fixup) → Blender (Z up, nose +Y).
GLTF_TO_BLENDER = Matrix(((1, 0, 0, 0), (0, 0, -1, 0), (0, 1, 0, 0), (0, 0, 0, 1)))


def to_gltf(v):
    """A Blender-space direction in glTF coordinates."""
    return [v[0], v[2], -v[1]]


# ── Loading ─────────────────────────────────────────────────────────────────


def load(filename):
    """Import ``art/models/<filename>`` and flatten it: each mesh object
    single-user, in world space, identity transform, no parent; empties,
    cameras and lights removed. Nodes outside the file's default scene
    (Blender imports them, the game never shows them) are dropped."""
    bpy.ops.wm.read_factory_settings(use_empty=True)
    path = os.path.join(SOURCE_DIR, filename)
    bpy.ops.import_scene.gltf(filepath=path)
    shown = _scene_node_names(path)
    for o in [o for o in bpy.data.objects if _base_name(o.name) not in shown]:
        bpy.data.objects.remove(o, do_unlink=True)
    meshes = [o for o in bpy.data.objects if o.type == "MESH"]
    for o in meshes:
        world = o.matrix_world.copy()
        if o.data.users > 1:
            o.data = o.data.copy()
        o.parent = None
        o.data.transform(world)
        o.matrix_world = Matrix.Identity(4)
    for o in [o for o in bpy.data.objects if o.type != "MESH"]:
        bpy.data.objects.remove(o, do_unlink=True)
    return meshes


def _scene_node_names(path):
    """Names of the nodes reachable from a glb's default scene."""
    import struct

    with open(path, "rb") as f:
        f.read(12)
        length, _ = struct.unpack("<I4s", f.read(8))
        gltf = json.loads(f.read(length))
    nodes = gltf["nodes"]
    scene = gltf["scenes"][gltf.get("scene", 0)]
    names, stack = set(), list(scene["nodes"])
    while stack:
        n = nodes[stack.pop()]
        names.add(n.get("name", ""))
        stack += n.get("children", [])
    # Blender names meshes after their node, or after the mesh.
    for n in [nodes[i] for i in range(len(nodes)) if nodes[i].get("name", "") in names]:
        if "mesh" in n:
            names.add(gltf["meshes"][n["mesh"]].get("name", ""))
    return names


def _base_name(name):
    """A Blender name without its ``.001``-style duplicate suffix."""
    head, dot, tail = name.rpartition(".")
    return head if dot and tail.isdigit() and len(tail) == 3 else name


def fixup(translation, yaw, scale):
    """Bake the game's per-model fixup (glTF space: scale, then yaw about
    +Y, then translate — ``flight::model_fixup``) into every mesh."""
    t = Matrix.Translation(Vector(translation))
    r = Matrix.Rotation(yaw, 4, "Y")
    s = Matrix.Scale(scale, 4)
    glb = t @ r @ s
    m = GLTF_TO_BLENDER @ glb @ GLTF_TO_BLENDER.inverted()
    for o in meshes():
        o.data.transform(m)


def meshes():
    return [o for o in bpy.data.objects if o.type == "MESH"]


def bounds(objs=None):
    """(min, max) corners over ``objs`` (default: every mesh)."""
    lo = Vector((math.inf,) * 3)
    hi = Vector((-math.inf,) * 3)
    for o in objs or meshes():
        for v in o.data.vertices:
            co = o.matrix_world @ v.co
            lo = Vector(map(min, lo, co))
            hi = Vector(map(max, hi, co))
    return lo, hi


def obj(name):
    return bpy.data.objects[name]


# ── Cutting parts out ───────────────────────────────────────────────────────


def join(objs, name):
    """Join ``objs`` into one object called ``name``."""
    objs = list(objs)
    ctx = {"active_object": objs[0], "selected_editable_objects": objs}
    with bpy.context.temp_override(**ctx):
        bpy.ops.object.join()
    objs[0].name = name
    return objs[0]


def split_loose(o, name=None):
    """Separate ``o`` into its connected pieces (faces sharing vertices);
    returns them, largest first — ``o`` keeps the largest piece, the others
    are new objects named ``<name or o.name>.<n>``."""
    bm = bmesh.new()
    bm.from_mesh(o.data)
    layer = bm.faces.layers.int.get("rig_piece") or bm.faces.layers.int.new("rig_piece")
    seen, comps = set(), []
    for f in bm.faces:
        if f.index in seen:
            continue
        seen.add(f.index)
        stack, comp = [f], []
        while stack:
            g = stack.pop()
            comp.append(g)
            for v in g.verts:
                for h in v.link_faces:
                    if h.index not in seen:
                        seen.add(h.index)
                        stack.append(h)
        comps.append(comp)
    comps.sort(key=len, reverse=True)
    for n, comp in enumerate(comps):
        for f in comp:
            f[layer] = n
    bm.to_mesh(o.data)
    bm.free()
    base = name or o.name
    pieces = [o]
    for n in range(1, len(comps)):
        pieces.append(_extract(o, f"{base}.{n}",
                               lambda f, bm, n=n: f[bm.faces.layers.int["rig_piece"]] == n))
    return pieces


def bisect(o, co, no, within=None):
    """Cut ``o``'s faces along the plane through ``co`` with normal ``no``
    (inserting edges, deleting nothing), optionally only faces whose centre
    satisfies ``within(center) -> bool`` — so a later ``cut`` separates
    along a clean line instead of along the nearest existing edges."""
    bm = bmesh.new()
    bm.from_mesh(o.data)
    faces = [f for f in bm.faces if within is None or within(f.calc_center_median())]
    geom = list({e for f in faces for e in f.edges}) + faces + list({v for f in faces for v in f.verts})
    bmesh.ops.bisect_plane(bm, geom=geom, plane_co=Vector(co), plane_no=Vector(no))
    bm.to_mesh(o.data)
    bm.free()


def cut(o, name, inside):
    """Separate the faces of ``o`` whose centre satisfies ``inside(center)``
    into a new object ``name``. Returns it (``None`` if nothing matched)."""
    return _extract(o, name, lambda f, bm: inside(f.calc_center_median()))


def _extract(o, name, pick):
    """Move the faces of ``o`` for which ``pick(face, bmesh)`` holds into a
    new object ``name`` (same materials, world space); ``None`` if none."""
    new_mesh = o.data.copy()
    moved = 0
    for mesh, keep in ((new_mesh, True), (o.data, False)):
        bm = bmesh.new()
        bm.from_mesh(mesh)
        doomed = [f for f in bm.faces if bool(pick(f, bm)) != keep]
        if keep:
            moved = len(bm.faces) - len(doomed)
        if keep and moved == 0:
            bm.free()
            bpy.data.meshes.remove(new_mesh)
            return None
        bmesh.ops.delete(bm, geom=doomed, context="FACES")
        bm.to_mesh(mesh)
        bm.free()
    part = bpy.data.objects.new(name, new_mesh)
    bpy.context.scene.collection.objects.link(part)
    return part


def box(lo, hi):
    """A predicate for ``cut``: inside the axis-aligned box ``lo``..``hi``."""
    lo, hi = Vector(lo), Vector(hi)
    return lambda c: all(lo[i] <= c[i] <= hi[i] for i in range(3))


def polygon_xy(points, z=(-math.inf, math.inf)):
    """A predicate for ``cut``: inside the plan-view polygon ``points``
    (x, y pairs) and between heights ``z``."""
    def inside(c):
        if not z[0] <= c.z <= z[1]:
            return False
        x, y, hit = c.x, c.y, False
        for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1]):
            if (y1 > y) != (y2 > y) and x < x1 + (y - y1) * (x2 - x1) / (y2 - y1):
                hit = not hit
        return hit
    return inside


PLANES = {"xy": (0, 1, 2), "yz": (1, 2, 0), "xz": (0, 2, 1)}


def free_meshes():
    """Meshes that are not (yet) parts."""
    return [o for o in meshes() if "aces" not in o and o.parent is None]


def cut_outline(name, points, plane="xy", depth=(-math.inf, math.inf), objs=None,
                slice_edges=True, margin=0.3):
    """Cut everything inside an outline into one new object ``name``.

    ``points``: the outline as (a, b) pairs in ``plane`` ("xy": plan view,
    "yz": side view, "xz": front view); ``depth``: the range along the
    remaining axis (e.g. z for a plan-view outline — keep it tight around
    the surface so nothing else is caught). First every face near the
    outline is sliced along its edges (``slice_edges``), so the part comes
    out with a clean hinge line instead of ragged triangles; then faces
    whose centre lies inside go to the part. ``objs`` limits the source
    meshes (default: every mesh that is not a part yet)."""
    ia, ib, ic = PLANES[plane]
    lo_a = min(p[0] for p in points) - margin
    hi_a = max(p[0] for p in points) + margin
    lo_b = min(p[1] for p in points) - margin
    hi_b = max(p[1] for p in points) + margin

    def near(c):
        return (lo_a <= c[ia] <= hi_a and lo_b <= c[ib] <= hi_b
                and depth[0] <= c[ic] <= depth[1])

    def inside(c):
        if not depth[0] <= c[ic] <= depth[1]:
            return False
        x, y, hit = c[ia], c[ib], False
        for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1]):
            if (y1 > y) != (y2 > y) and x < x1 + (y - y1) * (x2 - x1) / (y2 - y1):
                hit = not hit
        return hit

    taken = []
    for o in objs or free_meshes():
        if slice_edges:
            for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1]):
                co = [0.0, 0.0, 0.0]
                co[ia], co[ib] = x1, y1
                d = [0.0, 0.0, 0.0]
                d[ia], d[ib] = x2 - x1, y2 - y1
                depth_axis = [0.0, 0.0, 0.0]
                depth_axis[ic] = 1.0
                no = Vector(d).cross(Vector(depth_axis))
                if no.length > 1e-9:
                    bisect(o, co, no.normalized(), within=near)
        piece = cut(o, f"{name}.{len(taken)}", inside)
        if piece is not None:
            taken.append(piece)
    if not taken:
        raise ValueError(f"cut_outline({name!r}): nothing inside the outline")
    return join(taken, name) if len(taken) > 1 else _rename(taken[0], name)


def cut_pieces(name, lo, hi, objs=None, whole=True):
    """Cut every connected piece lying inside the box ``lo``..``hi`` into
    one new object ``name`` — gear legs, wheels and doors are usually
    separate pieces. ``whole``: the piece must lie entirely inside;
    otherwise its centre suffices."""
    lo, hi = Vector(lo), Vector(hi)

    def in_box(v):
        return all(lo[i] <= v[i] <= hi[i] for i in range(3))

    taken = []
    for o in objs or free_meshes():
        bm = bmesh.new()
        bm.from_mesh(o.data)
        layer = bm.faces.layers.int.get("rig_take") or bm.faces.layers.int.new("rig_take")
        seen = set()
        any_taken = False
        for f in bm.faces:
            if f.index in seen:
                continue
            seen.add(f.index)
            stack, comp = [f], []
            while stack:
                g = stack.pop()
                comp.append(g)
                for v in g.verts:
                    for h in v.link_faces:
                        if h.index not in seen:
                            seen.add(h.index)
                            stack.append(h)
            verts = {v for g in comp for v in g.verts}
            if whole:
                take = all(in_box(v.co) for v in verts)
            else:
                c = sum((v.co for v in verts), Vector()) / len(verts)
                take = in_box(c)
            for g in comp:
                g[layer] = 1 if take else 0
            any_taken |= take
        bm.to_mesh(o.data)
        bm.free()
        if any_taken:
            piece = _extract(o, f"{name}.{len(taken)}",
                             lambda f, bm: f[bm.faces.layers.int["rig_take"]] == 1)
            if piece is not None:
                taken.append(piece)
    if not taken:
        raise ValueError(f"cut_pieces({name!r}): no piece inside the box")
    return join(taken, name) if len(taken) > 1 else _rename(taken[0], name)


def _rename(o, name):
    o.name = name
    return o


def take(names, name):
    """Existing objects (by name, e.g. a model's own ``LeftCanard``) as one
    part object ``name``."""
    objs = [obj(n) for n in names]
    return join(objs, name) if len(objs) > 1 else _rename(objs[0], name)


def mirror_x(points):
    """A plan-view polygon mirrored to the other wing."""
    return [(-x, y) for x, y in points]


# ── Rigging ─────────────────────────────────────────────────────────────────


def part(o, role, hinge_a, hinge_b, mix=None, gear=None, hide=None, parent=None):
    """Make ``o`` a moving part: pivot on the hinge line ``hinge_a`` →
    ``hinge_b`` (Blender airframe coordinates; positive rotation by the
    right-hand rule about a → b), deflecting by ``mix`` (degrees per unit
    pitch / roll / yaw command) and/or retracting by ``gear`` segments.
    ``hide``: hidden from that gear progress on (1.0: once stowed).
    ``parent``: a part it rides on (gear wheels on legs, doors on doors)."""
    a, b = Vector(hinge_a), Vector(hinge_b)
    # Children attached earlier stay where they are in the world.
    children = [(c, c.matrix_world.copy()) for c in o.children]
    o.data.transform(Matrix.Translation(-a))
    o.matrix_world = Matrix.Translation(a)
    for c, world in children:
        c.matrix_parent_inverse = o.matrix_world.inverted()
        c.matrix_world = world
    if parent is not None:
        o.parent = parent
        o.matrix_parent_inverse = parent.matrix_world.inverted()
    axis = (b - a).normalized()
    spec = {"role": role, "axis": [round(x, 5) for x in to_gltf(axis)]}
    if mix:
        spec["mix"] = list(mix)
    if gear:
        spec["gear"] = [list(seg) for seg in gear]
    if hide is not None:
        spec["hide"] = hide
    o["aces"] = json.dumps(spec)
    return o


def center(o):
    lo, hi = bounds([o])
    return (lo + hi) / 2


def hinge(o, a, b, plane="xy", reach=0.15):
    """Complete a hinge line drawn in 2D (``a``, ``b`` in ``plane``, as read
    off a render) to 3D: the missing coordinate of each end is the mean of
    ``o``'s vertices within ``reach`` of that end (widening until some are
    found) — the middle of the surface's thickness at the hinge."""
    ia, ib, ic = PLANES[plane]
    verts = [o.matrix_world @ v.co for v in o.data.vertices]
    out = []
    for p2 in (a, b):
        r = reach
        while True:
            near = [v for v in verts if (v[ia] - p2[0]) ** 2 + (v[ib] - p2[1]) ** 2 <= r * r]
            if near or r > 5:
                break
            r *= 1.5
        depth = sum(v[ic] for v in near) / len(near) if near else center(o)[ic]
        p3 = [0.0, 0.0, 0.0]
        p3[ia], p3[ib], p3[ic] = p2[0], p2[1], depth
        out.append(Vector(p3))
    return out


def surface(o, role, a, b, plane="xy", pitch=0.0, roll=0.0, yaw=0.0, parent=None):
    """A control surface hinged along ``a`` → ``b`` (3D points, or 2D ones
    in ``plane`` completed by ``hinge``), deflecting ``pitch`` / ``roll`` /
    ``yaw`` degrees per unit command.

    The axis is oriented so the signs mean the same on every surface:
    a horizontal hinge runs left → right (+x), so a **positive angle moves
    the trailing edge down**; a vertical hinge runs bottom → top (+z), so a
    **positive angle moves the trailing edge right**. The presets below
    fill in the usual mixes."""
    if len(a) == 2:
        a, b = hinge(o, a, b, plane)
    a, b = Vector(a), Vector(b)
    d = b - a
    vertical = abs(d.z) > max(abs(d.x), abs(d.y))
    if (vertical and d.z < 0) or (not vertical and d.x < 0):
        a, b = b, a
    return part(o, role, a, b, mix=(pitch, roll, yaw), parent=parent)


def side(o):
    """+1 for a part on the right half of the airframe, -1 on the left."""
    return 1.0 if center(o).x > 0 else -1.0


# Presets — deflections are degrees at full command. Pull (pitch +1) raises
# elevator trailing edges; roll right (+1) raises the right aileron's; yaw
# right (+1) swings the rudder's trailing edge right.


def aileron(o, a, b, deg=20.0, plane="xy"):
    return surface(o, "aileron", a, b, plane, roll=-deg * side(o))


def elevator(o, a, b, deg=25.0, plane="xy"):
    return surface(o, "elevator", a, b, plane, pitch=-deg)


def rudder(o, a, b, deg=25.0, plane="yz"):
    return surface(o, "rudder", a, b, plane, yaw=deg)


def stabilator(o, a, b, pitch=15.0, roll=0.0, plane="xy"):
    """All-moving tailplane half; ``roll`` > 0 makes it a taileron."""
    return surface(o, "stabilator", a, b, plane, pitch=-pitch, roll=-roll * side(o))


def canard(o, a, b, deg=15.0, plane="xy"):
    """All-moving foreplane: pull raises its leading edge."""
    return surface(o, "canard", a, b, plane, pitch=deg)


def elevon(o, a, b, pitch=20.0, roll=20.0, plane="xy"):
    """Trailing-edge surface doing both elevator and aileron work
    (deltas, flaperons)."""
    return surface(o, "elevon", a, b, plane, pitch=-pitch, roll=-roll * side(o))


def gear(o, role, a, b, deg=0.0, t=(0.15, 0.85), parent=None, hide=None, segments=None):
    """A landing-gear part rotating ``deg`` about ``a`` → ``b`` (3D) while
    the retraction runs from progress ``t[0]`` to ``t[1]`` (0 = down, as
    modeled; 1 = up). Doors that close behind the gear: ``t=(0.8, 1.0)``;
    doors that open for the gear and close again: pass ``segments``
    instead, e.g. ``[(0, .2, open), (.8, 1, -open)]``.
    ``hide``: hidden from that progress on (1.0 once stowed — for gear
    that has no bay modeled, or would poke through the skin)."""
    return part(o, role, a, b, gear=segments or [(t[0], t[1], deg)], parent=parent, hide=hide)


def attach(o, parent):
    """Make ``o`` ride on ``parent`` (a wheel on its leg) without moving
    on its own."""
    o.parent = parent
    o.matrix_parent_inverse = parent.matrix_world.inverted()
    return o


def nozzle(name, center, radius):
    """Mark an engine nozzle exit (centre, radius) for the exhaust visuals."""
    e = bpy.data.objects.new(name, None)
    e.location = Vector(center)
    e["aces"] = json.dumps({"role": "nozzle", "radius": radius})
    bpy.context.scene.collection.objects.link(e)
    return e


# ── Export ──────────────────────────────────────────────────────────────────


def export(filename):
    path = os.path.join(OUTPUT_DIR, filename)
    for o in [o for o in bpy.data.objects if o.type in {"CAMERA", "LIGHT"}]:
        bpy.data.objects.remove(o, do_unlink=True)
    bpy.ops.export_scene.gltf(
        filepath=path,
        export_format="GLB",
        export_yup=True,
        export_extras=True,
        export_apply=True,
        export_animations=False,
        export_cameras=False,
        export_lights=False,
        export_draco_mesh_compression_enable=False,
        export_image_format="AUTO",
    )
    print(f"RIG exported {path}")


# ── Analysis renders ────────────────────────────────────────────────────────

# name → (camera direction it looks along, camera up), Blender airframe space.
VIEWS = {
    "top": ((0, 0, -1), (0, 1, 0)),
    "bottom": ((0, 0, 1), (0, 1, 0)),
    "left": ((1, 0, 0), (0, 0, 1)),
    "right": ((-1, 0, 0), (0, 0, 1)),
    "front": ((0, -1, 0), (0, 0, 1)),
    "rear": ((0, 1, 0), (0, 0, 1)),
}


def render(outdir, prefix, views=("top", "left", "rear", "front", "bottom"),
           color="texture", labels=None, focus=None, size=1400, persp=True):
    """Orthographic Workbench renders of the current scene into
    ``outdir/<prefix>_<view>.png`` plus ``.json`` sidecars (camera frame,
    and the projected centre of every labeled object) that
    ``annotate.py`` turns into a meter grid and labels.

    ``color``: "texture" (the model's own look — panel lines show where
    control surfaces are), "object" (every object its own ``obj.color``,
    e.g. set by ``palette()``), "material" or "random".
    ``labels``: objects to label (default: every part with ``aces`` extras).
    ``focus``: (min, max) box to frame instead of the whole model.
    ``persp``: also a 3/4 perspective view (no grid)."""
    from bpy_extras.object_utils import world_to_camera_view

    scene = bpy.context.scene
    scene.render.engine = "BLENDER_WORKBENCH"
    shading = scene.display.shading
    shading.light = "STUDIO"
    shading.color_type = {"texture": "TEXTURE", "object": "OBJECT",
                          "material": "MATERIAL", "random": "RANDOM"}[color]
    shading.show_cavity = True
    shading.cavity_type = "BOTH"
    shading.show_object_outline = True
    scene.render.film_transparent = False
    scene.world = scene.world or bpy.data.worlds.new("World")
    shading.background_type = "VIEWPORT"
    shading.background_color = (0.62, 0.66, 0.70)
    scene.render.resolution_x = size
    scene.render.resolution_y = int(size * 0.72)
    scene.render.image_settings.file_format = "PNG"

    lo, hi = focus or bounds()
    lo, hi = Vector(lo), Vector(hi)
    center = (lo + hi) / 2
    extent = hi - lo
    if labels is None:
        labels = [o for o in bpy.data.objects if "aces" in o]

    cam_data = bpy.data.cameras.new("rig_cam")
    cam = bpy.data.objects.new("rig_cam", cam_data)
    scene.collection.objects.link(cam)
    scene.camera = cam
    os.makedirs(outdir, exist_ok=True)
    aspect = scene.render.resolution_x / scene.render.resolution_y

    for view in views:
        look, up = (Vector(v) for v in VIEWS[view])
        right = look.cross(up).normalized()
        # Frame the box: width along `right`, height along `up`.
        w = sum(abs(extent[i] * right[i]) for i in range(3))
        h = sum(abs(extent[i] * up[i]) for i in range(3))
        cam_data.type = "ORTHO"
        cam_data.ortho_scale = max(w, h * aspect) * 1.08
        cam_data.clip_start = 0.01
        cam_data.clip_end = 1000.0
        rot = Matrix((right, up, -look)).transposed()
        cam.matrix_world = Matrix.Translation(center - look * 100.0) @ rot.to_4x4()
        bpy.context.view_layer.update()
        path = os.path.join(outdir, f"{prefix}_{view}")
        scene.render.filepath = path + ".png"
        bpy.ops.render.render(write_still=True)
        meta = {
            "view": view,
            "right": list(right), "up": list(up),
            "center": list(center), "ortho_scale": cam_data.ortho_scale,
            "width": scene.render.resolution_x, "height": scene.render.resolution_y,
            "labels": [],
        }
        for o in labels:
            olo, ohi = bounds([o]) if o.type == "MESH" else (o.location, o.location)
            p = world_to_camera_view(scene, cam, (Vector(olo) + Vector(ohi)) / 2)
            meta["labels"].append({"name": o.name, "u": p.x, "v": p.y})
        with open(path + ".json", "w") as f:
            json.dump(meta, f)

    if persp:
        cam_data.type = "PERSP"
        cam_data.lens = 50
        d = max(extent) * 1.9
        eye = center + Vector((-0.8, 0.9, 0.55)).normalized() * d
        look = (center - eye).normalized()
        right = look.cross(Vector((0, 0, 1))).normalized()
        up = right.cross(look)
        cam.matrix_world = Matrix.Translation(eye) @ Matrix((right, up, -look)).transposed().to_4x4()
        scene.render.filepath = os.path.join(outdir, f"{prefix}_persp.png")
        bpy.ops.render.render(write_still=True)
    bpy.data.objects.remove(cam, do_unlink=True)


def palette(objs):
    """Give each object in ``objs`` a distinct viewport colour (for
    ``render(color="object")``); everything else light grey."""
    for o in meshes():
        o.color = (0.8, 0.8, 0.8, 1.0)
    for i, o in enumerate(objs):
        h = (i * 0.61803398875) % 1.0
        r, g, b = _hsv(h, 0.75, 0.95)
        o.color = (r, g, b, 1.0)


def _hsv(h, s, v):
    i = int(h * 6) % 6
    f = h * 6 - int(h * 6)
    p, q, t = v * (1 - s), v * (1 - f * s), v * (1 - (1 - f) * s)
    return [(v, t, p), (q, v, p), (p, v, t), (p, q, v), (t, p, v), (v, p, q)][i]


def pose(pitch=0.0, roll=0.0, yaw=0.0, gear=0.0):
    """Pose every rigged part like the game would (for verification
    renders): deflect by the commands, retract the gear to progress
    ``gear`` (0 = down, 1 = up). Blender-space inverse of the glTF axis."""
    for o in bpy.data.objects:
        o.hide_render = False
    for o in bpy.data.objects:
        if "aces" not in o:
            continue
        spec = json.loads(o["aces"])
        if spec.get("role") == "nozzle":
            continue
        gx, gy, gz = spec["axis"]
        axis = Vector((gx, -gz, gy))
        deg = sum(m * c for m, c in zip(spec.get("mix", [0, 0, 0]), (pitch, roll, yaw)))
        for t0, t1, d in spec.get("gear", []):
            u = min(max((gear - t0) / max(t1 - t0, 1e-6), 0.0), 1.0)
            deg += d * (u * u * (3 - 2 * u))
        o.rotation_mode = "AXIS_ANGLE"
        o.rotation_axis_angle = (math.radians(deg), *axis)
        o.hide_render = "hide" in spec and gear >= spec["hide"]
    # A hidden part hides what rides on it, as in the game.
    def hide_below(o):
        for c in o.children:
            c.hide_render = c.hide_render or o.hide_render
            hide_below(c)
    for o in bpy.data.objects:
        if o.parent is None:
            hide_below(o)


def args():
    """Arguments after ``--`` on the Blender command line."""
    return sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
