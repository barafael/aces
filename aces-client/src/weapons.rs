//! Weapons & damage (milestone 4): the gun, the heat-seeking missile with
//! its lock, and the damage → death → respawn loop.
//!
//! Authority follows PLAN.md: the shooter owns its weapons and claims hits
//! (`GameEvent::Damage`), the victim owns its HP and confirms death
//! (`GameEvent::Killed`). Everything crosses the wire through the host's
//! canonical `Sequenced` stream, so all peers apply events in one order —
//! including the host's own, via loopback.
//!
//! Missiles are simulated by *every* peer from the `MissileLaunched` event
//! (they are short-lived; the simulations only need to look alike). Only
//! the shooter's simulation claims damage.
//!
//! `ACES_TEST_FIRE=1` enables a hands-free auto-dogfight hook: the plane
//! aims itself at the nearest enemy, locks, and fires — used by the
//! two-instance end-to-end verification.

use bevy::prelude::*;

use aces_net::{DamageCause, GameEvent, NetState};
use aces_protocol::spawn_point;
use std::env;

use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::model::FlightState;
use crate::flight::{LocalPlane, RemotePlane, SimPose, Velocity};
use crate::net::{NetIn, NetOut};
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
/// Damage per missile hit.
pub const MISSILE_DAMAGE: f32 = 60.0;
/// Missiles in the magazine.
pub const MISSILE_COUNT: u8 = 6;
/// IR acquisition cone half-angle [rad].
pub const IR_CONE: f32 = 10f32.to_radians();
/// The lock holds up to this cone before it drops [rad].
pub const IR_DROP_CONE: f32 = 20f32.to_radians();
/// IR lock range [m].
pub const IR_RANGE: f32 = 4000.0;
/// Seconds inside the cone until the lock is solid.
pub const IR_LOCK_TIME: f32 = 1.5;
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

// ── State ───────────────────────────────────────────────────────────────────

/// Health of a plane (local *and* remote; remotes' is fed from snapshots).
#[derive(Component, Debug, Clone, Copy)]
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
}

/// One missile in flight, simulated identically on every peer.
#[derive(Component)]
pub struct Missile {
    pub owner: String,
    /// 0 = IR (radar arrives in milestone 5). Read when milestone 5's
    /// guidance differentiates; carried so the wire kind lands as data.
    #[allow(dead_code)]
    pub kind: u8,
    pub target: Option<String>,
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
}

/// The IR lock: whose heat the missile would chase and how solid it is.
#[derive(Resource, Default)]
pub struct IrLock {
    pub target: Option<String>,
    /// 0..=1; ≥ 1 means locked.
    pub progress: f32,
}

/// Local consumables and respawn scheduling.
#[derive(Resource)]
pub struct Loadout {
    pub missiles: u8,
    /// Countdown while dead, `None` while flying.
    pub respawn_in: Option<f32>,
    /// Respawn count this session; spawn slots advance around the ring.
    pub respawns: usize,
}

/// Shared explosion assets: one sphere mesh, one base material that each
/// explosion clones so the fade-out animates independently.
#[derive(Resource, Clone)]
struct ExplosionAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

pub struct WeaponsPlugin;

impl Plugin for WeaponsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WeaponSlot>()
            .init_resource::<IrLock>()
            .init_resource::<GunCooldown>()
            .insert_resource(Loadout {
                missiles: MISSILE_COUNT,
                respawn_in: None,
                respawns: 0,
            })
            .add_systems(OnEnter(Phase::InGame), rearm)
            .add_systems(OnExit(Phase::InGame), rearm)
            .add_systems(Startup, init_explosion_assets)
            .add_systems(
                Update,
                (
                    update_ir_lock.run_if(in_state(Phase::InGame)),
                    fire_control.run_if(in_state(Phase::InGame)),
                    apply_events.run_if(in_state(Phase::InGame)),
                    respawn.run_if(in_state(Phase::InGame)),
                    sync_remote_death.run_if(in_state(Phase::InGame)),
                    animate_explosions,
                    auto_dogfight.run_if(in_state(Phase::InGame)),
                ),
            )
            .add_systems(FixedUpdate, step_missiles.run_if(in_state(Phase::InGame)));
    }
}

/// Seconds since the last gun shot.
#[derive(Resource, Default, Deref, DerefMut)]
struct GunCooldown(f32);

fn init_explosion_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mesh = meshes.add(Sphere::new(1.0));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.55, 0.15, 0.9),
        emissive: LinearRgba::rgb(2.5, 1.2, 0.3),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    commands.insert_resource(ExplosionAssets { mesh, material });
}

