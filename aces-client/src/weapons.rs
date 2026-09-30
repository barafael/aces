//! Weapons & damage: the gun, heat-seeking and radar missiles with their
//! locks, countermeasures and the RWR, the damage → death → respawn loop,
//! and the score.
//!
//! Authority follows PLAN.md: the shooter owns its weapons and claims hits
//! (`GameEvent::Damage`), the victim owns its HP and confirms death
//! (`GameEvent::Killed`). Everything authoritative crosses the wire through
//! the host's canonical `Sequenced` stream, so all peers apply events in one
//! order — including the host's own, via loopback. Countermeasure pops and
//! radar-lock announcements are unsequenced `Ephemeral`s.
//!
//! Missiles are simulated by *every* peer: the shooter spawns its own at
//! fire time, everyone else from the `MissileLaunched` event (they are
//! short-lived; the simulations only need to look alike). Only the
//! shooter's simulation claims damage, against the plane it actually hit.
//!
//! Countermeasures: flares distract a heat-seeker into chasing a decoy
//! point; chaff breaks radar guidance into a straight coast. Every
//! simulation of a missile consults the target's deployments — the target's
//! own included — (see [`decoy_for`]); the shooter's decides the hit.
//!
//! `ACES_TEST_FIRE=1` (read at startup) enables a hands-free auto-dogfight
//! hook: the plane aims itself at the nearest enemy, locks, fires and pops
//! countermeasures — used by the two-instance end-to-end verification. It
//! flies with energy discipline (see `RECOVER_SPEED`): a max-G pursuit
//! mushes into a stall and never merges.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use aces_net::{CmKind, DamageCause, Ephemeral, GameEvent, MissileKind, NetState};
use aces_protocol::Combat;
use std::collections::{HashMap, VecDeque};
use std::env;

use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::instructor::Instructor;
use crate::flight::{
    Aircraft, FlightState, Life, LocalPlane, RemotePlane, SimPose, SpawnSet, Velocity, spawn_state,
};
use crate::net::{NetFxOut, NetIn, NetOut};
use crate::{NetworkMode, Phase};

// ── Tuning ──────────────────────────────────────────────────────────────────

// Hit points, the gun's damage and rate, the missile stores and the radar
// range are the aircraft's (`aces_protocol::Combat`).

/// Gun effective range [m].
pub const GUN_RANGE: f32 = 1500.0;
/// A plane counts as hit within this radius of the ray [m].
pub const HIT_SPHERE: f32 = 12.0;
/// Damage per missile hit (both kinds).
pub const MISSILE_DAMAGE: f32 = 60.0;
/// The IR seeker: narrow and short-ranged.
pub const IR_LOCK: LockSpec = LockSpec {
    cone: 10f32.to_radians(),
    drop_cone: 20f32.to_radians(),
    range: 4000.0,
    time: 1.5,
};
/// The radar: a coarse, long-range cone around the nose. Its range is the
/// aircraft's ([`radar_lock`]).
pub const RADAR_LOCK: LockSpec = LockSpec {
    cone: 30f32.to_radians(),
    drop_cone: 40f32.to_radians(),
    range: 8000.0,
    time: 2.0,
};

/// The radar lock of an aircraft with `combat`; `None` without a radar.
pub fn radar_lock(combat: &Combat) -> Option<LockSpec> {
    (combat.radar_range > 0.0).then_some(LockSpec {
        range: combat.radar_range,
        ..RADAR_LOCK
    })
}
/// How often a held radar lock re-announces itself to the target [s].
pub const RADAR_ANNOUNCE_PERIOD: f32 = 0.5;
/// How long an announced radar lock shows on the target's RWR [s]: long
/// enough to ride out a lost announcement.
pub const RWR_LOCK_HOLD: f64 = 1.2;
/// Missile turn rate [rad/s] (pure pursuit, arcade).
pub const MISSILE_TURN: f32 = 6.0;
/// Missile acceleration while the motor burns [m/s²].
pub const MISSILE_ACCEL: f32 = 300.0;
/// Missile speed cap [m/s].
pub const MISSILE_MAX_SPEED: f32 = 900.0;
/// Motor burn time [s].
pub const MISSILE_MOTOR: f32 = 2.5;
/// Missile lifetime [s].
pub const MISSILE_LIFE: f32 = 10.0;
/// Detonation distance [m].
pub const MISSILE_PROXIMITY: f32 = 15.0;
/// Seconds dead before respawning.
pub const RESPAWN_DELAY: f32 = 3.0;
/// Life of an explosion visual [s].
pub const EXPLOSION_LIFE: f32 = 0.8;
/// Where shots and missiles leave the airframe, ahead of its center [m].
const MUZZLE_OFFSET: f32 = 8.0;

// ── Countermeasures ─────────────────────────────────────────────────────────

/// Countermeasures of each kind carried.
pub const CM_MAX: u8 = 12;
/// Seconds per regenerated countermeasure of one kind.
pub const CM_REGEN_TIME: f32 = 5.0;
/// A deployment older than this cannot decoy anything [s].
pub const CM_VALID_WINDOW: f64 = 1.5;
/// A flare distracts a seeker that comes this close to it [m].
pub const FLARE_DISTRACT_DIST: f32 = 200.0;
/// Chaff breaks radar guidance from this close [m].
pub const CHAFF_BREAK_DIST: f32 = 250.0;

// ── State ───────────────────────────────────────────────────────────────────

/// Health of a plane (local *and* remote; remotes' is fed from snapshots).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Health {
    pub hp: f32,
    pub dead: bool,
}

impl Health {
    /// A fresh plane of an aircraft type with `combat`.
    pub fn full(combat: &Combat) -> Self {
        Self {
            hp: combat.hp,
            dead: false,
        }
    }

    /// Still flying: not killed, and with hit points left.
    pub fn alive(&self) -> bool {
        !self.dead && self.hp > 0.0
    }

    /// Wire form for [`aces_protocol::PlaneSnapshot::hp`].
    pub fn hp_byte(&self) -> u8 {
        self.hp.clamp(0.0, 255.0) as u8
    }
}

/// One missile in flight, simulated on every peer.
#[derive(Component)]
pub struct Missile {
    pub owner: String,
    pub kind: MissileKind,
    /// The peer it is guided at; `None` once chaff broke the guidance.
    pub target: Option<String>,
    /// Set once a decoy captured the seeker: the missile chases this point
    /// and detonates harmlessly.
    pub decoy: Option<Vec3>,
    pub speed: f32,
    pub age: f32,
    pub motor: f32,
}

impl Missile {
    /// Still guided at `peer`: not decoyed, guidance not broken.
    pub fn threatens(&self, peer: &str) -> bool {
        self.decoy.is_none() && self.target.as_deref() == Some(peer)
    }
}

/// Which weapon `Ctrl` fires.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponSlot {
    Gun,
    #[default]
    IrMissile,
    RadarMissile,
}

impl WeaponSlot {
    /// The missile this slot fires, if it is a missile slot.
    pub fn missile(self) -> Option<MissileKind> {
        match self {
            WeaponSlot::Gun => None,
            WeaponSlot::IrMissile => Some(MissileKind::Ir),
            WeaponSlot::RadarMissile => Some(MissileKind::Radar),
        }
    }
}

/// A seeker's lock geometry and timing.
#[derive(Debug, Clone, Copy)]
pub struct LockSpec {
    /// Acquisition cone half-angle around the nose [rad].
    pub cone: f32,
    /// The lock holds up to this cone before it drops [rad].
    pub drop_cone: f32,
    /// Lock range [m].
    pub range: f32,
    /// Seconds inside the cone until the lock is solid.
    pub time: f32,
}

/// One seeker's lock: whom the missile would chase and how solid it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lock {
    pub target: Option<String>,
    /// 0..=1.2; ≥ 1 means locked.
    pub progress: f32,
}

impl Lock {
    /// The locked target, once the lock is solid.
    pub fn solid(&self) -> Option<&str> {
        if self.progress >= 1.0 {
            self.target.as_deref()
        } else {
            None
        }
    }
}

/// Both seekers' locks.
#[derive(Resource, Debug, Default, PartialEq)]
pub struct Locks {
    pub ir: Lock,
    pub radar: Lock,
}

