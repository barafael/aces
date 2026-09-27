//! Flight: the aircraft, its flight model, pilot input and the camera rig.
//!
//! Layout (see PLAN.md):
//! - [`input`]: War Thunder mouse aim — the mouse steers a world-space aim
//!   direction (and the view), WASD-QE keys override axes, `V` free look
//! - [`instructor`]: the flight computer that flies the nose onto the aim,
//!   with envelope protection
//! - [`model`]: rigid-body flight model (surfaces → moments → AoA/sideslip →
//!   lift/drag/side force), as a pure function so it can be tested headlessly
//! - [`camera`]: WT third-person camera looking along the aim
//!
//! The simulation runs on the fixed tick and writes [`SimPose`]; the rendered
//! [`Transform`] is interpolated between the last two poses every frame, so
//! motion stays smooth at any refresh rate. In a network game each peer is
//! authoritative over its own plane ([`LocalPlane`]); other players' planes
//! are [`RemotePlane`]s driven by snapshot interpolation (see `net`).

/// `(name, value)` pairs of tuning constants, for flight-log headers (angles
/// in radians, like the constants themselves).
#[cfg(not(target_arch = "wasm32"))]
macro_rules! tuning_table {
    ($($name:ident),* $(,)?) => {
        vec![$((stringify!($name), $name as f32)),*]
    };
}

pub mod camera;
pub mod input;
pub mod instructor;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod recorder;
#[cfg(not(target_arch = "wasm32"))]
pub mod replay;

#[cfg(not(target_arch = "wasm32"))]
/// Every tuning constant of the flight stack, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    let mut all = model::tuning();
    all.extend(instructor::tuning());
    all.extend(input::tuning());
    all.extend(camera::tuning());
    all
}

use bevy::prelude::*;

use aces_net::NetState;
use aces_protocol::{PlaneSnapshot, spawn_point};
use std::collections::VecDeque;

use self::input::{FlightInput, FreeLook, MouseAim};
use self::instructor::Instructor;
use self::model::FlightState;

pub struct FlightPlugin;

