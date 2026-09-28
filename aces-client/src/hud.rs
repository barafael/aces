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
use bevy::time::common_conditions::on_timer;
use bevy::ui::Val2;
use core::fmt::Write as _;
use core::time::Duration;

use aces_net::NetState;

use crate::Phase;
use crate::flight::input::{FlightInput, MouseAim};
use crate::flight::model::{FlightState, boost_fraction};
use crate::flight::{Aircraft, LocalPlane, camera};

/// Marker size in logical pixels.
const MARKER_SIZE: f32 = 28.0;
/// Flight info refresh rate [Hz]: faster than a pilot reads, slower than
/// re-shaping the text every frame.
const INFO_HZ: f32 = 15.0;
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

#[derive(Component)]
struct WeaponsInfo;

#[derive(Component)]
struct ThreatWarning;

#[derive(Component)]
struct KillFeedText;

#[derive(Component)]
struct ScoreboardOverlay;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud).add_systems(
            Update,
            (
                show_hud,
                (
                    update_markers,
                    update_info.run_if(on_timer(Duration::from_secs_f32(1.0 / INFO_HZ))),
                    update_weapons_hud,
                    update_score_hud,
                )
                    .after(camera::update_camera)
                    .run_if(in_state(Phase::InGame)),
            ),
        );
    }
}

