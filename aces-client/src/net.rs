//! Client-side networking: the matchbox socket pump and the game-side net
//! systems (snapshot broadcast, remote-plane interpolation).
//!
//! The lobby conversation lives here too, adapted from gnils: peers greet
//! with `Hello`, the host is the single roster authority (`Roster`
//! broadcasts) and starts the game with `Start`. In-game, semantic events
//! flow guest → host → host-sequenced `Sequenced` rebroadcasts (the event
//! set is still empty until milestone 4, but the sequencing plumbing runs).
//!
//! What is new versus gnils is the continuous-sim part: every peer
//! broadcasts its own plane's [`PlaneSnapshot`] on the unreliable channel
//! (`send_snapshots`), and remote planes are driven purely by interpolation
//! (`apply_snapshots` + `interpolate_remotes`, see [`interp_pose`]).

use bevy::prelude::*;

use aces_net::{
    CH_RELIABLE, GameEvent, GameStart, MatchboxSocket, NetMsg, NetState, PeerId, PeerState,
    broadcast_reliable, broadcast_unreliable, decode,
};
use aces_protocol::{PlaneSnapshot, SNAPSHOT_HZ, dequantize_surface, quantize_surface};
use std::collections::VecDeque;

use crate::flight::{LocalPlane, RemotePlane, SimPose, Surfaces, Velocity};
use crate::{NetworkMode, Phase};

pub struct ClientNetPlugin;

impl Plugin for ClientNetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetIn>()
            .init_resource::<NetOut>()
            .init_resource::<SnapshotPulse>()
            .add_systems(
                Update,
                (
                    handle_socket,
                    submit_events,
                    watch_start,
                    send_snapshots.run_if(in_state(Phase::InGame)),
                    apply_snapshots.run_if(in_state(Phase::InGame)),
                    interpolate_remotes.run_if(in_state(Phase::InGame)),
                    despawn_gone_remotes.run_if(in_state(Phase::InGame)),
                ),
            );
    }
}

/// Frame staging for messages that game systems consume.
#[derive(Resource, Default)]
pub struct NetIn {
    /// Snapshots received this frame: (sender peer, snapshot).
    pub snapshots: Vec<(String, PlaneSnapshot)>,
    /// Host-sequenced events in canonical order, applied once by
    /// `weapons::apply_events`. Includes the host's own submissions
    /// (loopback), so every peer applies through one path.
    pub sequenced: Vec<(u32, GameEvent)>,
}

/// Frame-scoped staging for outgoing game events. Systems push claims
/// (hits, launches) here; `submit_events` sends them through the host's
/// canonical sequencing.
#[derive(Resource, Default)]
pub struct NetOut {
    pub events: Vec<GameEvent>,
}

/// Snapshot broadcast pacing and the sender's tick counter.
#[derive(Resource, Default)]
struct SnapshotPulse {
    acc: f32,
    tick: u32,
}

// ── Interpolation parameters ────────────────────────────────────────────────

/// Remote planes are rendered this far behind the newest sample, so a single
/// lost or late snapshot never causes a visible hitch.
pub const INTERP_DELAY: f64 = 0.1;
/// How far past the newest sample remote planes may dead-reckon [s].
pub const MAX_EXTRAPOLATION: f64 = 0.25;
/// Snapshots kept per remote plane (≥ (INTERP_DELAY + jitter) · SNAPSHOT_HZ).
pub const MAX_HISTORY: usize = 32;

// ── Socket pump ─────────────────────────────────────────────────────────────

