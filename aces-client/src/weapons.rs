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
use std::env;

use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::instructor::Instructor;
use crate::flight::model::FlightState;
use crate::flight::{Aircraft, LocalPlane, RemotePlane, SimPose, Velocity, spawn_state};
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

    /// Still flying: not killed, and with hit points left.
    pub fn alive(&self) -> bool {
        !self.dead && self.hp > 0.0
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

/// Shared weapon visuals, built once: every missile and explosion reuses
/// the same meshes and materials, so nothing is uploaded per shot and the
/// renderer can batch them.
#[derive(Resource, Clone)]
struct WeaponAssets {
    missile_mesh: Handle<Mesh>,
    missile_material: Handle<StandardMaterial>,
    explosion_mesh: Handle<Mesh>,
    /// The explosion fade-out, pre-baked: each explosion steps through
    /// these on its own clock instead of mutating a material every frame.
    explosion_fade: Vec<Handle<StandardMaterial>>,
}

/// Steps of the pre-baked explosion fade.
const EXPLOSION_FADE_STEPS: usize = 12;

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
            .add_systems(Startup, init_weapon_assets)
            .add_systems(
                Update,
                (
                    (
                        update_ir_lock,
                        fire_control,
                        apply_events,
                        respawn,
                        sync_remote_death,
                    )
                        .run_if(in_state(Phase::InGame)),
                    animate_explosions,
                ),
            )
            .add_systems(FixedUpdate, step_missiles.run_if(in_state(Phase::InGame)));
        // The test hook exists only when asked for at launch.
        if env::var("ACES_TEST_FIRE").is_ok() {
            app.add_systems(Update, auto_dogfight.run_if(in_state(Phase::InGame)));
        }
    }
}

/// Seconds since the last gun shot.
#[derive(Resource, Default, Deref, DerefMut)]
struct GunCooldown(f32);

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
    });
}

fn rearm(mut loadout: ResMut<Loadout>, mut lock: ResMut<IrLock>) {
    loadout.missiles = MISSILE_COUNT;
    loadout.respawn_in = None;
    loadout.respawns = 0;
    *lock = IrLock::default();
}

// ── Locking ─────────────────────────────────────────────────────────────────

/// Track the IR lock: nearest living enemy inside the cone. Acquire inside
/// [`IR_CONE`], hold up to [`IR_DROP_CONE`].
fn update_ir_lock(
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: RemoteTargets,
    mut lock: ResMut<IrLock>,
    time: Res<Time>,
) {
    let me = net.my_peer();
    let forward = plane.forward();
    let pos = plane.pos;

    let mut best: Option<(f32, &str)> = None; // (angle, peer)
    for (remote, transform, health) in &remotes {
        if !health.alive() || remote.peer == me {
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
                lock.target = Some(peer.to_string());
                lock.progress = 0.0;
            }
            lock.progress = (lock.progress + time.delta_secs() / IR_LOCK_TIME).min(1.2);
        }
        None if lock.target.is_some() => {
            lock.target = None;
            lock.progress = 0.0;
        }
        None => {}
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
    remotes: RemoteTargets,
) {
    let me = net.my_peer();
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
        out.events.extend(fire_gun(&plane, &remotes, me));
    }

    // Missile: needs a solid lock and a spare.
    if keys.any_just_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
        && *slot == WeaponSlot::IrMissile
        && flying
        && loadout.missiles > 0
        && lock.progress >= 1.0
        && let Some(target) = lock.target.clone()
    {
        loadout.missiles -= 1;
        out.events.push(launch_missile(&plane, me, target));
        // The local missile spawns through the loopback apply path, together
        // with everyone else's — one spawn path for the whole room.
    }
}

/// Where shots and missiles leave the airframe, ahead of its center [m].
const MUZZLE_OFFSET: f32 = 8.0;

/// One gun shot down `plane`'s nose: the damage claim, if it hits.
fn fire_gun(plane: &FlightState, remotes: &RemoteTargets, me: &str) -> Option<GameEvent> {
    let forward = plane.forward();
    let origin = plane.pos + forward * MUZZLE_OFFSET;
    gun_ray_hit(&origin, &forward, remotes, me).map(|victim| GameEvent::Damage {
        shooter: me.to_string(),
        victim,
        amount: GUN_DAMAGE,
        cause: DamageCause::Gun,
    })
}

/// The launch event of an IR missile from `plane` at `target`.
fn launch_missile(plane: &FlightState, me: &str, target: String) -> GameEvent {
    let pos = plane.pos + plane.forward() * MUZZLE_OFFSET;
    GameEvent::MissileLaunched {
        shooter: me.to_string(),
        missile: 0,
        kind: 0,
        pos: pos.to_array(),
        quat: plane.quat.to_array(),
        speed: (plane.speed() + 100.0).max(250.0),
        target: Some(target),
    }
}

