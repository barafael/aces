//! Pilot input, War Thunder "mouse aim" style.
//!
//! The mouse does not steer the plane — it steers the *view*. Mouse motion
//! rotates [`MouseAim`], a world-space aim direction the camera looks along;
//! the aim circle sits in the middle of the screen and the instructor flies
//! the nose onto it. Leave the mouse alone and the aim stays fixed in the
//! world, so the plane settles onto a steady heading.
//!
//! Horizontal mouse motion turns the aim about the world vertical and
//! vertical motion about the view's own horizontal axis, so the horizon
//! stays level. Keep pulling the mouse and the aim goes over the top (a
//! loop); pause while inverted and the view rolls upright again, so the
//! plane finishes an Immelmann instead.
//!
//! Keys override single axes of the instructor: `A`/`D` ailerons, `Q`/`E`
//! rudder. `W`/`S` move the throttle (past 100 % into afterburner). Holding
//! `Shift` switches the instructor's envelope protection off, so the plane
//! can be pulled into a stall. Holding `V` freezes the aim and lets the
//! mouse orbit the camera instead (the camera springs back on release).

use bevy::ecs::message::MessageReader;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::flight::model::smoothstep;

/// Mouse sensitivity [rad per mouse count].
pub const AIM_SENSITIVITY: f32 = 0.0022;
/// How fast an inverted or tilted view rolls back upright [1/s].
const LEVEL_RATE: f32 = 3.0;
/// Seconds without vertical mouse motion before the view starts leveling
/// (full strength at `.1`), so a deliberate loop is never fought.
const LEVEL_DELAY: (f32, f32) = (0.25, 0.6);

#[cfg(not(target_arch = "wasm32"))]
/// Mouse-aim tuning constants, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    let mut table = tuning_table![AIM_SENSITIVITY, LEVEL_RATE];
    table.push(("LEVEL_DELAY.0", LEVEL_DELAY.0));
    table.push(("LEVEL_DELAY.1", LEVEL_DELAY.1));
    table
}

/// The mouse-aim direction (and the camera's orientation): forward (-Z) is
/// where the pilot wants the nose, up (+Y) the camera's up.
#[derive(Resource, Clone, Copy, Debug)]
pub struct MouseAim {
    pub rot: Quat,
    /// Time since the last vertical mouse motion [s].
    since_pitch: f32,
}

impl Default for MouseAim {
    fn default() -> Self {
        Self::new(Quat::IDENTITY)
    }
}

impl MouseAim {
    /// Aim along `rot`'s forward, with the view leveled.
    pub fn new(rot: Quat) -> Self {
        let forward = rot * Vec3::NEG_Z;
        Self {
            rot: Transform::default().looking_to(forward, Vec3::Y).rotation,
            since_pitch: 0.0,
        }
    }

    /// Unit world-space aim direction.
    pub fn dir(&self) -> Vec3 {
        self.rot * Vec3::NEG_Z
    }

    /// Turn the aim by `delta` radians (x = right, y = down, like screen
    /// coordinates), then level the view if the pilot is not pitching.
    pub fn turn(&mut self, delta: Vec2, dt: f32) {
        let forward = self.rot * Vec3::NEG_Z;
        let up = self.rot * Vec3::Y;
        // Yaw about the world vertical (flipped while inverted, so the view
        // still turns toward the mouse), except near straight up/down where
        // only the view's own up axis makes sense.
        let yaw_axis = if forward.y.abs() < 0.95 {
            Vec3::Y * up.y.signum()
        } else {
            up
        };
        self.rot = Quat::from_axis_angle(yaw_axis, -delta.x) * self.rot;
        let right = self.rot * Vec3::X;
        self.rot = (Quat::from_axis_angle(right, -delta.y) * self.rot).normalize();

        self.since_pitch = if delta.y != 0.0 {
            0.0
        } else {
            self.since_pitch + dt
        };
        self.level(dt);
    }

    /// Roll the view toward upright about its forward axis.
    fn level(&mut self, dt: f32) {
        let forward = self.rot * Vec3::NEG_Z;
        let up = self.rot * Vec3::Y;
        let Some(horizon_up) = (Vec3::Y - forward * forward.y).try_normalize() else {
            return;
        };
        let angle = forward.dot(up.cross(horizon_up)).atan2(up.dot(horizon_up));
        let weight = smoothstep(LEVEL_DELAY.0, LEVEL_DELAY.1, self.since_pitch)
            * (1.0 - smoothstep(0.8, 0.95, forward.y.abs()));
        let k = (1.0 - (-LEVEL_RATE * dt).exp()) * weight;
        self.rot = (Quat::from_axis_angle(forward, angle * k) * self.rot).normalize();
    }
}

/// Free-look orbit angles [rad], offsets applied on top of the aim
/// orientation. Decayed back to zero by the camera while inactive.
#[derive(Resource, Default)]
pub struct FreeLook {
    pub active: bool,
    pub yaw: f32,
    pub pitch: f32,
}

