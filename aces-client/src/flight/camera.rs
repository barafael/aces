//! Chase camera with free look.
//!
//! The camera trails the plane by smoothly rotating toward it (never by
//! smoothing position, which would let fast rolls clip through the airframe).
//! While free look is active its orbit offsets come straight from
//! [`FreeLook`]; when released they spring back to zero.

use bevy::prelude::*;

use crate::flight::LocalPlane;
use crate::flight::input::FreeLook;

/// Camera position relative to the plane (forward is -Z, so +Z is behind).
pub const CAM_OFFSET: Vec3 = Vec3::new(0.0, 4.0, 24.0);
/// Chase smoothing rate [1/s] — how fast the camera orientation catches up
/// with the plane's.
pub const CAM_SMOOTHING: f32 = 5.0;
/// Spring-back rate of the free-look offsets after release [1/s].
pub const FREE_LOOK_RETURN: f32 = 8.0;

pub fn update_camera(
    time: Res<Time>,
    mut free_look: ResMut<FreeLook>,
    plane: Single<&Transform, With<LocalPlane>>,
    mut camera: Single<&mut Transform, (With<Camera3d>, Without<LocalPlane>)>,
    mut smoothed: Local<Option<Quat>>,
) {
    let dt = time.delta_secs();

    // Spring the orbit back behind the plane when the pilot releases V.
    if !free_look.active {
        let decay = (FREE_LOOK_RETURN * dt).min(1.0);
        free_look.yaw *= 1.0 - decay;
        free_look.pitch *= 1.0 - decay;
    }

    let target = plane.rotation
        * Quat::from_axis_angle(Vec3::Y, free_look.yaw)
        * Quat::from_axis_angle(Vec3::X, free_look.pitch);

    // First frame: snap, don't swoop in from identity.
    let smoothed = smoothed.get_or_insert(plane.rotation);
    *smoothed = smoothed.slerp(target, (CAM_SMOOTHING * dt).min(1.0));

    camera.translation = plane.translation + *smoothed * CAM_OFFSET;
    camera.rotation = *smoothed;
}