/// Every remote plane, as a weapon sees it.
type RemoteTargets<'w, 's> =
    Query<'w, 's, (&'static RemotePlane, &'static Transform, &'static Health), Without<LocalPlane>>;

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

/// Everyone simulates every missile: proportional navigation toward the
/// target's current on-screen position, motor burn, proximity fuse. Only
/// the owner's simulation claims damage.
#[allow(clippy::too_many_arguments)]
fn step_missiles(
    mut commands: Commands,
    assets: Res<WeaponAssets>,
    mut missiles: Query<(Entity, &mut Missile, &mut Transform), Without<RemotePlane>>,
    net: Res<NetState>,
    plane: Single<&FlightState, With<LocalPlane>>,
    remotes: RemoteTargets,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
) {
    let me = net.my_peer();
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
                detonate(
                    &mut commands,
                    &assets,
                    &missile,
                    transform.translation,
                    &mut out,
                );
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
                transform.rotation = (Quat::from_axis_angle(axis.normalize(), swing)
                    * transform.rotation)
                    .normalize();
            }
        }

        let heading = transform.rotation * Vec3::NEG_Z;
        transform.translation += heading * missile.speed * dt;

        // Any-plane proximity (a missile crossing the merge may hit anyone).
        let mut hit_anyone = false;
        for (remote, t, health) in remotes.iter() {
            if remote.peer == missile.owner || !health.alive() {
                continue;
            }
            if t.translation.distance(transform.translation) < MISSILE_PROXIMITY * 0.8 {
                detonate(
                    &mut commands,
                    &assets,
                    &missile,
                    transform.translation,
                    &mut out,
                );
                commands.entity(entity).despawn();
                hit_anyone = true;
                break;
            }
        }
        if hit_anyone {
            continue;
        }

        if missile.age > MISSILE_LIFE || transform.translation.y < 1.0 {
            spawn_explosion(&mut commands, &assets, transform.translation, 3.0);
            commands.entity(entity).despawn();
        }
    }
}

fn detonate(
    commands: &mut Commands,
    assets: &WeaponAssets,
    missile: &Missile,
    pos: Vec3,
    out: &mut NetOut,
) {
    spawn_explosion(commands, assets, pos, 8.0);
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
    assets: &WeaponAssets,
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
        Mesh3d(assets.missile_mesh.clone()),
        MeshMaterial3d(assets.missile_material.clone()),
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

fn spawn_explosion(commands: &mut Commands, assets: &WeaponAssets, pos: Vec3, size: f32) {
    commands.spawn((
        Explosion { age: 0.0, size },
        Mesh3d(assets.explosion_mesh.clone()),
        MeshMaterial3d(assets.explosion_fade[0].clone()),
        Transform::from_translation(pos),
        Visibility::default(),
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
        let step = ((t * EXPLOSION_FADE_STEPS as f32) as usize).min(EXPLOSION_FADE_STEPS - 1);
        if material.0 != assets.explosion_fade[step] {
            material.0 = assets.explosion_fade[step].clone();
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
    mut plane: Single<(&mut FlightState, &mut Health, &mut Visibility), With<LocalPlane>>,
    mut remotes: Query<(&RemotePlane, &Transform, &mut Health), Without<LocalPlane>>,
) {
    let me = net.my_peer();
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
                    spawn_explosion(&mut commands, &assets, plane.0.pos, 8.0);
                    out.events.push(GameEvent::Killed {
                        victim: me.to_string(),
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
                        spawn_explosion(&mut commands, &assets, transform.translation, 8.0);
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
            &Aircraft,
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
    let (aircraft, state, instructor, pose, velocity, health, transform, visibility) = &mut *plane;
    **state = spawn_state(index, &aircraft.airframe);
    **instructor = Instructor::default();
    **pose = SimPose::new(state.pos, state.quat);
    velocity.0 = state.vel;
    **health = Health::full();
    **transform = Transform::from_translation(state.pos).with_rotation(state.quat);
    **visibility = Visibility::Visible;
    loadout.missiles = MISSILE_COUNT;
    loadout.respawn_in = None;
    *lock = IrLock::default();
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

/// `ACES_TEST_FIRE=1`: aim at the nearest living enemy, lock, fire missiles
/// and gun — so the two-instance end-to-end verification runs hands-free.
#[allow(clippy::too_many_arguments)]
fn auto_dogfight(
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    plane: Single<(&FlightState, &Health), With<LocalPlane>>,
    remotes: RemoteTargets,
    mut aim: ResMut<MouseAim>,
    mut input: ResMut<FlightInput>,
    lock: Res<IrLock>,
    mut loadout: ResMut<Loadout>,
    mut cooldown: ResMut<GunCooldown>,
    mut out: ResMut<NetOut>,
    time: Res<Time>,
) {
    if *mode == NetworkMode::Solo {
        return;
    }
    if loadout_is_dead(&loadout) || plane.1.dead {
        return;
    }
    let me = net.my_peer();
    let (state, _) = *plane;

    // Nearest living enemy.
    let Some((target_pos, _)) = remotes
        .iter()
        .filter(|(r, _, h)| r.peer != me && h.alive())
        .map(|(_, t, _)| (t.translation, ()))
        .min_by(|a, b| {
            a.0.distance_squared(state.pos)
                .total_cmp(&b.0.distance_squared(state.pos))
        })
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

    // Fire everything that is ready — through the same shot and launch
    // code as the pilot's triggers.
    **cooldown += time.delta_secs();
    if **cooldown >= 1.0 / GUN_RATE {
        **cooldown = 0.0;
        out.events.extend(fire_gun(state, &remotes, me));
    }
    if lock.progress >= 1.0
        && loadout.missiles > 0
        && let Some(target) = lock.target.clone()
    {
        loadout.missiles -= 1;
        out.events.push(launch_missile(state, me, target));
    }
}
