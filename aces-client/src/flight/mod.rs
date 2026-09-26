//! Flight: the aircraft, its flight model, pilot input and the camera rig.
//!
//! Layout (see PLAN.md):
//! - [`input`]: WT-arcade mouse aim (the instructor flies the nose onto the
//!   cursor), WASD-QE keys, `V` free look
//! - [`model`]: semi-realistic point-mass flight model (lift/drag/AoA, stall),
//!   as a pure function so it can be tested headlessly
//! - [`camera`]: smoothed chase cam with free look
//!
//! The simulation runs on the fixed tick and writes [`SimPose`]; the rendered
//! [`Transform`] is interpolated between the last two poses every frame, so
//! motion stays smooth at any refresh rate. In a network game each peer is
//! authoritative over its own plane ([`LocalPlane`]); other players' planes
//! are [`RemotePlane`]s driven by snapshot interpolation (see `net`).

pub mod camera;
pub mod input;
pub mod model;

use bevy::prelude::*;

use aces_net::NetState;
use aces_protocol::{spawn_point, PlaneSnapshot};
use std::collections::VecDeque;

use self::input::{FlightInput, FreeLook};

pub struct FlightPlugin;

impl Plugin for FlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .add_systems(OnEnter(crate::Phase::InGame), spawn_planes)
            .add_systems(OnExit(crate::Phase::InGame), despawn_planes)
            .add_systems(
                Update,
                (
                    input::gather_input.run_if(in_state(crate::Phase::InGame)),
                    interpolate_pose,
                    camera::update_camera
                        .after(input::gather_input)
                        .after(interpolate_pose),
                ),
            )
            .add_systems(FixedUpdate, model::step_flight);
    }
}

/// The plane this peer flies. Authoritative locally; broadcast via snapshots.
#[derive(Component)]
pub struct LocalPlane;

/// Another player's plane, driven by snapshot interpolation — never
/// simulated locally, and therefore without [`SimPose`].
#[derive(Component)]
pub struct RemotePlane {
    pub peer: String,
    /// (local receive time [s], snapshot), oldest first.
    pub history: VecDeque<(f64, PlaneSnapshot)>,
}

/// Marker + pilot-owned state of the local aircraft.
#[derive(Component)]
pub struct Aircraft {
    /// 0 = idle, 1 = full throttle.
    pub throttle: f32,
}

/// The simulated pose, advanced on the fixed tick. The rendered
/// [`Transform`] is interpolated between `previous` and `current` every
/// frame, so motion stays smooth at any refresh rate.
#[derive(Component, Clone, Copy)]
pub struct SimPose {
    pub previous: (Vec3, Quat),
    pub current: (Vec3, Quat),
}

impl SimPose {
    pub fn new(translation: Vec3, rotation: Quat) -> Self {
        Self {
            previous: (translation, rotation),
            current: (translation, rotation),
        }
    }
}

/// Place the rendered aircraft between its last two simulated poses by the
/// fraction of a fixed tick that has elapsed since the latest one.
fn interpolate_pose(fixed: Res<Time<Fixed>>, mut planes: Query<(&SimPose, &mut Transform)>) {
    let t = fixed.overstep_fraction();
    for (pose, mut transform) in &mut planes {
        transform.translation = pose.previous.0.lerp(pose.current.0, t);
        transform.rotation = pose.previous.1.slerp(pose.current.1, t);
    }
}

/// World-space velocity of an aircraft [m/s].
#[derive(Component, Default, Deref, DerefMut)]
pub struct Velocity(pub Vec3);

