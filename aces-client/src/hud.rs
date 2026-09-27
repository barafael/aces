//! Flight HUD, War Thunder arcade style.
//!
//! - **Aim circle**: where the mouse aim points — the instructor flies the
//!   nose there. Sits near the screen center, since the camera looks along
//!   the aim.
//! - **Nose cross**: where the plane (and later its guns) actually points.
//! - **Flight-path marker**: where the plane is actually *going*. The gap
//!   between nose cross and flight-path marker is the angle of attack and
//!   sideslip — watch it open up in hard pulls and stalls.
//! - **Flight info** (top left): the aircraft, speed, altitude, throttle
//!   (with the airframe's boost: AB or WEP), load factor,
//!   angle of attack, the limiter state, the flight recorder and a stall
//!   warning.
//!
//! Markers are small procedural textures on absolutely positioned UI nodes,
//! projected with the camera transform the chase cam wrote this frame.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::Phase;
use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::model::{FlightState, THROTTLE_MAX};
use crate::flight::{Aircraft, LocalPlane, camera};

/// Marker size in logical pixels.
const MARKER_SIZE: f32 = 28.0;
/// Distance along each marker's ray where it is projected [m] — far enough
/// that the camera's offset from the plane hardly shifts it.
const MARKER_DISTANCE: f32 = 1500.0;

#[derive(Component)]
struct HudRoot;

/// A projected HUD marker.
#[derive(Component, Clone, Copy)]
enum Marker {
    Aim,
    Nose,
    Path,
}

#[derive(Component)]
struct FlightInfo;

#[derive(Component)]
struct StallWarning;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud).add_systems(
            Update,
            (
                show_hud,
                (update_markers, update_info)
                    .after(camera::update_camera)
                    .run_if(in_state(Phase::InGame)),
            ),
        );
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            Update,
            show_recording
                .after(update_info)
                .run_if(in_state(Phase::InGame)),
        );
    }
}

/// Rasterize a `SIZE`×`SIZE` white-on-transparent marker from a coverage
/// function of the pixel center's offset from the image center.
fn marker_texture(coverage: impl Fn(f32, f32) -> f32) -> Image {
    const SIZE: usize = 32;
    let mut data = Vec::with_capacity(4 * SIZE * SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - SIZE as f32 / 2.0;
            let dy = y as f32 + 0.5 - SIZE as f32 / 2.0;
            let a = coverage(dx, dy).clamp(0.0, 1.0);
            data.extend_from_slice(&[255, 255, 255, (a * 255.0) as u8]);
        }
    }
    Image::new(
        Extent3d {
            width: SIZE as u32,
            height: SIZE as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    )
}

/// Soft-edged band of half-width `half` around distance `d == r`.
fn band(d: f32, r: f32, half: f32) -> f32 {
    (half + 0.75 - (d - r).abs()) / 1.5
}

fn ring(dx: f32, dy: f32) -> f32 {
    band((dx * dx + dy * dy).sqrt(), 11.0, 1.1)
}

/// A "+" with an open center.
fn cross(dx: f32, dy: f32) -> f32 {
    let (ax, ay) = (dx.abs(), dy.abs());
    let arm = |along: f32, across: f32| {
        if (4.0..=13.0).contains(&along) {
            band(across, 0.0, 1.1)
        } else {
            0.0
        }
    };
    arm(ax, ay).max(arm(ay, ax))
}

/// The classic flight-path marker: a small circle with wings and a fin.
fn path_symbol(dx: f32, dy: f32) -> f32 {
    let circle = band((dx * dx + dy * dy).sqrt(), 5.5, 1.0);
    let wings = if (6.5..=13.0).contains(&dx.abs()) {
        band(dy, 0.0, 0.9)
    } else {
        0.0
    };
    let fin = if (-12.0..=-6.5).contains(&dy) {
        band(dx, 0.0, 0.9)
    } else {
        0.0
    };
    circle.max(wings).max(fin)
}

fn marker_node(image: Handle<Image>, color: Color) -> (Node, ImageNode, Visibility) {
    (
        Node {
            position_type: PositionType::Absolute,
            width: Val::Px(MARKER_SIZE),
            height: Val::Px(MARKER_SIZE),
            ..default()
        },
        ImageNode {
            color,
            image,
            ..default()
        },
        Visibility::Hidden,
    )
}