impl Locks {
    pub fn of(&self, kind: MissileKind) -> &Lock {
        match kind {
            MissileKind::Ir => &self.ir,
            MissileKind::Radar => &self.radar,
        }
    }
}

/// Local consumables and respawn scheduling.
#[derive(Resource, Debug)]
pub struct Loadout {
    pub ir_missiles: u8,
    pub radar_missiles: u8,
    /// Countdown while dead, `None` while flying.
    pub respawn_in: Option<f32>,
    /// Respawn count this session; spawn slots advance around the ring.
    pub respawns: usize,
}

impl Default for Loadout {
    fn default() -> Self {
        Self::new(&aces_protocol::aircraft::STANDARD_COMBAT)
    }
}

impl Loadout {
    /// Full stores for an aircraft type with `combat`, flying.
    pub fn new(combat: &Combat) -> Self {
        Self {
            ir_missiles: combat.ir_missiles,
            radar_missiles: combat.radar_missiles,
            respawn_in: None,
            respawns: 0,
        }
    }

    /// Not shot down (or waiting to respawn).
    pub fn flying(&self) -> bool {
        self.respawn_in.is_none()
    }

    /// Missiles of `kind` left.
    pub fn missiles(&self, kind: MissileKind) -> u8 {
        match kind {
            MissileKind::Ir => self.ir_missiles,
            MissileKind::Radar => self.radar_missiles,
        }
    }

    /// Take one missile of `kind` off the rail. Whether one was left.
    fn take_missile(&mut self, kind: MissileKind) -> bool {
        let left = match kind {
            MissileKind::Ir => &mut self.ir_missiles,
            MissileKind::Radar => &mut self.radar_missiles,
        };
        let taken = *left > 0;
        *left = left.saturating_sub(1);
        taken
    }

    /// Full missile stores of an aircraft type with `combat`.
    pub fn restock(&mut self, combat: &Combat) {
        self.ir_missiles = combat.ir_missiles;
        self.radar_missiles = combat.radar_missiles;
    }
}

/// Countermeasure stores with slow regeneration.
#[derive(Resource, Debug)]
pub struct Countermeasures {
    pub flares: u8,
    pub chaff: u8,
    flare_timer: f32,
    chaff_timer: f32,
}

impl Default for Countermeasures {
    fn default() -> Self {
        Self {
            flares: CM_MAX,
            chaff: CM_MAX,
            flare_timer: 0.0,
            chaff_timer: 0.0,
        }
    }
}

impl Countermeasures {
    fn regen(&mut self, dt: f32) {
        for (store, timer) in [
            (&mut self.flares, &mut self.flare_timer),
            (&mut self.chaff, &mut self.chaff_timer),
        ] {
            if *store < CM_MAX {
                *timer += dt;
                if *timer >= CM_REGEN_TIME {
                    *store += 1;
                    *timer = 0.0;
                }
            }
        }
    }

    /// Take one countermeasure of `kind`. Whether one was left.
    fn take(&mut self, kind: CmKind) -> bool {
        let store = match kind {
            CmKind::Flares => &mut self.flares,
            CmKind::Chaff => &mut self.chaff,
        };
        let taken = *store > 0;
        *store = store.saturating_sub(1);
        taken
    }
}

/// A countermeasure deployment someone made, kept briefly for missile
/// simulations to consult.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub peer: String,
    pub kind: CmKind,
    pub at: f64,
    pub pos: Vec3,
}

/// Recent deployments across the room — this peer's own included.
#[derive(Resource, Default)]
pub struct CmDeployments(pub Vec<Deployment>);

/// The RWR picture: what is threatening *me* right now.
#[derive(Resource, Default)]
pub struct Rwr {
    /// Until when an enemy radar lock is being held on me.
    pub radar_locked_until: Option<f64>,
    /// A guided heat-seeker is chasing me.
    pub ir_inbound: bool,
    /// A guided radar missile is chasing me.
    pub radar_inbound: bool,
}

/// Kills and deaths per peer, counted from the canonical `Sequenced`
/// stream — every peer, the victim included, counts the same events, so
/// boards agree.
#[derive(Resource, Default, Clone)]
pub struct Scoreboard {
    pub entries: HashMap<String, Score>,
}

/// One peer's tally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Score {
    pub kills: u32,
    pub deaths: u32,
}

impl Scoreboard {
    fn record_kill(&mut self, shooter: &str, victim: &str) {
        self.entries.entry(shooter.to_string()).or_default().kills += 1;
        self.entries.entry(victim.to_string()).or_default().deaths += 1;
    }

    /// One peer's tally.
    pub fn get(&self, peer: &str) -> Score {
        self.entries.get(peer).copied().unwrap_or_default()
    }

    /// The board as shown: every pilot in the roster (scoreless ones too),
    /// plus anyone who scored and has since left; most kills first, then
    /// fewest deaths, then by name.
    pub fn rows(&self, net: &NetState) -> Vec<(String, Score)> {
        let mut rows: Vec<(String, Score)> = net
            .players
            .iter()
            .map(|p| (p.name.clone(), self.get(&p.peer)))
            .collect();
        rows.extend(
            self.entries
                .iter()
                .filter(|(peer, _)| !net.players.iter().any(|p| &p.peer == *peer))
                .map(|(peer, score)| (callsign(net, peer), *score)),
        );
        rows.sort_by(|(a_name, a), (b_name, b)| {
            b.kills
                .cmp(&a.kills)
                .then(a.deaths.cmp(&b.deaths))
                .then(a_name.cmp(b_name))
        });
        rows
    }
}

/// How long a kill stays in the feed [s].
pub const KILL_FEED_LIFE: f64 = 6.0;
/// Feed capacity (oldest dropped beyond this).
pub const KILL_FEED_MAX: usize = 5;

/// Recent kills for the HUD feed, newest last.
#[derive(Resource, Default)]
pub struct KillFeed {
    entries: VecDeque<(f64, String)>,
}

impl KillFeed {
    fn push(&mut self, now: f64, text: String) {
        self.entries.push_back((now, text));
        if self.entries.len() > KILL_FEED_MAX {
            self.entries.pop_front();
        }
    }

    /// The entries younger than [`KILL_FEED_LIFE`], oldest first.
    pub fn fresh(&self, now: f64) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(move |(at, _)| now - *at < KILL_FEED_LIFE)
            .map(|(_, text)| text.as_str())
    }
}

/// The pilot name a peer goes by (roster name, or a short peer id).
pub fn callsign(net: &NetState, peer: &str) -> String {
    net.players
        .iter()
        .find(|p| p.peer == peer)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("pilot-{}", peer.get(..4).unwrap_or(peer)))
}

/// Shared weapon visuals, built once: every missile, explosion and decoy
/// reuses the same meshes and materials, so nothing is uploaded per shot
/// and the renderer can batch them.
#[derive(Resource, Clone)]
struct WeaponAssets {
    missile_mesh: Handle<Mesh>,
    missile_material: Handle<StandardMaterial>,
    explosion_mesh: Handle<Mesh>,
    /// The explosion fade-out, pre-baked: each explosion steps through
    /// these on its own clock instead of mutating a material every frame.
    explosion_fade: Vec<Handle<StandardMaterial>>,
    flare: Handle<StandardMaterial>,
    chaff: Handle<StandardMaterial>,
}

/// Steps of the pre-baked explosion fade.
const EXPLOSION_FADE_STEPS: usize = 12;

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponSlot>()
            .init_resource::<Locks>()
            .init_resource::<Loadout>()
            .init_resource::<Countermeasures>()
            .init_resource::<CmDeployments>()
            .init_resource::<Rwr>()
            .init_resource::<Scoreboard>()
            .init_resource::<KillFeed>()
            .add_systems(Startup, init_weapon_assets)
            .add_systems(OnEnter(Phase::InGame), rearm)
            .add_systems(OnExit(Phase::InGame), rearm)
            .add_systems(
                Update,
                (
                    (
                        update_locks,
                        announce_radar_lock.after(update_locks),
                        deploy_countermeasures,
                        fire_control,
                        apply_events,
                        apply_ephemeral,
                        prune_deployments,
                        update_rwr,
                        sync_remote_death,
                    )
                        .run_if(in_state(Phase::InGame)),
                    animate_explosions,
                ),
            )
            .add_systems(
                FixedUpdate,
                (
                    step_missiles,
                    // On the fixed tick, before the plane flies again.
                    respawn.in_set(SpawnSet),
                )
                    .run_if(in_state(Phase::InGame)),
            );
        // The test hook exists only when asked for at launch.
        if env::var("ACES_TEST_FIRE").is_ok() {
            app.add_systems(
                Update,
                (auto_dogfight, auto_countermeasures).run_if(in_state(Phase::InGame)),
            );
        }
    }
}