/// Smoothed commanded angular rates [rad/s], positive pitch = nose up,
/// positive yaw = nose right, positive roll = right wing down.
#[derive(Component, Default, Clone, Copy)]
pub struct AngularRates {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

/// Angle of attack [rad], positive when the nose sits above the velocity
/// vector. Diagnostic for now; the HUD will read it later.
#[derive(Component, Default, Deref, DerefMut)]
pub struct AngleOfAttack(pub f32);

/// Aerodynamic stall state, including the buffet oscillator phase used to
/// shake the airframe while stalled.
#[derive(Component, Default)]
pub struct StallState {
    pub active: bool,
    pub buffet_phase: f32,
}

// ── Spawning ────────────────────────────────────────────────────────────────

/// Enter the game: spawn the local plane at this peer's spawn slot and one
/// remote plane per other roster entry. Solo mode spawns only the local
/// plane at slot 0.
fn spawn_planes(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    net: Res<NetState>,
    mode: Res<crate::NetworkMode>,
) {
    if *mode == crate::NetworkMode::Solo {
        spawn_local(&mut commands, &mut meshes, &mut materials, 0);
        return;
    }

    // Spawn remote planes first so `Single<..., With<LocalPlane>>` systems
    // never observe a frame with the wrong plane count.
    for (index, player) in net.players.iter().enumerate() {
        if net.is_me(&player.peer) {
            continue;
        }
        spawn_remote(&mut commands, &mut meshes, &mut materials, &player.peer, index);
    }
    let index = net.my_index().unwrap_or(0);
    spawn_local(&mut commands, &mut meshes, &mut materials, index);
}

/// Spawn the local aircraft at spawn slot `index`.
fn spawn_local(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    index: usize,
) {
    let (pos, yaw) = spawn_point(index);
    let quat = Quat::from_rotation_y(yaw);
    let vel = quat * Vec3::NEG_Z * 150.0;

    let fuselage = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.57, 0.60),
        perceptual_roughness: 0.6,
        ..default()
    });
    let accent = materials.add(StandardMaterial {
        base_color: Color::srgb(0.25, 0.27, 0.30),
        perceptual_roughness: 0.7,
        ..default()
    });

    commands
        .spawn((
            LocalPlane,
            Aircraft { throttle: 0.7 },
            SimPose::new(pos.into(), quat),
            Velocity(vel),
            AngularRates::default(),
            AngleOfAttack::default(),
            StallState::default(),
            Transform::from_translation(pos.into()).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(parent, meshes, fuselage, accent);
        });
}

/// Spawn a remote player's plane (placeholder airframe, tinted by a hash of
/// the peer id so planes are tellable apart).
fn spawn_remote(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    peer: &str,
    index: usize,
) {
    let (pos, yaw) = spawn_point(index);
    let quat = Quat::from_rotation_y(yaw);

    // Stable pseudo-color from the peer id (FNV-1a).
    let mut hash: u32 = 0x811c_9dc5;
    for b in peer.bytes() {
        hash = hash.wrapping_mul(0x0100_0193).wrapping_add(b as u32);
    }
    let hue = (hash % 360) as f32;

    let fuselage = materials.add(StandardMaterial {
        base_color: Color::hsl(hue, 0.45, 0.55),
        perceptual_roughness: 0.6,
        ..default()
    });
    let accent = materials.add(StandardMaterial {
        base_color: Color::hsl(hue, 0.45, 0.25),
        perceptual_roughness: 0.7,
        ..default()
    });

    commands
        .spawn((
            RemotePlane {
                peer: peer.to_string(),
                history: VecDeque::new(),
            },
            Transform::from_translation(pos.into()).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(parent, meshes, fuselage, accent);
        });
}

/// The placeholder low-poly airframe shared by local and remote planes
/// (replaced by glTF models in milestone 6).
fn spawn_airframe(
    parent: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    meshes: &mut ResMut<Assets<Mesh>>,
    fuselage: Handle<StandardMaterial>,
    accent: Handle<StandardMaterial>,
) {
    // Fuselage: capsule lying along -Z/+Z.
    parent.spawn((
        Mesh3d(meshes.add(Capsule3d::new(1.2, 12.0))),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_rotation(Quat::from_rotation_x(core::f32::consts::FRAC_PI_2)),
    ));
    // Main wings.
    parent.spawn((
        Mesh3d(meshes.add(Cuboid::new(16.0, 0.4, 2.6))),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_xyz(0.0, 0.0, 0.5),
    ));
    // Horizontal stabilizer.
    parent.spawn((
        Mesh3d(meshes.add(Cuboid::new(6.0, 0.3, 1.6))),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_xyz(0.0, 0.3, 6.2),
    ));
    // Vertical fin.
    parent.spawn((
        Mesh3d(meshes.add(Cuboid::new(0.3, 2.4, 2.0))),
        MeshMaterial3d(accent.clone()),
        Transform::from_xyz(0.0, 1.4, 6.2),
    ));
}

/// Leave the game: planes are rebuilt from scratch on the next entry.
fn despawn_planes(
    mut commands: Commands,
    locals: Query<Entity, With<LocalPlane>>,
    remotes: Query<Entity, With<RemotePlane>>,
) {
    for entity in locals.iter().chain(remotes.iter()) {
        commands.entity(entity).despawn();
    }
}
