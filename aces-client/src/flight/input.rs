//! Pilot input: mouse = virtual stick (instructor style), WASD-QE keys,
//! `V` free look.
//!
//! While `V` is held the mouse stops steering and its motion deltas orbit
//! the camera instead; on release the camera springs back (see `camera`).

use bevy::ecs::message::MessageReader;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

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

pub fn gather_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut free_look: ResMut<FreeLook>,
    mut input: ResMut<FlightInput>,
) {
    free_look.active = keys.pressed(KeyCode::KeyV);
    if free_look.active {
        // The mouse orbits the camera; the plane holds its turn.
        input.pitch = 0.0;
        input.yaw = 0.0;
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
        // Steering: cursor offset from the window center is the stick
        // deflection. Cursor above center → nose up, right → nose right.
        match window.cursor_position() {
            Some(cursor) => {
                let center = Vec2::new(window.width() * 0.5, window.height() * 0.5);
                let offset = (cursor - center) / center;
                input.pitch = (-offset.y).clamp(-1.0, 1.0);
                input.yaw = offset.x.clamp(-1.0, 1.0);
            }
            None => {
                input.pitch = 0.0;
                input.yaw = 0.0;
            }
        }
        mouse_motion.clear();
    }

    // A/D roll: D = right wing down.
    input.roll = (keys.pressed(KeyCode::KeyD) as i32 - keys.pressed(KeyCode::KeyA) as i32) as f32;
    // Q/E rudder, on top of the mouse yaw: E = nose right.
    let rudder = (keys.pressed(KeyCode::KeyE) as i32 - keys.pressed(KeyCode::KeyQ) as i32) as f32;
    input.yaw = (input.yaw + rudder).clamp(-1.0, 1.0);
    // W/S throttle.
    input.throttle_delta =
        (keys.pressed(KeyCode::KeyW) as i32 - keys.pressed(KeyCode::KeyS) as i32) as f32;
}
