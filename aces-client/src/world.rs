//! World scenery: ocean, sky, sun, and the camera that views it.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// Radius of the sky sphere. Must stay inside the camera's far plane; the
/// linear fog ends well before it, so the ocean never visibly clips.
const SKY_RADIUS: f32 = 30_000.0;
/// Edge length of the ocean plane, centered on the origin.
const OCEAN_SIZE: f32 = 120_000.0;
/// The fog color; matches the sky gradient's horizon so the ocean fades
/// seamlessly into the sky.
const HORIZON: Color = Color::srgb(0.75, 0.85, 0.92);
/// One grid cell on the ocean = one kilometer.
const GRID_CELL: f32 = 1000.0;

pub fn setup_world(
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

    // Ocean: one big plane, y = 0, with a 1 km grid for spatial reference.
    // The grid texture tiles GRID_CELL-sized squares across the whole plane;
    // the material color tints it into the sea palette (white stays sea).
    let grid_texture = images.add(grid_image());
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(OCEAN_SIZE, OCEAN_SIZE))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.03, 0.12, 0.20),
            base_color_texture: Some(grid_texture),
            uv_transform: Affine2::from_scale(Vec2::splat(OCEAN_SIZE / GRID_CELL)),
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

    // Camera rig spawn point; the flight chase cam drives it every frame.
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

/// One 128×128 grid cell (= [`GRID_CELL`]) as an sRGB image, with a full
/// mipmap chain so the lines stay calm at grazing angles and distance.
///
/// White = open water (the material color passes through unchanged); the
/// border lines are gray, which darkens the tinted sea color. Center lines
/// mark the half-kilometer, fainter.
fn grid_image() -> Image {
    const SIZE: usize = 128;
    const LINE: u8 = 140; // border line brightness
    const HALF_LINE: u8 = 190; // 500 m line brightness
    const MID: usize = SIZE / 2;

    let mut top = vec![0u8; 4 * SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let b = if x < 2 || y < 2 {
                LINE
            } else if x == MID || y == MID {
                HALF_LINE
            } else {
                255
            };
            let i = 4 * (y * SIZE + x);
            top[i..i + 3].fill(b);
            top[i + 3] = 255;
        }
    }

    // Build the mipmap chain with a 2×2 box filter: without it the grid
    // shimmers into moiré at distance. Level 0 goes into Image::new (which
    // validates the base size), the smaller levels are appended after.
    let mut mips = vec![top];
    let (mut level, mut w, mut h) = (1u32, SIZE, SIZE);
    while w > 1 || h > 1 {
        mips.push(downsample(mips.last().expect("level 0 exists"), w, h));
        w = (w / 2).max(1);
        h = (h / 2).max(1);
        level += 1;
    }

    let mut image = Image::new(
        Extent3d {
            width: SIZE as u32,
            height: SIZE as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        mips.remove(0),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    );
    for mip in mips {
        if let Some(data) = image.data.as_mut() {
            data.extend_from_slice(&mip);
        }
    }
    image.texture_descriptor.mip_level_count = level;
    // The grid must tile across the ocean, not clamp at the texture edge.
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        ..default()
    });
    image
}

/// Halve an RGBA8 image with a plain 2×2 average. `w`/`h` are the current
/// dimensions; sizes below 2 repeat their last row/column.
fn downsample(pixels: &[u8], w: usize, h: usize) -> Vec<u8> {
    let nw = (w / 2).max(1);
    let nh = (h / 2).max(1);
    let mut out = vec![0u8; 4 * nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let sum: u32 = [
                    (2 * y, 2 * x),
                    (2 * y, (2 * x + 1).min(w - 1)),
                    ((2 * y + 1).min(h - 1), 2 * x),
                    ((2 * y + 1).min(h - 1), (2 * x + 1).min(w - 1)),
                ]
                .iter()
                .map(|&(sy, sx)| pixels[4 * (sy * w + sx) + c] as u32)
                .sum();
                out[4 * (y * nw + x) + c] = (sum / 4) as u8;
            }
        }
    }
    out
}
