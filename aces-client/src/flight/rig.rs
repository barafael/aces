//! Rigged airframes: the glTF models' moving parts.
//!
//! The rig pipeline (`tools/rig`, Blender) exports every model in airframe
//! space — meters, nose -Z, centred — with its moving parts as separate
//! nodes, each pivoted on its hinge and tagged in its glTF extras (`aces`, a
//! JSON string; the format is documented in `tools/rig/rigkit.py`):
//!
//! - control surfaces deflect with the plane's [`Surfaces`] (degrees per
//!   unit of elevator / aileron / rudder command),
//! - landing gear retracts with the plane's [`Gear`] (`G` toggles it),
//! - engine nozzles anchor the exhaust ([`Engine`], throttle-driven).
//!
//! Parts are found when a model instance spawns (its nodes' `GltfExtras`
//! appear), tied to the plane and the model variant they belong to, and
//! posed every frame while that variant is the one shown. Remote planes
//! pose from their snapshots' surfaces, gear and throttle.

use bevy::gltf::GltfExtras;
use bevy::prelude::*;
use core::f32::consts::FRAC_PI_2;
use serde::Deserialize;

use super::model::boost_fraction;
use super::{AirframeModel, FlightState, Life, LocalPlane, Surfaces};
use crate::Phase;

/// Seconds for a full gear extension or retraction.
pub const GEAR_CYCLE: f32 = 4.0;

pub struct RigPlugin;

impl Plugin for RigPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(equip_plane)
            .add_systems(Startup, init_exhaust_assets)
            .add_systems(
                Update,
                (
                    (gear_lever, raise_gear_on_respawn, local_engine)
                        .run_if(in_state(Phase::InGame)),
                    discover_parts,
                    (pose_parts, drive_exhaust).after(discover_parts),
                ),
            );
    }
}

// ── Plane state the rig reads ───────────────────────────────────────────────

/// A plane's landing gear: the lever, and the retraction progress the
/// parts are posed at (0 = down, as the models are built; 1 = up). Planes
/// spawn in the air, gear up.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Gear {
    /// The lever: extend (`true`) or retract.
    pub down: bool,
    /// 0 = fully extended … 1 = fully retracted.
    pub progress: f32,
}

impl Default for Gear {
    fn default() -> Self {
        Self {
            down: false,
            progress: 1.0,
        }
    }
}

impl Gear {
    /// Move the gear toward its lever for `dt` seconds.
    pub fn run(&mut self, dt: f32) {
        let target = if self.down { 0.0 } else { 1.0 };
        let step = dt / GEAR_CYCLE;
        self.progress = if self.progress < target {
            (self.progress + step).min(target)
        } else {
            (self.progress - step).max(target)
        };
    }
}

/// A plane's engines, as the exhaust shows them.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct Engine {
    /// 0..1 dry power, above 1 the boost range (see `model::THROTTLE_MAX`).
    pub throttle: f32,
}

/// Every plane (the ones carrying [`Surfaces`]) gets gear and engines.
fn equip_plane(add: On<Add, Surfaces>, mut commands: Commands) {
    commands
        .entity(add.entity)
        .insert((Gear::default(), Engine::default()));
}

/// `G` works the local plane's gear lever; the gear follows at
/// [`GEAR_CYCLE`].
fn gear_lever(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut planes: Query<&mut Gear, With<LocalPlane>>,
) {
    for mut gear in &mut planes {
        if keys.just_pressed(KeyCode::KeyG) {
            gear.down = !gear.down;
        }
        let mut next = *gear;
        next.run(time.delta_secs());
        gear.set_if_neq(next);
    }
}

/// A respawned plane appears in the air: gear up.
fn raise_gear_on_respawn(mut planes: Query<&mut Gear, (With<LocalPlane>, Changed<Life>)>) {
    for mut gear in &mut planes {
        gear.set_if_neq(Gear::default());
    }
}

fn local_engine(mut planes: Query<(&FlightState, &mut Engine), With<LocalPlane>>) {
    for (state, mut engine) in &mut planes {
        engine.set_if_neq(Engine {
            throttle: state.throttle,
        });
    }
}

// ── Part specs ──────────────────────────────────────────────────────────────

/// A node's glTF extras, as the rig pipeline writes them.
#[derive(Deserialize)]
struct Extras {
    aces: String,
}

/// One tagged node (see the module docs and `tools/rig/rigkit.py`).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct PartSpec {
    pub role: String,
    /// Hinge axis, airframe space (the node's parent frame at rest).
    #[serde(default)]
    pub axis: [f32; 3],
    /// Degrees per unit elevator / aileron / rudder command.
    #[serde(default)]
    pub mix: [f32; 3],
    /// `[t0, t1, degrees]`: rotation reached (smoothly) while the gear
    /// retracts from progress `t0` to `t1`; segments add up.
    #[serde(default)]
    pub gear: Vec<[f32; 3]>,
    /// Hidden from this gear progress on.
    pub hide: Option<f32>,
    /// Nozzle exit radius [m] (nozzles only).
    pub radius: Option<f32>,
}

