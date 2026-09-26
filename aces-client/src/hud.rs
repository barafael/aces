//! Minimal aiming HUD: the mouse cursor ring (the aim point) and the nose
//! marker (where the plane is actually pointing).
//!
//! The instructor flies the nose marker onto the cursor ring — seeing both
//! is what makes mouse aim legible. Both are small ring textures rendered as
//! absolutely positioned UI nodes; the OS cursor is hidden while flying.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::{CursorOptions, PrimaryWindow};

use crate::flight::Aircraft;

/// Marker size in logical pixels.
const MARKER_SIZE: f32 = 26.0;
/// Where the nose marker is sampled along the nose ray [m].
const NOSE_MARKER_DISTANCE: f32 = 10_000.0;

#[derive(Component)]
struct CursorMarker;

#[derive(Component)]
struct NoseMarker;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_markers)
            .add_systems(Update, (hide_os_cursor, update_markers));
    }
}

/// A 24×24 white ring with a soft 2 px edge; tinted per marker.
fn ring_texture() -> Image {
    const SIZE: usize = 24;
    const INNER: f32 = 8.5;
    const OUTER: f32 = 10.5;

    let mut data = Vec::with_capacity(4 * SIZE * SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - SIZE as f32 / 2.0;
            let dy = y as f32 + 0.5 - SIZE as f32 / 2.0;
            let d = (dx * dx + dy * dy).sqrt();
            let a = ((OUTER - d) / (OUTER - INNER)).clamp(0.0, 1.0)
                * ((d - INNER + 1.5) / 1.5).clamp(0.0, 1.0);
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

fn spawn_markers(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let ring = images.add(ring_texture());

    // The aim point (replaces the OS cursor): soft green ring.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(-2.0 * MARKER_SIZE),
            top: Val::Px(-2.0 * MARKER_SIZE),
            width: Val::Px(MARKER_SIZE),
            height: Val::Px(MARKER_SIZE),
            ..default()
        },
        ImageNode {
            color: Color::srgba(0.35, 1.0, 0.45, 0.9),
            image: ring.clone(),
            ..default()
        },
        CursorMarker,
    ));

    // Where the nose points: orange ring the instructor flies onto the
    // cursor ring.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(-2.0 * MARKER_SIZE),
            top: Val::Px(-2.0 * MARKER_SIZE),
            width: Val::Px(MARKER_SIZE),
            height: Val::Px(MARKER_SIZE),
            ..default()
        },
        ImageNode {
            color: Color::srgba(1.0, 0.55, 0.15, 0.9),
            image: ring,
            ..default()
        },
        NoseMarker,
    ));
}

/// Hide the OS cursor on the first frame (the window exists by then for
/// sure); the cursor ring takes over.
fn hide_os_cursor(mut cursor: Single<&mut CursorOptions>, mut done: Local<bool>) {
    if !*done {
        cursor.visible = false;
        *done = true;
    }
}

#[allow(clippy::type_complexity)]
fn update_markers(
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), (With<Camera3d>, Without<Aircraft>)>,
    plane: Single<&Transform, With<Aircraft>>,
    mut cursor_marker: Single<(&mut Node, &mut Visibility), (With<CursorMarker>, Without<NoseMarker>)>,
    mut nose_marker: Single<(&mut Node, &mut Visibility), (With<NoseMarker>, Without<CursorMarker>)>,
) {
    let half = MARKER_SIZE / 2.0;

    // Cursor ring follows the (hidden) OS cursor.
    match window.cursor_position() {
        Some(pos) => {
            cursor_marker.0.left = Val::Px(pos.x - half);
            cursor_marker.0.top = Val::Px(pos.y - half);
            *cursor_marker.1 = Visibility::Visible;
        }
        None => *cursor_marker.1 = Visibility::Hidden,
    }

    // Nose marker at the projection of the nose ray.
    let nose_point = plane.translation + plane.forward() * NOSE_MARKER_DISTANCE;
    match camera.0.world_to_viewport(camera.1, nose_point) {
        Ok(screen) if screen.x >= 0.0 && screen.y >= 0.0 => {
            nose_marker.0.left = Val::Px(screen.x - half);
            nose_marker.0.top = Val::Px(screen.y - half);
            *nose_marker.1 = Visibility::Visible;
        }
        _ => *nose_marker.1 = Visibility::Hidden,
    }
}