/// Replace a text's content only when it differs: every write re-shapes the
/// text and re-lays out the UI.
pub fn set_text(text: &mut Mut<Text>, value: &str) {
    if text.0 != value {
        value.clone_into(&mut text.0);
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

/// A marker node, placed at the top left and moved by its [`UiTransform`]
/// (which, unlike `Node` offsets, does not re-run the layout each frame).
fn marker_node(image: Handle<Image>, color: Color) -> (Node, UiTransform, ImageNode, Visibility) {
    (
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Px(MARKER_SIZE),
            height: Val::Px(MARKER_SIZE),
            ..default()
        },
        UiTransform::default(),
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
    let font: Handle<Font> = assets.load(crate::menu::UI_FONT);

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
                        font: font.clone().into(),
                        font_size: FontSize::Px(22.0),
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.3, 0.2)),
                    Visibility::Hidden,
                ));
            });

            // Weapons & countermeasures, top right.
            hud.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(16.0),
                    top: Val::Px(12.0),
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.45)),
            ))
            .with_children(|panel| {
                panel.spawn((
                    WeaponsInfo,
                    Text::default(),
                    TextFont {
                        font: font.clone().into(),
                        font_size: FontSize::Px(18.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.85, 0.92, 1.0)),
                ));
            });

            // RWR threat line, top center.
            hud.spawn((
                ThreatWarning,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(12.0),
                    ..default()
                },
                Text::default(),
                TextFont {
                    font: font.clone().into(),
                    font_size: FontSize::Px(24.0),
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.25, 0.15)),
                Visibility::Hidden,
            ));

            // Kill feed, under the threat line.
            hud.spawn((
                KillFeedText,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(46.0),
                    ..default()
                },
                Text::default(),
                TextFont {
                    font: font.clone().into(),
                    font_size: FontSize::Px(17.0),
                    ..default()
                },
                TextColor(Color::srgba(0.95, 0.95, 0.98, 0.9)),
            ));

            // Scoreboard overlay, held on Tab.
            hud.spawn((
                ScoreboardOverlay,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(24.0),
                    padding: UiRect::axes(Val::Px(18.0), Val::Px(12.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.75)),
                Text::default(),
                TextFont {
                    font: font.into(),
                    font_size: FontSize::Px(19.0),
                    ..default()
                },
                TextColor(Color::srgb(0.9, 0.95, 1.0)),
                Visibility::Hidden,
            ));
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
    mut markers: Query<(&Marker, &mut UiTransform, &mut Visibility)>,
) {
    // The camera moved this frame, after transform propagation last ran:
    // project with its fresh local transform (it has no parent).
    let (camera, camera_transform) = *camera;
    let camera_global = GlobalTransform::from(*camera_transform);
    let (transform, state) = *plane;

    for (marker, mut ui_transform, mut visibility) in &mut markers {
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
                let corner = screen - MARKER_SIZE / 2.0;
                let translation = Val2::px(corner.x, corner.y);
                if ui_transform.translation != translation {
                    ui_transform.translation = translation;
                }
                visibility.set_if_neq(Visibility::Inherited);
            }
            None => {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

fn update_info(
    plane: Single<(&FlightState, &Aircraft), With<LocalPlane>>,
    input: Res<FlightInput>,
    #[cfg(not(target_arch = "wasm32"))] recorder: Res<crate::flight::recorder::FlightRecorder>,
    mut info: Single<&mut Text, With<FlightInfo>>,
    mut stall: Single<&mut Visibility, With<StallWarning>>,
    mut buffer: Local<String>,
) {
    let (s, aircraft) = *plane;
    buffer.clear();
    let _ = write!(
        buffer,
        "{}\nSPD {:>5.0} km/h\nALT {:>5.0} m\nTHR ",
        aircraft.name(),
        s.speed() * 3.6,
        s.pos.y,
    );
    let _ = if s.throttle > 1.0 + 1e-3 {
        write!(
            buffer,
            "{} {:>3.0} %",
            aircraft.airframe.boost_label(),
            boost_fraction(s.throttle) * 100.0
        )
    } else {
        write!(buffer, "{:>3.0} %", s.throttle * 100.0)
    };
    let _ = write!(
        buffer,
        "\nG {:>5.1}   AoA {:>4.1}°",
        s.g_load,
        s.alpha.to_degrees()
    );
    if input.limiter_off {
        buffer.push_str("\nLIMITER OFF");
    }
    #[cfg(not(target_arch = "wasm32"))]
    if recorder.path().is_some() {
        buffer.push_str("\nREC · M marks");
    }
    set_text(&mut info, &buffer);
    stall.set_if_neq(if s.stalled {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
}

/// Weapon selection, stores, lock status and the RWR picture.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_weapons_hud(
    time: Res<Time>,
    slot: Res<crate::weapons::WeaponSlot>,
    loadout: Res<crate::weapons::Loadout>,
    countermeasures: Res<crate::weapons::Countermeasures>,
    ir: Res<crate::weapons::IrLock>,
    radar: Res<crate::weapons::RadarLock>,
    rwr: Res<crate::weapons::Rwr>,
    mut weapons: Single<&mut Text, With<WeaponsInfo>>,
    mut threat: Single<
        (&mut Text, &mut Visibility),
        (With<ThreatWarning>, Without<WeaponsInfo>),
    >,
    mut buffer: Local<String>,
) {
    let slot_label = match *slot {
        crate::weapons::WeaponSlot::Gun => "GUN",
        crate::weapons::WeaponSlot::IrMissile => "IR",
        crate::weapons::WeaponSlot::RadarMissile => "RADAR",
    };
    let lock_label = |progress: f32, name: &str| {
        if progress >= 1.0 {
            format!("{name} LOCKED")
        } else if progress > 0.0 {
            format!("{name} {:>3.0}%", progress * 100.0)
        } else {
            format!("{name} —")
        }
    };
    buffer.clear();
    let _ = write!(
        buffer,
        "[{slot_label}]  SPACE gun\nIR {}  RADAR {}\nFLARES {}  CHAFF {}\n{}  {}",
        loadout.ir_missiles,
        loadout.radar_missiles,
        countermeasures.flares,
        countermeasures.chaff,
        lock_label(ir.progress, "IR"),
        lock_label(radar.progress, "RDR"),
    );
    set_text(&mut weapons, &buffer);

    // RWR: newest threat wins; flash while active.
    let now = time.elapsed_secs_f64();
    let missile = rwr
        .missile
        .filter(|(until, _)| now <= *until)
        .map(|(_, kind)| match kind {
            crate::weapons::MissileKind::Ir => "MISSILE — FLARES (F)",
            crate::weapons::MissileKind::Radar => "MISSILE — CHAFF (C)",
        });
    let locked = rwr
        .radar_locked_until
        .filter(|until| now <= *until)
        .map(|_| "RADAR LOCK — CHAFF (C)");
    let threat_text = missile.or(locked);
    match threat_text {
        Some(text) => {
            set_text(&mut threat.0, text);
            let blink = ((now * 3.0) as u64).is_multiple_of(2);
            *threat.1 = if blink {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
        None => {
            set_text(&mut threat.0, "");
            *threat.1 = Visibility::Hidden;
        }
    }
}

/// Kill feed (recent kills) and the Tab scoreboard.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_score_hud(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    scoreboard: Res<crate::weapons::Scoreboard>,
    kill_feed: Res<crate::weapons::KillFeed>,
    mut feed: Single<&mut Text, With<KillFeedText>>,
    mut board: Single<
        (&mut Text, &mut Visibility),
        (With<ScoreboardOverlay>, Without<KillFeedText>),
    >,
    mut buffer: Local<String>,
) {
    let now = time.elapsed_secs_f64();

    buffer.clear();
    for line in kill_feed.fresh(now) {
        buffer.push_str(line);
        buffer.push('\n');
    }
    set_text(&mut feed, &buffer);

    if keys.pressed(KeyCode::Tab) {
        let mut rows: Vec<(&String, crate::weapons::Score)> = scoreboard
            .entries
            .iter()
            .map(|(peer, score)| (peer, *score))
            .collect();
        rows.sort_by(|a, b| {
            b.1.kills
                .cmp(&a.1.kills)
                .then(b.1.deaths.cmp(&a.1.deaths))
                .then(a.0.cmp(b.0))
        });
        buffer.clear();
        buffer.push_str("PILOT        K    D\n");
        for (peer, score) in rows {
            let name = crate::weapons::callsign(&net, peer);
            buffer.push_str(&format!(
                "{:<12} {:<4} {}\n",
                truncate(&name, 12),
                score.kills,
                score.deaths
            ));
        }
        set_text(&mut board.0, &buffer);
        *board.1 = Visibility::Visible;
    } else {
        *board.1 = Visibility::Hidden;
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let cut: String = value.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}
