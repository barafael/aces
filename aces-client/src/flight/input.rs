//! Pilot input: War Thunder arcade-style mouse aim.
//!
//! The mouse cursor is an *aim point*. Each frame the cursor is unprojected
//! into a world direction, expressed in the plane's frame, and the
//! [`instructor`] controller commands pitch/yaw/roll to fly the nose onto
//! it: it banks into off-axis targets and converts the remaining error into
//! pull, leveling the wings again once the aim is captured. The plane can
//! be flown purely with the mouse.
//!
//! `A`/`D` roll and `Q`/`E` rudder are added on top for fine control; `W`/`S`
//! set the throttle. Holding `V` suspends the instructor and turns the mouse
//! into a free-look orbit (the camera module springs it back on release).

use bevy::ecs::message::MessageReader;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::flight::Aircraft;

/// Free-look orbit angles [rad], offsets applied on top of the plane's
/// orientation. Decayed back to zero by the camera while inactive.
#[derive(Resource, Default)]
pub struct FreeLook {
    pub active: bool,
    pub yaw: f32,
    pub pitch: f32,
}

/// Mouse free-look sensitivity [rad per logical pixel].
const FREE_LOOK_SENSITIVITY: f32 = 0.004;

/// Pilot commands for the current frame. All axes in [-1, 1]; pitch/yaw/roll
/// are *rate* commands, `throttle_delta` integrates in the flight model.
#[derive(Resource, Default)]
pub struct FlightInput {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    pub throttle_delta: f32,
}

// ── Instructor tuning ───────────────────────────────────────────────────────

/// Errors smaller than this are treated as captured [rad].
const AIM_DEADZONE: f32 = 0.5f32.to_radians();
/// Roll command per rad of horizontal aim error.
const ROLL_GAIN: f32 = 2.2;
/// Pull command per rad of combined vertical error + banking pull.
const PULL_GAIN: f32 = 2.5;
/// Yaw command per rad of horizontal aim error (fine tracking).
const YAW_GAIN: f32 = 1.2;
/// Extra pull contribution from horizontal error while the bank develops.
const PULL_CAP: f32 = 30f32.to_radians();
/// Wing-leveling strength (gain on the sine of the bank angle).
const LEVEL_GAIN: f32 = 1.5;
/// Wing leveling clamps so it never fights deliberate maneuvers hard.
const LEVEL_CLAMP: f32 = 0.5;

/// Map an aim point in the plane's frame to rate commands.
///
/// `aim_local`: the desired nose direction in the plane's local frame
/// (forward = -Z, up = +Y, right = +X). `right_y`: the plane's right vector's
/// world-up component (the sine of the bank angle; negative = banked right).
///
/// Returns `(pitch, yaw, roll)` rate commands in [-1, 1]: positive pitch =
/// nose up, positive yaw = nose right, positive roll = right wing down.
pub fn instructor(aim_local: Vec3, right_y: f32) -> (f32, f32, f32) {
    let err = Vec3::NEG_Z.angle_between(aim_local);
    let psi = aim_local.x.atan2(-aim_local.z); // + = target right
    let theta = (aim_local.y / aim_local.length()).asin(); // + = target up

    if err <= AIM_DEADZONE {
        // Aim captured: hold attitude, gently level the wings.
        return (0.0, 0.0, (right_y * LEVEL_GAIN).clamp(-LEVEL_CLAMP, LEVEL_CLAMP));
    }

    // Bank into the target, then pull; while the bank develops, the target
    // drifts "up" in the plane's frame, so the loop self-corrects toward a
    // coordinated banked turn instead of a flat skid.
    let roll = (psi * ROLL_GAIN).clamp(-1.0, 1.0);
    let pull = theta + psi.abs().min(PULL_CAP);
    let pitch = (pull * PULL_GAIN).clamp(-1.0, 1.0);
    let yaw = (psi * YAW_GAIN).clamp(-1.0, 1.0);
    (pitch, yaw, roll)
}

