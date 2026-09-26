//! Semi-realistic point-mass flight model.
//!
//! The simulation state is kept in [`FlightState`] and advanced by the pure
//! function [`step`], so the whole flight model is unit-testable without a
//! Bevy `App`. [`step_flight`] shims the ECS components in and out on the
//! fixed 60 Hz tick, writing [`SimPose`]; rendering interpolates from there.
//!
//! Model: accelerations in world space, no explicit mass (specific forces).
//! - Thrust along the nose from the throttle.
//! - Lift perpendicular to the velocity, in the plane's lift plane, from a
//!   linear lift curve that collapses past the stall angle of attack.
//! - Parasitic + induced drag.
//! - A lateral "fin" force damping sideslip.
//! - A gentle alignment assist rotating the velocity toward the nose (the
//!   "instructor" that makes mouse steering flyable); nearly disabled while
//!   stalled so stalls are real but recoverable.

use bevy::prelude::*;

use crate::flight::input::FlightInput;
use crate::flight::{Aircraft, AngleOfAttack, AngularRates, SimPose, StallState, Velocity};

// ── Constants (tuned for a ~16 m wingspan jet) ──────────────────────────────

/// Gravitational acceleration [m/s²].
pub const GRAVITY: f32 = 9.81;
/// Specific thrust at full throttle [m/s²].
pub const MAX_THRUST_ACC: f32 = 14.0;
/// Lift accel = `LIFT_K · CL · v²`; with the curve below this trims
/// ~150 m/s cruise at ~6° AoA and ~84 m/s stall speed.
pub const LIFT_K: f32 = 0.0010;
/// Lift curve slope per radian.
pub const CL_ALPHA: f32 = 4.5;
/// Lift at zero AoA.
pub const CL0: f32 = 0.15;
/// AoA where the lift curve breaks [rad].
pub const STALL_ALPHA: f32 = 16f32.to_radians();
/// AoA where the lift loss bottoms out [rad].
pub const STALL_END_ALPHA: f32 = 26f32.to_radians();
/// Fraction of lift retained at the bottom of the stall.
pub const STALL_LIFT_FLOOR: f32 = 0.3;
/// Parasitic drag accel = `DRAG_K0 · v²`.
pub const DRAG_K0: f32 = 0.00016;
/// Induced drag accel = `INDUCED_K · CL² · v²`.
pub const INDUCED_K: f32 = 0.0006;
/// Sideslip damping (fin) [1/s].
pub const SIDE_DAMP: f32 = 0.6;
/// Velocity-toward-nose alignment assist [1/s] (the instructor).
pub const ALIGN_RATE: f32 = 1.2;
/// Alignment assist retained while stalled, so recovery is possible but the
/// stall bites.
pub const STALL_ALIGN_FACTOR: f32 = 0.15;

/// Commanded angular rates at full deflection and reference speed [rad/s].
pub const MAX_PITCH_RATE: f32 = 1.5;
pub const MAX_YAW_RATE: f32 = 0.45;
pub const MAX_ROLL_RATE: f32 = 2.8;
/// Speed the rate limits are defined at; slower flight turns slower.
pub const RATE_REF_SPEED: f32 = 120.0;
/// Clamp range for the speed scaling of the rates.
pub const RATE_SPEED_FACTOR: (f32, f32) = (0.3, 1.3);
/// How fast smoothed rates approach their command [1/s].
pub const RATE_SMOOTHING: f32 = 8.0;
/// Throttle change rate [1/s].
pub const THROTTLE_RATE: f32 = 0.4;
/// Buffet (airframe shake) frequency while stalled [rad/s].
pub const BUFFET_FREQUENCY: f32 = 30.0;
/// Buffet amplitude as an extra pitch/yaw rate [rad/s].
pub const BUFFET_AMPLITUDE: f32 = 0.02;
/// Hard floor above the ocean; no damage yet (milestone 4).
pub const MIN_ALTITUDE: f32 = 3.0;

// ── Pure simulation core ────────────────────────────────────────────────────

