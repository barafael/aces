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
pub mod rig;

#[cfg(not(target_arch = "wasm32"))]
/// Every tuning constant of the flight stack, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    let mut all = model::tuning();
    all.extend(instructor::tuning());
    all.extend(input::tuning());
    all.extend(camera::tuning());
    all
}

use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy::world_serialization::{WorldAsset, WorldAssetRoot};

use aces_net::NetState;
use aces_protocol::{PlaneSnapshot, spawn_point};
use core::f32::consts::FRAC_PI_2;
use std::collections::VecDeque;

use self::input::{FlightInput, FreeLook, MouseAim};
use self::instructor::Instructor;
pub use self::model::FlightState;

pub struct FlightPlugin;

impl Plugin for FlightPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(recorder::RecorderPlugin {
            dir: recorder::log_dir_from_env(),
        });
        app.add_plugins(rig::RigPlugin)
            .init_resource::<FlightInput>()
            .init_resource::<FreeLook>()
            .init_resource::<MouseAim>()
            .add_systems(Startup, init_plane_assets)
            .add_systems(OnEnter(crate::Phase::InGame), spawn_planes)
            .add_systems(OnExit(crate::Phase::InGame), despawn_planes)
            .add_systems(
                Update,
                (
                    input::gather_input.run_if(in_state(crate::Phase::InGame)),
                    interpolate_pose,
                    animate_control_surfaces,
                    cycle_aircraft.run_if(in_state(crate::Phase::InGame)),
                    settle_models,
                    camera::update_camera
                        .after(input::gather_input)
                        .after(interpolate_pose),
                ),
            )
            .configure_sets(FixedUpdate, SpawnSet.before(model::step_flight))
            .add_systems(FixedUpdate, model::step_flight);
    }
}

/// The plane this peer flies. Authoritative locally; broadcast via snapshots.
#[derive(Component)]
pub struct LocalPlane;

/// Fixed-tick systems that (re)spawn the local plane. They run before the
/// flight model steps, and the flight recorder notes the spawn after them,
/// so a spawn always lands between two ticks — never mid-frame.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpawnSet;

/// Which life of the local plane this is, bumped on every respawn. A
/// respawn resets the plane in place, so this is how observers (the flight
/// recorder) tell a fresh plane from one that has been flying.
#[derive(Component, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Life(pub u32);

/// Which aircraft type a plane is, with its flight characteristics and
/// combat stores. On every plane: the local one flies on the airframe and
/// fights with the combat numbers, remote ones pick their model and hit
/// points by it.
#[derive(Component, Clone, Copy, Debug)]
pub struct Aircraft {
    /// Index into [`aces_protocol::AIRCRAFT`].
    pub index: u8,
    pub airframe: aces_protocol::Airframe,
    pub combat: aces_protocol::Combat,
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
            combat: kind.combat,
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
        (transform.translation, transform.rotation) =
            pose.previous.interpolate_stable(&pose.current, t);
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

impl Surfaces {
    /// `[elevator, aileron, rudder]`: the order logs and snapshots use.
    pub fn to_array(self) -> [f32; 3] {
        [self.elevator, self.aileron, self.rudder]
    }

    pub fn from_array([elevator, aileron, rudder]: [f32; 3]) -> Self {
        Self {
            elevator,
            aileron,
            rudder,
        }
    }
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
    assets: Res<PlaneAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    net: Res<NetState>,
    mode: Res<crate::NetworkMode>,
    mut aim: ResMut<MouseAim>,
) {
    // The aircraft chosen in the lobby (solo keeps the last choice).
    let mine = Aircraft::of_type(net.aircraft);
    if *mode == crate::NetworkMode::Solo {
        *aim = spawn_local(&mut commands, &assets, 0, mine);
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
            &assets,
            &mut materials,
            &player.peer,
            index,
            Aircraft::of_type(player.aircraft),
        );
    }
    let index = net.my_index().unwrap_or(0);
    *aim = spawn_local(&mut commands, &assets, index, mine);
}