/// The whole room conversation, one pump: peer changes, host election,
/// greetings, the host's roster broadcasts, the `Start`, in-game event
/// sequencing, and snapshot staging.
fn handle_socket(
    socket: Option<ResMut<MatchboxSocket>>,
    mut net: ResMut<NetState>,
    mut net_in: ResMut<NetIn>,
    mode: Res<NetworkMode>,
) {
    if *mode == NetworkMode::Solo {
        return;
    }
    let Some(mut socket) = socket else {
        return;
    };

    let Ok(peer_updates) = socket.try_update_peers() else {
        return;
    };
    let mut peers_changed = false;
    for (peer, peer_state) in peer_updates {
        match peer_state {
            PeerState::Connected if !net.peers.contains(&peer) => {
                net.peers.push(peer);
                peers_changed = true;
                info!(%peer, "peer connected");
            }
            PeerState::Disconnected => {
                let before = net.peers.len();
                net.peers.retain(|&p| p != peer);
                peers_changed |= net.peers.len() != before;
                info!(%peer, "peer disconnected");
            }
            _ => {}
        }
    }

    let my_id_just_set = net.my_id.is_none() && socket.id().is_some();
    if my_id_just_set {
        net.my_id = socket.id();
    }
    if peers_changed || my_id_just_set {
        net.refresh_sorted();
    }
    if let Some(my_id) = net.my_id
        && (peers_changed || my_id_just_set)
    {
        let was_host = net.is_host;
        net.is_host = net.sorted_all().first() == Some(&my_id);
        if net.is_host && !was_host {
            // Newly elected (initial election or failover): continue the
            // sequence after the last applied event (0 if none were applied
            // yet).
            net.next_seq = net.last_applied_seq.map(|s| s + 1).unwrap_or(0);
            info!("host election: this peer is now the host");
        }
    }

    let peers = net.peers.clone();

    // The host prunes roster entries whose peer has left the mesh.
    if net.is_host && prune_departed(&mut net) {
        publish_roster(&mut socket, &peers, &net);
    }

    // Announce ourselves to peers we have not greeted yet; a re-greet is
    // also how a rename or aircraft change propagates.
    let me = net.my_id.map(|id| id.to_string()).unwrap_or_default();
    if net.name.is_empty() && !me.is_empty() {
        net.name = format!("player-{}", &me[..me.len().min(4)]);
    }
    let unacquainted: Vec<PeerId> = peers
        .iter()
        .filter(|p| !net.greeted.contains(p))
        .copied()
        .collect();
    if !unacquainted.is_empty() {
        let (name, aircraft) = (net.name.clone(), net.aircraft);
        broadcast_reliable(
            &mut socket,
            &peers,
            &NetMsg::Hello {
                name: name.clone(),
                aircraft,
            },
        );
        if net.is_host && upsert_player(&mut net, &me, name, aircraft) {
            publish_roster(&mut socket, &peers, &net);
        }
        info!(name = %net.name, greeted = unacquainted.len(), "greeted the room");
        net.greeted.extend(unacquainted);
    }

    let reliable = socket.channel_mut(CH_RELIABLE).receive();
    let unreliable = socket.channel_mut(aces_net::CH_UNRELIABLE).receive();

    for (peer, raw) in reliable.into_iter().chain(unreliable) {
        let Some(msg) = decode(&raw) else {
            continue;
        };
        let peer_str = peer.to_string();
        match msg {
            NetMsg::Hello { name, aircraft } => {
                if net.is_host && upsert_player(&mut net, &peer_str, name, aircraft) {
                    publish_roster(&mut socket, &peers, &net);
                }
            }
            NetMsg::Roster(players) => {
                if !net.is_host {
                    net.players = players;
                }
            }
            NetMsg::Start { seed, players } => {
                if !net.is_host {
                    net.players = players.clone();
                    net.start = Some(GameStart { seed, players });
                    info!("the host started the game");
                }
            }
            NetMsg::Game(ev) => {
                if !net.is_host {
                    // Not the host: forward the submission to whoever we
                    // currently consider the host (transient election
                    // disagreement right after a peer connect).
                    if let Some(host) = net.host_id()
                        && let Some(encoded) = aces_net::enc_msg(&NetMsg::Game(ev))
                    {
                        let _ = socket.channel_mut(CH_RELIABLE).try_send(encoded, host);
                    }
                    continue;
                }
                // Sequence the submission and rebroadcast. The host applies
                // its own events through the same `Sequenced` path as
                // everyone else (loopback).
                let seq = net.next_seq;
                net.next_seq += 1;
                broadcast_reliable(&mut socket, &peers, &NetMsg::Sequenced { seq, event: ev.clone() });
                net_in.sequenced.push((seq, ev));
            }
            NetMsg::Sequenced { seq, event } => {
                // Apply-once, in canonical order.
                if net.last_applied_seq.is_none_or(|last| seq > last) {
                    net.last_applied_seq = Some(seq);
                    net_in.sequenced.push((seq, event));
                }
            }
            NetMsg::Snapshot(s) => {
                net_in.snapshots.push((peer_str, s));
            }
        }
    }
}

