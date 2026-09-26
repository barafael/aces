//! Flight: the aircraft, its flight model, pilot input and the camera rig.
//!
//! Layout (see PLAN.md):
//! - [`input`]: keyboard + mouse → [`input::FlightInput`] (mouse = virtual
//!   stick, `V` temporarily turns the mouse into a free-look orbit)
//! - [`model`]: semi-realistic point-mass flight model (lift/drag/AoA, stall),
//!   as a pure function so it can be tested headlessly
//! - [`camera`]: smoothed chase cam with free look

pub mod camera;
pub mod input;
pub mod model;

use bevy::prelude::*;

use crate::flight::input::{FlightInput, FreeLook};

pub struct FlightPlugin;

impl Plugin for FlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .add_systems(Startup, spawn_aircraft)
            .add_systems(
                Update,
                (
                    input::gather_input,
                    camera::update_camera.after(input::gather_input),
                ),
            )
            .add_systems(FixedUpdate, model::step_flight);
    }
}

/// Marker + pilot-owned state of the (for now: single) aircraft.
#[derive(Component)]
pub struct Aircraft {
    /// 0 = idle, 1 = full throttle.
    pub throttle: f32,
}

/// World-space velocity of the aircraft [m/s].
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

/// Spawn the solo aircraft with a placeholder low-poly airframe.
///
/// Orientation convention: forward = -Z, up = +Y, right = +X (Bevy default),
/// identity rotation points the nose down -Z. Milestone 3 turns this into
/// per-peer spawned aircraft; the mesh is replaced by glTF models in
/// milestone 6.
fn spawn_aircraft(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
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
            Aircraft {
                throttle: 0.7,
            },
            Velocity(Vec3::new(0.0, 0.0, -150.0)),
            AngularRates::default(),
            AngleOfAttack::default(),
            StallState::default(),
            Transform::from_xyz(0.0, 500.0, 0.0),
            Visibility::default(),
        ))
        .with_children(|parent| {
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
        });
}
