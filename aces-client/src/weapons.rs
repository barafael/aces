//! Weapons & damage (milestones 4–5): gun, heat-seeking and radar missiles
//! with their locks, countermeasures, and the damage → death → respawn loop.
//!
//! Authority follows PLAN.md: the shooter owns its weapons and claims hits
//! (`GameEvent::Damage`), the victim owns its HP and confirms death
//! (`GameEvent::Killed`). Everything authoritative crosses the wire through
//! the host's canonical `Sequenced` stream; countermeasure pops and radar
//! lock announcements are fire-and-forget `Ephemeral`s on the unreliable
//! channel.
//!
//! Missiles are simulated by *every* peer from the `MissileLaunched` event
//! (they are short-lived; the simulations only need to look alike). Only
//! the owner's simulation claims damage.
//!
//! Countermeasures: flares distract a heat-seeker into chasing a decoy
//! point; chaff breaks radar guidance into a ballistic coast. Whether a
//! decoy applies is decided by the missile owner from the victim's
//! deployment ephemerals (see [`decoy_for`]).
//!
//! `ACES_TEST_FIRE=1` enables a hands-free auto-dogfight hook: the plane
//! aims itself at the nearest enemy, locks, fires and pops countermeasures
//! — used by the two-instance end-to-end verification.
//!
//! The hook flies with energy discipline (see `RECOVER_SPEED`): a max-G
//! pursuit mushes into a stall and never merges.

use bevy::prelude::*;

use aces_net::{CmKind, DamageCause, Ephemeral, GameEvent, NetState};
use aces_protocol::spawn_point;
use std::collections::HashMap;
use std::env;

use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::{FlightState, LocalPlane, RemotePlane, SimPose, Velocity};
use crate::net::{NetFxOut, NetIn, NetOut};
use crate::{NetworkMode, Phase};

// ── Tuning ──────────────────────────────────────────────────────────────────

/// Health of a fresh plane.
pub const MAX_HEALTH: f32 = 100.0;
/// Damage per gun hit.
pub const GUN_DAMAGE: f32 = 8.0;
/// Gun shots per second.
pub const GUN_RATE: f32 = 15.0;
/// Gun effective range [m].
pub const GUN_RANGE: f32 = 1500.0;
/// A plane counts as hit within this radius of the ray [m].
pub const HIT_SPHERE: f32 = 12.0;
/// Damage per missile hit (both kinds).
pub const MISSILE_DAMAGE: f32 = 60.0;
/// Heat-seeker magazine.
pub const IR_MISSILE_COUNT: u8 = 6;
/// Radar missile magazine.
pub const RADAR_MISSILE_COUNT: u8 = 4;
/// IR acquisition cone half-angle [rad].
pub const IR_CONE: f32 = 10f32.to_radians();
/// The IR lock holds up to this cone before it drops [rad].
pub const IR_DROP_CONE: f32 = 20f32.to_radians();
/// IR lock range [m].
pub const IR_RANGE: f32 = 4000.0;
/// Seconds inside the IR cone until the lock is solid.
pub const IR_LOCK_TIME: f32 = 1.5;
/// Radar acquisition cone half-angle [rad] — coarse, front aspect.
pub const RADAR_CONE: f32 = 30f32.to_radians();
/// The radar lock holds up to this cone before it drops [rad].
pub const RADAR_DROP_CONE: f32 = 40f32.to_radians();
/// Radar lock range [m].
pub const RADAR_RANGE: f32 = 8000.0;
/// Seconds inside the radar cone until the lock is solid.
pub const RADAR_LOCK_TIME: f32 = 2.0;
/// How often a held radar lock re-announces itself to the target [s].
pub const RADAR_ANNOUNCE_PERIOD: f32 = 0.5;
/// Missile turn rate [rad/s] (proportional navigation, arcade).
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
    pub fn full() -> Self {
        Self {
            hp: MAX_HEALTH,
            dead: false,
        }
    }

    /// Wire form for [`aces_protocol::PlaneSnapshot::hp`].
    pub fn hp_byte(&self) -> u8 {
        self.hp.clamp(0.0, 255.0) as u8
    }

    /// Flying, not shot down.
    pub fn alive(&self) -> bool {
        !self.dead && self.hp > 0.0
    }
}

/// Missile guidance family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissileKind {
    /// Chases heat; distracted by flares.
    Ir,
    /// Chases radar returns; broken by chaff.
    Radar,
}

impl MissileKind {
    /// Wire byte for [`GameEvent::MissileLaunched`].
    pub fn byte(self) -> u8 {
        match self {
            MissileKind::Ir => 0,
            MissileKind::Radar => 1,
        }
    }

    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => MissileKind::Radar,
            _ => MissileKind::Ir,
        }
    }

    /// Which countermeasure defeats this missile.
    pub fn counter(self) -> CmKind {
        match self {
            MissileKind::Ir => CmKind::Flares,
            MissileKind::Radar => CmKind::Chaff,
        }
    }
}