/// Host: drop roster entries whose peer has left. Returns whether anything
/// changed.
fn prune_departed(net: &mut NetState) -> bool {
    let me = net.my_id.map(|id| id.to_string());
    let connected: std::collections::HashSet<String> =
        net.peers.iter().map(|p| p.to_string()).collect();
    let before = net.players.len();
    net.players
        .retain(|p| me.as_ref() == Some(&p.peer) || connected.contains(&p.peer));
    net.players.len() != before
}

/// Host: insert or refresh a roster entry. Returns whether the roster
/// changed.
pub fn upsert_player(net: &mut NetState, peer: &str, name: String, aircraft: u8) -> bool {
    if let Some(existing) = net.players.iter_mut().find(|p| p.peer == peer) {
        if existing.name != name || existing.aircraft != aircraft {
            existing.name = name;
            existing.aircraft = aircraft;
            return true;
        }
        false
    } else {
        net.players.push(aces_protocol::PlayerInfo {
            peer: peer.to_string(),
            name,
            aircraft,
        });
        true
    }
}

pub fn publish_roster(socket: &mut MatchboxSocket, peers: &[PeerId], net: &NetState) {
    broadcast_reliable(socket, peers, &NetMsg::Roster(net.players.clone()));
}

/// Drain the game's outgoing event queue through the host's canonical
/// sequencing (host: sequence + broadcast + loopback; guest: forward to the
/// host, who sequences it back to everyone).
fn submit_events(
    socket: Option<ResMut<MatchboxSocket>>,
    mut net: ResMut<NetState>,
    mut net_in: ResMut<NetIn>,
    mut out: ResMut<NetOut>,
    mode: Res<NetworkMode>,
) {
    if out.events.is_empty() || *mode == NetworkMode::Solo {
        out.events.clear();
        return;
    }
    let Some(mut socket) = socket else {
        out.events.clear();
        return;
    };
    let peers = net.peers.clone();
    for ev in out.events.drain(..) {
        if net.is_host {
            let seq = net.next_seq;
            net.next_seq += 1;
            broadcast_reliable(&mut socket, &peers, &NetMsg::Sequenced { seq, event: ev.clone() });
            net_in.sequenced.push((seq, ev));
        } else if let Some(host) = net.host_id()
            && let Some(encoded) = aces_net::enc_msg(&NetMsg::Game(ev))
        {
            let _ = socket.channel_mut(CH_RELIABLE).try_send(encoded, host);
        }
    }
}

/// Apply the host's `Start`: move from the lobby into the game.
fn watch_start(net: Res<NetState>, state: Res<State<Phase>>, mut next: ResMut<NextState<Phase>>) {
    if net.start.is_some() && state.get() == &Phase::Lobby {
        next.set(Phase::InGame);
    }
}

// ── Snapshots ───────────────────────────────────────────────────────────────