/// Meshes and materials every placeholder airframe shares, built once:
/// shared handles let the renderer batch the planes, and nothing is
/// re-uploaded when a game starts. (Only remote tints are per plane.)
#[derive(Resource)]
pub struct PlaneAssets {
    fuselage: Handle<Mesh>,
    wing: Handle<Mesh>,
    stabilizer: Handle<Mesh>,
    fin: Handle<Mesh>,
    elevator: Handle<Mesh>,
    aileron: Handle<Mesh>,
    rudder: Handle<Mesh>,
    surface_material: Handle<StandardMaterial>,
    local_fuselage: Handle<StandardMaterial>,
    local_accent: Handle<StandardMaterial>,
    /// glTF model per aircraft, by [`aces_protocol::AIRCRAFT`] index;
    /// `None` (the placeholder, or a model that failed to load) keeps the
    /// procedural airframe. All are loaded up front so `P` cycles
    /// instantly.
    scenes: Vec<Option<PlaneModel>>,
}

/// An aircraft's glTF scene, and whether it is ready to show.
struct PlaneModel {
    scene: Handle<WorldAsset>,
    /// Loaded, meshes and textures included (see [`settle_models`]).
    ready: bool,
}

impl PlaneAssets {
    /// The airframe variant shown for aircraft `selected`: its model once it
    /// is ready, the placeholder (`0`) until then — or for good, when it has
    /// none (e.g. a checkout without the LFS models).
    fn shown(&self, selected: u8) -> u8 {
        if self
            .scenes
            .get(usize::from(selected))
            .is_some_and(|model| model.as_ref().is_some_and(|model| model.ready))
        {
            selected
        } else {
            0
        }
    }
}

fn init_plane_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Res<AssetServer>,
) {
    let mut material = |color: Color, roughness: f32| {
        materials.add(StandardMaterial {
            base_color: color,
            perceptual_roughness: roughness,
            ..default()
        })
    };
    let surface_material = material(Color::srgb(0.38, 0.40, 0.43), 0.7);
    let local_fuselage = material(Color::srgb(0.55, 0.57, 0.60), 0.6);
    let local_accent = material(Color::srgb(0.25, 0.27, 0.30), 0.7);
    let scenes = aces_protocol::AIRCRAFT
        .iter()
        .map(|kind| {
            kind.model.map(|path| PlaneModel {
                scene: assets.load(GltfAssetLabel::Scene(0).from_asset(path)),
                ready: false,
            })
        })
        .collect();
    commands.insert_resource(PlaneAssets {
        fuselage: meshes.add(Capsule3d::new(1.2, 12.0)),
        wing: meshes.add(Cuboid::new(16.0, 0.4, 2.6)),
        stabilizer: meshes.add(Cuboid::new(6.0, 0.3, 1.6)),
        fin: meshes.add(Cuboid::new(0.3, 2.4, 2.0)),
        elevator: meshes.add(Cuboid::new(5.4, 0.12, 0.5)),
        aileron: meshes.add(Cuboid::new(2.4, 0.12, 0.55)),
        rudder: meshes.add(Cuboid::new(0.12, 2.0, 0.8)),
        surface_material,
        local_fuselage,
        local_accent,
        scenes,
    });
}

/// A fresh plane of airframe `airframe` at spawn slot `index`, flying level
/// along the ring at spawn speed. Shared by the first spawn and respawns.
pub fn spawn_state(index: usize, airframe: &aces_protocol::Airframe) -> FlightState {
    let (pos, yaw) = spawn_point(index);
    let speed = SPAWN_SPEED_STALLS * airframe.stall_speed();
    FlightState::new(
        pos.into(),
        Quat::from_rotation_y(yaw),
        speed,
        SPAWN_THROTTLE,
    )
}