/// Pilot rate commands in [-1, 1]; positive pitch = nose up, positive yaw =
/// nose right, positive roll = right wing down.
#[derive(Default, Clone, Copy)]
pub struct PilotInput {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    pub throttle_delta: f32,
}

/// Everything the flight model needs and produces; a plain value so tests
/// can run it headlessly.
#[derive(Clone, Copy)]
pub struct FlightState {
    pub pos: Vec3,
    pub quat: Quat,
    pub vel: Vec3,
    pub rates: AngularRates,
    pub throttle: f32,
    pub alpha: f32,
    pub stalled: bool,
    pub buffet_phase: f32,
}

/// Lift coefficient at `alpha`, linear below the stall, sagging to
/// [`STALL_LIFT_FLOOR`] of the linear value by [`STALL_END_ALPHA`].
pub fn lift_coefficient(alpha: f32) -> f32 {
    let linear = CL0 + CL_ALPHA * alpha;
    let excess = (alpha.abs() - STALL_ALPHA) / (STALL_END_ALPHA - STALL_ALPHA);
    if excess <= 0.0 {
        linear
    } else {
        linear * (1.0 - (1.0 - STALL_LIFT_FLOOR) * excess.min(1.0))
    }
}

impl Default for FlightState {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            quat: Quat::IDENTITY,
            vel: Vec3::ZERO,
            rates: AngularRates::default(),
            throttle: 0.0,
            alpha: 0.0,
            stalled: false,
            buffet_phase: 0.0,
        }
    }
}

/// Advance the flight state by `dt` seconds.
pub fn step(state: &mut FlightState, input: &PilotInput, dt: f32) {
    // Throttle.
    state.throttle = (state.throttle + input.throttle_delta * THROTTLE_RATE * dt).clamp(0.0, 1.0);

    // Commanded rates, scaled by airspeed so slow flight is sluggish.
    let speed = state.vel.length();
    let speed_factor = (speed / RATE_REF_SPEED).clamp(RATE_SPEED_FACTOR.0, RATE_SPEED_FACTOR.1);
    let target = AngularRates {
        pitch: input.pitch * MAX_PITCH_RATE * speed_factor,
        yaw: input.yaw * MAX_YAW_RATE * speed_factor,
        roll: input.roll * MAX_ROLL_RATE * speed_factor,
    };
    let blend = (RATE_SMOOTHING * dt).min(1.0);
    state.rates.pitch += (target.pitch - state.rates.pitch) * blend;
    state.rates.yaw += (target.yaw - state.rates.yaw) * blend;
    state.rates.roll += (target.roll - state.rates.roll) * blend;

    // Buffet: airframe shake while stalled, deterministic in phase.
    let buffet = if state.stalled {
        state.buffet_phase += BUFFET_FREQUENCY * dt;
        BUFFET_AMPLITUDE * state.buffet_phase.sin()
    } else {
        0.0
    };

    // Integrate attitude in local space. Positive rotation about local X
    // pitches the nose up (-Z toward +Y); positive rotation about local -Z
    // rolls the right wing down; positive yaw input is nose right, i.e.
    // negative rotation about local Y.
    state.quat = state.quat
        * Quat::from_axis_angle(Vec3::X, (state.rates.pitch + buffet) * dt)
        * Quat::from_axis_angle(Vec3::Y, -state.rates.yaw * dt)
        * Quat::from_axis_angle(Vec3::NEG_Z, state.rates.roll * dt);
    // Keep rounding error from accumulating into a non-unit quaternion.
    state.quat = state.quat.normalize();

    let forward = state.quat * Vec3::NEG_Z;
    let up = state.quat * Vec3::Y;
    let right = state.quat * Vec3::X;

    // Aerodynamic angles from the velocity in the plane's frame.
    let v_local = state.quat.inverse() * state.vel;
    let forward_speed = -v_local.z;
    state.alpha = (-v_local.y).atan2(forward_speed.max(1e-3));
    let cl = lift_coefficient(state.alpha);
    state.stalled = state.alpha.abs() > STALL_ALPHA;

    // Force accumulation [m/s²].
    let mut acc = Vec3::new(0.0, -GRAVITY, 0.0);
    acc += forward * (state.throttle * MAX_THRUST_ACC);

    if speed > 1.0 {
        let vel_dir = state.vel / speed;
        // Lift: perpendicular to the velocity, in the plane containing the
        // plane's up vector.
        let lift_dir = (up - vel_dir * up.dot(vel_dir)).normalize_or_zero();
        acc += lift_dir * (LIFT_K * cl * speed * speed);
        // Drag: parasitic + induced, opposing the velocity.
        let drag = speed * speed * (DRAG_K0 + INDUCED_K * cl * cl);
        acc -= vel_dir * drag;
        // Fin: damp lateral (sideslip) velocity.
        acc -= right * (v_local.x * SIDE_DAMP);

        // Instructor: rotate the velocity toward the nose so the plane goes
        // where it points; nearly off while stalled.
        let align = ALIGN_RATE
            * if state.stalled {
                STALL_ALIGN_FACTOR
            } else {
                1.0
            }
            * dt;
        let aligned_dir = (vel_dir + (forward - vel_dir) * align).normalize_or_zero();
        state.vel = aligned_dir * speed;
    }

    state.vel += acc * dt;
    state.pos += state.vel * dt;

    // Water is not lethal yet: skim along the surface instead.
    if state.pos.y < MIN_ALTITUDE {
        state.pos.y = MIN_ALTITUDE;
        state.vel.y = state.vel.y.max(0.0);
    }
}