/// One missile in flight, simulated identically on every peer.
#[derive(Component)]
pub struct Missile {
    pub owner: String,
    pub kind: MissileKind,
    pub target: Option<String>,
    /// Set once a decoy captured the seeker: the missile chases this point
    /// and detonates harmlessly.
    pub decoy: Option<Vec3>,
    pub speed: f32,
    pub age: f32,
    pub motor: f32,
}

/// Which weapon `Ctrl` fires.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponSlot {
    Gun,
    #[default]
    IrMissile,
    RadarMissile,
}

/// The IR lock: whose heat the missile would chase and how solid it is.
#[derive(Resource, Default)]
pub struct IrLock {
    pub target: Option<String>,
    /// 0..=1; ≥ 1 means locked.
    pub progress: f32,
}

/// The radar lock: coarser than IR, longer range, chaff-counterable.
#[derive(Resource, Default)]
pub struct RadarLock {
    pub target: Option<String>,
    pub progress: f32,
}

/// Local consumables and respawn scheduling.
#[derive(Resource)]
pub struct Loadout {
    pub ir_missiles: u8,
    pub radar_missiles: u8,
    /// Countdown while dead, `None` while flying.
    pub respawn_in: Option<f32>,
    /// Respawn count this session; spawn slots advance around the ring.
    pub respawns: usize,
}

/// Countermeasure stores with slow regeneration.
#[derive(Resource)]
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
        if self.flares < CM_MAX {
            self.flare_timer += dt;
            if self.flare_timer >= CM_REGEN_TIME {
                self.flares += 1;
                self.flare_timer = 0.0;
            }
        }
        if self.chaff < CM_MAX {
            self.chaff_timer += dt;
            if self.chaff_timer >= CM_REGEN_TIME {
                self.chaff += 1;
                self.chaff_timer = 0.0;
            }
        }
    }
}

/// A countermeasure deployment someone made, kept briefly for missile
/// owners to consult.
#[derive(Debug, Clone)]
pub struct Deployment {
    pub peer: String,
    pub kind: CmKind,
    pub at: f64,
    pub pos: Vec3,
}

/// Recent deployments across the room (fed by ephemerals).
#[derive(Resource, Default)]
pub struct CmDeployments(pub Vec<Deployment>);

/// The RWR picture: what is threatening *me* right now.
#[derive(Resource, Default)]
pub struct Rwr {
    /// Until when an enemy radar lock is being held on me.
    pub radar_locked_until: Option<f64>,
    /// An inbound (launched-at-me) missile and its guidance.
    pub missile: Option<(f64, MissileKind)>,
}

/// Kills and deaths per peer, counted from the canonical `Sequenced`
/// stream — every peer counts the same events, so boards agree.
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
    fn kill(&mut self, peer: &str) {
        self.entries.entry(peer.to_string()).or_default().kills += 1;
    }

    fn death(&mut self, peer: &str) {
        self.entries.entry(peer.to_string()).or_default().deaths += 1;
    }

    /// One peer's tally (test helper; the HUD reads `entries` directly).
    #[cfg(test)]
    pub fn get(&self, peer: &str) -> Score {
        self.entries.get(peer).copied().unwrap_or_default()
    }
}

/// Recent kills for the HUD feed, newest last.
#[derive(Resource, Default)]
pub struct KillFeed {
    pub entries: Vec<(f64, String)>,
}

/// How long a kill stays in the feed [s].
pub const KILL_FEED_LIFE: f64 = 6.0;
/// Feed capacity (oldest dropped beyond this).
pub const KILL_FEED_MAX: usize = 5;

impl KillFeed {
    fn push(&mut self, now: f64, text: String) {
        self.entries.push((now, text));
        while self.entries.len() > KILL_FEED_MAX {
            self.entries.remove(0);
        }
    }

    /// Drop expired entries; keep only fresh ones.
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
        .unwrap_or_else(|| format!("pilot-{}", &peer[..peer.len().min(4)]))
}

/// Shared explosion assets: one sphere mesh, base materials that each
/// explosion clones so fade-outs animate independently.
#[derive(Resource, Clone)]
struct ExplosionAssets {
    mesh: Handle<Mesh>,
    explosion: Handle<StandardMaterial>,
    flare: Handle<StandardMaterial>,
    chaff: Handle<StandardMaterial>,
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponSlot>()
            .init_resource::<AutoFireIntent>()
            .init_resource::<IrLock>()
            .init_resource::<RadarLock>()
            .init_resource::<Countermeasures>()
            .init_resource::<CmDeployments>()
            .init_resource::<Rwr>()
            .init_resource::<Scoreboard>()
            .init_resource::<KillFeed>()
            .init_resource::<GunCooldown>()
            .init_resource::<LockAnnounce>()
            .insert_resource(Loadout {
                ir_missiles: IR_MISSILE_COUNT,
                radar_missiles: RADAR_MISSILE_COUNT,
                respawn_in: None,
                respawns: 0,
            })
            .add_systems(Startup, init_explosion_assets)
            .add_systems(OnEnter(Phase::InGame), rearm)
            .add_systems(OnExit(Phase::InGame), rearm)
            .add_systems(
                Update,
                (
                    update_locks.run_if(in_state(Phase::InGame)),
                    announce_radar_lock.run_if(in_state(Phase::InGame)),
                    deploy_countermeasures.run_if(in_state(Phase::InGame)),
                    fire_control.run_if(in_state(Phase::InGame)),
                    apply_events.run_if(in_state(Phase::InGame)),
                    apply_ephemeral.run_if(in_state(Phase::InGame)),
                    prune_deployments.run_if(in_state(Phase::InGame)),
                    respawn.run_if(in_state(Phase::InGame)),
                    sync_remote_death.run_if(in_state(Phase::InGame)),
                    animate_explosions,
                    (
                        auto_dogfight_aim,
                        auto_dogfight_fire.after(auto_dogfight_aim),
                    )
                        .run_if(in_state(Phase::InGame)),
                ),
            )
            .add_systems(FixedUpdate, step_missiles.run_if(in_state(Phase::InGame)));
    }
}