#[allow(clippy::type_complexity)]
pub fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), (With<Camera3d>, Without<Aircraft>)>,
    plane: Single<&Transform, With<Aircraft>>,
    mut free_look: ResMut<FreeLook>,
    mut input: ResMut<FlightInput>,
) {
    free_look.active = keys.pressed(KeyCode::KeyV);
    if free_look.active {
        // The mouse orbits the camera; the plane holds its turn.
        input.pitch = 0.0;
        input.yaw = 0.0;
        input.roll = 0.0;
        for motion in mouse_motion.read() {
            free_look.yaw -= motion.delta.x * FREE_LOOK_SENSITIVITY;
            free_look.pitch -= motion.delta.y * FREE_LOOK_SENSITIVITY;
        }
        free_look.pitch = free_look.pitch.clamp(-1.4, 1.4);
        if free_look.yaw > core::f32::consts::PI {
            free_look.yaw -= core::f32::consts::TAU;
        } else if free_look.yaw < -core::f32::consts::PI {
            free_look.yaw += core::f32::consts::TAU;
        }
    } else {
        // Instructor: fly the nose onto the cursor.
        match window
            .cursor_position()
            .and_then(|cursor| camera.0.viewport_to_world(camera.1, cursor).ok())
        {
            Some(ray) => {
                let aim_local = plane.rotation.inverse() * *ray.direction;
                let right_y = (plane.rotation * Vec3::X).y;
                (input.pitch, input.yaw, input.roll) = instructor(aim_local, right_y);
            }
            None => {
                input.pitch = 0.0;
                input.yaw = 0.0;
                input.roll = 0.0;
            }
        }
        mouse_motion.clear();
    }

    // Manual roll and rudder are added on top of the instructor.
    input.roll = (input.roll
        + (keys.pressed(KeyCode::KeyD) as i32 - keys.pressed(KeyCode::KeyA) as i32) as f32)
        .clamp(-1.0, 1.0);
    let rudder = (keys.pressed(KeyCode::KeyE) as i32 - keys.pressed(KeyCode::KeyQ) as i32) as f32;
    input.yaw = (input.yaw + rudder).clamp(-1.0, 1.0);
    // W/S throttle.
    input.throttle_delta =
        (keys.pressed(KeyCode::KeyW) as i32 - keys.pressed(KeyCode::KeyS) as i32) as f32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Aim ~30° right and level: bank right, yaw right, pull into the turn.
    #[test]
    fn target_right_banks_and_pulls() {
        let aim = Quat::from_rotation_y(-30f32.to_radians()) * Vec3::NEG_Z;
        let (pitch, yaw, roll) = instructor(aim, 0.0);
        assert!(roll > 0.3, "must bank right, roll={roll}");
        assert!(yaw > 0.05, "must yaw right, yaw={yaw}");
        assert!(pitch > 0.05, "must pull, pitch={pitch}");
    }

    /// Aim straight up: pull up, no yaw, no bank (wings stay level).
    #[test]
    fn target_above_pulls() {
        let aim = Quat::from_rotation_x(30f32.to_radians()) * Vec3::NEG_Z;
        let (pitch, yaw, roll) = instructor(aim, 0.0);
        assert!(pitch > 0.5, "must pull hard, pitch={pitch}");
        assert!(yaw.abs() < 1e-4, "no yaw needed, yaw={yaw}");
        assert!(roll.abs() < 1e-4, "no bank needed, roll={roll}");
    }

    /// Aim nearly straight ahead while banked: capture, and level out.
    #[test]
    fn captured_aim_levels_wings() {
        let (pitch, yaw, roll) = instructor(Vec3::NEG_Z, -0.5);
        assert_eq!((pitch, yaw), (0.0, 0.0));
        // Banked right (right vector points down) → level by rolling left.
        assert!(roll < 0.0, "banked right must roll back left, roll={roll}");
    }

    /// A target behind the plane commands maximum bank and full pull.
    #[test]
    fn target_behind_commands_full_effort() {
        let aim = Vec3::Z; // directly behind
        let (pitch, _yaw, roll) = instructor(aim, 0.0);
        assert!((roll - 1.0).abs() < 1e-4, "full bank right, roll={roll}");
        assert!((pitch - 1.0).abs() < 1e-4, "full pull, pitch={pitch}");
    }

    /// Aim below the nose: push the nose down.
    #[test]
    fn target_below_pushes() {
        let aim = Quat::from_rotation_x(-20f32.to_radians()) * Vec3::NEG_Z;
        let (pitch, _yaw, _roll) = instructor(aim, 0.0);
        assert!(pitch < -0.2, "must push, pitch={pitch}");
    }
}