fn init_weapon_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let explosion_fade = (0..EXPLOSION_FADE_STEPS)
        .map(|step| {
            let left = 1.0 - step as f32 / EXPLOSION_FADE_STEPS as f32;
            materials.add(StandardMaterial {
                base_color: Color::srgba(1.0, 0.55, 0.15, 0.9 * left),
                emissive: LinearRgba::rgb(2.5, 1.2, 0.3) * left,
                alpha_mode: AlphaMode::Blend,
                ..default()
            })
        })
        .collect();
    commands.insert_resource(WeaponAssets {
        missile_mesh: meshes.add(Capsule3d::new(0.15, 1.6)),
        missile_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.9, 0.9, 0.92),
            emissive: LinearRgba::rgb(1.0, 0.55, 0.15) * 2.5,
            ..default()
        }),
        // A UV sphere: a few hundred triangles instead of the default
        // icosphere's ~20k for a blurry fireball.
        explosion_mesh: meshes.add(Sphere::new(1.0).mesh().uv(24, 12)),
        explosion_fade,
        flare: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.85, 0.4, 0.95),
            emissive: LinearRgba::rgb(4.0, 3.0, 1.0),
            ..default()
        }),
        chaff: materials.add(StandardMaterial {
            base_color: Color::srgba(0.75, 0.78, 0.82, 0.8),
            emissive: LinearRgba::rgb(0.4, 0.42, 0.45),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
    });
}

/// A fresh game (and leaving one) starts from full stores of the chosen
/// aircraft, no locks, no threats and an empty board.
#[allow(clippy::too_many_arguments)]
fn rearm(
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
    mut locks: ResMut<Locks>,
    mut countermeasures: ResMut<Countermeasures>,
    mut deployments: ResMut<CmDeployments>,
    mut rwr: ResMut<Rwr>,
    mut scoreboard: ResMut<Scoreboard>,
    mut kill_feed: ResMut<KillFeed>,
) {
    *loadout = Loadout::new(&aces_protocol::aircraft(net.aircraft).combat);
    *locks = Locks::default();
    *countermeasures = Countermeasures::default();
    *deployments = CmDeployments::default();
    *rwr = Rwr::default();
    *scoreboard = Scoreboard::default();
    *kill_feed = KillFeed::default();
}

/// Every remote plane, as a weapon sees it.
type RemoteTargets<'w, 's> =
    Query<'w, 's, (&'static RemotePlane, &'static Transform, &'static Health), Without<LocalPlane>>;

// ── Decoy logic (pure) ──────────────────────────────────────────────────────

/// What a deployment stream does to one missile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoy {
    Nothing,
    /// The seeker captured this point: chase and detonate harmlessly.
    Distract(Vec3),
    /// Guidance is broken: fly straight.
    BreakLock,
}

/// Would `target`'s latest countermeasure of the matching kind capture this
/// missile? A deployment counts while fresh ([`CM_VALID_WINDOW`]) and near
/// the missile (kind-specific distance).
pub fn decoy_for(
    kind: MissileKind,
    target: &str,
    missile_pos: Vec3,
    deployments: &[Deployment],
    now: f64,
) -> Decoy {
    let want = kind.counter();
    let max_dist = match kind {
        MissileKind::Ir => FLARE_DISTRACT_DIST,
        MissileKind::Radar => CHAFF_BREAK_DIST,
    };
    deployments
        .iter()
        .rev()
        .find(|d| {
            d.peer == target
                && d.kind == want
                && now - d.at <= CM_VALID_WINDOW
                && d.pos.distance(missile_pos) <= max_dist
        })
        .map(|d| match d.kind {
            CmKind::Flares => Decoy::Distract(d.pos),
            CmKind::Chaff => Decoy::BreakLock,
        })
        .unwrap_or(Decoy::Nothing)
}

// ── Locking ─────────────────────────────────────────────────────────────────

/// Track both locks on the living enemies. A wreck locks nothing.
fn update_locks(
    net: Res<NetState>,
    loadout: Res<Loadout>,
    plane: Single<(&FlightState, &Aircraft), With<LocalPlane>>,
    remotes: RemoteTargets,
    mut locks: ResMut<Locks>,
    time: Res<Time>,
) {
    let (plane, aircraft) = *plane;
    if !loadout.flying() {
        if *locks != Locks::default() {
            *locks = Locks::default();
        }
        return;
    }
    let me = net.my_peer();
    let candidates: Vec<(&str, Vec3)> = remotes
        .iter()
        .filter(|(remote, _, health)| health.alive() && remote.peer != me)
        .map(|(remote, transform, _)| (remote.peer.as_str(), transform.translation))
        .collect();
    let (pos, forward, dt) = (plane.pos, plane.forward(), time.delta_secs());
    let locks = &mut *locks;
    update_lock(&mut locks.ir, &IR_LOCK, &candidates, pos, forward, dt);
    match radar_lock(&aircraft.combat) {
        Some(spec) => update_lock(&mut locks.radar, &spec, &candidates, pos, forward, dt),
        None => locks.radar = Lock::default(),
    }
}

/// Advance one lock by `dt`, seen from `pos` looking along `forward`, over
/// the candidate targets `(peer, position)`. The held target keeps the
/// lock while it stays inside the drop cone — a plane crossing nearer the
/// center does not steal it; otherwise the candidate nearest the nose
/// inside the acquisition cone is acquired from scratch.
fn update_lock(
    lock: &mut Lock,
    spec: &LockSpec,
    candidates: &[(&str, Vec3)],
    pos: Vec3,
    forward: Vec3,
    dt: f32,
) {
    let angle_to = |target: Vec3| {
        let to_target = target - pos;
        let dist = to_target.length();
        (1.0..=spec.range)
            .contains(&dist)
            .then(|| forward.angle_between(to_target / dist))
    };

    let held = lock.target.as_deref().is_some_and(|held| {
        candidates
            .iter()
            .find(|(peer, _)| *peer == held)
            .and_then(|(_, target)| angle_to(*target))
            .is_some_and(|angle| angle <= spec.drop_cone)
    });
    if held {
        lock.progress = (lock.progress + dt / spec.time).min(1.2);
        return;
    }

    let best = candidates
        .iter()
        .filter_map(|(peer, target)| Some((angle_to(*target)?, *peer)))
        .filter(|(angle, _)| *angle <= spec.cone)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match best {
        Some((_, peer)) => {
            lock.target = Some(peer.to_string());
            lock.progress = dt / spec.time;
        }
        None if *lock != Lock::default() => *lock = Lock::default(),
        None => {}
    }
}

/// While a radar lock is held, keep announcing it to the victim (RWR) —
/// the moment it goes solid, then every [`RADAR_ANNOUNCE_PERIOD`].
fn announce_radar_lock(
    locks: Res<Locks>,
    mut out: ResMut<NetFxOut>,
    time: Res<Time>,
    mut since: Local<f32>,
) {
    let Some(target) = locks.radar.solid() else {
        *since = RADAR_ANNOUNCE_PERIOD;
        return;
    };
    *since += time.delta_secs();
    if *since < RADAR_ANNOUNCE_PERIOD {
        return;
    }
    *since = 0.0;
    out.effects.push(Ephemeral::RadarLocking {
        target: target.to_string(),
    });
}

// ── Countermeasures ─────────────────────────────────────────────────────────

/// Everything a countermeasure pop touches.
#[derive(SystemParam)]
struct Dispenser<'w, 's> {
    commands: Commands<'w, 's>,
    assets: Res<'w, WeaponAssets>,
    stores: ResMut<'w, Countermeasures>,
    deployments: ResMut<'w, CmDeployments>,
    out: ResMut<'w, NetFxOut>,
    net: Res<'w, NetState>,
    time: Res<'w, Time>,
}