/// Pilot commands for the flight model's fixed tick.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub struct FlightInput {
    /// World-space aim direction the instructor flies the nose onto.
    pub aim: Vec3,
    /// Keyboard aileron override (replaces the instructor's roll).
    pub roll_override: Option<f32>,
    /// Keyboard rudder override (replaces the instructor's yaw).
    pub yaw_override: Option<f32>,
    /// Throttle lever: -1, 0 or 1.
    pub throttle_delta: f32,
    /// The pilot switched the instructor's envelope protection off (AoA and
    /// G limits, ground avoidance).
    pub limiter_off: bool,
}

/// -1, 0 or 1 from a pair of keys.
fn axis(keys: &ButtonInput<KeyCode>, positive: KeyCode, negative: KeyCode) -> f32 {
    keys.pressed(positive) as i32 as f32 - keys.pressed(negative) as i32 as f32
}

pub fn gather_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    mut aim: ResMut<MouseAim>,
    mut free_look: ResMut<FreeLook>,
    mut input: ResMut<FlightInput>,
) {
    let delta: Vec2 = mouse_motion.read().map(|m| m.delta).sum::<Vec2>() * AIM_SENSITIVITY;

    free_look.active = keys.pressed(KeyCode::KeyV);
    if free_look.active {
        // The mouse orbits the camera; the aim (and so the plane) holds.
        free_look.yaw -= delta.x;
        free_look.pitch = (free_look.pitch - delta.y).clamp(-1.4, 1.4);
        free_look.yaw = crate::flight::model::wrap_angle(free_look.yaw);
        aim.turn(Vec2::ZERO, time.delta_secs());
    } else {
        aim.turn(delta, time.delta_secs());
    }
    input.aim = aim.dir();

    let roll = axis(&keys, KeyCode::KeyD, KeyCode::KeyA);
    input.roll_override = (roll != 0.0).then_some(roll);
    let yaw = axis(&keys, KeyCode::KeyE, KeyCode::KeyQ);
    input.yaw_override = (yaw != 0.0).then_some(yaw);
    input.throttle_delta = axis(&keys, KeyCode::KeyW, KeyCode::KeyS);
    input.limiter_off = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn mouse_right_turns_the_aim_right() {
        let mut aim = MouseAim::default();
        aim.turn(Vec2::new(0.2, 0.0), DT);
        assert!(aim.dir().x > 0.15, "{:?}", aim.dir());
        assert!(aim.dir().y.abs() < 1e-5);
    }

    #[test]
    fn mouse_up_raises_the_aim() {
        let mut aim = MouseAim::default();
        aim.turn(Vec2::new(0.0, -0.2), DT);
        assert!(aim.dir().y > 0.15, "{:?}", aim.dir());
    }

    /// Diagonal motion must not tilt the horizon: the view's right axis
    /// stays horizontal.
    #[test]
    fn horizon_stays_level() {
        let mut aim = MouseAim::default();
        for _ in 0..200 {
            aim.turn(Vec2::new(0.01, -0.004), DT);
        }
        let right = aim.rot * Vec3::X;
        assert!(right.y.abs() < 1e-3, "horizon tilted: right={right:?}");
    }

    /// Continuous upward motion loops the aim all the way around instead of
    /// getting stuck at the vertical.
    #[test]
    fn continuous_pull_loops_over_the_top() {
        let mut aim = MouseAim::default();
        let mut went_inverted = false;
        let steps = (core::f32::consts::TAU / 0.02).round() as usize;
        for _ in 0..steps {
            aim.turn(Vec2::new(0.0, -0.02), DT);
            went_inverted |= (aim.rot * Vec3::Y).y < -0.5;
        }
        assert!(went_inverted, "never went over the top");
        assert!(
            aim.dir().angle_between(Vec3::NEG_Z) < 0.05,
            "a full loop should come back around: {:?}",
            aim.dir()
        );
    }

    /// Pausing after a half loop rolls the view upright, facing back the
    /// way it came (an Immelmann).
    #[test]
    fn pause_while_inverted_levels_the_view() {
        let mut aim = MouseAim::default();
        let steps = (core::f32::consts::PI / 0.02).round() as usize;
        for _ in 0..steps {
            aim.turn(Vec2::new(0.0, -0.02), DT);
        }
        assert!(
            (aim.rot * Vec3::Y).y < -0.9,
            "not inverted after a half loop"
        );
        for _ in 0..(3.0 / DT) as usize {
            aim.turn(Vec2::ZERO, DT);
        }
        assert!((aim.rot * Vec3::Y).y > 0.99, "still inverted");
        assert!(
            aim.dir().z > 0.99,
            "leveling must not move the aim: {:?}",
            aim.dir()
        );
    }
}
