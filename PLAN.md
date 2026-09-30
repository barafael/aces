# aces — Flight Combat Simulator

An Ace Combat–inspired 3D flight combat game. Rust + Bevy 0.19, peer-to-peer
multiplayer for as many players as join, playable natively and in the browser.

## Architecture

Workspace mirroring gnils:

```
aces/
├── Cargo.toml            # workspace: client, net, protocol; wasm-release profile
├── PLAN.md
├── aces-protocol/        # NO bevy dep: wire types, constants, aircraft stats (serde)
├── aces-net/             # matchbox_socket (barafael fork), NetState, lobby, sequencing
└── aces-client/          # bevy app (native + wasm via trunk)
```

## Netcode (gnils pattern, adapted for continuous sim)

Turn-based lockstep doesn't fit a flight sim, so we keep gnils' scaffolding
(matchbox P2P, `wss://omdurman-matchbox.fly.dev` signaling, reliable +
unreliable channels, postcard, pet names, room ids, host = lowest peer id,
host-sequenced events) but change the authority model:

- **Own-plane authority**: every peer simulates its own aircraft locally at
  60 Hz `FixedUpdate` and broadcasts snapshots (pos, quat, vel, throttle, HP)
  at 20 Hz on the **unreliable** channel. Remote planes are interpolated
  (100 ms buffer). No determinism requirements.
- **Lobby** (`Hello`/`Roster`/`Start`): no seats — every peer is a player with
  a chosen aircraft. Host broadcasts `Start` with seed + roster; all spawn
  together. Late joiners wait in lobby (host can restart).
- **Sequenced events** (reliable, host-sequenced): missile launches, gun-hit /
  damage claims, kills. Trust-based P2P: shooter claims hits, victim applies
  damage to their own HP and broadcasts it.
- **Missiles**: every peer simulates each missile (pure pursuit, arcade) —
  the shooter from fire time, everyone else from the sequenced launch event;
  only the shooter's simulation claims hits.
- **Ephemeral** (unsequenced, peer to peer, attributed to the sender):
  countermeasure pops on the reliable channel (they decide whether a
  missile hits, so none may be lost), "I'm locking you" announcements
  (drive the victim's RWR; they repeat) on the unreliable one.
- **Deathmatch**: unlimited duration, death → explosion → 3 s respawn at a
  random spawn point, score per kill.

## Flight model (War Thunder arcade–style rigid body)

- Surfaces → moments → attitude → AoA/sideslip → forces. Nothing commands
  a rotation rate: rate-limited actuators move elevator/ailerons/rudder;
  pitch and yaw are statically stable (the elevator/rudder set the AoA /
  sideslip the airframe weathervanes to, stiffness ∝ dynamic pressure),
  ailerons accelerate the roll against roll damping. Adverse yaw, dihedral
  effect, roll due to yaw rate; rolling about the body axis under load
  turns AoA into sideslip — planes slip and twist.
- Forces: lift from a lift curve that rounds off into flat-plate lift past
  the stall, parasitic + induced + transonic wave drag, flat-plate drag
  post-stall and in sideslip, side force from sideslip, thrust (engine
  spools after the lever; afterburner above 100 %), gravity; exponential
  atmosphere. Hard turns bleed speed, dives regain it.
- Stall: lift collapses, the nose breaks down, buffet, roll damping
  reverses (saturating autorotation) and the windward/retreating wing
  drops — an incipient spin that recovers hands-off.
- Controls: **War Thunder mouse aim** — the mouse steers a world-space aim
  direction and the camera looks along it (plane fixed on screen below the
  aim circle). The **instructor** flies the nose onto the aim by steering
  the lift vector: the acceleration ⊥ the path that holds it against
  gravity plus turns it toward the aim is rolled above the canopy and
  pulled — wings level when on target, gentle banks for small offsets,
  bank-and-pull for large ones, push or split-S for aims below. The last
  few degrees sideways go to the rudder (gun aiming), with an integrator
  on the nose's lateral error (only near capture and only while the error
  creeps, so approaches never wind it up) that removes the slip's lag; the
  pitch channel is damped by the nose's own pitch rate (not the error's —
  stepwise mouse aim would kick the elevator). While maneuvering the
  rudder coordinates. Lateral error is measured from the flight path so
  nose slip can't feed back into the bank (the cause of an early wing-
  rocking oscillation at small offsets). AoA limit, G limit and ground
  avoidance (hold **Shift** to fly without them and stall). **A/D** / **Q/E** override roll / rudder.