fn spawn_hud(mut commands: Commands, mut images: ResMut<Assets<Image>>, assets: Res<AssetServer>) {
    let ring = images.add(marker_texture(ring));
    let cross = images.add(marker_texture(cross));
    let path = images.add(marker_texture(path_symbol));
    let font: Handle<Font> = assets.load("fonts/FreeSansBold.ttf");

    commands
        .spawn((
            HudRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                position_type: PositionType::Absolute,
                ..default()
            },
            Visibility::Hidden,
        ))
        .with_children(|hud| {
            hud.spawn((
                Marker::Aim,
                marker_node(ring, Color::srgba(1.0, 1.0, 1.0, 0.85)),
            ));
            hud.spawn((
                Marker::Nose,
                marker_node(cross, Color::srgba(1.0, 0.6, 0.15, 0.95)),
            ));
            hud.spawn((
                Marker::Path,
                marker_node(path, Color::srgba(0.35, 1.0, 0.45, 0.9)),
            ));

            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(16.0),
                    top: Val::Px(12.0),
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                    flex_direction: FlexDirection::Column,
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.45)),
            ))
            .with_children(|info| {
                info.spawn((
                    FlightInfo,
                    Text::default(),
                    TextFont {
                        font: font.clone().into(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 1.0, 0.88)),
                ));
                info.spawn((
                    StallWarning,
                    Text::new("STALL"),
                    TextFont {
                        font: font.into(),
                        font_size: FontSize::Px(22.0),
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.3, 0.2)),
                    Visibility::Hidden,
                ));
            });
        });
}

/// The HUD only exists in flight.
fn show_hud(phase: Res<State<Phase>>, mut root: Single<&mut Visibility, With<HudRoot>>) {
    root.set_if_neq(if phase.get() == &Phase::InGame {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
}

#[allow(clippy::type_complexity)]
fn update_markers(
    camera: Single<(&Camera, &Transform), (With<Camera3d>, Without<LocalPlane>)>,
    plane: Single<(&Transform, &FlightState), With<LocalPlane>>,
    aim: Res<MouseAim>,
    mut markers: Query<(&Marker, &mut Node, &mut Visibility)>,
) {
    // The camera moved this frame, after transform propagation last ran:
    // project with its fresh local transform (it has no parent).
    let (camera, camera_transform) = *camera;
    let camera_global = GlobalTransform::from(*camera_transform);
    let (transform, state) = *plane;

    for (marker, mut node, mut visibility) in &mut markers {
        let direction = match marker {
            Marker::Aim => Some(aim.dir()),
            Marker::Nose => Some(transform.forward().into()),
            Marker::Path => state.vel.try_normalize().filter(|_| state.speed() > 5.0),
        };
        let screen = direction
            .filter(|dir| dir.dot(camera_transform.forward().into()) > 0.0)
            .and_then(|dir| {
                let point = transform.translation + dir * MARKER_DISTANCE;
                camera.world_to_viewport(&camera_global, point).ok()
            });
        match screen {
            Some(screen) => {
                node.left = Val::Px(screen.x - MARKER_SIZE / 2.0);
                node.top = Val::Px(screen.y - MARKER_SIZE / 2.0);
                visibility.set_if_neq(Visibility::Inherited);
            }
            None => {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

/// Tell the pilot the flight is being recorded, and how to mark a moment.
#[cfg(not(target_arch = "wasm32"))]
fn show_recording(
    recorder: Res<crate::flight::recorder::FlightRecorder>,
    mut info: Single<&mut Text, With<FlightInfo>>,
) {
    if recorder.path().is_some() {
        info.0.push_str("\nREC · M marks");
    }
}

fn update_info(
    plane: Single<(&FlightState, &Aircraft), With<LocalPlane>>,
    input: Res<FlightInput>,
    mut info: Single<&mut Text, With<FlightInfo>>,
    mut stall: Single<&mut Visibility, With<StallWarning>>,
) {
    let (s, aircraft) = *plane;
    let throttle = if s.throttle > 1.0 + 1e-3 {
        let boost = (s.throttle - 1.0) / (THROTTLE_MAX - 1.0);
        format!(
            "{} {:>3.0} %",
            aircraft.airframe.boost_label(),
            boost * 100.0
        )
    } else {
        format!("{:>3.0} %", s.throttle * 100.0)
    };
    info.0 = format!(
        "{}\nSPD {:>5.0} km/h\nALT {:>5.0} m\nTHR {throttle}\nG {:>5.1}   AoA {:>4.1}°",
        aircraft.name(),
        s.speed() * 3.6,
        s.pos.y,
        s.g_load,
        s.alpha.to_degrees(),
    );
    if input.limiter_off {
        info.0.push_str("\nLIMITER OFF");
    }
    stall.set_if_neq(if s.stalled {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
}