fn rearm(mut loadout: ResMut<Loadout>, mut lock: ResMut<IrLock>) {
    loadout.missiles = MISSILE_COUNT;
    loadout.respawn_in = None;
    loadout.respawns = 0;
    *lock = IrLock::default();
}

fn my_peer(net: &NetState) -> String {
    net.my_id.map(|id| id.to_string()).unwrap_or_default()
}

// ── Locking ─────────────────────────────────────────────────────────────────

/// Track the IR lock: nearest living enemy inside the cone. Acquire inside
/// [`IR_CONE`], hold up to [`IR_DROP_CONE`].
fn update_ir_lock(
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut lock: ResMut<IrLock>,
    time: Res<Time>,
) {
    let me = my_peer(&net);
    let forward = plane.forward();
    let pos = plane.pos;

    let mut best: Option<(f32, &str)> = None; // (angle, peer)
    for (remote, transform, health) in &remotes {
        if health.dead || health.hp <= 0.0 || remote.peer == me {
            continue;
        }
        let to_target = transform.translation - pos;
        let dist = to_target.length();
        if dist > IR_RANGE || dist < 1.0 {
            continue;
        }
        let angle = forward.angle_between(to_target / dist);
        let cone = if lock.target.as_deref() == Some(remote.peer.as_str()) {
            IR_DROP_CONE
        } else {
            IR_CONE
        };
        if angle <= cone && best.is_none_or(|(best_angle, _)| angle < best_angle) {
            best = Some((angle, &remote.peer));
        }
    }

    match best {
        Some((_, peer)) => {
            if lock.target.as_deref() != Some(peer) {
                lock.progress = 0.0;
            }
            lock.target = Some(peer.to_string());
            lock.progress = (lock.progress + time.delta_secs() / IR_LOCK_TIME).min(1.2);
        }
        None => {
            lock.target = None;
            lock.progress = 0.0;
        }
    }
}