- HUD: aim circle, nose cross, flight-path marker (the gap is AoA and
  sideslip), speed/altitude/throttle/G/AoA readout, stall warning.
- World: flat ocean plane with a 1 km grid (mipmapped), gradient sky sphere +
  linear distance fog, soft world bounds, scattered spawn points. (Terrain =
  stretch goal.)

## Flight tuning (recorder + replay)

Native builds record every flight to `flightlogs/flight-<UTC>.jsonl`
(debug builds by default; `ACES_FLIGHT_LOG=<dir>` to choose, `off` to
disable, release builds only when set). One JSON record per line: a header
with every tuning constant, the spawn state, a `frame` record per rendered
frame (raw mouse events, held keys, aim, free look, camera, rendered plane
pose) and a `tick` record per fixed tick (flight-model input, instructor
internals, surface command, full state). `M` drops a mark.

`cargo run -- replay flightlogs/<log>.jsonl [--mouse] [--csv out.csv]`
re-flies the log through the current code (`fly_tick`, shared with the
game): bit-exact with unchanged constants; after a change it lists the
constants that changed since the recording and compares twitchiness
metrics (rates, angular accelerations, control travel/reversals,
saturation, G onset, sideslip, aim error) recorded vs replayed, for the
whole flight and ±2 s around each mark. `--mouse` re-derives the aim from
the raw mouse events (to tune sensitivity/leveling too).

## Weapons & countermeasures (cone-based, arcade)

| Weapon | Lock | Counter |
|---|---|---|
| Gun (`Space`) | none — hitscan raycast, tracers | — |
| IR missile (`Ctrl` + slot 2) | target in ~10° cone, < 4 km, ~1.5 s lock | flares distract if deployed within window |
| Radar missile (`Ctrl` + slot 3) | target in ~30° cone, < 8 km, ~2 s lock | chaff breaks guidance → missile flies straight |

- **`F` flare**, **`C` chaff** — separate stores (12 each), regen 1 per 5 s.
  RWR announces the threat type (flares vs IR, chaff vs radar) so the player
  pops the right one.
- Lock logic runs on the shooter from snapshots; a held radar lock is
  announced to its target (RWR) via ephemeral messages. The inbound-missile
  warning comes from the target's own simulation of each missile aimed at
  it: on while one is guided at it, off once it hits, expires or is decoyed.

## Aircraft

Aircraft are data: `aces-protocol/src/aircraft.rs` holds the registry
`AIRCRAFT` (name + `Airframe`), selectable in the lobby (`←`/`→` cycle,
shown when there is more than one). An `Airframe` carries every number the flight
model and instructor read — aerodynamics (wing loading, lift curve, stall,
drag polar, wave drag), engine (jet/propeller, dry and boost thrust, thrust
lapse with speed and density, spool rates), control response (pitch/yaw
frequencies and damping, roll rate and time constant, dihedral, adverse
yaw, control stiffening, actuator speed), stall behavior (autorotation,
wing drop, pitch break, buffet) and structural G limits. The flight code
has no per-plane constants; the instructor inverts whatever airframe it
flies, so it needs no per-plane tuning. Next to the airframe each entry
carries its `Combat` stores — hit points, gun (damage per hit × shots per
second), heat-seekers, radar missiles and radar lock range (0: no radar,
so no radar missiles) — and its glTF model. The roster is below.