/// Spawn speed in multiples of the airframe's 1 g stall speed (~180 m/s
/// for the placeholder jet), and spawn throttle.
const SPAWN_SPEED_STALLS: f32 = 3.15;
const SPAWN_THROTTLE: f32 = 1.0;

/// Spawn the local aircraft at spawn slot `index`; returns the mouse aim
/// pointing along its nose.
fn spawn_local(
    commands: &mut Commands,
    assets: &PlaneAssets,
    index: usize,
    aircraft: Aircraft,
) -> MouseAim {
    let state = spawn_state(index, &aircraft.airframe);
    let (pos, quat) = (state.pos, state.quat);

    commands
        .spawn((
            LocalPlane,
            Life::default(),
            aircraft,
            state,
            crate::weapons::Health::full(&aircraft.combat),
            Instructor::default(),
            SimPose::new(pos, quat),
            Velocity(state.vel),
            Surfaces::default(),
            model::LastCommand::default(),
            Transform::from_translation(pos).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(
                parent,
                assets,
                aircraft.index,
                assets.local_fuselage.clone(),
                assets.local_accent.clone(),
            );
        });
    MouseAim::new(quat)
}

/// Spawn a remote player's plane (placeholder airframe, tinted by a hash of
/// the peer id so planes are tellable apart).
fn spawn_remote(
    commands: &mut Commands,
    assets: &PlaneAssets,
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
            crate::weapons::Health::full(&aircraft.combat),
            aircraft,
            Surfaces::default(),
            Transform::from_translation(pos.into()).with_rotation(quat),
            Visibility::default(),
        ))
        .with_children(|parent| {
            spawn_airframe(parent, assets, aircraft.index, fuselage, accent);
        });
}

/// One displayed airframe variant on a plane: the procedural placeholder is
/// `0`, the glTF models sit at their [`aces_protocol::AIRCRAFT`] index.
/// Every plane carries every variant; exactly one is visible, and
/// [`cycle_aircraft`] swaps between them.
#[derive(Component, Clone, Copy)]
struct AirframeModel(u8);

/// `P` switches the local plane to the next aircraft type, solo only: the
/// new airframe takes over the current flight (a new [`Life`], so the
/// flight log starts a new segment), with the new type's full hit points
/// and stores. Solo keeps the choice for the next flight. Over the network
/// the other peers only know the lobby's choice, so there it does nothing.
#[allow(clippy::type_complexity)]
fn cycle_aircraft(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<crate::NetworkMode>,
    assets: Res<PlaneAssets>,
    mut net: ResMut<NetState>,
    mut loadout: ResMut<crate::weapons::Loadout>,
    mut planes: Query<
        (
            &mut Aircraft,
            &mut Life,
            &mut crate::weapons::Health,
            &Children,
        ),
        With<LocalPlane>,
    >,
    mut models: Query<(&AirframeModel, &mut Visibility)>,
) {
    if !keys.just_pressed(KeyCode::KeyP) || *mode != crate::NetworkMode::Solo {
        return;
    }
    let Ok((mut aircraft, mut life, mut health, children)) = planes.single_mut() else {
        return;
    };
    if !health.alive() {
        return;
    }
    let next = (aircraft.index + 1) % aces_protocol::AIRCRAFT_COUNT;
    *aircraft = Aircraft::of_type(next);
    life.0 += 1;
    *health = crate::weapons::Health::full(&aircraft.combat);
    loadout.restock(&aircraft.combat);
    net.aircraft = next;
    show_model(children, assets.shown(next), &mut models);
}