impl Dispenser<'_, '_> {
    /// Pop one countermeasure of `kind` at `pos`, if any is left: tell the
    /// room, note it on this peer's own deployment board too (its
    /// simulations of missiles chasing it must see the decoy the shooter's
    /// simulation sees, or they fly through it and explode on the plane),
    /// and play the visual. Whether one was left.
    fn pop(&mut self, kind: CmKind, pos: Vec3) -> bool {
        if !self.stores.take(kind) {
            return false;
        }
        self.out.effects.push(Ephemeral::Countermeasures {
            kind,
            pos: pos.to_array(),
        });
        self.deployments.0.push(Deployment {
            peer: self.net.my_peer().to_string(),
            kind,
            at: self.time.elapsed_secs_f64(),
            pos,
        });
        spawn_decoy_visual(&mut self.commands, &self.assets, kind, pos);
        true
    }
}

/// `F` flares, `C` chaff. Regeneration refills both stores slowly.
fn deploy_countermeasures(
    keys: Res<ButtonInput<KeyCode>>,
    loadout: Res<Loadout>,
    plane: Single<&FlightState, With<LocalPlane>>,
    mut dispenser: Dispenser,
) {
    let dt = dispenser.time.delta_secs();
    dispenser.stores.regen(dt);
    if !loadout.flying() {
        return;
    }
    for (key, kind) in [
        (KeyCode::KeyF, CmKind::Flares),
        (KeyCode::KeyC, CmKind::Chaff),
    ] {
        if keys.just_pressed(key) {
            dispenser.pop(kind, plane.pos);
        }
    }
}

fn spawn_decoy_visual(commands: &mut Commands, assets: &WeaponAssets, kind: CmKind, pos: Vec3) {
    let (material, size, offsets): (_, _, &[Vec3]) = match kind {
        // A short burst of bright points.
        CmKind::Flares => (
            &assets.flare,
            1.2,
            &[
                Vec3::ZERO,
                Vec3::new(2.0, 1.0, -1.0),
                Vec3::new(-2.0, 0.5, 1.0),
            ],
        ),
        // One grey cloud.
        CmKind::Chaff => (&assets.chaff, 2.5, &[Vec3::ZERO]),
    };
    for offset in offsets {
        commands.spawn((
            Explosion {
                age: 0.0,
                size,
                fades: false,
            },
            Mesh3d(assets.explosion_mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(pos + *offset),
            Visibility::default(),
            DespawnOnExit(Phase::InGame),
        ));
    }
}

// ── Firing ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn fire_control(
    mut commands: Commands,
    assets: Res<WeaponAssets>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    mut slot: ResMut<WeaponSlot>,
    locks: Res<Locks>,
    mut loadout: ResMut<Loadout>,
    mut out: ResMut<NetOut>,
    plane: Single<(&FlightState, &Aircraft), With<LocalPlane>>,
    remotes: RemoteTargets,
    mut gun_cooldown: Local<f32>,
) {
    let (plane, aircraft) = *plane;
    for (key, choice) in [
        (KeyCode::Digit1, WeaponSlot::Gun),
        (KeyCode::Digit2, WeaponSlot::IrMissile),
        (KeyCode::Digit3, WeaponSlot::RadarMissile),
    ] {
        if keys.just_pressed(key) {
            *slot = choice;
        }
    }
    if !loadout.flying() {
        return;
    }
    let me = net.my_peer();

    // Gun: hitscan straight down the nose.
    *gun_cooldown += time.delta_secs();
    if keys.pressed(KeyCode::Space) && gun_ready(&mut gun_cooldown, &aircraft.combat) {
        out.events
            .extend(fire_gun(plane, &aircraft.combat, &remotes, me));
    }

    // Missile: the selected kind, on a solid lock, with one left.
    if keys.any_just_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
        && let Some(kind) = slot.missile()
        && let Some(target) = locks.of(kind).solid()
        && loadout.take_missile(kind)
    {
        launch_missile(
            &mut commands,
            &assets,
            &mut out,
            plane,
            me,
            kind,
            target.to_string(),
        );
    }
}

/// Whether the gun of an aircraft with `combat` can fire again, `cooldown`
/// seconds after its last shot; resets the cooldown if so.
fn gun_ready(cooldown: &mut f32, combat: &Combat) -> bool {
    let ready = combat.gun_rate > 0.0 && *cooldown >= 1.0 / combat.gun_rate;
    if ready {
        *cooldown = 0.0;
    }
    ready
}

/// One gun shot down `plane`'s nose: the damage claim, if it hits.
fn fire_gun(
    plane: &FlightState,
    combat: &Combat,
    remotes: &RemoteTargets,
    me: &str,
) -> Option<GameEvent> {
    let forward = plane.forward();
    let origin = plane.pos + forward * MUZZLE_OFFSET;
    gun_ray_hit(&origin, &forward, remotes, me).map(|victim| GameEvent::Damage {
        shooter: me.to_string(),
        victim,
        amount: combat.gun_damage,
        cause: DamageCause::Gun,
    })
}

/// Launch a `kind` missile from `plane` at `target`: announce it to the
/// room, and spawn this peer's own simulation of it right away (everyone
/// else spawns theirs from the sequenced event).
fn launch_missile(
    commands: &mut Commands,
    assets: &WeaponAssets,
    out: &mut NetOut,
    plane: &FlightState,
    me: &str,
    kind: MissileKind,
    target: String,
) {
    let pos = plane.pos + plane.forward() * MUZZLE_OFFSET;
    let speed = (plane.speed() + 100.0).max(250.0);
    info!(?kind, %target, "firing a missile");
    out.events.push(GameEvent::MissileLaunched {
        shooter: me.to_string(),
        kind,
        pos: pos.to_array(),
        quat: plane.quat.to_array(),
        speed,
        target: Some(target.clone()),
    });
    spawn_missile_entity(
        commands,
        assets,
        me,
        kind,
        pos,
        plane.quat,
        speed,
        Some(target),
    );
}

/// Nearest living enemy plane within the gun's hit sphere along the ray.
fn gun_ray_hit(origin: &Vec3, forward: &Vec3, remotes: &RemoteTargets, me: &str) -> Option<String> {
    let mut best: Option<(f32, String)> = None;
    for (remote, transform, health) in remotes.iter() {
        if !health.alive() || remote.peer == me {
            continue;
        }
        let to_plane = transform.translation - *origin;
        let along = to_plane.dot(*forward);
        if along < 0.0 || along > GUN_RANGE {
            continue;
        }
        // Closest approach of the ray to the plane's center.
        let perp = (to_plane - *forward * along).length();
        if perp <= HIT_SPHERE
            && best
                .as_ref()
                .is_none_or(|(best_along, _)| along < *best_along)
        {
            best = Some((along, remote.peer.clone()));
        }
    }
    best.map(|(_, victim)| victim)
}

// ── Missile simulation ──────────────────────────────────────────────────────