Adding a plane: add an `AIRCRAFT` entry (start from `PLACEHOLDER_JET` with
struct-update syntax), check its envelope with `cargo test -p aces-client
performance -- --ignored --nocapture` (stall/top/boost speed, roll rate,
sustained and instantaneous turn, corner speed, climb, G) — the
`every_airframe_*` tests fly the instructor's whole battery (level hold,
small offsets without rocking, 90° turn, G/AoA protection, stall recovery,
tail slide) on every registered aircraft and the `test_airframes`. Flight logs record the airframe at spawn; `replay`
re-flies with the aircraft's current definition and lists what changed
(`--recorded-airframe` flies the logged one). The sixteen real models
(F-15E, F/A-141F, MiG-19, MiG-23MLD, Gripen, T-38, Su-25, MiG-21, F-14,
F-16C, Eurofighter, SR-71, MiG-15, F-5, Super Étendard, Su-47) live in
`aces-client/assets/models`, stored with Git LFS (`*.glb`, ~140 MB of
Sketchfab exports — install git-lfs before cloning, or run `git lfs pull`
after). An aircraft whose model is missing
flies the placeholder airframe.
Third-party models are credited per their licenses (two CC BY-NC-SA 4.0 —
non-commercial —, the rest CC BY 4.0): each `AIRCRAFT` entry carries its
`Credit`, shown on the main menu's credits screen (`K`, `←`/`→` turn its
pages) and listed in `CREDITS.md` (with the font); a test keeps the three
in sync.
Solo, `P` switches the plane in flight to the next aircraft (airframe,
combat stores and model; a new flight-log segment); over the network the
lobby's choice stands. The downloads are kept untouched in
`art/models`; the game ships the versions the rig pipeline exports (see
below), already scaled, rotated and centred — the Sketchfab exports face
arbitrary axes, are wildly off scale and are not centered. A rig script's
`FIXUP` is found by measuring the world-space vertex bounding box of the
glTF's default scene (node transforms applied), finding the nose (the fin
is at the tail), yawing it onto -Z, scaling the length to the real
aircraft's and moving the scaled, yawed bounding-box centre to the origin.
Check the Sketchfab download before adding it: some are posed (banked,
pitched), are whole scenes, or need `KHR_materials_pbrSpecularGlossiness`,
which bevy_gltf refuses to load.

### The roster

Each airframe is grounded in the real type — wing loading, thrust-to-weight,
aspect ratio, G limit, handling reputation (sources in each constant's
doc) — then fitted to game targets at 540 km/h near sea level: the game
runs specific thrust at ~0.8 of the real value, squeezes top speeds into
~880–1250 km/h and roll rates to ~0.6 of the real ones. Newer generations
are more capable; older ones trade missiles and sensors for guns,
toughness or agility. Numbers from the performance report (sustained turn
with boost / instantaneous turn [°/s], roll [°/s], top speed with boost
[km/h], climb [m/s]) and combat (HP, gun damage per second, IR + radar
missiles, radar range [km]):

| Gen | Aircraft | Turn | Roll | Top | Climb | Combat | Strengths | Weaknesses |
|---|---|---|---|---|---|---|---|---|
| 1 | MiG-15bis | 15.9 / 21.5 | 125 | 930 | 48 | 110, 126, 0+0, — | low-speed turn; heavy 37/23 mm guns | no missiles or radar; no afterburner, stiff controls at speed |
| 2 | MiG-19S | 17.0 / 21.0 | 140 | 1060 | 98 | 100, 169, 2+0, — | best early-jet turn; heaviest gun (3× 30 mm) | slow-spooling engines, departs; 2 IR, no radar |
| 2 | MiG-21bis | 14.0 / 21.5 | 130 | 1045 | 129 | 85, 128, 4+2, 4 | speed and climb; good first turn | worst energy bleed (delta); fragile |
| 2 | F-5A | 15.5 / 20.0 | 170 | 1000 | 75 | 95, 112, 2+0, — | fast roll; responsive engines | little thrust and top speed; 2 IR, no radar |
| 2 | T-38A | 14.5 / 19.0 | 195 | 1010 | 67 | 85, 72, 2+0, — | fastest roll; snappy response | weakest gun (arcade gun pod); poor sustained turn, fragile |
| 3 | SR-71 | 8.0 / 10.0 | 57 | 1250 | 63 | 120, —, 0+3, 12 | by far the fastest; long radar (YF-12A fit) | barely turns (3.5 g); no gun, no IR |
| 3 | Super Étendard | 14.0 / 20.0 | 135 | 965 | 53 | 100, 144, 2+0, — | low-speed agility; 2× 30 mm | no afterburner; no radar missiles |
| 3 | Su-25 | 13.0 / 16.9 | 103 | 882 | 49 | 160, 168, 2+0, — | toughest airframe; devastating gun | slowest, weakest climb; no radar |
| 3 | MiG-23MLD | 14.5 / 18.5 | 113 | 1065 | 118 | 100, 128, 4+2, 7 | acceleration and speed; radar missiles | poorest sustained turn; departs if pushed |
| 4 | F-14A | 17.0 / 23.0 | 105 | 1076 | 69 | 115, 120, 2+6, 11 | longest radar, most radar missiles; low-speed nose authority | underpowered (TF30); slow roll, departs |
| 4 | F-15E | 18.0 / 21.5 | 125 | 1075 | 127 | 125, 120, 4+4, 10 | big engines, long radar; second-toughest | heavy: slow roll, mediocre turn |
| 4 | F-16C | 20.5 / 23.0 | 170 | 1085 | 153 | 100, 120, 2+4, 8 | turn-and-burn benchmark; carefree FBW, fast roll | middling radar; 2 IR |
| 4.5 | JAS 39C Gripen | 19.5 / 25.0 | 170 | 1050 | 127 | 90, 130, 2+4, 8.5 | instantaneous turn and roll; small | least thrust of the modern jets; light airframe |
| 4.5 | Typhoon | 21.5 / 25.0 | 160 | 1100 | 165 | 105, 130, 2+4, 9.5 | best energy fighter: speed, climb, sustained turn | big target; 2 IR |
| 5 | Su-47 | 20.5 / 27.0 | 140 | 1050 | 169 | 110, 132, 4+4, 9 | best nose authority, benign stall; thrust | heavy, slow roll; middling top speed |
| 5 | F/A-141F (fictional) | 19.0 / 23.0 | 150 | 1080 | 120 | 110, 120, 2+4, 9 | well-rounded; long radar, tough | naval weight dulls climb and turn |