/// The auto-dogfight's firing decision for this frame (see the two
/// `auto_dogfight_*` systems).
#[derive(Resource, Default)]
struct AutoFireIntent {
    gun: bool,
    missile: Option<(MissileKind, String)>,
}

/// Seconds since the last gun shot.
#[derive(Resource, Default, Deref, DerefMut)]
struct GunCooldown(f32);

/// Seconds since the last radar-lock announcement.
#[derive(Resource, Default, Deref, DerefMut)]
struct LockAnnounce(f32);

fn init_explosion_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Sphere::new(1.0));
    let explosion = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.55, 0.15, 0.9),
        emissive: LinearRgba::rgb(2.5, 1.2, 0.3),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let flare = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.85, 0.4, 0.95),
        emissive: LinearRgba::rgb(4.0, 3.0, 1.0),
        ..default()
    });
    let chaff = materials.add(StandardMaterial {
        base_color: Color::srgba(0.75, 0.78, 0.82, 0.8),
        emissive: LinearRgba::rgb(0.4, 0.42, 0.45),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    commands.insert_resource(ExplosionAssets {
        mesh,
        explosion,
        flare,
        chaff,
    });
}

fn rearm(
    mut loadout: ResMut<Loadout>,
    mut lock: ResMut<IrLock>,
    mut radar: ResMut<RadarLock>,
    mut countermeasures: ResMut<Countermeasures>,
    mut scoreboard: ResMut<Scoreboard>,
    mut kill_feed: ResMut<KillFeed>,
) {
    loadout.ir_missiles = IR_MISSILE_COUNT;
    loadout.radar_missiles = RADAR_MISSILE_COUNT;
    loadout.respawn_in = None;
    loadout.respawns = 0;
    *lock = IrLock::default();
    *radar = RadarLock::default();
    *countermeasures = Countermeasures::default();
    scoreboard.entries.clear();
    kill_feed.entries.clear();
}

fn my_peer(net: &NetState) -> String {
    net.my_id().map(|id| id.to_string()).unwrap_or_default()
}

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

/// Track both locks: nearest living enemy inside each cone. Acquire inside
/// the acquire cone, hold up to the drop cone.
fn update_locks(
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut ir: ResMut<IrLock>,
    mut radar: ResMut<RadarLock>,
    time: Res<Time>,
) {
    let me = my_peer(&net);
    let forward = plane.forward();
    let pos = plane.pos;

    // One DerefMut per lock, then disjoint field borrows through the plain
    // reference.
    let ir_lock: &mut IrLock = &mut ir;
    let radar_lock: &mut RadarLock = &mut radar;
    let mut ir_view = LockView {
        target: &mut ir_lock.target,
        progress: &mut ir_lock.progress,
    };
    let mut radar_view = LockView {
        target: &mut radar_lock.target,
        progress: &mut radar_lock.progress,
    };
    update_lock(
        &mut ir_view,
        &remotes,
        &me,
        forward,
        pos,
        IR_CONE,
        IR_DROP_CONE,
        IR_RANGE,
        IR_LOCK_TIME,
        time.delta_secs(),
    );
    update_lock(
        &mut radar_view,
        &remotes,
        &me,
        forward,
        pos,
        RADAR_CONE,
        RADAR_DROP_CONE,
        RADAR_RANGE,
        RADAR_LOCK_TIME,
        time.delta_secs(),
    );
}

/// Field view over either lock resource.
struct LockView<'a> {
    target: &'a mut Option<String>,
    progress: &'a mut f32,
}

/// Track one lock: nearest living enemy inside the cone. Acquire inside the
/// acquire cone, hold up to the drop cone.
#[allow(clippy::too_many_arguments)]
fn update_lock(
    lock: &mut LockView,
    remotes: &Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    me: &str,
    forward: Vec3,
    pos: Vec3,
    cone: f32,
    drop_cone: f32,
    range: f32,
    lock_time: f32,
    dt: f32,
) {
    let mut best: Option<(f32, &str)> = None; // (angle, peer)
    for (remote, transform, health) in remotes.iter() {
        if !health.alive() || remote.peer == me {
            continue;
        }
        let to_target = transform.translation - pos;
        let dist = to_target.length();
        if dist > range || dist < 1.0 {
            continue;
        }
        let angle = forward.angle_between(to_target / dist);
        let cone = if lock.target.as_deref() == Some(remote.peer.as_str()) {
            drop_cone
        } else {
            cone
        };
        if angle <= cone && best.is_none_or(|(best_angle, _)| angle < best_angle) {
            best = Some((angle, &remote.peer));
        }
    }

    match best {
        Some((_, peer)) => {
            if lock.target.as_deref() != Some(peer) {
                *lock.progress = 0.0;
            }
            *lock.target = Some(peer.to_string());
            *lock.progress = (*lock.progress + dt / lock_time).min(1.2);
        }
        None => {
            *lock.target = None;
            *lock.progress = 0.0;
        }
    }
}