impl Plugin for FlightPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(recorder::RecorderPlugin {
            dir: recorder::log_dir_from_env(),
        });
        app.init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .init_resource::<MouseAim>()
            .add_systems(OnEnter(crate::Phase::InGame), spawn_planes)
            .add_systems(OnExit(crate::Phase::InGame), despawn_planes)
            .add_systems(
                Update,
                (
                    input::gather_input.run_if(in_state(crate::Phase::InGame)),
                    interpolate_pose,
                    animate_control_surfaces,
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

/// Which aircraft type a plane is, with its flight characteristics. On
/// every plane: the local one flies on the airframe, remote ones will pick
/// their model by it (milestone 6).
#[derive(Component, Clone, Copy, Debug)]
pub struct Aircraft {
    /// Index into [`aces_protocol::AIRCRAFT`].
    pub index: u8,
    pub airframe: aces_protocol::Airframe,
}

impl Aircraft {
    /// Aircraft type `index` (unknown indices fall back to the first).
    pub fn of_type(index: u8) -> Self {
        let kind = aces_protocol::aircraft(index);
        Self {
            index: if usize::from(index) < aces_protocol::AIRCRAFT.len() {
                index
            } else {
                0
            },
            airframe: kind.airframe,
        }
    }

    pub fn name(&self) -> &'static str {
        aces_protocol::aircraft(self.index).name
    }
}

/// Another player's plane, driven by snapshot interpolation — never
/// simulated locally, and therefore without [`SimPose`].
#[derive(Component)]
pub struct RemotePlane {
    pub peer: String,
    /// (local receive time [s], snapshot), oldest first.
    pub history: VecDeque<(f64, PlaneSnapshot)>,
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

/// Body angular velocity [rad/s]: positive pitch = nose up, positive yaw =
/// nose right, positive roll = right wing down.
#[derive(Default, Clone, Copy, Debug)]
pub struct AngularRates {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

/// Control-surface positions in [-1, 1] — as commanded, or as the
/// actuators actually hold them. Positive elevator = pull (trailing edge
/// up), positive aileron = roll right, positive rudder = yaw right. Every
/// plane carries the actual positions for the animated airframe: the local
/// plane from its simulation, remote planes from their snapshots.
#[derive(Component, Default, Clone, Copy, Debug, PartialEq)]
pub struct Surfaces {
    pub elevator: f32,
    pub aileron: f32,
    pub rudder: f32,
}

/// A visible, deflecting control surface on an airframe.
#[derive(Component, Clone, Copy)]
pub enum ControlSurface {
    /// Horizontal stabilizer trailing edge (pitch).
    Elevator,
    /// Wing trailing edge panels; `left` distinguishes the pair.
    Aileron { left: bool },
    /// Vertical fin trailing edge (yaw).
    Rudder,
}

/// Max elevator deflection [rad].
pub const ELEVATOR_DEFLECT: f32 = 25f32.to_radians();
/// Max aileron deflection [rad].
pub const AILERON_DEFLECT: f32 = 22f32.to_radians();
/// Max rudder deflection [rad].
pub const RUDDER_DEFLECT: f32 = 28f32.to_radians();

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
    mut aim: ResMut<MouseAim>,
) {
    // The aircraft chosen in the lobby (solo keeps the last choice).
    let mine = Aircraft::of_type(net.aircraft);
    if *mode == crate::NetworkMode::Solo {
        *aim = spawn_local(&mut commands, &mut meshes, &mut materials, 0, mine);
        return;
    }

    // Spawn remote planes first so `Single<..., With<LocalPlane>>` systems
    // never observe a frame with the wrong plane count.
    for (index, player) in net.players.iter().enumerate() {
        if net.is_me(&player.peer) {
            continue;
        }
        spawn_remote(
            &mut commands,
            &mut meshes,
            &mut materials,
            &player.peer,
            index,
            Aircraft::of_type(player.aircraft),
        );
    }
    let index = net.my_index().unwrap_or(0);
    *aim = spawn_local(&mut commands, &mut meshes, &mut materials, index, mine);
}

/// Spawn speed in multiples of the airframe's 1 g stall speed (~180 m/s
/// for the placeholder jet), and spawn throttle.
const SPAWN_SPEED_STALLS: f32 = 3.15;
const SPAWN_THROTTLE: f32 = 1.0;

/// Spawn the local aircraft at spawn slot `index`; returns the mouse aim
/// pointing along its nose.
fn spawn_local(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    index: usize,
    aircraft: Aircraft,
) -> MouseAim {
    let (pos, yaw) = spawn_point(index);
    let quat = Quat::from_rotation_y(yaw);
    let speed = SPAWN_SPEED_STALLS * aircraft.airframe.stall_speed();
    let state = FlightState::new(pos.into(), quat, speed, SPAWN_THROTTLE);

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
            aircraft,
            state,
            Instructor::default(),
            SimPose::new(pos.into(), quat),
            Velocity(state.vel),
            Surfaces::default(),
            model::TickTelemetry::default(),
            Transform::from_translation(pos.into()).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(parent, meshes, materials, fuselage, accent);
        });
    MouseAim::new(quat)
}

/// Spawn a remote player's plane (placeholder airframe, tinted by a hash of
/// the peer id so planes are tellable apart).
fn spawn_remote(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    peer: &str,
    index: usize,
    aircraft: Aircraft,
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
            aircraft,
            Surfaces::default(),
            Transform::from_translation(pos.into()).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(parent, meshes, materials, fuselage, accent);
        });
}

/// The placeholder low-poly airframe shared by local and remote planes
/// (replaced by glTF models in milestone 6), with hinged control surfaces
/// that [`animate_control_surfaces`] deflects.
fn spawn_airframe(
    parent: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
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

    // Control surfaces: each is a hinge entity on the hinge line with the
    // panel offset behind it, so rotating the hinge swings the panel.
    let surface_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.38, 0.40, 0.43),
        perceptual_roughness: 0.7,
        ..default()
    });

    // Elevator: stabilizer trailing edge (stabilizer chord ends at z = 7.0).
    parent
        .spawn((
            ControlSurface::Elevator,
            Transform::from_xyz(0.0, 0.3, 6.8),
            Visibility::default(),
        ))
        .with_children(|hinge| {
            hinge.spawn((
                Mesh3d(meshes.add(Cuboid::new(5.4, 0.12, 0.5))),
                MeshMaterial3d(surface_material.clone()),
                Transform::from_xyz(0.0, 0.0, 0.3),
            ));
        });

    // Ailerons: outboard wing trailing edge (wing chord ends at z = 1.8).
    for (x, left) in [(-6.3, true), (6.3, false)] {
        parent
            .spawn((
                ControlSurface::Aileron { left },
                Transform::from_xyz(x, 0.0, 1.55),
                Visibility::default(),
            ))
            .with_children(|hinge| {
                hinge.spawn((
                    Mesh3d(meshes.add(Cuboid::new(2.4, 0.12, 0.55))),
                    MeshMaterial3d(surface_material.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.32),
                ));
            });
    }

    // Rudder: fin trailing edge (fin chord ends at z = 7.2).
    parent
        .spawn((
            ControlSurface::Rudder,
            Transform::from_xyz(0.0, 1.5, 6.9),
            Visibility::default(),
        ))
        .with_children(|hinge| {
            hinge.spawn((
                Mesh3d(meshes.add(Cuboid::new(0.12, 2.0, 0.8))),
                MeshMaterial3d(surface_material.clone()),
                Transform::from_xyz(0.0, 0.0, 0.45),
            ));
        });
}

