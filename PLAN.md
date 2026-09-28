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
- **Missiles**: owner-simulated with proportional navigation; launch event
  spawns it on all peers, snapshots stream on the unreliable channel.
- **Ephemeral** (unreliable): tracers, countermeasure pops, "I'm locking you"
  (drives victim's RWR), flare/chaff deployment events.
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
| IR missile (`Ctrl` + slot 2) | target in ~10° cone, < 4 km, ~2 s lock | flares distract if deployed within window |
| Radar missile (`Ctrl` + slot 3) | target in ~30° cone, < 8 km, ~2 s lock | chaff breaks lock → missile goes ballistic |

- **`F` flare**, **`C` chaff** — separate stores (12 each), regen 1 per 5 s.
  RWR announces the threat type (flares vs IR, chaff vs radar) so the player
  pops the right one.
- Lock logic runs on the shooter from snapshots; lock reticle + RWR warning
  via ephemeral messages.

## Aircraft

Aircraft are data: `aces-protocol/src/aircraft.rs` holds the registry
`AIRCRAFT` (name + `Airframe`), selectable in the lobby (number keys, shown
when there is more than one). An `Airframe` carries every number the flight
model and instructor read — aerodynamics (wing loading, lift curve, stall,
drag polar, wave drag), engine (jet/propeller, dry and boost thrust, thrust
lapse with speed and density, spool rates), control response (pitch/yaw
frequencies and damping, roll rate and time constant, dihedral, adverse
yaw, control stiffening, actuator speed), stall behavior (autorotation,
wing drop, pitch break, buffet) and structural G limits. The flight code
has no per-plane constants; the instructor inverts whatever airframe it
flies, so it needs no per-plane tuning. HP, mass for collisions and the
glTF model path join the entry in milestones 4–6. Currently: the
placeholder jet.

Adding a plane: add an `AIRCRAFT` entry (start from `PLACEHOLDER_JET` with
struct-update syntax), check its envelope with `cargo test -p aces-client
performance -- --ignored --nocapture` (stall/top/boost speed, roll rate,
sustained turn), and add it to `test_airframes` if it opens new territory —
`every_airframe_*` tests fly the instructor's whole battery (level hold,
small offsets without rocking, 90° turn, G/AoA protection, stall recovery,
tail slide) on each. Flight logs record the airframe at spawn; `replay`
re-flies with the aircraft's current definition and lists what changed
(`--recorded-airframe` flies the logged one). The three real models
(F-15E, F/A-141F, MiG-19) live in `aces-client/assets/models`, stored with
Git LFS (`*.glb`, ~85 MB of Sketchfab exports — install git-lfs before
cloning, or run `git lfs pull` after). An aircraft whose model is missing
flies the placeholder airframe.
Third-party models are credited per their licenses (two CC BY-NC-SA 4.0 —
non-commercial —, one CC BY 4.0): each `AIRCRAFT` entry carries its
`Credit`, shown on the main menu's credits screen (`K`) and listed in
`CREDITS.md` (with the font); a test keeps the three in sync.
Their airframes still fly as the placeholder jet, and `P` cycles the
displayed model in flight. The client keeps per-model scale/orientation
fixups (`flight::model_fixup`) — the Sketchfab exports face arbitrary
axes, are wildly off scale and are not centered.

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
| `P` | Cycle the displayed aircraft (models; airframes identical so far) |
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
- Native + browser (wasm via trunk, share links with `?room=`).
- Semi-realistic flight model; cone-based arcade radar/IR.
- 3-crate workspace like gnils; bevy 0.19 to match gnils.
- Respawn deathmatch (no rounds); lobby-start-only (no mid-game join for v1);
  trust-based hits (no anti-cheat — fine for friends).