/// Everyone simulates every missile: decoys, pure pursuit of the target's
/// current on-screen position, motor burn, proximity fuse. Only the owner's
/// simulation claims damage.
#[allow(clippy::too_many_arguments)]
fn step_missiles(
    mut commands: Commands,
    assets: Res<WeaponAssets>,
    deployments: Res<CmDeployments>,
    mut missiles: Query<(Entity, &mut Missile, &mut Transform), Without<RemotePlane>>,
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: RemoteTargets,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
) {
    let me = net.my_peer();
    let now = time.elapsed_secs_f64();
    let dt = time.delta_secs();

    for (entity, mut missile, mut transform) in &mut missiles {
        missile.age += dt;
        if missile.motor > 0.0 {
            missile.motor -= dt;
            missile.speed = (missile.speed + MISSILE_ACCEL * dt).min(MISSILE_MAX_SPEED);
        } else {
            missile.speed *= 1.0 - 0.25 * dt;
        }

        // Countermeasures can capture the seeker (once).
        if missile.decoy.is_none()
            && let Some(target) = missile.target.as_deref()
        {
            match decoy_for(
                missile.kind,
                target,
                transform.translation,
                &deployments.0,
                now,
            ) {
                Decoy::Distract(point) => {
                    if missile.owner == me {
                        info!("the seeker went for a decoy");
                    }
                    missile.decoy = Some(point);
                }
                Decoy::BreakLock => {
                    if missile.owner == me {
                        info!("guidance broken by chaff");
                    }
                    missile.target = None;
                }
                Decoy::Nothing => {}
            }
        }

        // Where is the missile headed this tick? A decoy point replaces the
        // target, and the fuse disarms — it detonates harmlessly.
        let armed = missile.decoy.is_none();
        let guided_pos = missile.decoy.or_else(|| {
            missile.target.as_deref().and_then(|peer| {
                if peer == me {
                    Some(plane.pos)
                } else {
                    remotes
                        .iter()
                        .find(|(r, ..)| r.peer == peer)
                        .map(|(_, t, _)| t.translation)
                }
            })
        });

        if let Some(aim) = guided_pos {
            let dist = aim.distance(transform.translation);
            if armed && dist < MISSILE_PROXIMITY {
                let victim = missile.target.clone();
                detonate(
                    &mut commands,
                    &assets,
                    &missile,
                    transform.translation,
                    victim,
                    me,
                    &mut out,
                );
                commands.entity(entity).despawn();
                continue;
            }
            if !armed && dist < MISSILE_PROXIMITY * 0.6 {
                spawn_explosion(&mut commands, &assets, transform.translation, 4.0);
                commands.entity(entity).despawn();
                continue;
            }

            // Pure pursuit: turn toward the aim point at the seeker's rate.
            let desired = (aim - transform.translation).normalize_or_zero();
            let current = transform.rotation * Vec3::NEG_Z;
            let axis = current.cross(desired);
            if desired != Vec3::ZERO && current != Vec3::ZERO && axis.length_squared() > 1e-8 {
                let swing = current.angle_between(desired).min(MISSILE_TURN * dt);
                transform.rotation = (Quat::from_axis_angle(axis.normalize(), swing)
                    * transform.rotation)
                    .normalize();
            }
        }

        let heading = transform.rotation * Vec3::NEG_Z;
        transform.translation += heading * missile.speed * dt;

        // Any-plane proximity (a missile crossing the merge may hit anyone,
        // and then that plane takes the damage) — but a decoyed seeker
        // ignores planes.
        if armed
            && let Some((remote, ..)) = remotes.iter().find(|(remote, t, health)| {
                remote.peer != missile.owner
                    && health.alive()
                    && t.translation.distance(transform.translation) < MISSILE_PROXIMITY * 0.8
            })
        {
            detonate(
                &mut commands,
                &assets,
                &missile,
                transform.translation,
                Some(remote.peer.clone()),
                me,
                &mut out,
            );
            commands.entity(entity).despawn();
            continue;
        }

        if missile.age > MISSILE_LIFE || transform.translation.y < 1.0 {
            if missile.owner == me {
                info!(
                    age = missile.age,
                    y = transform.translation.y,
                    "missile expired"
                );
            }
            spawn_explosion(&mut commands, &assets, transform.translation, 3.0);
            commands.entity(entity).despawn();
        }
    }
}

/// A missile goes off at `pos`, hitting `victim` if any. Every peer shows
/// the explosion; only the shooter's simulation claims the damage (the
/// other simulations of the same missile only need to look alike).
fn detonate(
    commands: &mut Commands,
    assets: &WeaponAssets,
    missile: &Missile,
    pos: Vec3,
    victim: Option<String>,
    me: &str,
    out: &mut NetOut,
) {
    if missile.owner == me {
        info!(?victim, "missile detonated");
    }
    spawn_explosion(commands, assets, pos, 8.0);
    out.events.extend(missile_claim(missile, victim, me));
}

