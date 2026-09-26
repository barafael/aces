//! aces — flight combat simulator client.
//!
//! Milestone 2: solo flight. Ocean, sky, one flyable aircraft with a
//! semi-realistic flight model, mouse virtual stick, chase cam with free
//! look (see PLAN.md). Weapons and networking arrive in later milestones.

mod flight;

use bevy::asset::{AssetMetaCheck, RenderAssetUsages};
use bevy::image::Image;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::WindowResolution;

/// Radius of the sky sphere. Must stay inside the camera's far plane; the
/// linear fog ends well before it, so the ocean never visibly clips.
const SKY_RADIUS: f32 = 30_000.0;
/// Edge length of the ocean plane, centered on the origin.
const OCEAN_SIZE: f32 = 120_000.0;
/// The fog color; matches the sky gradient's horizon so the ocean fades
/// seamlessly into the sky.
const HORIZON: Color = Color::srgb(0.75, 0.85, 0.92);

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "aces".into(),
                        resolution: WindowResolution::new(1600, 900),
                        resizable: true,
                        // Web: let the canvas grow with the browser viewport.
                        // Ignored natively; required so wasm fills the page.
                        canvas: Some("#game".into()),
                        fit_canvas_to_parent: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    // Web dev servers answer the optional `<asset>.meta`
                    // probe with the index page; parsing that as meta fails
                    // and rejects every asset (gray screen). Same as gnils.
                    meta_check: AssetMetaCheck::Never,
                    ..default()
                }),
        )
        // The fixed tick the flight model will run at (milestone 2).
        .insert_resource(Time::<Fixed>::from_hz(aces_protocol::TICK_HZ))
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 400.0,
            ..default()
        })
        .add_systems(Startup, setup_world)
        .add_plugins(flight::FlightPlugin)
        .run();
}

fn setup_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    // Sky: a huge inward-visible sphere with an unlit vertical gradient.
    // A procedural image keeps the build asset-free; a real cubemap skybox
    // can replace this later without touching anything else.
    let sky_texture = images.add(sky_gradient_image());
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(SKY_RADIUS).mesh().build())),
        MeshMaterial3d(materials.add(StandardMaterial {
            unlit: true,
            base_color_texture: Some(sky_texture),
            // Visible from inside the sphere.
            cull_mode: None,
            // The sky must not fog into itself; only world geometry fogs.
            fog_enabled: false,
            ..default()
        })),
    ));

    // Ocean: one big plane, y = 0. Storm-blue, slightly glossy.
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(OCEAN_SIZE, OCEAN_SIZE))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.03, 0.12, 0.20),
            perceptual_roughness: 0.25,
            metallic: 0.1,
            reflectance: 0.4,
            ..default()
        })),
    ));

    // Sun.
    commands.spawn((
        DirectionalLight {
            illuminance: 30_000.0,
            ..default()
        },
        Transform::from_xyz(0.4, 0.8, 0.25).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // Static stand-in camera at flight altitude, gazing at the horizon.
    // Milestone 2 replaces this with the chase/free-look rig.
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 70f32.to_radians(),
            near: 0.5,
            far: 80_000.0,
            ..default()
        }),
        Transform::from_xyz(0.0, 400.0, 1_200.0)
            .looking_at(Vec3::new(0.0, 300.0, -5_000.0), Vec3::Y),
        DistanceFog {
            color: HORIZON,
            falloff: FogFalloff::Linear {
                start: 4_000.0,
                end: 28_000.0,
            },
            ..default()
        },
    ));
}

/// A 1×256 sRGB vertical gradient for the sky sphere.
///
/// Bevy's uv sphere has v = 0 at the zenith and image row 0 is v = 0, so the
/// first row of the image is the color straight up and row 255 is straight
/// down. Below the horizon the gradient drifts to a hazy gray-blue (rarely
/// seen except inverted flight).
fn sky_gradient_image() -> Image {
    const HEIGHT: usize = 256;
    let zenith = [0.20_f32, 0.42, 0.75];
    let horizon = [0.75, 0.85, 0.92];
    let under = [0.45, 0.55, 0.62];

    let mut data = Vec::with_capacity(4 * HEIGHT);
    for row in 0..HEIGHT {
        let t = row as f32 / (HEIGHT - 1) as f32;
        let (a, b, f) = if t < 0.5 {
            (zenith, horizon, t / 0.5)
        } else {
            (horizon, under, (t - 0.5) / 0.5)
        };
        for c in 0..3 {
            data.push(((a[c] + (b[c] - a[c]) * f) * 255.0) as u8);
        }
        data.push(255);
    }

    Image::new(
        Extent3d {
            width: 1,
            height: HEIGHT as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    )
}