/// Make exactly airframe variant `shown` visible among a plane's
/// `children`.
fn show_model(
    children: &Children,
    shown: u8,
    models: &mut Query<(&AirframeModel, &mut Visibility)>,
) {
    let mut iter = models.iter_many_mut(children);
    while let Some((model, mut visibility)) = iter.fetch_next() {
        visibility.set_if_neq(if model.0 == shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// Models load in the background. Until an aircraft's model is ready —
/// meshes and textures included — its planes show the placeholder airframe
/// instead of flying invisible (in the browser the models take a while);
/// once it is, they switch to it. A model that failed to load (its `.glb`
/// is missing, or needs a glTF extension Bevy lacks) is dropped for good.
fn settle_models(
    server: Res<AssetServer>,
    mut assets: ResMut<PlaneAssets>,
    planes: Query<(&Aircraft, &Children)>,
    mut models: Query<(&AirframeModel, &mut Visibility)>,
    mut settled: Local<bool>,
) {
    if *settled {
        return;
    }
    let (mut changed, mut pending) = (false, false);
    for (index, slot) in assets
        .bypass_change_detection()
        .scenes
        .iter_mut()
        .enumerate()
    {
        let Some(model) = slot.as_mut().filter(|model| !model.ready) else {
            continue;
        };
        let id = model.scene.id();
        if server.load_state(id).is_failed() {
            let name = aces_protocol::aircraft(index as u8).name;
            warn!("the {name} model did not load; showing the placeholder airframe");
            *slot = None;
            changed = true;
        } else if server.load_state(id).is_loaded()
            && !server.recursive_dependency_load_state(id).is_loading()
        {
            // Loaded — or a texture failed (logged), which shouldn't hide
            // the whole model.
            debug!(
                aircraft = aces_protocol::aircraft(index as u8).name,
                "model ready"
            );
            model.ready = true;
            changed = true;
        } else {
            pending = true;
        }
    }
    if changed {
        assets.set_changed();
        for (aircraft, children) in &planes {
            show_model(children, assets.shown(aircraft.index), &mut models);
        }
    }
    *settled = !pending;
}

/// Every displayed airframe variant (the procedural placeholder with its
/// hinged control surfaces plus one glTF model per real aircraft), tagged
/// by [`AirframeModel`]; exactly the plane's own `selected` one is visible.
/// The glTF models carry no hinges — their surfaces don't animate yet.
fn spawn_airframe(
    parent: &mut bevy::ecs::hierarchy::ChildSpawnerCommands,
    assets: &PlaneAssets,
    selected: u8,
    fuselage: Handle<StandardMaterial>,
    accent: Handle<StandardMaterial>,
) {
    let shown = assets.shown(selected);
    let visible = |index: u8| {
        if index == shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    };

    // ── Procedural placeholder (aircraft 0) ─────────────────────────────────
    let placeholder = || (AirframeModel(0), visible(0));

    // Fuselage: capsule lying along -Z/+Z.
    parent.spawn((
        placeholder(),
        Mesh3d(assets.fuselage.clone()),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_rotation(Quat::from_rotation_x(FRAC_PI_2)),
    ));
    // Main wings.
    parent.spawn((
        placeholder(),
        Mesh3d(assets.wing.clone()),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_xyz(0.0, 0.0, 0.5),
    ));
    // Horizontal stabilizer.
    parent.spawn((
        placeholder(),
        Mesh3d(assets.stabilizer.clone()),
        MeshMaterial3d(fuselage.clone()),
        Transform::from_xyz(0.0, 0.3, 6.2),
    ));
    // Vertical fin.
    parent.spawn((
        placeholder(),
        Mesh3d(assets.fin.clone()),
        MeshMaterial3d(accent),
        Transform::from_xyz(0.0, 1.4, 6.2),
    ));

    // Control surfaces: each is a hinge entity on the hinge line with the
    // panel offset behind it, so rotating the hinge swings the panel.
    let surface_material = &assets.surface_material;

    // Elevator: stabilizer trailing edge (stabilizer chord ends at z = 7.0).
    parent
        .spawn((
            placeholder(),
            ControlSurface::Elevator,
            Transform::from_xyz(0.0, 0.3, 6.8),
        ))
        .with_children(|hinge| {
            hinge.spawn((
                Mesh3d(assets.elevator.clone()),
                MeshMaterial3d(surface_material.clone()),
                Transform::from_xyz(0.0, 0.0, 0.3),
            ));
        });

    // Ailerons: outboard wing trailing edge (wing chord ends at z = 1.8).
    for (x, left) in [(-6.3, true), (6.3, false)] {
        parent
            .spawn((
                placeholder(),
                ControlSurface::Aileron { left },
                Transform::from_xyz(x, 0.0, 1.55),
            ))
            .with_children(|hinge| {
                hinge.spawn((
                    Mesh3d(assets.aileron.clone()),
                    MeshMaterial3d(surface_material.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.32),
                ));
            });
    }

    // Rudder: fin trailing edge (fin chord ends at z = 7.2).
    parent
        .spawn((
            placeholder(),
            ControlSurface::Rudder,
            Transform::from_xyz(0.0, 1.5, 6.9),
        ))
        .with_children(|hinge| {
            hinge.spawn((
                Mesh3d(assets.rudder.clone()),
                MeshMaterial3d(surface_material.clone()),
                Transform::from_xyz(0.0, 0.0, 0.45),
            ));
        });

    // ── glTF models ─────────────────────────────────────────────────────────
    for (index, model) in assets.scenes.iter().enumerate() {
        let Some(model) = model else { continue };
        let index = index as u8;
        parent.spawn((
            AirframeModel(index),
            // The rig pipeline (tools/rig) exports every model in airframe
            // space already: meters, nose -Z, centred.
            Transform::default(),
            WorldAssetRoot(model.scene.clone()),
            visible(index),
        ));
    }
}

/// Deflect every plane's control surfaces to the actuator positions in its
/// [`Surfaces`] — the same deflections the flight model is flying on.
///
/// Sign conventions (nose is -Z): pull → elevator trailing edge up; roll
/// right → right aileron up, left down; yaw right → rudder trailing edge
/// right.
fn animate_control_surfaces(
    planes: Query<(&Surfaces, &Children), Changed<Surfaces>>,
    mut hinges: Query<(&ControlSurface, &mut Transform)>,
) {
    for (s, children) in &planes {
        let mut iter = hinges.iter_many_mut(children);
        while let Some((surface, mut transform)) = iter.fetch_next() {
            let (axis, angle) = match surface {
                ControlSurface::Elevator => (Vec3::X, -s.elevator * ELEVATOR_DEFLECT),
                ControlSurface::Aileron { left: true } => (Vec3::X, s.aileron * AILERON_DEFLECT),
                ControlSurface::Aileron { left: false } => (Vec3::X, -s.aileron * AILERON_DEFLECT),
                ControlSurface::Rudder => (Vec3::Y, s.rudder * RUDDER_DEFLECT),
            };
            transform.rotation = Quat::from_axis_angle(axis, angle);
        }
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

    /// Aircraft whose model is missing — or still loading — show the
    /// placeholder, not nothing.
    #[test]
    fn missing_models_show_the_placeholder() {
        let model = |ready| {
            Some(PlaneModel {
                scene: default(),
                ready,
            })
        };
        let assets = PlaneAssets {
            fuselage: default(),
            wing: default(),
            stabilizer: default(),
            fin: default(),
            elevator: default(),
            aileron: default(),
            rudder: default(),
            surface_material: default(),
            local_fuselage: default(),
            local_accent: default(),
            scenes: vec![None, model(true), model(false), None],
        };
        assert_eq!(assets.shown(0), 0);
        assert_eq!(assets.shown(1), 1, "a loaded model is shown");
        assert_eq!(assets.shown(2), 0, "a loading model falls back meanwhile");
        assert_eq!(assets.shown(3), 0, "a missing model falls back");
        assert_eq!(assets.shown(9), 0, "an unknown aircraft falls back");
    }
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
                Life::default(),
                Aircraft::of_type(0),
                state,
                Instructor::default(),
                SimPose::new(state.pos, state.quat),
                Velocity(state.vel),
                Surfaces::default(),
                model::LastCommand::default(),
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
            .configure_sets(FixedUpdate, SpawnSet.before(model::step_flight))
            .init_resource::<RespawnNow>()
            .add_systems(
                FixedUpdate,
                (respawn_on_request.in_set(SpawnSet), model::step_flight),
            );

        let quat = Quat::from_rotation_y(0.7);
        let state = FlightState::new(Vec3::new(10.0, 2500.0, -40.0), quat, 170.0, 0.9);
        app.insert_resource(MouseAim::new(quat));
        app.world_mut()
            .spawn((Camera3d::default(), Transform::default()));
        app.world_mut().spawn((
            LocalPlane,
            Life::default(),
            Aircraft::of_type(0),
            state,
            Instructor::default(),
            SimPose::new(state.pos, state.quat),
            Velocity(state.vel),
            Surfaces::default(),
            model::LastCommand::default(),
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
                650 => app.world_mut().resource_mut::<RespawnNow>().0 = true,
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
        let ticks: usize = log.segments.iter().map(|s| s.ticks.len()).sum();
        assert_eq!(rows.count(), ticks);

        assert!(log.header.is_some());
        assert_eq!(log.skipped, 0);
        assert_eq!(log.marks.len(), 1, "the M mark is missing");
        // The respawn started a second segment.
        assert_eq!(
            log.segments.len(),
            2,
            "the respawn must start a new segment"
        );
        let frames: usize = log.segments.iter().map(|s| s.frames.len()).sum();
        assert!(ticks > 350, "only {ticks} ticks");
        assert!(frames >= 899, "only {frames} frames");
        let first = &log.segments[0];
        assert!(first.frames.iter().any(|f| f.keys.contains('V')));
        assert!(first.ticks.iter().any(|t| t.input.roll == Some(1.0)));
        assert!(first.ticks.iter().any(|t| t.input.limiter_off));

        for (i, segment) in log.segments.iter().enumerate() {
            for mouse in [false, true] {
                let (airframe, _) = replay::airframe_for(segment, false);
                let (samples, divergence) = replay::replay(segment, &airframe, mouse);
                assert_eq!(samples.len(), segment.ticks.len());
                assert!(
                    divergence.exact,
                    "segment {i} replay (mouse: {mouse}) diverged: {divergence:?}"
                );
            }
        }
        let report = replay::report(&log, false, false, None).unwrap();
        println!("{report}");
        assert_eq!(report.matches("replay: exact").count(), 2, "{report}");
        assert!(report.contains("mark at t ="), "{report}");
    }

    /// What [`respawn_on_request`] resets on the local plane.
    type RespawnParts = (
        &'static Aircraft,
        &'static mut Life,
        &'static mut FlightState,
        &'static mut Instructor,
        &'static mut SimPose,
        &'static mut Velocity,
        &'static mut Transform,
    );

    /// Test trigger for [`respawn_on_request`].
    #[derive(Resource, Default)]
    struct RespawnNow(bool);

    /// Die and come back, the way `weapons::respawn` does it: on the fixed
    /// tick in the spawn set, in place, with a new life, spawn state,
    /// controller and aim.
    fn respawn_on_request(
        mut now: ResMut<RespawnNow>,
        mut aim: ResMut<MouseAim>,
        plane: Single<RespawnParts, With<LocalPlane>>,
    ) {
        if !std::mem::take(&mut now.0) {
            return;
        }
        let (aircraft, mut life, mut state, mut instructor, mut pose, mut velocity, mut transform) =
            plane.into_inner();
        life.0 += 1;
        *state = spawn_state(3, &aircraft.airframe);
        *instructor = Instructor::default();
        *pose = SimPose::new(state.pos, state.quat);
        velocity.0 = state.vel;
        *transform = Transform::from_translation(state.pos).with_rotation(state.quat);
        *aim = MouseAim::new(state.quat);
    }
}