/// Broadcast this peer's plane state at [`SNAPSHOT_HZ`], from the simulated
/// pose (not the render-interpolated one).
fn send_snapshots(
    time: Res<Time>,
    mut pulse: ResMut<SnapshotPulse>,
    socket: Option<ResMut<MatchboxSocket>>,
    net: Res<NetState>,
    plane: Single<(&SimPose, &Velocity, &Surfaces, &crate::weapons::Health), With<LocalPlane>>,
) {
    pulse.acc += time.delta_secs();
    if pulse.acc < 1.0 / SNAPSHOT_HZ {
        return;
    }
    pulse.acc %= 1.0 / SNAPSHOT_HZ;

    let Some(mut socket) = socket else {
        return;
    };
    if net.peers.is_empty() {
        return;
    }
    pulse.tick = pulse.tick.wrapping_add(1);

    let (pos, quat) = plane.0.current;
    broadcast_unreliable(
        &mut socket,
        &net.peers,
        &NetMsg::Snapshot(PlaneSnapshot {
            tick: pulse.tick,
            pos: pos.to_array(),
            rot: quat.to_array(),
            vel: plane.1.0.to_array(),
            surfaces: [
                quantize_surface(plane.2.elevator),
                quantize_surface(plane.2.aileron),
                quantize_surface(plane.2.rudder),
            ],
            hp: plane.3.hp_byte(),
        }),
    );
}

/// Drain received snapshots into the remote planes' histories. Older ticks
/// than the newest known one are dropped (reorder protection; the tick
/// wraps only after ~6.8 years at 20 Hz).
fn apply_snapshots(
    time: Res<Time>,
    mut net_in: ResMut<NetIn>,
    mut remotes: Query<&mut RemotePlane>,
) {
    let now = time.elapsed_secs_f64();
    for (peer, s) in net_in.snapshots.drain(..) {
        let Some(mut remote) = remotes.iter_mut().find(|r| r.peer == peer) else {
            continue;
        };
        if let Some((_, last)) = remote.history.back()
            && s.tick <= last.tick
        {
            continue;
        }
        if remote.history.is_empty() {
            info!(peer = %remote.peer, "receiving snapshots from a remote plane");
        }
        remote.history.push_back((now, s));
        while remote.history.len() > MAX_HISTORY {
            remote.history.pop_front();
        }
    }
}

/// Drive the rendered remote planes (pose and control surfaces) from their
/// snapshot histories.
fn interpolate_remotes(
    time: Res<Time>,
    mut remotes: Query<(&RemotePlane, &mut Transform, &mut Surfaces)>,
) {
    let now = time.elapsed_secs_f64();
    for (remote, mut transform, mut surfaces) in &mut remotes {
        if let Some(sample) = interp_pose(&remote.history, now) {
            transform.translation = sample.pos;
            transform.rotation = sample.rot;
            *surfaces = sample.surfaces;
        }
    }
}

/// Where a remote plane renders, and how its surfaces are deflected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemoteSample {
    pub pos: Vec3,
    pub rot: Quat,
    pub surfaces: Surfaces,
    /// The sender's reported health (0 while dead).
    pub hp: f32,
}

impl RemoteSample {
    fn of(s: &PlaneSnapshot) -> Self {
        Self {
            pos: Vec3::from(s.pos),
            rot: Quat::from_array(s.rot).normalize(),
            surfaces: Surfaces {
                elevator: dequantize_surface(s.surfaces[0]),
                aileron: dequantize_surface(s.surfaces[1]),
                rudder: dequantize_surface(s.surfaces[2]),
            },
            hp: s.hp as f32,
        }
    }

    fn lerp(&self, other: &Self, f: f32) -> Self {
        Self {
            pos: self.pos.lerp(other.pos, f),
            rot: self.rot.slerp(other.rot, f),
            surfaces: Surfaces {
                elevator: self.surfaces.elevator.lerp(other.surfaces.elevator, f),
                aileron: self.surfaces.aileron.lerp(other.surfaces.aileron, f),
                rudder: self.surfaces.rudder.lerp(other.surfaces.rudder, f),
            },
            // Health is discrete state, not a pose: take the nearest side's
            // value so a death is never interpolated away.
            hp: if f < 0.5 { self.hp } else { other.hp },
        }
    }
}

