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

## Flight model (semi-realistic)

- State: velocity vector; AoA computed from velocity vs nose. Lift ∝ AoA (with
  stall past critical AoA → lift collapse), induced + parasitic drag, thrust
  from throttle, gravity.
- Controls: **mouse = aim cursor, War Thunder arcade style** — the cursor is
  an aim point; an instructor controller banks into off-axis targets and
  pulls, flying the nose onto the cursor (flyable with mouse alone). A HUD
  cursor ring shows the aim point, an orange nose marker shows where the
  plane actually points. **A/D roll** and **Q/E rudder** add manual inputs,
  stall possible but forgiving. Hard G limits to keep it playable.
- World: flat ocean plane with a 1 km grid (mipmapped), gradient sky sphere +
  linear distance fog, soft world bounds, scattered spawn points. (Terrain =
  stretch goal.)

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

5 glTF models (`assets/aircraft/*.glb`) selectable in the lobby, with a stats
table in aces-protocol: max thrust, mass, max AoA, HP, agility multipliers
(pitch/roll/yaw). Fallback low-poly placeholder until the real models arrive.

### glb export recipe (verified against bevy_gltf 0.19)

Bevy's first-class format is glTF; `.glb` is ideal for wasm (single
self-contained file = one HTTP request). **Export from Blender with
compression OFF**: bevy 0.19 supports neither Draco, nor meshopt, nor
`KHR_mesh_quantization`, nor the `KHR_texture_basisu` extension syntax.
+Y up (Blender default), textures embedded, keep them ≤ 2k for wasm payload.

## Controls

| Key | Action |
| --- | --- |
| `Mouse` | Aim cursor — instructor flies the nose onto it |
| `A` / `D` | Roll |
| `Q` / `E` | Rudder |
| `W` / `S` | Throttle |
| `Space` | Fire gun |
| `Ctrl` | Fire selected missile |
| `1` / `2` / `3` | Select gun / IR missile / radar missile |
| `F` | Flares |
| `C` | Chaff |
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

- Mouse = aim cursor with WT-arcade instructor; V = free look; F/C = separate
  flare/chaff keys.
- Dev profile builds deps at `opt-level = 1`: unoptimized debug wasm was
  ~1.4 GB and OOM-killed wasm-bindgen; browser dev uses `trunk serve`
  (debug) or `trunk build --release` (42 MB wasm) when memory is tight.
- Native + browser (wasm via trunk, share links with `?room=`).
- Semi-realistic flight model; cone-based arcade radar/IR.
- 3-crate workspace like gnils; bevy 0.19 to match gnils.
- Respawn deathmatch (no rounds); lobby-start-only (no mid-game join for v1);
  trust-based hits (no anti-cheat — fine for friends).