// ── ECS shim ────────────────────────────────────────────────────────────────

/// Everything the flight model touches on the aircraft entity.
type PlaneParts = (
    &'static mut SimPose,
    &'static mut Velocity,
    &'static mut Aircraft,
    &'static mut AngularRates,
    &'static mut AngleOfAttack,
    &'static mut StallState,
);

pub fn step_flight(
    time: Res<Time>,
    input: Res<FlightInput>,
    mut plane: Single<PlaneParts, With<Aircraft>>,
) {
    let (pose, velocity, aircraft, rates, alpha, stall) = &mut *plane;
    let (pos, quat) = pose.current;
    let mut state = FlightState {
        pos,
        quat,
        vel: velocity.0,
        rates: **rates,
        throttle: aircraft.throttle,
        alpha: alpha.0,
        stalled: stall.active,
        buffet_phase: stall.buffet_phase,
    };

    step(
        &mut state,
        &PilotInput {
            pitch: input.pitch,
            yaw: input.yaw,
            roll: input.roll,
            throttle_delta: input.throttle_delta,
        },
        time.delta_secs(),
    );

    pose.previous = pose.current;
    pose.current = (state.pos, state.quat);
    velocity.0 = state.vel;
    aircraft.throttle = state.throttle;
    alpha.0 = state.alpha;
    stall.active = state.stalled;
    stall.buffet_phase = state.buffet_phase;
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn cruise_state() -> FlightState {
        FlightState {
            pos: Vec3::new(0.0, 1000.0, 0.0),
            vel: Vec3::new(0.0, 0.0, -150.0),
            throttle: 0.7,
            ..default()
        }
    }

    fn run(state: &mut FlightState, input: PilotInput, seconds: f32) {
        for _ in 0..(seconds / DT) as usize {
            step(state, &input, DT);
        }
    }

    /// The milestone 2 acceptance criterion "fly": trimmed cruise holds a
    /// sane altitude band and speed without pilot input.
    #[test]
    fn cruise_flies_level() {
        let mut state = cruise_state();
        run(&mut state, PilotInput::default(), 30.0);
        assert!(
            (80.0..260.0).contains(&state.vel.length()),
            "speed {} out of band",
            state.vel.length()
        );
        assert!(
            (300.0..1400.0).contains(&state.pos.y),
            "altitude {} out of band",
            state.pos.y
        );
        assert!(!state.stalled, "stalled in straight cruise");
    }

    /// Pulling hard past the stall angle must break the lift curve…
    #[test]
    fn full_pull_stalls() {
        let mut state = cruise_state();
        run(
            &mut state,
            PilotInput {
                pitch: 1.0,
                throttle_delta: 1.0,
                ..default()
            },
            6.0,
        );
        assert!(
            state.stalled,
            "never stalled the aircraft; alpha={}",
            state.alpha
        );
    }

    /// …and releasing the stick must let it recover.
    #[test]
    fn stall_recovers() {
        let mut state = cruise_state();
        run(
            &mut state,
            PilotInput {
                pitch: 1.0,
                throttle_delta: 1.0,
                ..default()
            },
            6.0,
        );
        run(
            &mut state,
            PilotInput {
                throttle_delta: 1.0,
                ..default()
            },
            10.0,
        );
        assert!(!state.stalled, "did not recover; alpha={}", state.alpha);
        assert!(
            (80.0..320.0).contains(&state.vel.length()),
            "speed {} out of band after recovery",
            state.vel.length()
        );
    }

    /// D rolls the right wing down: after a quarter second of full roll the
    /// plane's up vector must lean toward the original right direction
    /// (past 90° it would point the other way instead).
    #[test]
    fn d_rolls_right() {
        let mut state = cruise_state();
        let right_before = state.quat * Vec3::X;
        run(
            &mut state,
            PilotInput {
                roll: 1.0,
                ..default()
            },
            0.25,
        );
        let up_after = state.quat * Vec3::Y;
        let lean = up_after.dot(right_before);
        assert!(
            (0.2..0.95).contains(&lean),
            "up={up_after:?} lean={lean} — wrong direction or overshoot"
        );
    }

    /// Turning is rate-limited and near-circular at full deflection: after a
    /// sustained level pull the heading should have swung most of the way
    /// around without exploding the speed.
    #[test]
    fn sustained_pull_turns() {
        let mut state = cruise_state();
        let forward_before = state.quat * Vec3::NEG_Z;
        run(
            &mut state,
            PilotInput {
                pitch: 1.0,
                ..default()
            },
            2.5,
        );
        let forward_after = state.quat * Vec3::NEG_Z;
        let angle = forward_before.angle_between(forward_after);
        assert!(angle > 2.0, "only turned {angle} rad in 2.5 s of full pull");
        assert!(state.vel.length().is_finite() && state.vel.length() > 30.0);
    }

    /// Long sessions of constant manoeuvring must not let the attitude
    /// quaternion drift away from unit length.
    #[test]
    fn attitude_stays_normalized() {
        let mut state = cruise_state();
        let input = PilotInput {
            pitch: 0.3,
            yaw: 0.7,
            roll: 0.9,
            throttle_delta: 1.0,
        };
        run(&mut state, input, 600.0);
        assert!(
            (state.quat.length() - 1.0).abs() < 1e-5,
            "quat length drifted to {}",
            state.quat.length()
        );
    }

    /// The lift curve is linear below the stall and collapses past it.
    #[test]
    fn lift_curve_stalls() {
        let cl_low = lift_coefficient(4f32.to_radians());
        let cl_stall = lift_coefficient(STALL_ALPHA - 1e-4);
        let cl_deep = lift_coefficient(24f32.to_radians());
        // Slope sanity below the stall (CL_ALPHA · ~12° of AoA ≈ 0.94).
        assert!((cl_stall - cl_low) > 0.8, "lift curve too flat");
        // The stall must actually remove lift (at 24°, 0.8 through the
        // stall zone, the linear value 2.04 has dropped to ~0.64·cl_stall).
        assert!(
            cl_deep < cl_stall * 0.7,
            "no lift collapse at 24°: {cl_deep}"
        );
        // And flatten out at the floor.
        let cl_deeper = lift_coefficient(STALL_END_ALPHA);
        assert!((cl_deeper - cl_deep).abs() < 0.25);
    }
}