The placeholder jet is the F-16C-like reference (20.0 / 22.4, 150, 1079,
148; standard combat 100, 120, 6+4, 8).

### Rigging: control surfaces, landing gear, nozzles (`tools/rig`)

Every model goes through a Blender rig script, `tools/rig/models/<name>.py`
(`tools/rig/build.sh` rebuilds all; Blender 5.2): it imports
`art/models/<file>`, bakes the `FIXUP` in, cuts the moving parts out —
along outlines read off textured renders (`cut_outline`, sliced clean
along the hinge line) or as whole loose pieces (`cut_pieces`) — pivots each
on its hinge line, tags it, marks the engine nozzle exits, and exports
`aces-client/assets/models/<file>`. Conventions (airframe frame, meters,
+X right, +Y nose, +Z up in Blender) and the tag format are documented in
`tools/rig/rigkit.py`; the presets (`aileron`, `elevator`, `rudder`,
`stabilator` (+ taileron roll), `canard`, `elevon`, `gear`) fix the signs.
The tags ride in glTF node extras: `{"role", "axis", "mix": [pitch, roll,
yaw] degrees per unit command, "gear": [[t0, t1, degrees]], "hide": t}`,
nozzles `{"role": "nozzle", "radius"}`.

In the game (`flight::rig`), a model instance's tagged nodes become
`RigPart`s posed every frame from the plane's `Surfaces` and `Gear`
(retraction progress 0 = down … 1 = up; `G` toggles, planes spawn gear up,
4 s cycle) and `Nozzle`s carrying the exhaust (a throttle-scaled core and
afterburner plume for now — the anchors and `Engine` state are where the
real exhaust effects go). Snapshots carry gear progress and throttle, so
remote planes animate the same.

Analysis tools: `survey.py` (textured or piece-coloured orthographic
close-ups, labeled, with a piece table), `annotate.py` (meter grid onto
them — coordinates go straight into a rig script), `rig.py --check`
(renders neutral / pull / roll / yaw / gear half / gear up), `sheet.py`
(contact sheets).

### glb export recipe (verified against bevy_gltf 0.19)

Bevy's first-class format is glTF; `.glb` is ideal for wasm (single
self-contained file = one HTTP request). **Export from Blender with
compression OFF**: bevy 0.19 supports neither Draco, nor meshopt, nor
`KHR_mesh_quantization`, nor the `KHR_texture_basisu` extension syntax.
+Y up (Blender default), textures embedded, keep them ≤ 2k for wasm payload.

## Controls

| Key | Action |
| --- | --- |
| `Mouse` | Aim (and view) — the instructor flies the nose onto it |
| `A` / `D` | Roll (overrides the instructor) |
| `Q` / `E` | Rudder (overrides the instructor) |
| `W` / `S` | Throttle (past 100 % into afterburner) |
| `Shift` (hold) | Limiter off: no AoA/G protection or ground avoidance |
| `Space` | Fire gun |
| `Ctrl` | Fire selected missile |
| `1` / `2` / `3` | Select gun / IR missile / radar missile |
| `F` | Flares |
| `C` | Chaff |
| `M` | Mark the flight log ("this felt wrong") |
| `P` | Solo: switch to the next aircraft in flight |
| `G` | Landing gear up / down |
| `V` (hold) | Free look: mouse temporarily orbits the camera; release springs back to chase view |
| `Esc` | Menu |

