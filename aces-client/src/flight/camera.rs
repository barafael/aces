//! War Thunder–style third-person camera.
//!
//! The camera does not ride on the plane's attitude: it looks along the
//! mouse aim ([`MouseAim`]) and orbits the plane at a fixed offset in its
//! own frame. The plane therefore always sits at the same spot on screen,
//! slightly below the aim circle, and you watch it bank, slip and pull
//! toward where you are looking. While free look is active its orbit
//! offsets come from [`FreeLook`]; when released they spring back to zero.

use bevy::prelude::*;

use crate::flight::LocalPlane;
use crate::flight::input::{FreeLook, MouseAim};

/// Camera position relative to the plane in the camera's frame (forward is
/// -Z, so +Z is behind; +Y lifts the plane below the screen center).
pub const CAM_OFFSET: Vec3 = Vec3::new(0.0, 4.5, 26.0);
/// How fast the camera orientation follows the aim [1/s] — high, so the
/// view feels attached to the mouse, but above zero to hide mouse jitter.
pub const CAM_SMOOTHING: f32 = 18.0;
/// Spring-back rate of the free-look offsets after release [1/s].
pub const FREE_LOOK_RETURN: f32 = 8.0;

#[cfg(not(target_arch = "wasm32"))]
/// Camera tuning constants, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    tuning_table![
        CAM_OFFSET_UP,
        CAM_OFFSET_BACK,
        CAM_SMOOTHING,
        FREE_LOOK_RETURN
    ]
}

#[cfg(not(target_arch = "wasm32"))]
const CAM_OFFSET_UP: f32 = CAM_OFFSET.y;
#[cfg(not(target_arch = "wasm32"))]
const CAM_OFFSET_BACK: f32 = CAM_OFFSET.z;

pub fn update_camera(
    time: Res<Time>,
    mut free_look: ResMut<FreeLook>,
    aim: Res<MouseAim>,
    plane: Single<(Entity, &Transform), With<LocalPlane>>,
    mut camera: Single<&mut Transform, (With<Camera3d>, Without<LocalPlane>)>,
    mut smoothed: Local<Option<(Entity, Quat)>>,
) {
    let dt = time.delta_secs();
    let (plane_entity, plane) = *plane;

    // Spring the orbit back behind the plane when the pilot releases V.
    if !free_look.active {
        free_look.yaw.smooth_nudge(&0.0, FREE_LOOK_RETURN, dt);
        free_look.pitch.smooth_nudge(&0.0, FREE_LOOK_RETURN, dt);
    }

    let target = aim.rot
        * Quat::from_axis_angle(Vec3::Y, free_look.yaw)
        * Quat::from_axis_angle(Vec3::X, free_look.pitch);

    // A new plane (first frame, respawn, new game): snap, don't swoop.
    let rot = match *smoothed {
        Some((entity, mut rot)) if entity == plane_entity => {
            rot.smooth_nudge(&target, CAM_SMOOTHING, dt);
            rot
        }
        _ => target,
    };
    *smoothed = Some((plane_entity, rot));

    camera.translation = plane.translation + rot * CAM_OFFSET;
    camera.rotation = rot;
}