/// What a remote plane should render at local time `now`, given its
/// `(receive time, snapshot)` history (oldest first).
///
/// Renders [`INTERP_DELAY`] behind the newest sample: normally interpolating
/// between the two samples bracketing the target time; if the stream stalls,
/// dead-reckons along the newest velocity for at most [`MAX_EXTRAPOLATION`].
/// `None` when there is nothing to render yet.
pub fn interp_pose(history: &VecDeque<(f64, PlaneSnapshot)>, now: f64) -> Option<RemoteSample> {
    let (t_last, newest) = history.back()?;
    let target = now - INTERP_DELAY;
    if target >= *t_last {
        // Stalled stream: dead-reckon along the last velocity.
        let overshoot = (target - t_last).min(MAX_EXTRAPOLATION) as f32;
        let mut sample = RemoteSample::of(newest);
        sample.pos += Vec3::from(newest.vel) * overshoot;
        return Some(sample);
    }

    let (t_first, oldest) = history.front()?;
    if target <= *t_first {
        return Some(RemoteSample::of(oldest));
    }

    // Find the bracketing pair and interpolate.
    let mut prev = history.front()?;
    for next in history.iter().skip(1) {
        if target < next.0 {
            let f = ((target - prev.0) / (next.0 - prev.0)) as f32;
            return Some(RemoteSample::of(&prev.1).lerp(&RemoteSample::of(&next.1), f));
        }
        prev = next;
    }
    Some(RemoteSample::of(newest))
}

/// Despawn remote planes whose peer is no longer in the roster.
fn despawn_gone_remotes(
    mut commands: Commands,
    net: Res<NetState>,
    remotes: Query<(Entity, &RemotePlane)>,
) {
    for (entity, remote) in &remotes {
        if !net.players.iter().any(|p| p.peer == remote.peer) {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(t: f64, pos: [f32; 3], vel: [f32; 3]) -> (f64, PlaneSnapshot) {
        (
            t,
            PlaneSnapshot {
                tick: (t * 20.0) as u32,
                pos,
                rot: [0.0, 0.0, 0.0, 1.0],
                vel,
                surfaces: [0, 64, -127],
                hp: 100,
            },
        )
    }

    #[test]
    fn empty_history_renders_nothing() {
        assert_eq!(interp_pose(&VecDeque::new(), 100.0), None);
    }

    #[test]
    fn single_sample_renders_itself() {
        let h = VecDeque::from(vec![snap(10.0, [1.0, 2.0, 3.0], [0.0; 3])]);
        let sample = interp_pose(&h, 10.0 + 5.0).unwrap();
        assert_eq!(
            (sample.pos, sample.rot),
            (Vec3::new(1.0, 2.0, 3.0), Quat::IDENTITY)
        );
        assert_eq!(sample.surfaces.rudder, -1.0);
    }

    #[test]
    fn interpolates_between_brackets() {
        // Samples at 0.1 s spacing; render time is 0.05 s behind newest.
        let h = VecDeque::from(vec![
            snap(1.00, [0.0, 0.0, 0.0], [0.0; 3]),
            snap(1.10, [10.0, 0.0, 0.0], [0.0; 3]),
        ]);
        let now = 1.20; // target = 1.10 - wait: now - 0.1 = 1.10 → newest
        // target 1.10 == t_last → extrapolation branch with overshoot 0.
        let pos = interp_pose(&h, now).unwrap().pos;
        assert!((pos.x - 10.0).abs() < 1e-4, "{pos:?}");

        let now = 1.15; // target 1.05: halfway between the samples
        let pos = interp_pose(&h, now).unwrap().pos;
        assert!((pos.x - 5.0).abs() < 1e-4, "{pos:?}");
    }

    #[test]
    fn stalled_stream_dead_reckons_capped() {
        let h = VecDeque::from(vec![
            snap(1.00, [0.0, 0.0, 0.0], [100.0, 0.0, 0.0]),
            snap(1.10, [10.0, 0.0, 0.0], [100.0, 0.0, 0.0]),
        ]);
        // 0.5 s after the last sample: dead-reckoning is capped at 0.25 s
        // worth of velocity beyond the newest position.
        let pos = interp_pose(&h, 1.60).unwrap().pos;
        assert!((pos.x - 10.0 - 25.0).abs() < 1e-3, "{pos:?}");
    }
}
