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

use crate::flight::LocalPlane;

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
/// Lateral error that commands full bank. Banking is what converts a
/// horizontal error into a pull, so it leads the turn.
const BANK_BAND: f32 = 40f32.to_radians();
/// Vertical error that commands full pull/push.
const PITCH_BAND: f32 = 25f32.to_radians();
/// Vertical error that commands full rudder-yaw (fine tracking only; the
/// pitch axis is far stronger, which is why the instructor banks).
const YAW_BAND: f32 = 16f32.to_radians();
/// Weight of the direct rudder-yaw relative to a full band.
const YAW_AUTHORITY: f32 = 0.5;
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
///
/// WT-arcade shape: every axis is *proportional* to its own error, so
/// commands only saturate for genuinely large offsets. Banking into a
/// lateral error makes the target drift "up" in the plane's frame, which
/// grows the pull on its own — the stagger that turns bank+pull into a
/// coordinated turn. Commands are computed fresh each frame, so the loop
/// self-corrects all the way onto the cursor.
pub fn instructor(aim_local: Vec3, right_y: f32) -> (f32, f32, f32) {
    let err = Vec3::NEG_Z.angle_between(aim_local);
    let psi = aim_local.x.atan2(-aim_local.z); // + = target right
    let theta = (aim_local.y / aim_local.length()).asin(); // + = target up

    if err <= AIM_DEADZONE {
        // Aim captured: hold attitude, gently level the wings.
        return (
            0.0,
            0.0,
            (right_y * LEVEL_GAIN).clamp(-LEVEL_CLAMP, LEVEL_CLAMP),
        );
    }

    let roll = (psi / BANK_BAND).clamp(-1.0, 1.0);
    let pitch = (theta / PITCH_BAND).clamp(-1.0, 1.0);
    let yaw = YAW_AUTHORITY * (psi / YAW_BAND).clamp(-1.0, 1.0);
    (pitch, yaw, roll)
}

#[allow(clippy::type_complexity)]
pub fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), (With<Camera3d>, Without<LocalPlane>)>,
    plane: Single<&Transform, With<LocalPlane>>,
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
    use crate::flight::model::{step, FlightState, PilotInput};

    /// Aim ~30° right and level: bank right to set up the turn, nudge with
    /// rudder; the pull must NOT spike (it grows as the bank develops).
    #[test]
    fn target_right_banks() {
        let aim = Quat::from_rotation_y(-30f32.to_radians()) * Vec3::NEG_Z;
        let (pitch, yaw, roll) = instructor(aim, 0.0);
        assert!(roll > 0.3, "must bank right, roll={roll}");
        assert!(yaw > 0.05, "must yaw right, yaw={yaw}");
        assert!(
            pitch.abs() < 0.15,
            "lateral error alone must not yank the pitch, pitch={pitch}"
        );
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

    /// A target directly behind commands maximum bank and rudder; the pull
    /// comes from the bank, not from a vertical error that does not exist.
    #[test]
    fn target_behind_banks_hard() {
        let aim = Vec3::Z; // directly behind
        let (pitch, yaw, roll) = instructor(aim, 0.0);
        assert!((roll - 1.0).abs() < 1e-4, "full bank right, roll={roll}");
        assert!(yaw > 0.0, "rudder assists the swing, yaw={yaw}");
        assert!(pitch.abs() < 1e-4, "no vertical error, pitch={pitch}");
    }

    /// Aim below the nose: push the nose down.
    #[test]
    fn target_below_pushes() {
        let aim = Quat::from_rotation_x(-20f32.to_radians()) * Vec3::NEG_Z;
        let (pitch, _yaw, _roll) = instructor(aim, 0.0);
        assert!(pitch < -0.2, "must push, pitch={pitch}");
    }

    // ── Closed loop: instructor + flight model, like the real game ─────────
    // These are the tests that pin the WT behavior: the nose must actually
    // converge onto the cursor from any offset.

    const DT: f32 = 1.0 / 60.0;

    fn fly_onto(initial: Quat, target: Vec3, seconds: f32) -> Quat {
        let mut state = FlightState {
            quat: initial,
            vel: initial * Vec3::NEG_Z * 150.0,
            throttle: 1.0,
            ..default()
        };
        for _ in 0..(seconds / DT) as usize {
            let aim_local = state.quat.inverse() * target;
            let right_y = (state.quat * Vec3::X).y;
            let (pitch, yaw, roll) = instructor(aim_local, right_y);
            step(
                &mut state,
                &PilotInput {
                    pitch,
                    yaw,
                    roll,
                    throttle_delta: 0.0,
                },
                DT,
            );
        }
        state.quat
    }

    fn off(vertical: f32, horizontal: f32) -> Quat {
        // A world-space aim direction offset from the nose by `vertical`
        // (up positive) and `horizontal` (right positive) degrees.
        Quat::from_rotation_y(-horizontal.to_radians())
            * Quat::from_rotation_x(vertical.to_radians())
    }

    /// The user-facing contract: an offset cursor is reached — here 30°
    /// right, 10° up, via a banked turn.
    #[test]
    fn nose_converges_onto_offset_cursor() {
        let target = off(10.0, 30.0) * Vec3::NEG_Z;
        let quat = fly_onto(Quat::IDENTITY, target, 10.0);
        let err = (quat * Vec3::NEG_Z).angle_between(target).to_degrees();
        assert!(err < 3.0, "nose {err:.1}° off target after 10 s");
    }

    /// A cursor far above is reached by looping up.
    #[test]
    fn nose_converges_onto_cursor_above() {
        let target = off(60.0, 0.0) * Vec3::NEG_Z;
        let quat = fly_onto(Quat::IDENTITY, target, 12.0);
        let err = (quat * Vec3::NEG_Z).angle_between(target).to_degrees();
        assert!(err < 3.0, "nose {err:.1}° off target after 12 s");
    }

    /// A cursor almost behind is reached by turning around.
    #[test]
    fn nose_converges_onto_cursor_near_behind() {
        let target = off(0.0, 170.0) * Vec3::NEG_Z;
        let quat = fly_onto(Quat::IDENTITY, target, 20.0);
        let err = (quat * Vec3::NEG_Z).angle_between(target).to_degrees();
        assert!(err < 5.0, "nose {err:.1}° off target after 20 s");
    }

    /// After capture the instructor holds the nose on the cursor instead of
    /// oscillating around it.
    #[test]
    fn capture_is_stable_not_oscillatory() {
        let target = off(5.0, 5.0) * Vec3::NEG_Z;
        let quat = fly_onto(Quat::IDENTITY, target, 30.0);
        let err = (quat * Vec3::NEG_Z).angle_between(target).to_degrees();
        assert!(err < 1.0, "steady-state error {err:.1}° too large");
    }
}