/// While a radar lock is held, keep announcing it to the victim (RWR).
fn announce_radar_lock(
    radar: Res<RadarLock>,
    mut announce: ResMut<LockAnnounce>,
    mut out: ResMut<NetFxOut>,
    time: Res<Time>,
) {
    **announce += time.delta_secs();
    if **announce < RADAR_ANNOUNCE_PERIOD {
        return;
    }
    **announce = 0.0;
    if radar.progress >= 1.0
        && let Some(target) = &radar.target
    {
        out.effects.push(Ephemeral::RadarLocking {
            target: target.clone(),
        });
    }
}

// ── Countermeasures ─────────────────────────────────────────────────────────

/// `F` flares, `C` chaff: decrement the store, broadcast the deployment,
/// play the local visual. Regeneration refills both stores slowly.
#[allow(clippy::too_many_arguments)]
fn deploy_countermeasures(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    mut countermeasures: ResMut<Countermeasures>,
    mut out: ResMut<NetFxOut>,
    explosions: Res<ExplosionAssets>,
) {
    countermeasures.regen(time.delta_secs());
    let me = my_peer(&net);
    let pos = plane.pos;

    let want = if keys.just_pressed(KeyCode::KeyF) {
        Some(CmKind::Flares)
    } else if keys.just_pressed(KeyCode::KeyC) {
        Some(CmKind::Chaff)
    } else {
        None
    };

    if let Some(kind) = want {
        let store = match kind {
            CmKind::Flares => &mut countermeasures.flares,
            CmKind::Chaff => &mut countermeasures.chaff,
        };
        if *store == 0 {
            return;
        }
        *store -= 1;
        out.effects.push(Ephemeral::Countermeasures {
            peer: me,
            kind,
            pos: pos.to_array(),
        });
        spawn_decoy_visual(&mut commands, &explosions, kind, pos);
    }
}

fn spawn_decoy_visual(
    commands: &mut Commands,
    assets: &ExplosionAssets,
    kind: CmKind,
    pos: Vec3,
) {
    match kind {
        CmKind::Flares => {
            // A short burst of bright falling points.
            for offset in [
                Vec3::ZERO,
                Vec3::new(2.0, 1.0, -1.0),
                Vec3::new(-2.0, 0.5, 1.0),
            ] {
                commands.spawn((
                    Explosion {
                        age: 0.0,
                        size: 1.2,
                        fades: false,
                    },
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(assets.flare.clone()),
                    Transform::from_translation(pos + offset),
                    Visibility::default(),
                ));
            }
        }
        CmKind::Chaff => {
            commands.spawn((
                Explosion {
                    age: 0.0,
                    size: 2.5,
                    fades: false,
                },
                Mesh3d(assets.mesh.clone()),
                MeshMaterial3d(assets.chaff.clone()),
                Transform::from_translation(pos),
                Visibility::default(),
            ));
        }
    }
}

// ── Firing ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn fire_control(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    mut slot: ResMut<WeaponSlot>,
    ir: Res<IrLock>,
    radar: Res<RadarLock>,
    mut loadout: ResMut<Loadout>,
    mut cooldown: ResMut<GunCooldown>,
    mut out: ResMut<NetOut>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
) {
    let me = my_peer(&net);
    let flying = loadout.respawn_in.is_none();

    // Weapon select.
    if keys.just_pressed(KeyCode::Digit1) {
        *slot = WeaponSlot::Gun;
    }
    if keys.just_pressed(KeyCode::Digit2) {
        *slot = WeaponSlot::IrMissile;
    }
    if keys.just_pressed(KeyCode::Digit3) {
        *slot = WeaponSlot::RadarMissile;
    }

    // Gun: hitscan straight down the nose.
    **cooldown += time.delta_secs();
    let gun_period = 1.0 / GUN_RATE;
    if keys.pressed(KeyCode::Space) && **cooldown >= gun_period && flying {
        **cooldown = 0.0;
        let forward = plane.forward();
        let origin = plane.pos + forward * 8.0;
        if let Some(victim) = gun_ray_hit(&origin, &forward, &remotes, &me) {
            out.events.push(GameEvent::Damage {
                shooter: me.clone(),
                victim,
                amount: GUN_DAMAGE,
                cause: DamageCause::Gun,
            });
        }
    }

    // Missiles: fire the selected kind if its lock is solid.
    let attempt = match *slot {
        WeaponSlot::IrMissile => Some((
            MissileKind::Ir,
            ir.progress,
            ir.target.clone(),
            loadout.ir_missiles > 0,
        )),
        WeaponSlot::RadarMissile => Some((
            MissileKind::Radar,
            radar.progress,
            radar.target.clone(),
            loadout.radar_missiles > 0,
        )),
        WeaponSlot::Gun => None,
    };
    let trigger = keys.just_pressed(KeyCode::ControlLeft) || keys.just_pressed(KeyCode::ControlRight);
    if let Some((kind, progress, target, left)) = attempt
        && trigger
        && flying
        && left
        && progress >= 1.0
        && let Some(target) = target
    {
        match kind {
            MissileKind::Ir => loadout.ir_missiles -= 1,
            MissileKind::Radar => loadout.radar_missiles -= 1,
        }
        let forward = plane.forward();
        let pos = plane.pos + forward * 8.0;
        let speed = (plane.speed() + 100.0).max(250.0);
        out.events.push(GameEvent::MissileLaunched {
            shooter: me.clone(),
            missile: 0,
            kind: kind.byte(),
            pos: pos.to_array(),
            quat: plane.quat.to_array(),
            speed,
            target: Some(target.clone()),
        });
        // The shooter spawns and simulates its own missile; everyone else
        // spawns it from this event.
        info!(kind = ?kind, target = %target, "firing a missile");
        spawn_missile_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &me,
            kind,
            pos,
            plane.quat,
            speed,
            Some(target),
        );
    }
}