/// Deflect every plane's control surfaces to the actuator positions in its
/// [`Surfaces`] — the same deflections the flight model is flying on.
///
/// Sign conventions (nose is -Z): pull → elevator trailing edge up; roll
/// right → right aileron up, left down; yaw right → rudder trailing edge
/// right.
fn animate_control_surfaces(
    planes: Query<&Surfaces>,
    mut hinges: Query<(&ControlSurface, &mut Transform, &ChildOf)>,
) {
    for (surface, mut transform, child_of) in &mut hinges {
        let Ok(s) = planes.get(child_of.parent()) else {
            continue;
        };
        let (axis, angle) = match surface {
            ControlSurface::Elevator => (Vec3::X, -s.elevator * ELEVATOR_DEFLECT),
            ControlSurface::Aileron { left: true } => (Vec3::X, s.aileron * AILERON_DEFLECT),
            ControlSurface::Aileron { left: false } => (Vec3::X, -s.aileron * AILERON_DEFLECT),
            ControlSurface::Rudder => (Vec3::Y, s.rudder * RUDDER_DEFLECT),
        };
        transform.rotation = Quat::from_axis_angle(axis, angle);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::input::InputPlugin;
    use bevy::input::mouse::MouseMotion;
    use bevy::time::TimeUpdateStrategy;
    use core::time::Duration;

    /// The whole local-flight chain, headless: mouse motion → aim → fixed
    /// tick instructor → physics → pose/surfaces, with the real systems.
    #[test]
    fn mouse_motion_flies_the_local_plane() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, InputPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 60.0,
            )))
            .insert_resource(Time::<Fixed>::from_hz(aces_protocol::TICK_HZ))
            .init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .insert_resource(MouseAim::new(Quat::IDENTITY))
            .add_systems(Update, input::gather_input)
            .add_systems(FixedUpdate, model::step_flight);
        let state = FlightState::new(Vec3::new(0.0, 3000.0, 0.0), Quat::IDENTITY, 180.0, 1.0);
        let plane = app
            .world_mut()
            .spawn((
                LocalPlane,
                Aircraft::of_type(0),
                state,
                Instructor::default(),
                SimPose::new(state.pos, state.quat),
                Velocity(state.vel),
                Surfaces::default(),
                model::TickTelemetry::default(),
            ))
            .id();

        // Swing the aim ~45° right over a quarter second, then let it be.
        for frame in 0..600 {
            if frame < 15 {
                app.world_mut().write_message(MouseMotion {
                    delta: Vec2::new(24.0, 0.0),
                });
            }
            app.update();
            if frame == 30 {
                let surfaces = *app.world().get::<Surfaces>(plane).unwrap();
                assert!(
                    surfaces.aileron.abs() > 0.1,
                    "no roll into the turn: {surfaces:?}"
                );
            }
        }

        let aim = app.world().resource::<MouseAim>().dir();
        assert!(aim.x > 0.6, "aim did not swing right: {aim:?}");
        let state = app.world().get::<FlightState>(plane).unwrap();
        let err = state.forward().angle_between(aim).to_degrees();
        assert!(err < 2.0, "plane did not follow the aim: {err:.1}° off");
        let pose = app.world().get::<SimPose>(plane).unwrap();
        assert_eq!(pose.current, (state.pos, state.quat));
    }

    fn key(app: &mut App, key_code: KeyCode, state: bevy::input::ButtonState) {
        app.world_mut()
            .write_message(bevy::input::keyboard::KeyboardInput {
                key_code,
                logical_key: bevy::input::keyboard::Key::Unidentified(
                    bevy::input::keyboard::NativeKey::Unidentified,
                ),
                state,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
    }

    /// The recorder captures a whole flight (mouse, free look, keyboard
    /// override, a mark) through the real game systems, and both replay
    /// modes reproduce it bit for bit.
    #[test]
    fn recorded_flight_replays_exactly() {
        use bevy::input::ButtonState::{Pressed, Released};
        use bevy::state::app::StatesPlugin;

        let dir = std::env::temp_dir().join(format!(
            "aces-flightlog-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, InputPlugin, StatesPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
                1.0 / 144.0,
            )))
            .insert_resource(Time::<Fixed>::from_hz(aces_protocol::TICK_HZ))
            .init_state::<crate::Phase>()
            .init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .add_plugins(recorder::RecorderPlugin {
                dir: Some(dir.clone()),
            })
            .add_systems(
                Update,
                (
                    input::gather_input,
                    interpolate_pose,
                    camera::update_camera
                        .after(input::gather_input)
                        .after(interpolate_pose),
                ),
            )
            .add_systems(FixedUpdate, model::step_flight);

        let quat = Quat::from_rotation_y(0.7);
        let state = FlightState::new(Vec3::new(10.0, 2500.0, -40.0), quat, 170.0, 0.9);
        app.insert_resource(MouseAim::new(quat));
        app.world_mut()
            .spawn((Camera3d::default(), Transform::default()));
        app.world_mut().spawn((
            LocalPlane,
            Aircraft::of_type(0),
            state,
            Instructor::default(),
            SimPose::new(state.pos, state.quat),
            Velocity(state.vel),
            Surfaces::default(),
            model::TickTelemetry::default(),
            Transform::from_translation(state.pos).with_rotation(quat),
        ));
        app.world_mut()
            .resource_mut::<NextState<crate::Phase>>()
            .set(crate::Phase::InGame);

        for frame in 0..900 {
            // Wander the mouse around, uneven per frame like a real one.
            if frame % 3 != 0 && frame < 700 {
                let f = frame as f32;
                app.world_mut().write_message(MouseMotion {
                    delta: Vec2::new((f * 0.05).sin() * 9.0, (f * 0.031).cos() * 6.0),
                });
            }
            match frame {
                200 => key(&mut app, KeyCode::KeyV, Pressed),
                320 => key(&mut app, KeyCode::KeyV, Released),
                400 => key(&mut app, KeyCode::KeyD, Pressed),
                430 => key(&mut app, KeyCode::KeyD, Released),
                500 => key(&mut app, KeyCode::ShiftLeft, Pressed),
                560 => key(&mut app, KeyCode::ShiftLeft, Released),
                600 => key(&mut app, KeyCode::KeyM, Pressed),
                601 => key(&mut app, KeyCode::KeyM, Released),
                _ => {}
            }
            app.update();
        }
        app.world_mut()
            .resource_mut::<NextState<crate::Phase>>()
            .set(crate::Phase::Menu);
        app.update();

        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(
            files.len(),
            1,
            "expected one flight log in {}",
            dir.display()
        );
        let log = replay::load(&files[0].path()).unwrap();
        let csv = dir.join("replay.csv");
        let mouse_report = replay::report(&log, true, false, Some(&csv)).unwrap();
        let csv = std::fs::read_to_string(&csv).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(mouse_report.contains("replay: exact"), "{mouse_report}");
        let mut rows = csv.lines();
        assert!(
            rows.next()
                .unwrap()
                .starts_with("segment,tick,t,rec_err_deg")
        );
        assert_eq!(rows.count(), log.segments[0].ticks.len());

        assert!(log.header.is_some());
        assert_eq!(log.skipped, 0);
        assert_eq!(log.marks.len(), 1, "the M mark is missing");
        let segment = &log.segments[0];
        assert!(
            segment.ticks.len() > 350,
            "only {} ticks",
            segment.ticks.len()
        );
        assert!(
            segment.frames.len() >= 899,
            "only {} frames",
            segment.frames.len()
        );
        assert!(segment.frames.iter().any(|f| f.keys.contains('V')));
        assert!(segment.ticks.iter().any(|t| t.input.roll == Some(1.0)));
        assert!(segment.ticks.iter().any(|t| t.input.limiter_off));

        for mouse in [false, true] {
            let (airframe, _) = replay::airframe_for(segment, false);
            let (samples, divergence) = replay::replay(segment, &airframe, mouse);
            assert_eq!(samples.len(), segment.ticks.len());
            assert!(
                divergence.exact,
                "replay (mouse: {mouse}) diverged: {divergence:?}"
            );
        }
        let report = replay::report(&log, false, false, None).unwrap();
        println!("{report}");
        assert!(report.contains("replay: exact"), "{report}");
        assert!(report.contains("mark at t ="), "{report}");
    }
}