/// The damage claim for `missile` hitting `victim`, as seen by peer `me`:
/// only the missile's owner claims.
fn missile_claim(missile: &Missile, victim: Option<String>, me: &str) -> Option<GameEvent> {
    (missile.owner == me).then_some(())?;
    Some(GameEvent::Damage {
        shooter: missile.owner.clone(),
        victim: victim?,
        amount: MISSILE_DAMAGE,
        cause: DamageCause::Missile,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_missile_entity(
    commands: &mut Commands,
    assets: &WeaponAssets,
    owner: &str,
    kind: MissileKind,
    pos: Vec3,
    quat: Quat,
    speed: f32,
    target: Option<String>,
) {
    commands.spawn((
        Missile {
            owner: owner.to_string(),
            kind,
            target,
            decoy: None,
            speed,
            age: 0.0,
            motor: MISSILE_MOTOR,
        },
        Mesh3d(assets.missile_mesh.clone()),
        MeshMaterial3d(assets.missile_material.clone()),
        Transform::from_translation(pos).with_rotation(quat),
        Visibility::default(),
        DespawnOnExit(Phase::InGame),
    ));
}

// ── Explosions ──────────────────────────────────────────────────────────────

#[derive(Component)]
struct Explosion {
    age: f32,
    size: f32,
    /// Fireballs fade; flare/chaff visuals just burn out.
    fades: bool,
}

fn spawn_explosion(commands: &mut Commands, assets: &WeaponAssets, pos: Vec3, size: f32) {
    commands.spawn((
        Explosion {
            age: 0.0,
            size,
            fades: true,
        },
        Mesh3d(assets.explosion_mesh.clone()),
        MeshMaterial3d(assets.explosion_fade[0].clone()),
        Transform::from_translation(pos),
        Visibility::default(),
        DespawnOnExit(Phase::InGame),
    ));
}

fn animate_explosions(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<WeaponAssets>,
    mut explosions: Query<
        (
            Entity,
            &mut Explosion,
            &mut Transform,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<LocalPlane>,
    >,
) {
    for (entity, mut explosion, mut transform, mut material) in &mut explosions {
        explosion.age += time.delta_secs();
        if explosion.age > EXPLOSION_LIFE {
            commands.entity(entity).despawn();
            continue;
        }
        let t = explosion.age / EXPLOSION_LIFE;
        transform.scale = Vec3::splat(explosion.size * (0.4 + 1.6 * t));
        if explosion.fades {
            let step = ((t * EXPLOSION_FADE_STEPS as f32) as usize).min(EXPLOSION_FADE_STEPS - 1);
            if material.0 != assets.explosion_fade[step] {
                material.0 = assets.explosion_fade[step].clone();
            }
        }
    }
}

// ── Applying the sequenced stream ───────────────────────────────────────────

/// Apply host-sequenced events. Everyone runs this, including the host
/// (loopback) — one apply path for the whole room.
#[allow(clippy::too_many_arguments)]
fn apply_events(
    mut commands: Commands,
    mut net_in: ResMut<NetIn>,
    mut out: ResMut<NetOut>,
    assets: Res<WeaponAssets>,
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
    mut scoreboard: ResMut<Scoreboard>,
    mut kill_feed: ResMut<KillFeed>,
    mut plane: Single<(&FlightState, &mut Health, &mut Visibility), With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform), Without<LocalPlane>>,
    time: Res<Time>,
) {
    let me = net.my_peer();
    let now = time.elapsed_secs_f64();
    for (_, event) in net_in.sequenced.drain(..) {
        match event {
            GameEvent::MissileLaunched {
                shooter,
                kind,
                pos,
                quat,
                speed,
                target,
            } => {
                // My own launches spawned at fire time (`launch_missile`).
                if shooter == me {
                    continue;
                }
                if target.as_deref() == Some(me) {
                    info!(%shooter, ?kind, "missile launch warning");
                }
                spawn_missile_entity(
                    &mut commands,
                    &assets,
                    &shooter,
                    kind,
                    Vec3::from(pos),
                    Quat::from_array(quat).normalize(),
                    speed,
                    target,
                );
                info!(%shooter, "missile in the air");
            }
            GameEvent::Damage {
                shooter,
                victim,
                amount,
                cause: _,
            } => {
                if victim != me || !loadout.flying() {
                    continue; // the victim applies its own damage
                }
                let (state, health, visibility) = &mut *plane;
                health.hp -= amount;
                info!(shooter = %shooter, hp = health.hp, "took damage");
                if health.hp <= 0.0 {
                    health.dead = true;
                    health.hp = 0.0;
                    **visibility = Visibility::Hidden;
                    loadout.respawn_in = Some(RESPAWN_DELAY);
                    loadout.respawns += 1;
                    spawn_explosion(&mut commands, &assets, state.pos, 8.0);
                    out.events.push(GameEvent::Killed {
                        victim: me.to_string(),
                        shooter: shooter.clone(),
                    });
                    info!(%shooter, "shot down; respawning");
                }
            }
            GameEvent::Killed { victim, shooter } => {
                // My own explosion played when the Damage arm killed me. A
                // remote's snapshots carry its health (they hide the plane,
                // and bring it back once it respawns); the kill itself is
                // the explosion.
                if victim != me {
                    for (remote, transform) in &remotes {
                        if remote.peer == victim {
                            spawn_explosion(&mut commands, &assets, transform.translation, 8.0);
                        }
                    }
                }
                // Everyone — the victim included — counts the same
                // canonical event, so every board agrees.
                scoreboard.record_kill(&shooter, &victim);
                kill_feed.push(
                    now,
                    format!(
                        "{} shot down {}",
                        callsign(&net, &shooter),
                        callsign(&net, &victim)
                    ),
                );
                if shooter == me {
                    info!(%victim, "kill confirmed");
                }
            }
        }
    }
}

/// Consume unsequenced effects: deploy visuals room-wide, keep the
/// deployment board fresh for missile simulations, and announce radar
/// locks. The sender is the peer an effect concerns.
fn apply_ephemeral(
    mut commands: Commands,
    mut net_in: ResMut<NetIn>,
    assets: Res<WeaponAssets>,
    net: Res<NetState>,
    mut deployments: ResMut<CmDeployments>,
    mut rwr: ResMut<Rwr>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    let me = net.my_peer();
    for (sender, effect) in net_in.ephemeral.drain(..) {
        match effect {
            Ephemeral::Countermeasures { kind, pos } => {
                let pos = Vec3::from(pos);
                deployments.0.push(Deployment {
                    peer: sender,
                    kind,
                    at: now,
                    pos,
                });
                spawn_decoy_visual(&mut commands, &assets, kind, pos);
            }
            Ephemeral::RadarLocking { target } => {
                if target == me {
                    if rwr.radar_locked_until.is_none_or(|until| until < now) {
                        info!(%sender, "RWR: radar lock");
                    }
                    rwr.radar_locked_until = Some(now + RWR_LOCK_HOLD);
                }
            }
        }
    }
}

/// Drop stale deployments so the decoy board never grows.
fn prune_deployments(time: Res<Time>, mut deployments: ResMut<CmDeployments>) {
    let now = time.elapsed_secs_f64();
    deployments
        .0
        .retain(|d| now - d.at <= CM_VALID_WINDOW + 1.0);
}

/// The inbound-missile half of the RWR: every guided missile chasing me.
/// This peer simulates each missile aimed at it (from its launch event), so
/// the warning lasts exactly as long as the threat — it ends when the
/// missile hits, expires, or is decoyed.
fn update_rwr(net: Res<NetState>, missiles: Query<&Missile>, mut rwr: ResMut<Rwr>) {
    let me = net.my_peer();
    let (mut ir, mut radar) = (false, false);
    for missile in missiles.iter().filter(|m| m.threatens(me)) {
        match missile.kind {
            MissileKind::Ir => ir = true,
            MissileKind::Radar => radar = true,
        }
    }
    if (rwr.ir_inbound, rwr.radar_inbound) != (ir, radar) {
        rwr.ir_inbound = ir;
        rwr.radar_inbound = radar;
    }
}

/// Dead locals come back after [`RESPAWN_DELAY`] on the next ring slot.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn respawn(
    time: Res<Time>,
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
    mut countermeasures: ResMut<Countermeasures>,
    mut locks: ResMut<Locks>,
    mut rwr: ResMut<Rwr>,
    mut aim: ResMut<MouseAim>,
    mut plane: Single<
        (
            &Aircraft,
            &mut Life,
            &mut FlightState,
            &mut Instructor,
            &mut SimPose,
            &mut Velocity,
            &mut Health,
            &mut Transform,
            &mut Visibility,
        ),
        With<LocalPlane>,
    >,
) {
    let Some(mut remaining) = loadout.respawn_in else {
        return;
    };
    remaining -= time.delta_secs();
    if remaining > 0.0 {
        loadout.respawn_in = Some(remaining);
        return;
    }

    let index = net.my_index().unwrap_or(0) + loadout.respawns;
    let (aircraft, life, state, instructor, pose, velocity, health, transform, visibility) =
        &mut *plane;
    // A fresh plane, exactly like the first spawn (see `spawn_local`): its
    // own spawn state, controller and view along the new heading.
    life.0 += 1;
    **state = spawn_state(index, &aircraft.airframe);
    **instructor = Instructor::default();
    *aim = MouseAim::new(state.quat);
    **pose = SimPose::new(state.pos, state.quat);
    velocity.0 = state.vel;
    **health = Health::full(&aircraft.combat);
    **transform = Transform::from_translation(state.pos).with_rotation(state.quat);
    **visibility = Visibility::Visible;
    loadout.restock(&aircraft.combat);
    loadout.respawn_in = None;
    *countermeasures = Countermeasures::default();
    *locks = Locks::default();
    *rwr = Rwr::default();
    info!("respawned");
}

/// Remote planes whose health changed since the system last ran.
type RemoteHealthChanged = (With<RemotePlane>, Changed<Health>);

/// Remote planes stay hidden while their snapshots report them dead.
fn sync_remote_death(mut remotes: Query<(&Health, &mut Visibility), RemoteHealthChanged>) {
    for (health, mut visibility) in &mut remotes {
        visibility.set_if_neq(if health.alive() {
            Visibility::Visible
        } else {
            Visibility::Hidden
        });
    }
}

// ── Auto-dogfight test hook ─────────────────────────────────────────────────

/// Below this speed the hook flies level to regain energy instead of
/// turning: a sustained max-G pursuit bleeds speed into a stall-mush
/// (observed live), and a stalled fighter neither merges nor fights.
const RECOVER_SPEED: f32 = 130.0;

/// The hook pops a countermeasure once an inbound missile is this close [m].
const CM_POP_DIST: f32 = 250.0;

/// Seconds between the hook's countermeasure pops: one per inbound
/// missile, not the whole store in one pass.
const CM_POP_INTERVAL: f64 = 1.0;

/// Seconds between the hook's missile launches: one at a time, not the
/// whole magazine on consecutive frames.
const AUTO_LAUNCH_INTERVAL: f64 = 3.0;

/// `ACES_TEST_FIRE=1`: aim at the nearest living enemy (lead pursuit, with
/// energy discipline), then fire the gun and missiles — through the same
/// shot and launch code as the pilot's triggers — so the two-instance
/// end-to-end verification runs hands-free.
#[allow(clippy::too_many_arguments)]
fn auto_dogfight(
    mut commands: Commands,
    assets: Res<WeaponAssets>,
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    plane: Single<(&FlightState, &Aircraft), With<LocalPlane>>,
    remotes: RemoteTargets,
    mut aim: ResMut<MouseAim>,
    mut input: ResMut<FlightInput>,
    locks: Res<Locks>,
    mut loadout: ResMut<Loadout>,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
    mut gun_cooldown: Local<f32>,
    mut last_launch: Local<Option<f64>>,
) {
    if *mode == NetworkMode::Solo || !loadout.flying() {
        return;
    }
    let me = net.my_peer();
    let (state, aircraft) = *plane;

    // Nearest living enemy, with its latest reported velocity for lead
    // pursuit.
    let Some((target_pos, target_vel)) = remotes
        .iter()
        .filter(|(r, _, h)| r.peer != me && h.alive())
        .filter_map(|(r, t, _)| {
            r.history
                .back()
                .map(|(_, snap)| (t.translation, Vec3::from(snap.vel)))
        })
        .min_by(|a, b| {
            a.0.distance_squared(state.pos)
                .total_cmp(&b.0.distance_squared(state.pos))
        })
    else {
        return;
    };

    // Energy discipline: a slow fighter regains speed level before it turns
    // again — a max-G pursuit mushes into a stall (observed live).
    let speed = state.speed();
    let slow = speed < RECOVER_SPEED;
    let dir = if slow {
        let level = state.forward() * Vec3::new(1.0, 0.0, 1.0).normalize_or_zero();
        (level + Vec3::Y * 0.1).normalize_or_zero()
    } else {
        // Lead pursuit: aim where the enemy will be when we get there.
        let to_enemy = target_pos - state.pos;
        let lead_time = (to_enemy.length() / speed.max(100.0)).min(3.0);
        let aim_point = target_pos + target_vel * lead_time + Vec3::Y * 30.0;
        (aim_point - state.pos).normalize_or_zero()
    };
    if dir == Vec3::ZERO {
        return;
    }
    *aim = MouseAim::new(Transform::default().looking_to(dir, Vec3::Y).rotation);
    input.aim = aim.dir();

    // While slow, do not shoot: closing energy matters more.
    if slow {
        return;
    }

    *gun_cooldown += time.delta_secs();
    if gun_ready(&mut gun_cooldown, &aircraft.combat) {
        out.events
            .extend(fire_gun(state, &aircraft.combat, &remotes, me));
    }

    // Prefer the IR lock, fall back to radar.
    let now = time.elapsed_secs_f64();
    if last_launch.is_none_or(|at| now - at >= AUTO_LAUNCH_INTERVAL)
        && let Some((kind, target)) = [MissileKind::Ir, MissileKind::Radar]
            .into_iter()
            .filter(|kind| loadout.missiles(*kind) > 0)
            .find_map(|kind| Some((kind, locks.of(kind).solid()?)))
        && loadout.take_missile(kind)
    {
        *last_launch = Some(now);
        launch_missile(
            &mut commands,
            &assets,
            &mut out,
            state,
            me,
            kind,
            target.to_string(),
        );
    }
}

/// The hook's defense: once a guided missile chasing this plane is close
/// enough for a decoy to matter (this peer simulates every missile aimed at
/// it, so the distance is known), pop what defeats *that* missile beside
/// the plane, slightly off the missile's approach.
fn auto_countermeasures(
    mode: Res<NetworkMode>,
    loadout: Res<Loadout>,
    plane: Single<&FlightState, With<LocalPlane>>,
    missiles: Query<(&Missile, &Transform)>,
    mut dispenser: Dispenser,
    mut last_pop: Local<Option<f64>>,
) {
    if *mode == NetworkMode::Solo || !loadout.flying() {
        return;
    }
    let now = dispenser.time.elapsed_secs_f64();
    if last_pop.is_some_and(|at| now - at < CM_POP_INTERVAL) {
        return;
    }
    let me = dispenser.net.my_peer();
    let Some((missile, transform)) = missiles.iter().find(|(missile, transform)| {
        missile.threatens(me) && transform.translation.distance(plane.pos) < CM_POP_DIST
    }) else {
        return;
    };
    let pos = plane.pos + (plane.pos - transform.translation).normalize_or_zero() * 20.0;
    if dispenser.pop(missile.kind.counter(), pos) {
        *last_pop = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aces_net::PeerId;
    use aces_protocol::PlayerInfo;
    use bevy::ecs::system::RunSystemOnce;

    impl WeaponAssets {
        /// Default handles: enough for systems that only spawn entities.
        fn placeholder() -> Self {
            Self {
                missile_mesh: default(),
                missile_material: default(),
                explosion_mesh: default(),
                explosion_fade: vec![default(); EXPLOSION_FADE_STEPS],
                flare: default(),
                chaff: default(),
            }
        }
    }

    /// A net state whose own peer id is known; returns it with the id.
    fn net_as_me() -> (NetState, String) {
        let mut net = NetState::default();
        net.set_my_id(PeerId(uuid::Uuid::from_u128(7)));
        let me = net.my_peer().to_string();
        (net, me)
    }

    fn deployment(peer: &str, kind: CmKind, at: f64, pos: Vec3) -> Deployment {
        Deployment {
            peer: peer.to_string(),
            kind,
            at,
            pos,
        }
    }

    fn missile(owner: &str) -> Missile {
        Missile {
            owner: owner.to_string(),
            kind: MissileKind::Ir,
            target: Some("target".into()),
            decoy: None,
            speed: 300.0,
            age: 0.0,
            motor: 0.0,
        }
    }

    /// Flares distract an incoming heat-seeker when fresh and near.
    #[test]
    fn flares_distract_ir() {
        let now = 100.0;
        let missile_pos = Vec3::new(100.0, 500.0, 0.0);
        let flare_pos = Vec3::new(90.0, 495.0, 0.0);
        let d = vec![deployment("victim", CmKind::Flares, now - 0.5, flare_pos)];
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Distract(flare_pos)
        );
    }

    /// A stale or far deployment does nothing; chaff does not distract IR.
    #[test]
    fn decoys_have_limits() {
        let now = 100.0;
        let missile_pos = Vec3::new(100.0, 500.0, 0.0);
        let flare_pos = Vec3::new(90.0, 495.0, 0.0);

        // Stale.
        let d = vec![deployment("victim", CmKind::Flares, now - 2.0, flare_pos)];
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
        // Far.
        let d = vec![deployment(
            "victim",
            CmKind::Flares,
            now - 0.5,
            missile_pos + Vec3::X * (FLARE_DISTRACT_DIST + 1.0),
        )];
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
        // Wrong kind.
        let d = vec![deployment("victim", CmKind::Chaff, now - 0.5, flare_pos)];
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
        // Somebody else's flares.
        let d = vec![deployment("other", CmKind::Flares, now - 0.5, flare_pos)];
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
    }

    /// Chaff breaks radar guidance; flares do not.
    #[test]
    fn chaff_breaks_radar() {
        let now = 100.0;
        let missile_pos = Vec3::new(100.0, 500.0, 0.0);
        let chaff_pos = Vec3::new(150.0, 520.0, 0.0);
        let d = vec![deployment("victim", CmKind::Chaff, now - 0.4, chaff_pos)];
        assert_eq!(
            decoy_for(MissileKind::Radar, "victim", missile_pos, &d, now),
            Decoy::BreakLock
        );
        let d = vec![deployment("victim", CmKind::Flares, now - 0.4, chaff_pos)];
        assert_eq!(
            decoy_for(MissileKind::Radar, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
    }

    /// Only a fresh deployment counts, however recent the others.
    #[test]
    fn only_fresh_deployments_count() {
        let now = 100.0;
        let missile_pos = Vec3::ZERO;
        let d = vec![
            deployment(
                "victim",
                CmKind::Flares,
                now - 1.6,
                Vec3::new(5.0, 0.0, 0.0),
            ),
            deployment("victim", CmKind::Chaff, now - 0.1, Vec3::new(6.0, 0.0, 0.0)),
        ];
        // The IR seeker only sees flares; the old flare is beyond the window,
        // so nothing distracts it.
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
    }

    /// Popping a countermeasure notes it on this peer's own board too: the
    /// victim's simulation of a missile chasing it must fall for the same
    /// decoy as the shooter's, or the victim watches it fly through the
    /// flares and explode on the plane (while the shooter's says "missed").
    #[test]
    fn own_countermeasures_decoy_my_own_simulation() {
        let (net, me) = net_as_me();
        let mut world = World::new();
        world.insert_resource(net);
        world.insert_resource(WeaponAssets::placeholder());
        world.init_resource::<Countermeasures>();
        world.init_resource::<CmDeployments>();
        world.init_resource::<NetFxOut>();
        world.insert_resource(Time::<()>::default());

        let flare_pos = Vec3::new(0.0, 500.0, 0.0);
        let popped = world
            .run_system_once(move |mut dispenser: Dispenser| {
                dispenser.pop(CmKind::Flares, flare_pos)
            })
            .unwrap();

        assert!(popped);
        assert_eq!(world.resource::<Countermeasures>().flares, CM_MAX - 1);
        assert_eq!(
            world.resource::<NetFxOut>().effects.len(),
            1,
            "the room hears"
        );
        let board = &world.resource::<CmDeployments>().0;
        assert_eq!(
            decoy_for(MissileKind::Ir, &me, flare_pos + Vec3::X * 50.0, board, 0.5),
            Decoy::Distract(flare_pos)
        );
    }

    /// An empty store pops nothing — and never wraps around.
    #[test]
    fn stores_do_not_underflow() {
        let mut stores = Countermeasures {
            chaff: 0,
            ..default()
        };
        assert!(!stores.take(CmKind::Chaff));
        assert_eq!(stores.chaff, 0);

        let mut loadout = Loadout {
            radar_missiles: 1,
            ..default()
        };
        assert!(loadout.take_missile(MissileKind::Radar));
        assert!(!loadout.take_missile(MissileKind::Radar));
        assert_eq!(loadout.radar_missiles, 0);
    }

    /// A held lock stays on its target while it is inside the drop cone,
    /// even when another plane crosses nearer the center.
    #[test]
    fn a_held_lock_is_not_stolen() {
        let spec = IR_LOCK;
        let forward = Vec3::NEG_Z;
        let ahead =
            |degrees: f32| Quat::from_rotation_y(degrees.to_radians()) * Vec3::NEG_Z * 1000.0;
        let mut lock = Lock::default();

        // Acquire `a` near the center.
        for _ in 0..200 {
            update_lock(
                &mut lock,
                &spec,
                &[("a", ahead(5.0))],
                Vec3::ZERO,
                forward,
                0.01,
            );
        }
        assert_eq!(lock.solid(), Some("a"));

        // `b` crosses dead center while `a` drifts inside the drop cone.
        let both = [("a", ahead(15.0)), ("b", ahead(0.0))];
        update_lock(&mut lock, &spec, &both, Vec3::ZERO, forward, 0.01);
        assert_eq!(lock.solid(), Some("a"), "b stole the lock");

        // `a` leaves the drop cone: the lock moves to `b`, from scratch.
        let both = [("a", ahead(25.0)), ("b", ahead(0.0))];
        update_lock(&mut lock, &spec, &both, Vec3::ZERO, forward, 0.01);
        assert_eq!(lock.target.as_deref(), Some("b"));
        assert!(lock.solid().is_none(), "a new target starts unlocked");

        // Nobody in range: the lock drops.
        update_lock(&mut lock, &spec, &[], Vec3::ZERO, forward, 0.01);
        assert_eq!(lock, Lock::default());
    }

    /// Every peer simulates every missile, but a hit is claimed exactly
    /// once: by the shooter's simulation, against the plane it hit.
    #[test]
    fn only_the_shooter_claims_missile_damage() {
        let m = missile("shooter");
        assert!(missile_claim(&m, Some("target".into()), "bystander").is_none());
        assert!(missile_claim(&m, Some("target".into()), "target").is_none());
        match missile_claim(&m, Some("other".into()), "shooter") {
            Some(GameEvent::Damage {
                shooter, victim, ..
            }) => {
                assert_eq!(shooter, "shooter");
                assert_eq!(victim, "other", "the plane actually hit takes the damage");
            }
            other => panic!("expected a damage claim, got {other:?}"),
        }
        assert!(
            missile_claim(&m, None, "shooter").is_none(),
            "a dud claims nothing"
        );
    }

    /// Missiles, explosions and decoys belong to the game: leaving it (back
    /// to the menu) cleans them up instead of leaving them frozen in the
    /// scene, or carrying them into the next game.
    #[test]
    fn leaving_the_game_clears_missiles_and_explosions() {
        use bevy::state::app::StatesPlugin;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<Phase>();
        app.world_mut()
            .resource_mut::<NextState<Phase>>()
            .set(Phase::InGame);
        app.update();

        let assets = WeaponAssets::placeholder();
        let mut commands = app.world_mut().commands();
        spawn_missile_entity(
            &mut commands,
            &assets,
            "me",
            MissileKind::Ir,
            Vec3::ZERO,
            Quat::IDENTITY,
            300.0,
            None,
        );
        spawn_explosion(&mut commands, &assets, Vec3::ZERO, 8.0);
        spawn_decoy_visual(&mut commands, &assets, CmKind::Chaff, Vec3::ZERO);
        app.world_mut().flush();
        let count = |app: &mut App| {
            let world = app.world_mut();
            let missiles = world.query::<&Missile>().iter(world).count();
            let explosions = world.query::<&Explosion>().iter(world).count();
            (missiles, explosions)
        };
        assert_eq!(count(&mut app), (1, 2));

        app.world_mut()
            .resource_mut::<NextState<Phase>>()
            .set(Phase::Menu);
        app.update();
        assert_eq!(
            count(&mut app),
            (0, 0),
            "missiles/explosions survived leaving the game"
        );
    }

    /// Every peer counts every kill — the victim its own death too — so the
    /// boards and feeds agree.
    #[test]
    fn everyone_counts_every_kill() {
        let (mut net, me) = net_as_me();
        net.players = vec![
            PlayerInfo {
                peer: me.clone(),
                name: "otter".into(),
                aircraft: 0,
            },
            PlayerInfo {
                peer: "falcon-peer".into(),
                name: "falcon".into(),
                aircraft: 0,
            },
        ];
        let mut world = World::new();
        world.insert_resource(net);
        world.insert_resource(WeaponAssets::placeholder());
        world.init_resource::<NetOut>();
        world.init_resource::<Loadout>();
        world.init_resource::<Scoreboard>();
        world.init_resource::<KillFeed>();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(NetIn {
            sequenced: vec![
                (
                    0,
                    GameEvent::Killed {
                        victim: me.clone(),
                        shooter: "falcon-peer".into(),
                    },
                ),
                (
                    1,
                    GameEvent::Killed {
                        victim: "falcon-peer".into(),
                        shooter: me.clone(),
                    },
                ),
            ],
            ..default()
        });
        world.spawn((
            LocalPlane,
            FlightState::new(Vec3::ZERO, Quat::IDENTITY, 150.0, 1.0),
            Health::full(&aces_protocol::aircraft::STANDARD_COMBAT),
            Visibility::default(),
        ));

        world.run_system_once(apply_events).unwrap();

        let board = world.resource::<Scoreboard>();
        assert_eq!(
            board.get(&me),
            Score {
                kills: 1,
                deaths: 1
            },
            "my own death"
        );
        assert_eq!(
            board.get("falcon-peer"),
            Score {
                kills: 1,
                deaths: 1
            }
        );
        let feed: Vec<_> = world.resource::<KillFeed>().fresh(0.0).collect();
        assert_eq!(
            feed,
            vec!["falcon shot down otter", "otter shot down falcon"]
        );
    }

    /// The board lists the whole roster (scoreless pilots too), most kills
    /// first, then fewest deaths.
    #[test]
    fn scoreboard_rows_rank_the_roster() {
        let mut net = NetState::default();
        net.players = ["a", "b", "c", "d"]
            .map(|peer| PlayerInfo {
                peer: peer.into(),
                name: format!("pilot {peer}"),
                aircraft: 0,
            })
            .to_vec();
        let mut board = Scoreboard::default();
        board.record_kill("a", "b");
        board.record_kill("c", "b");
        board.record_kill("c", "a");
        board.record_kill("gone", "a"); // a pilot who has left the room

        let names: Vec<_> = board
            .rows(&net)
            .into_iter()
            .map(|(name, score)| (name, score.kills, score.deaths))
            .collect();
        assert_eq!(
            names,
            vec![
                ("pilot c".into(), 2, 0),
                ("pilot-gone".into(), 1, 0),
                ("pilot a".into(), 1, 2),
                ("pilot d".into(), 0, 0),
                ("pilot b".into(), 0, 2),
            ]
        );
    }

    /// The kill feed keeps the freshest few entries and expires the rest.
    #[test]
    fn kill_feed_expires() {
        let mut feed = KillFeed::default();
        feed.push(10.0, "old".into());
        feed.push(10.0 + KILL_FEED_LIFE - 1.0, "fresh".into());
        let now = 10.0 + KILL_FEED_LIFE;
        let fresh: Vec<_> = feed.fresh(now).collect();
        assert_eq!(fresh, vec!["fresh"]);

        for i in 0..(KILL_FEED_MAX + 2) {
            feed.push(now, format!("n{i}"));
        }
        let kept: Vec<_> = feed.fresh(now).collect();
        let newest: Vec<_> = (2..KILL_FEED_MAX + 2).map(|i| format!("n{i}")).collect();
        assert_eq!(kept, newest, "the newest entries survive, in order");
    }
}