/// Nearest living enemy plane within the gun's hit sphere along the ray.
fn gun_ray_hit(
    origin: &Vec3,
    forward: &Vec3,
    remotes: &Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    me: &str,
) -> Option<String> {
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
            && best.as_ref().is_none_or(|(best_along, _)| along < *best_along)
        {
            best = Some((along, remote.peer.clone()));
        }
    }
    best.map(|(_, victim)| victim)
}

// ── Missile simulation ──────────────────────────────────────────────────────

/// Everyone simulates every missile: proportional navigation toward the
/// target's current on-screen position, motor burn, proximity fuse. Only
/// the owner's simulation claims damage.
#[allow(clippy::too_many_arguments)]
fn step_missiles(
    mut commands: Commands,
    explosions: Res<ExplosionAssets>,
    deployments: Res<CmDeployments>,
    mut missiles: Query<(Entity, &mut Missile, &mut Transform), Without<RemotePlane>>,
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
) {
    let me = my_peer(&net);
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
            && let Some(target) = missile.target.clone()
        {
            match decoy_for(missile.kind, &target, transform.translation, &deployments.0, now) {
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
        // target (and the fuse disarms — it detonates harmlessly).
        let guided_pos = missile.decoy.or_else(|| {
            missile.target.as_ref().and_then(|peer| {
                if *peer == me {
                    Some(plane.pos)
                } else {
                    remotes
                        .iter()
                        .find(|(r, ..)| r.peer == *peer)
                        .map(|(_, t, _)| t.translation)
                }
            })
        });
        let armed = missile.decoy.is_none();

        if let Some(aim) = guided_pos {
            let to_target = aim - transform.translation;
            let dist = to_target.length();

            let fuse_radius = if armed {
                MISSILE_PROXIMITY
            } else {
                MISSILE_PROXIMITY * 0.6
            };
            if dist < fuse_radius {
                if armed {
                    detonate(
                        &mut commands,
                        &explosions,
                        &missile,
                        transform.translation,
                        &mut out,
                        &me,
                    );
                } else {
                    spawn_explosion(&mut commands, &explosions, transform.translation, 4.0);
                }
                commands.entity(entity).despawn();
                continue;
            }

            // Proportional navigation: chase the aim point.
            let desired = (aim - transform.translation).normalize_or_zero();
            let current = transform.rotation * Vec3::NEG_Z;
            let axis = current.cross(desired);
            if desired != Vec3::ZERO && current != Vec3::ZERO && axis.length_squared() > 1e-8 {
                let swing = current.angle_between(desired).min(MISSILE_TURN * dt);
                transform.rotation =
                    (Quat::from_axis_angle(axis.normalize(), swing) * transform.rotation)
                        .normalize();
            }
        }

        let heading = transform.rotation * Vec3::NEG_Z;
        transform.translation += heading * missile.speed * dt;

        // Any-plane proximity — but a decoyed seeker ignores planes.
        if armed {
            let mut hit_anyone = false;
            for (remote, t, health) in remotes.iter() {
                if remote.peer == missile.owner || !health.alive() {
                    continue;
                }
                if t.translation.distance(transform.translation) < MISSILE_PROXIMITY * 0.8 {
                    detonate(
                        &mut commands,
                        &explosions,
                        &missile,
                        transform.translation,
                        &mut out,
                        &me,
                    );
                    commands.entity(entity).despawn();
                    hit_anyone = true;
                    break;
                }
            }
            if hit_anyone {
                continue;
            }
        }

        if missile.age > MISSILE_LIFE || transform.translation.y < 1.0 {
            if missile.owner == me {
                info!(age = missile.age, y = transform.translation.y, "missile expired");
            }
            spawn_explosion(&mut commands, &explosions, transform.translation, 3.0);
            commands.entity(entity).despawn();
        }
    }
}

/// Claim the hit (owner-authoritative) and play the fireball.
fn detonate(
    commands: &mut Commands,
    explosions: &ExplosionAssets,
    missile: &Missile,
    pos: Vec3,
    out: &mut NetOut,
    me: &str,
) {
    if missile.owner == me {
        info!("missile detonated");
    }
    spawn_explosion(commands, explosions, pos, 8.0);
    // Only the owner's simulation claims damage — everyone else's identical
    // simulation stays visual.
    if missile.owner == me
        && let Some(victim) = &missile.target
    {
        out.events.push(GameEvent::Damage {
            shooter: missile.owner.clone(),
            victim: victim.clone(),
            amount: MISSILE_DAMAGE,
            cause: DamageCause::Missile,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_missile_entity(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
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
        Mesh3d(meshes.add(Capsule3d::new(0.15, 1.6))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.9, 0.9, 0.92),
            emissive: LinearRgba::rgb(1.0, 0.55, 0.15) * 2.5,
            ..default()
        })),
        Transform::from_translation(pos).with_rotation(quat),
        Visibility::default(),
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

fn spawn_explosion(
    commands: &mut Commands,
    assets: &ExplosionAssets,
    pos: Vec3,
    size: f32,
) {
    commands.spawn((
        Explosion {
            age: 0.0,
            size,
            fades: true,
        },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.explosion.clone()),
        Transform::from_translation(pos),
        Visibility::default(),
    ));
}

fn animate_explosions(
    time: Res<Time>,
    mut commands: Commands,
    mut explosions: Query<
        (
            Entity,
            &mut Explosion,
            &mut Transform,
            &MeshMaterial3d<StandardMaterial>,
        ),
        Without<LocalPlane>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, mut explosion, mut transform, material) in &mut explosions {
        explosion.age += time.delta_secs();
        if explosion.age > EXPLOSION_LIFE {
            commands.entity(entity).despawn();
            continue;
        }
        let t = explosion.age / EXPLOSION_LIFE;
        transform.scale = Vec3::splat(explosion.size * (0.4 + 1.6 * t));
        if explosion.fades {
            let handle: &Handle<StandardMaterial> = &material.0;
            if let Some(mut material) = materials.get_mut(handle) {
                material.base_color = Color::srgba(1.0, 0.55, 0.15, 0.9 * (1.0 - t));
                material.emissive = LinearRgba::rgb(2.5, 1.2, 0.3) * (1.0 - t);
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
    explosions: Res<ExplosionAssets>,
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rwr: ResMut<Rwr>,
    mut scoreboard: ResMut<Scoreboard>,
    mut kill_feed: ResMut<KillFeed>,
    mut plane: Single<(&mut FlightState, &mut Health, &mut Visibility), With<LocalPlane>>,
    mut remotes: Query<(&RemotePlane, &Transform, &mut Health), Without<LocalPlane>>,
    time: Res<Time>,
) {
    let me = my_peer(&net);
    let now = time.elapsed_secs_f64();
    for (_, event) in net_in.sequenced.drain(..) {
        match event {
            GameEvent::MissileLaunched {
                shooter,
                missile: _,
                kind,
                pos,
                quat,
                speed,
                target,
            } => {
                let kind = MissileKind::from_byte(kind);
                // My own launches already spawned at fire time.
                if shooter != me {
                    spawn_missile_entity(
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &shooter,
                        kind,
                        Vec3::from(pos),
                        Quat::from_array(quat).normalize(),
                        speed,
                        target.clone(),
                    );
                    info!(%shooter, "missile in the air");
                }
                // Missile launch warning for the target.
                if target.as_deref() == Some(me.as_str()) {
                    rwr.missile = Some((now + 3.0, kind));
                    info!(%shooter, "missile launch warning");
                }
            }
            GameEvent::Damage {
                shooter,
                victim,
                amount,
                cause: _,
            } => {
                if victim != me || loadout_is_dead(&loadout) {
                    continue; // the victim applies its own damage
                }
                plane.1.hp -= amount;
                info!(shooter = %shooter, hp = plane.1.hp, "took damage");
                if plane.1.hp <= 0.0 {
                    plane.1.dead = true;
                    plane.1.hp = 0.0;
                    *plane.2 = Visibility::Hidden;
                    loadout.respawn_in = Some(RESPAWN_DELAY);
                    loadout.respawns += 1;
                    spawn_explosion(&mut commands, &explosions, plane.0.pos, 8.0);
                    out.events.push(GameEvent::Killed {
                        victim: me.clone(),
                        shooter: shooter.clone(),
                    });
                    info!(%shooter, "shot down; respawning");
                }
            }
            GameEvent::Killed { victim, shooter } => {
                if victim == me {
                    continue; // my own death was handled in the Damage arm
                }
                for (remote, transform, mut health) in &mut remotes {
                    if remote.peer == victim {
                        spawn_explosion(&mut commands, &explosions, transform.translation, 8.0);
                        health.hp = 0.0;
                        health.dead = true;
                    }
                }
                // Bookkeeping: everyone counts the same canonical event.
                scoreboard.kill(&shooter);
                scoreboard.death(&victim);
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

/// Consume fire-and-forget effects: deploy visuals room-wide, keep the
/// deployment board fresh for missile owners, and announce radar locks.
#[allow(clippy::too_many_arguments)]
fn apply_ephemeral(
    mut commands: Commands,
    mut net_in: ResMut<NetIn>,
    explosions: Res<ExplosionAssets>,
    net: Res<NetState>,
    mut deployments: ResMut<CmDeployments>,
    mut rwr: ResMut<Rwr>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    let me = my_peer(&net);
    for (_sender, effect) in net_in.ephemeral.drain(..) {
        match effect {
            Ephemeral::Countermeasures { peer, kind, pos } => {
                let pos = Vec3::from(pos);
                deployments.0.push(Deployment {
                    peer,
                    kind,
                    at: now,
                    pos,
                });
                spawn_decoy_visual(&mut commands, &explosions, kind, pos);
            }
            Ephemeral::RadarLocking { target } => {
                if target == me {
                    rwr.radar_locked_until = Some(now + 1.2);
                    info!("RWR: radar lock");
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

fn loadout_is_dead(loadout: &Loadout) -> bool {
    loadout.respawn_in.is_some()
}

/// Dead locals come back after [`RESPAWN_DELAY`] on the next ring slot.
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn respawn(
    time: Res<Time>,
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
    mut countermeasures: ResMut<Countermeasures>,
    mut plane: Single<
        (
            &mut FlightState,
            &mut SimPose,
            &mut Velocity,
            &mut Health,
            &mut Transform,
            &mut Visibility,
        ),
        With<LocalPlane>,
    >,
    mut ir: ResMut<IrLock>,
    mut radar: ResMut<RadarLock>,
    mut rwr: ResMut<Rwr>,
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
    let (pos, yaw) = spawn_point(index);
    let quat = Quat::from_rotation_y(yaw);
    *plane.0 = FlightState::new(pos.into(), quat, 150.0, 0.7);
    *plane.1 = SimPose::new(pos.into(), quat);
    plane.2.0 = quat * Vec3::NEG_Z * 150.0;
    *plane.3 = Health::full();
    *plane.4 = Transform::from_translation(pos.into()).with_rotation(quat);
    *plane.5 = Visibility::Visible;
    loadout.ir_missiles = IR_MISSILE_COUNT;
    loadout.radar_missiles = RADAR_MISSILE_COUNT;
    loadout.respawn_in = None;
    *countermeasures = Countermeasures::default();
    *ir = IrLock::default();
    *radar = RadarLock::default();
    *rwr = Rwr::default();
    info!("respawned");
}

/// Remote planes stay hidden while their snapshots report them dead.
fn sync_remote_death(mut remotes: Query<(&RemotePlane, &Health, &mut Visibility)>) {
    for (_, health, mut visibility) in &mut remotes {
        *visibility = if health.dead || health.hp <= 0.0 {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
}

// ── Auto-dogfight test hook ─────────────────────────────────────────────────

/// Below this speed the hook flies level to regain energy instead of
/// turning: a sustained max-G pursuit bleeds speed into a stall-mush
/// (observed live), and a stalled fighter neither merges nor fights.
const RECOVER_SPEED: f32 = 130.0;

/// The hook pops countermeasures once an inbound missile is this close [m].
const CM_POP_DIST: f32 = 250.0;

/// `ACES_TEST_FIRE=1`: aim at the nearest living enemy, lock, fire missiles
/// and gun, and pop countermeasures when warned — so the two-instance
/// end-to-end verification runs hands-free.
#[allow(clippy::too_many_arguments)]
fn auto_dogfight_aim(
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    plane: Single<(&FlightState, &Health), With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    inbound_missiles: Query<(&Missile, &Transform), Without<RemotePlane>>,
    mut aim: ResMut<MouseAim>,
    mut input: ResMut<FlightInput>,
    ir: Res<IrLock>,
    radar: Res<RadarLock>,
    loadout: Res<Loadout>,
    mut countermeasures: ResMut<Countermeasures>,
    mut fx: ResMut<NetFxOut>,
    rwr: Res<Rwr>,
    mut intent: ResMut<AutoFireIntent>,
    time: Res<Time>,
) {
    if env::var("ACES_TEST_FIRE").is_err() || *mode == NetworkMode::Solo {
        return;
    }
    if loadout_is_dead(&loadout) || plane.1.dead {
        return;
    }
    let me = my_peer(&net);
    let (state, _) = *plane;

    // React to threats: pop the matching countermeasure when an inbound
    // missile is close enough for a decoy to matter (the victim simulates
    // every missile aimed at it, so the distance is known).
    let now = time.elapsed_secs_f64();
    if let Some((_, warn_kind)) = rwr.missile.filter(|(until, _)| now <= *until) {
        let threatened = inbound_missiles.iter().find(|(m, t)| {
            m.target.as_deref() == Some(me.as_str())
                && t.translation.distance(state.pos) < CM_POP_DIST
        });
        if let Some((missile, transform)) = threatened {
            // Pop what defeats THIS missile; drop the decoy beside the plane,
            // slightly off the missile's approach.
            let kind = missile.kind.counter();
            let store = match kind {
                CmKind::Flares => &mut countermeasures.flares,
                CmKind::Chaff => &mut countermeasures.chaff,
            };
            if *store > 0 {
                *store -= 1;
                let pos = state.pos + (state.pos - transform.translation).normalize_or_zero() * 20.0;
                fx.effects.push(Ephemeral::Countermeasures {
                    peer: me.clone(),
                    kind,
                    pos: pos.to_array(),
                });
                let _ = warn_kind;
            }
        }
    }

    // Nearest living enemy, with its snapshot velocity for lead pursuit.
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
    let dir = if speed < RECOVER_SPEED {
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
    if speed < RECOVER_SPEED {
        return;
    }

    // What to fire this frame; `auto_dogfight_fire` executes.
    intent.gun = false;
    intent.missile = None;

    // Gun when the enemy is on the nose.
    let forward = state.forward();
    let origin = state.pos + forward * 8.0;
    intent.gun = gun_ray_hit(&origin, &forward, &remotes, &me).is_some();

    // Prefer the IR lock, fall back to radar.
    let (kind, progress, target, missiles_left) = if ir.progress >= 1.0 {
        (MissileKind::Ir, ir.progress, ir.target.clone(), loadout.ir_missiles)
    } else if radar.progress >= 1.0 {
        (
            MissileKind::Radar,
            radar.progress,
            radar.target.clone(),
            loadout.radar_missiles,
        )
    } else {
        return;
    };
    if progress >= 1.0 && missiles_left > 0 && let Some(target) = target {
        intent.missile = Some((kind, target));
    }
}

/// Execute the auto-dogfight's firing decisions (kept separate from
/// `auto_dogfight_aim` to stay under the system-parameter limit).
#[allow(clippy::too_many_arguments)]
fn auto_dogfight_fire(
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    plane: Single<(&FlightState, &Health), With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut loadout: ResMut<Loadout>,
    mut cooldown: ResMut<GunCooldown>,
    mut out: ResMut<NetOut>,
    intent: Res<AutoFireIntent>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
    time: Res<Time>,
) {
    if env::var("ACES_TEST_FIRE").is_err() || *mode == NetworkMode::Solo {
        return;
    }
    if loadout_is_dead(&loadout) || plane.1.dead {
        return;
    }
    let me = my_peer(&net);
    let state = plane.0;

    if intent.gun {
        **cooldown += time.delta_secs();
        if **cooldown >= 1.0 / GUN_RATE {
            **cooldown = 0.0;
            let forward = state.forward();
            let origin = state.pos + forward * 8.0;
            if let Some(victim) = gun_ray_hit(&origin, &forward, &remotes, &me) {
                out.events.push(GameEvent::Damage {
                    shooter: me.clone(),
                    victim,
                    amount: GUN_DAMAGE,
                    cause: DamageCause::Gun,
                });
            }
        }
    }

    if let Some((kind, target)) = intent.missile.clone() {
        match kind {
            MissileKind::Ir => loadout.ir_missiles -= 1,
            MissileKind::Radar => loadout.radar_missiles -= 1,
        }
        let forward = state.forward();
        let pos = state.pos + forward * 8.0;
        let speed = (state.speed() + 100.0).max(250.0);
        out.events.push(GameEvent::MissileLaunched {
            shooter: me.clone(),
            missile: 0,
            kind: kind.byte(),
            pos: pos.to_array(),
            quat: state.quat.to_array(),
            speed,
            target: Some(target.clone()),
        });
        // The shooter sims its own missiles (owner-authoritative hits);
        // everyone else spawns this one from the sequenced event.
        spawn_missile_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &me,
            kind,
            pos,
            state.quat,
            speed,
            Some(target),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(peer: &str, kind: CmKind, at: f64, pos: Vec3) -> Deployment {
        Deployment {
            peer: peer.to_string(),
            kind,
            at,
            pos,
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

    /// The scoreboard tallies kills and deaths per peer.
    #[test]
    fn scoreboard_counts() {
        let mut board = Scoreboard::default();
        board.kill("a");
        board.kill("a");
        board.death("b");
        assert_eq!(board.get("a"), Score { kills: 2, deaths: 0 });
        assert_eq!(board.get("b"), Score { kills: 0, deaths: 1 });
        assert_eq!(board.get("ghost"), Score::default());
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

        feed.push(now + 1.0, "newest".into());
        for i in 0..(KILL_FEED_MAX + 2) {
            feed.push(now + 1.0 + i as f64, format!("n{i}"));
        }
        assert!(feed.entries.len() <= KILL_FEED_MAX);
    }

    /// The latest matching deployment wins.
    #[test]
    fn latest_deployment_wins() {
        let now = 100.0;
        let missile_pos = Vec3::ZERO;
        let d = vec![
            deployment("victim", CmKind::Flares, now - 1.6, Vec3::new(5.0, 0.0, 0.0)),
            deployment("victim", CmKind::Chaff, now - 0.1, Vec3::new(6.0, 0.0, 0.0)),
        ];
        // The IR seeker only sees flares; the old flare is beyond the window,
        // so nothing distracts it.
        assert_eq!(
            decoy_for(MissileKind::Ir, "victim", missile_pos, &d, now),
            Decoy::Nothing
        );
    }
}