## HUD (Ace Combat flavor)

Gun reticle, missile lock reticle (acquiring/locked), speed/altitude tapes,
heading strip, radar minimap with blips, RWR "LOCKED" flash, weapon +
countermeasure counts, HP, kill feed / scoreboard (`Tab`).

## Modules (aces-client)

`main` (states: Menu/Lobby/InGame) · `assets` · `flight` (model, input,
camera) · `weapons` (gun, missiles, lock, countermeasures, damage) · `net`
(snapshot rx/interp, event application) · `hud` · `world`

## Milestones

1. **Scaffold**: 3 crates compile native + wasm; bevy window, ocean + sky.
   *Verify: `cargo run`, `trunk serve`.*
2. **Solo flight**: full flight model + controls + chase cam + free look +
   placeholder model. *Verify: fly, stall, recover.*
3. **Net layer**: lobby join/start, snapshot broadcast + interpolation of
   remote planes. *Verify: two windows fly together.*
4. **Guns + IR missile + HP/respawn** (local first, then sequenced events).
   *Verify: kill across two instances.*
5. **Radar missile + countermeasures + RWR.**
6. **HUD polish, 5 aircraft stats/models, score/kill feed, wasm release build.**

## Decisions log

- WT mouse aim: the mouse steers a world-space aim/view direction (cursor
  locked + hidden in flight), not a screen cursor; the instructor is a
  model-inverting flight computer (path rate → lift → AoA → elevator) on
  the fixed tick. V = free look; F/C = separate flare/chaff keys.
- Rigid-body flight model (moments, AoA/sideslip dynamics, soft stall with
  wing drop) instead of rate commands: "planes don't fly on rails".
- Snapshots carry the three surface positions (quantized to i8) so remote
  airframes animate too.
- Launching with a room id as the CLI arg / `?room=` parameter skips the
  menu, joins that room, and the elected host auto-starts once the roster
  has ≥ 2 players and every peer has greeted — hands-free two-instance
  testing (normal launches keep the F/H/J menu flow).
- Dev profile builds deps at `opt-level = 1`: unoptimized debug wasm was
  ~1.4 GB and OOM-killed wasm-bindgen; browser dev uses `trunk serve`
  (debug) or `trunk build --release` (42 MB wasm) when memory is tight.
- Milestone 3 verified with two live instances: host election + demotion,
  single `Start` with one seed, snapshot flow in both directions.
- `ACES_TEST_FIRE=1` enables a hands-free auto-dogfight (auto-aim, lock,
  fire) for two-instance end-to-end combat verification.
- Milestone 4 verified live: mutual missile kills across two instances —
  damage applied by the victim (100 → 40 → overkill), `Killed` confirmed,
  respawn on the next ring slot, HP riding the plane snapshots.
- Missiles: the shooter spawns its own at fire time (no host round trip
  before it sees it), everyone else from the sequenced launch; only the
  shooter's simulation claims a hit, against the plane actually hit (a
  bystander inside the fuse takes the damage, not the target).
- Milestone 5 verified live: chaff broke radar guidance and flares
  captured seekers across two instances — a perfectly-timed pop defeats a
  missile, stores (12 + 12, regen 5 s) make it a resource game.
- The auto-dogfight flies with energy discipline: below 130 m/s it flies
  level to regain speed (a max-G pursuit mushes into a stall and never
  merges — observed in the flight logs). It launches one missile per 3 s
  and pops one countermeasure per inbound missile.
- Every simulation of a missile — the victim's own included — consults the
  target's countermeasure pops, so the victim sees the decoy the shooter's
  simulation fell for (instead of a missile exploding on it harmlessly).
- A held lock stays on its target inside the drop cone: a plane crossing
  nearer the center does not steal it.
- Kills are counted by every peer, the victim included, from the canonical
  `Killed` stream; the scoreboard lists the whole roster.
- Native + browser (wasm via trunk, share links with `?room=`).
- Semi-realistic flight model; cone-based arcade radar/IR.
- 3-crate workspace like gnils; bevy 0.19 to match gnils.
- Respawn deathmatch (no rounds); lobby-start-only (no mid-game join for v1);
  trust-based hits (no anti-cheat — fine for friends).
