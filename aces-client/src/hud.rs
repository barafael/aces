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
//!   angle of attack, the limiter state, the gear while not stowed, the
//!   flight recorder and a stall warning.
//! - **Weapons** (top right): health, the selected weapon, missiles and
//!   countermeasures left, both locks.
//! - **Threat line** (top center): the RWR — inbound missiles and radar
//!   locks, naming the countermeasure that defeats them — or the respawn
//!   countdown while shot down; the kill feed below it.
//! - **Scoreboard** (hold `Tab`): every pilot's kills and deaths.
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
use crate::weapons::{
    Countermeasures, Health, KillFeed, Loadout, Lock, Locks, Rwr, Scoreboard, WeaponSlot,
};

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

/// One column of the scoreboard: a text per column, so the proportional
/// font still lines up.
#[derive(Component, Clone, Copy)]
enum ScoreColumn {
    Pilot,
    Kills,
    Deaths,
}

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

            // RWR threat line and the kill feed below it, top center. An
            // absolute node without horizontal insets would sit at the left
            // edge: this one spans the width and centers its children.
            hud.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                ..default()
            })
            .with_children(|center| {
                center.spawn((
                    ThreatWarning,
                    // Keeps its line while blank, so the feed never jumps.
                    Node {
                        min_height: Val::Px(30.0),
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
                center.spawn((
                    KillFeedText,
                    Text::default(),
                    TextFont {
                        font: font.clone().into(),
                        font_size: FontSize::Px(17.0),
                        ..default()
                    },
                    TextLayout::justify(Justify::Center),
                    TextColor(Color::srgba(0.95, 0.95, 0.98, 0.9)),
                ));
            });

            // Scoreboard overlay, held on Tab, centered.
            hud.spawn((
                ScoreboardOverlay,
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(24.0),
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                Visibility::Hidden,
            ))
            .with_children(|row| {
                row.spawn((
                    Node {
                        padding: UiRect::axes(Val::Px(18.0), Val::Px(12.0)),
                        column_gap: Val::Px(28.0),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.75)),
                ))
                .with_children(|panel| {
                    for (column, justify) in [
                        (ScoreColumn::Pilot, Justify::Left),
                        (ScoreColumn::Kills, Justify::Right),
                        (ScoreColumn::Deaths, Justify::Right),
                    ] {
                        panel.spawn((
                            column,
                            Text::default(),
                            TextFont {
                                font: font.clone().into(),
                                font_size: FontSize::Px(19.0),
                                ..default()
                            },
                            TextLayout::justify(justify),
                            TextColor(Color::srgb(0.9, 0.95, 1.0)),
                        ));
                    }
                });
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
    plane: Single<(&FlightState, &Aircraft, &crate::flight::rig::Gear), With<LocalPlane>>,
    input: Res<FlightInput>,
    #[cfg(not(target_arch = "wasm32"))] recorder: Res<crate::flight::recorder::FlightRecorder>,
    mut info: Single<&mut Text, With<FlightInfo>>,
    mut stall: Single<&mut Visibility, With<StallWarning>>,
    mut buffer: Local<String>,
) {
    let (s, aircraft, gear) = *plane;
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
    if gear.progress < 1.0 {
        buffer.push_str(match (gear.down, gear.progress <= 0.0) {
            (_, true) => "\nGEAR DOWN",
            (true, false) => "\nGEAR EXTENDING",
            (false, false) => "\nGEAR RETRACTING",
        });
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

/// Health, weapon selection, stores and locks; the threat line — the RWR
/// picture, or the respawn countdown while shot down.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_weapons_hud(
    time: Res<Time>,
    slot: Res<WeaponSlot>,
    loadout: Res<Loadout>,
    countermeasures: Res<Countermeasures>,
    locks: Res<Locks>,
    rwr: Res<Rwr>,
    health: Single<&Health, With<LocalPlane>>,
    mut weapons: Single<&mut Text, With<WeaponsInfo>>,
    mut threat: Single<(&mut Text, &mut Visibility), (With<ThreatWarning>, Without<WeaponsInfo>)>,
    mut buffer: Local<String>,
) {
    let slot_label = match *slot {
        WeaponSlot::Gun => "GUN",
        WeaponSlot::IrMissile => "IR",
        WeaponSlot::RadarMissile => "RADAR",
    };
    buffer.clear();
    let _ = write!(
        buffer,
        "HP {:.0}\n[{slot_label}]  SPACE gun\nIR {}  RADAR {}\nFLARES {}  CHAFF {}\n",
        health.hp.ceil(),
        loadout.ir_missiles,
        loadout.radar_missiles,
        countermeasures.flares,
        countermeasures.chaff,
    );
    write_lock(&mut buffer, "IR", &locks.ir);
    buffer.push_str("  ");
    write_lock(&mut buffer, "RDR", &locks.radar);
    set_text(&mut weapons, &buffer);

    // The respawn countdown while shot down (steady); otherwise the RWR,
    // missiles before locks, blinking while active.
    let now = time.elapsed_secs_f64();
    let (line, blinks) = match loadout.respawn_in {
        Some(remaining) => {
            buffer.clear();
            let _ = write!(buffer, "SHOT DOWN — back in {:.0} s", remaining.ceil());
            (Some(buffer.as_str()), false)
        }
        None => {
            let warning = match (rwr.ir_inbound, rwr.radar_inbound) {
                (true, true) => Some("MISSILES — FLARES (F) + CHAFF (C)"),
                (true, false) => Some("MISSILE — FLARES (F)"),
                (false, true) => Some("MISSILE — CHAFF (C)"),
                (false, false) => rwr
                    .radar_locked_until
                    .filter(|until| now <= *until)
                    .map(|_| "RADAR LOCK — CHAFF (C)"),
            };
            (warning, true)
        }
    };
    let (text, visibility) = &mut *threat;
    set_text(text, line.unwrap_or(""));
    let lit = !blinks || ((now * 3.0) as u64).is_multiple_of(2);
    // `Inherited`, never `Visible`: a `Visible` child would outlive the
    // HUD root being hidden (and stay on the menu).
    visibility.set_if_neq(if line.is_some() && lit {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
}

/// "IR LOCKED", "IR  45%" or "IR —".
fn write_lock(buffer: &mut String, name: &str, lock: &Lock) {
    let _ = if lock.progress >= 1.0 {
        write!(buffer, "{name} LOCKED")
    } else if lock.progress > 0.0 {
        write!(buffer, "{name} {:>3.0}%", lock.progress * 100.0)
    } else {
        write!(buffer, "{name} —")
    };
}

/// Kill feed (recent kills) and the Tab scoreboard.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_score_hud(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    net: Res<NetState>,
    scoreboard: Res<Scoreboard>,
    kill_feed: Res<KillFeed>,
    mut feed: Single<&mut Text, (With<KillFeedText>, Without<ScoreColumn>)>,
    mut overlay: Single<&mut Visibility, With<ScoreboardOverlay>>,
    mut columns: Query<(&ScoreColumn, &mut Text), Without<KillFeedText>>,
    mut buffer: Local<String>,
) {
    buffer.clear();
    for line in kill_feed.fresh(time.elapsed_secs_f64()) {
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);
    }
    set_text(&mut feed, &buffer);

    let held = keys.pressed(KeyCode::Tab);
    overlay.set_if_neq(if held {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
    // The board only changes on kills: rebuild it when it opens or changes.
    if !held || !(keys.just_pressed(KeyCode::Tab) || scoreboard.is_changed()) {
        return;
    }
    let rows = scoreboard.rows(&net);
    for (column, mut text) in &mut columns {
        buffer.clear();
        buffer.push_str(match column {
            ScoreColumn::Pilot => "PILOT",
            ScoreColumn::Kills => "K",
            ScoreColumn::Deaths => "D",
        });
        for (name, score) in &rows {
            buffer.push('\n');
            let _ = match column {
                ScoreColumn::Pilot => write!(buffer, "{}", truncate(name, 16)),
                ScoreColumn::Kills => write!(buffer, "{}", score.kills),
                ScoreColumn::Deaths => write!(buffer, "{}", score.deaths),
            };
        }
        set_text(&mut text, &buffer);
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