// ── Firing ──────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn fire_control(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    mut slot: ResMut<WeaponSlot>,
    lock: Res<IrLock>,
    mut loadout: ResMut<Loadout>,
    mut cooldown: ResMut<GunCooldown>,
    mut out: ResMut<NetOut>,
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

    // Missile: needs a solid lock and a spare.
    if (keys.just_pressed(KeyCode::ControlLeft) || keys.just_pressed(KeyCode::ControlRight))
        && *slot == WeaponSlot::IrMissile
        && flying
        && loadout.missiles > 0
        && lock.progress >= 1.0
        && let Some(target) = lock.target.clone()
    {
        loadout.missiles -= 1;
        let forward = plane.forward();
        let pos = plane.pos + forward * 8.0;
        let speed = (plane.speed() + 100.0).max(250.0);
        out.events.push(GameEvent::MissileLaunched {
            shooter: me.clone(),
            missile: 0,
            kind: 0,
            pos: pos.to_array(),
            quat: plane.quat.to_array(),
            speed,
            target: Some(target.clone()),
        });
        // The local missile spawns through the loopback apply path, together
        // with everyone else's — one spawn path for the whole room.
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
        if health.dead || health.hp <= 0.0 || remote.peer == me {
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
    mut missiles: Query<(Entity, &mut Missile, &mut Transform), Without<RemotePlane>>,
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
) {
    let me = my_peer(&net);
    let dt = time.delta_secs();

    for (entity, mut missile, mut transform) in &mut missiles {
        missile.age += dt;
        if missile.motor > 0.0 {
            missile.motor -= dt;
            missile.speed = (missile.speed + MISSILE_ACCEL * dt).min(MISSILE_MAX_SPEED);
        } else {
            missile.speed *= 1.0 - 0.25 * dt;
        }

        // Where is the target this tick? (Remote velocities aren't tracked
        // per-entity yet; plain pursuit converges fine.)
        let target_state = missile.target.as_ref().and_then(|peer| {
            if *peer == me {
                Some(plane.pos)
            } else {
                remotes
                    .iter()
                    .find(|(r, ..)| r.peer == *peer)
                    .map(|(_, t, _)| t.translation)
            }
        });

        if let Some(target_pos) = target_state {
            let to_target = target_pos - transform.translation;
            let dist = to_target.length();

            if dist < MISSILE_PROXIMITY {
                detonate(&mut commands, &explosions, &missile, transform.translation, &mut out);
                commands.entity(entity).despawn();
                continue;
            }

            // Proportional navigation: lead the target, chase the lead point.
            let lead_time = (dist / missile.speed.max(100.0)).min(2.0);
            let aim_point = target_pos; // no remote velocity yet; see above
            let _ = lead_time;
            let desired = (aim_point - transform.translation).normalize_or_zero();
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

        // Any-plane proximity (a missile crossing the merge may hit anyone).
        let mut hit_anyone = false;
        for (remote, t, health) in remotes.iter() {
            if remote.peer == missile.owner || health.dead || health.hp <= 0.0 {
                continue;
            }
            if t.translation.distance(transform.translation) < MISSILE_PROXIMITY * 0.8 {
                detonate(&mut commands, &explosions, &missile, transform.translation, &mut out);
                commands.entity(entity).despawn();
                hit_anyone = true;
                break;
            }
        }
        if hit_anyone {
            continue;
        }

        if missile.age > MISSILE_LIFE || transform.translation.y < 1.0 {
            spawn_explosion(&mut commands, &explosions, transform.translation, 3.0);
            commands.entity(entity).despawn();
        }
    }
}

fn detonate(
    commands: &mut Commands,
    explosions: &ExplosionAssets,
    missile: &Missile,
    pos: Vec3,
    out: &mut NetOut,
) {
    spawn_explosion(commands, explosions, pos, 8.0);
    if let Some(victim) = &missile.target {
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
    kind: u8,
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
}

fn spawn_explosion(
    commands: &mut Commands,
    assets: &ExplosionAssets,
    pos: Vec3,
    size: f32,
) {
    commands.spawn((
        Explosion { age: 0.0, size },
        Mesh3d(assets.mesh.clone()),
        MeshMaterial3d(assets.material.clone()),
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
        let handle: &Handle<StandardMaterial> = &material.0;
        if let Some(mut material) = materials.get_mut(handle) {
            material.base_color = Color::srgba(1.0, 0.55, 0.15, 0.9 * (1.0 - t));
            material.emissive = LinearRgba::rgb(2.5, 1.2, 0.3) * (1.0 - t);
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
    mut plane: Single<(&mut FlightState, &mut Health, &mut Visibility), With<LocalPlane>>,
    mut remotes: Query<(&RemotePlane, &Transform, &mut Health), Without<LocalPlane>>,
) {
    let me = my_peer(&net);
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
                // My own launches already spawned at fire time (fire_control).
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
                        target,
                    );
                    info!(%shooter, "missile in the air");
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
                if shooter == me {
                    info!(%victim, "kill confirmed");
                }
            }
        }
    }
}

fn loadout_is_dead(loadout: &Loadout) -> bool {
    loadout.respawn_in.is_some()
}

/// Dead locals come back after [`RESPAWN_DELAY`] on the next ring slot.
#[allow(clippy::type_complexity)]
fn respawn(
    time: Res<Time>,
    net: Res<NetState>,
    mut loadout: ResMut<Loadout>,
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
    mut lock: ResMut<IrLock>,
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
    loadout.missiles = MISSILE_COUNT;
    loadout.respawn_in = None;
    *lock = IrLock::default();
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

/// `ACES_TEST_FIRE=1`: aim at the nearest living enemy, lock, fire missiles
/// and gun — so the two-instance end-to-end verification runs hands-free.
#[allow(clippy::too_many_arguments)]
fn auto_dogfight(
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    plane: Single<(&FlightState, &Health), With<LocalPlane>>,
    remotes: Query<(&RemotePlane, &Transform, &Health), Without<LocalPlane>>,
    mut aim: ResMut<MouseAim>,
    mut input: ResMut<FlightInput>,
    lock: Res<IrLock>,
    mut loadout: ResMut<Loadout>,
    mut cooldown: ResMut<GunCooldown>,
    mut out: ResMut<NetOut>,
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

    // Nearest living enemy.
    let Some((target_pos, _)) = remotes
        .iter()
        .filter(|(r, _, h)| r.peer != me && !h.dead && h.hp > 0.0)
        .map(|(_, t, _)| (t.translation, ()))
        .min_by(|a, b| a.0.distance_squared(state.pos).total_cmp(&b.0.distance_squared(state.pos)))
    else {
        return;
    };

    // Aim straight at the enemy; the instructor does the flying.
    let dir = (target_pos - state.pos).normalize_or_zero();
    if dir == Vec3::ZERO {
        return;
    }
    *aim = MouseAim::new(Transform::default().looking_to(dir, Vec3::Y).rotation);
    input.aim = aim.dir();

    // Fire everything that is ready.
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
    if lock.progress >= 1.0
        && loadout.missiles > 0
        && let Some(target) = lock.target.clone()
    {
        loadout.missiles -= 1;
        let forward = state.forward();
        let pos = state.pos + forward * 8.0;
        out.events.push(GameEvent::MissileLaunched {
            shooter: me.clone(),
            missile: 0,
            kind: 0,
            pos: pos.to_array(),
            quat: state.quat.to_array(),
            speed: (state.speed() + 100.0).max(250.0),
            target: Some(target),
        });
    }
}