impl PartSpec {
    /// The spec in a node's extras JSON, if it carries one.
    pub fn parse(extras: &str) -> Option<Self> {
        let extras: Extras = serde_json::from_str(extras).ok()?;
        serde_json::from_str(&extras.aces)
            .inspect_err(|err| warn!("bad rig spec {:?}: {err}", extras.aces))
            .ok()
    }

    /// The rotation angle [rad] for these commands and gear progress.
    pub fn angle(&self, surfaces: &Surfaces, gear: f32) -> f32 {
        let [pitch, roll, yaw] = self.mix;
        let mut degrees =
            pitch * surfaces.elevator + roll * surfaces.aileron + yaw * surfaces.rudder;
        for &[t0, t1, d] in &self.gear {
            let u = ((gear - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
            degrees += d * u * u * (3.0 - 2.0 * u);
        }
        degrees.to_radians()
    }

    /// Whether the part is stowed out of sight at this gear progress.
    pub fn hidden(&self, gear: f32) -> bool {
        self.hide.is_some_and(|from| gear >= from)
    }
}

/// A moving part of a model instance.
#[derive(Component, Debug)]
pub struct RigPart {
    spec: PartSpec,
    axis: Vec3,
    /// The node's rest rotation (identity from the pipeline).
    rest: Quat,
    /// The plane it belongs to.
    plane: Entity,
    /// The model variant (an [`AirframeModel`] entity) it belongs to.
    model: Entity,
}

/// An engine nozzle exit of a model instance: the exhaust's anchor.
#[derive(Component, Debug)]
pub struct Nozzle {
    pub radius: f32,
    pub plane: Entity,
}

/// Tag the nodes of freshly spawned model instances.
fn discover_parts(
    mut commands: Commands,
    new: Query<(Entity, &GltfExtras, &Transform), Added<GltfExtras>>,
    parents: Query<&ChildOf>,
    planes: Query<(), With<Surfaces>>,
    models: Query<(), With<AirframeModel>>,
    exhaust: Res<ExhaustAssets>,
) {
    for (entity, extras, transform) in &new {
        let Some(spec) = PartSpec::parse(&extras.value) else {
            continue;
        };
        let (mut model, mut plane) = (None, None);
        for ancestor in parents.iter_ancestors(entity) {
            if model.is_none() && models.contains(ancestor) {
                model = Some(ancestor);
            }
            if planes.contains(ancestor) {
                plane = Some(ancestor);
                break;
            }
        }
        let (Some(model), Some(plane)) = (model, plane) else {
            continue;
        };
        if spec.role == "nozzle" {
            let radius = spec.radius.unwrap_or(0.4);
            commands
                .entity(entity)
                .insert(Nozzle { radius, plane })
                .with_children(|nozzle| spawn_flames(nozzle, &exhaust));
            continue;
        }
        let axis = Vec3::from(spec.axis).try_normalize().unwrap_or(Vec3::X);
        commands.entity(entity).insert(RigPart {
            spec,
            axis,
            rest: transform.rotation,
            plane,
            model,
        });
    }
}

/// Pose every part of every shown model from its plane's commands and gear.
fn pose_parts(
    planes: Query<(&Surfaces, &Gear)>,
    models: Query<&Visibility, (With<AirframeModel>, Without<RigPart>)>,
    mut parts: Query<(&RigPart, &mut Transform, &mut Visibility), Without<AirframeModel>>,
) {
    for (part, mut transform, mut visibility) in &mut parts {
        if models
            .get(part.model)
            .is_ok_and(|v| *v == Visibility::Hidden)
        {
            continue;
        }
        let Ok((surfaces, gear)) = planes.get(part.plane) else {
            continue;
        };
        let angle = part.spec.angle(surfaces, gear.progress);
        let rotation = Quat::from_axis_angle(part.axis, angle) * part.rest;
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        visibility.set_if_neq(if part.spec.hidden(gear.progress) {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        });
    }
}

// ── Exhaust ─────────────────────────────────────────────────────────────────
//
// A first, simple exhaust so the nozzle anchors show: a short hot core at
// any throttle and a long afterburner plume in the boost range, both
// additive cones scaled by the throttle. The great-looking version (shock
// diamonds, heat haze, light) builds on the same anchors and [`Engine`].

/// Shared exhaust meshes and materials.
#[derive(Resource)]
struct ExhaustAssets {
    cone: Handle<Mesh>,
    core: Handle<StandardMaterial>,
    plume: Handle<StandardMaterial>,
}

/// One exhaust layer, a child of its [`Nozzle`].
#[derive(Component, Debug, Clone, Copy)]
struct Flame {
    afterburner: bool,
}

fn init_exhaust_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut glow = |color: LinearRgba| {
        // Unlit draws the base colour (HDR values glow under bloom);
        // additive, so overlapping layers brighten instead of occluding.
        materials.add(StandardMaterial {
            base_color: Color::LinearRgba(color),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        })
    };
    let core = glow(LinearRgba::rgb(1.6, 1.3, 2.4));
    let plume = glow(LinearRgba::rgb(3.0, 1.2, 0.35));
    commands.insert_resource(ExhaustAssets {
        cone: meshes.add(Cone::new(1.0, 1.0)),
        core,
        plume,
    });
}

fn spawn_flames(nozzle: &mut bevy::ecs::hierarchy::ChildSpawnerCommands, assets: &ExhaustAssets) {
    for afterburner in [false, true] {
        nozzle.spawn((
            Flame { afterburner },
            Mesh3d(assets.cone.clone()),
            MeshMaterial3d(if afterburner {
                assets.plume.clone()
            } else {
                assets.core.clone()
            }),
            Transform::default(),
            Visibility::Hidden,
        ));
    }
}

/// Length of each layer (in nozzle radii) at `throttle`; `None`: off.
fn flame_length(throttle: f32, afterburner: bool) -> Option<f32> {
    if afterburner {
        let boost = boost_fraction(throttle);
        (boost > 0.02).then_some(2.5 + 6.0 * boost)
    } else {
        (throttle > 0.05).then_some(0.4 + 1.2 * throttle.min(1.0))
    }
}

/// Size the flames by their plane's throttle: a cone from the nozzle exit
/// aft (+Z), its base the exit.
fn drive_exhaust(
    planes: Query<&Engine>,
    nozzles: Query<&Nozzle>,
    mut flames: Query<(&Flame, &ChildOf, &mut Transform, &mut Visibility)>,
) {
    for (flame, child_of, mut transform, mut visibility) in &mut flames {
        let Ok(nozzle) = nozzles.get(child_of.parent()) else {
            continue;
        };
        let throttle = planes.get(nozzle.plane).map_or(0.0, |e| e.throttle);
        let Some(length) = flame_length(throttle, flame.afterburner) else {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        };
        let (r, l) = (nozzle.radius * 0.85, nozzle.radius * length);
        // The cone's tip points along +Y; turn it to +Z (aft).
        let next = Transform {
            translation: Vec3::new(0.0, 0.0, l / 2.0),
            rotation: Quat::from_rotation_x(FRAC_PI_2),
            scale: Vec3::new(r, l, r),
        };
        if *transform != next {
            *transform = next;
        }
        visibility.set_if_neq(Visibility::Inherited);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> PartSpec {
        let extras = serde_json::json!({ "aces": json }).to_string();
        PartSpec::parse(&extras).expect("parses")
    }

    /// The extras the pipeline writes parse; foreign extras are ignored.
    #[test]
    fn specs_parse_from_extras() {
        let aileron = spec(r#"{"role": "aileron", "axis": [1, 0, 0], "mix": [0, -20, 0]}"#);
        assert_eq!(aileron.role, "aileron");
        assert_eq!(aileron.mix, [0.0, -20.0, 0.0]);
        assert!(aileron.gear.is_empty() && aileron.hide.is_none());
        let nozzle = spec(r#"{"role": "nozzle", "radius": 0.3}"#);
        assert_eq!(nozzle.radius, Some(0.3));
        assert!(PartSpec::parse(r#"{"something": "else"}"#).is_none());
        assert!(PartSpec::parse("not json").is_none());
    }

    /// Commands mix linearly; gear segments ease in over their window and
    /// hold; hiding starts at its progress.
    #[test]
    fn angles_follow_commands_and_gear() {
        let part = spec(
            r#"{"role": "x", "axis": [1, 0, 0], "mix": [-25, 10, 0],
                "gear": [[0.2, 0.6, 90]], "hide": 0.9}"#,
        );
        let s = |elevator, aileron| Surfaces {
            elevator,
            aileron,
            rudder: 0.0,
        };
        let deg = |a: f32| a.to_degrees();
        assert!((deg(part.angle(&s(1.0, 0.0), 0.0)) + 25.0).abs() < 1e-4);
        assert!((deg(part.angle(&s(0.0, -1.0), 0.0)) + 10.0).abs() < 1e-4);
        assert!(deg(part.angle(&s(0.0, 0.0), 0.2)).abs() < 1e-4);
        assert!((deg(part.angle(&s(0.0, 0.0), 0.4)) - 45.0).abs() < 1e-3);
        assert!((deg(part.angle(&s(0.0, 0.0), 1.0)) - 90.0).abs() < 1e-4);
        assert!(!part.hidden(0.89) && part.hidden(0.9));
    }

    /// The gear runs toward its lever at GEAR_CYCLE and stops there.
    #[test]
    fn gear_runs_to_the_lever() {
        let mut gear = Gear::default();
        assert_eq!(gear.progress, 1.0, "spawns up");
        gear.down = true;
        gear.run(GEAR_CYCLE / 2.0);
        assert!((gear.progress - 0.5).abs() < 1e-6);
        gear.run(GEAR_CYCLE);
        assert_eq!(gear.progress, 0.0);
        gear.down = false;
        gear.run(GEAR_CYCLE * 2.0);
        assert_eq!(gear.progress, 1.0);
    }

    /// No flame at idle; the plume only in the boost range.
    #[test]
    fn flames_follow_the_throttle() {
        assert_eq!(flame_length(0.0, false), None);
        assert!(flame_length(0.8, false).is_some());
        assert_eq!(flame_length(1.0, true), None);
        assert!(flame_length(1.1, true).is_some());
    }
}
