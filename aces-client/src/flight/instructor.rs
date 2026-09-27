//! The War Thunder "instructor": a flight computer that flies the nose onto
//! the mouse-aim direction.
//!
//! It steers with the **lift vector**. Each tick it works out the
//! acceleration the airframe must produce perpendicular to the flight path:
//! what holds the path against gravity (and the thrust's share), plus a turn
//! of the path toward the aim at a rate proportional to the error. Then it
//! rolls that vector above the canopy and pulls (or pushes) its magnitude.
//! That one rule gives WT's behavior at every scale and is well conditioned
//! everywhere:
//! - aim straight ahead → the vector is just "hold me up" → wings level;
//! - aim a little to the side → a slightly tilted vector → a gentle bank;
//! - aim far off → a large turn demand → bank hard and pull (bank-and-yank);
//! - aim below → the vector shrinks and the nose drops; a vector pointing
//!   down is pushed for (negative g) while small, and rolled over for
//!   (split-S) when large, whichever keeps the wings nearer upright.
//!
//! Vertical error is measured from the nose (it rides α above the flight
//! path, and the nose is what must land on the aim); lateral error from the
//! flight path, so nose slip — adverse yaw, roll coupling, rudder — never
//! feeds back into the bank. The last few degrees laterally are closed with
//! the rudder instead, wings level: the precise gun-aiming mode. While
//! maneuvering, the rudder coordinates the turn (cancels adverse yaw, washes
//! out sideslip).
//!
//! **Protection**: an angle-of-attack limit below the stall, a G limit, and
//! ground avoidance that refuses to let the aim drag the plane into the sea.
//! The pilot can switch it off (hold `Shift`) to use the whole elevator —
//! and stall. Protection limits what the instructor commands, not the
//! physics: run out of speed and the plane still falls.
//!
//! The pitch channel inverts the flight model: needed lift → angle of
//! attack → elevator, so the instructor behaves the same at every speed and
//! altitude, and the limits can be expressed directly in g and degrees.

use bevy::prelude::*;
use core::f32::consts::PI;
use serde::{Deserialize, Serialize};

use crate::flight::Surfaces;
use crate::flight::model::{
    Airframe, FlightState, GRAVITY, V_REF, control_effectiveness, density_ratio, smoothstep,
    thrust, wrap_angle,
};

/// Lateral error below which the rudder alone corrects it, wings level
/// (fully by `.0`; banking takes over completely by `.1`) [rad].
pub const PRECISION_BAND: (f32, f32) = (1f32.to_radians(), 5f32.to_radians());
/// Flight-path rotation rate per radian of vertical aim error [1/s].
pub const PATH_GAIN: f32 = 2.0;
/// …and per radian of lateral error [1/s]. Gentler: turning the path
/// sideways takes a bank, and a quick sideways correction a steep one.
pub const LATERAL_GAIN: f32 = 1.0;
/// Cap on the commanded flight-path rotation rate [rad/s].
pub const MAX_PATH_RATE: f32 = 1.0;
/// Roll rate per radian of roll error toward the needed lift vector [1/s].
pub const ROLL_GAIN: f32 = 3.5;
/// Roll urgency: corrections this far off [rad] may roll at the airframe's
/// full rate; smaller ones are capped proportionally, down to
/// [`ROLL_RATE_MIN_CAP`], so small corrections bank in smoothly.
pub const ROLL_URGENT_ERROR: f32 = 30f32.to_radians();
/// Slowest roll-rate cap for small corrections [rad/s].
pub const ROLL_RATE_MIN_CAP: f32 = 1.0;
/// Extra aileron to overcome roll inertia: 1 doubles the correction for
/// the difference between desired and actual roll rate.
pub const ROLL_LEAD: f32 = 1.0;
/// Below this needed lift [g] its direction means little (zero-g arcs,
/// pointing straight up), so the roll command fades out.
pub const ROLL_MIN_LIFT: (f32, f32) = (0.1, 0.5);
/// Sideslip washout while maneuvering: rudder per rudder-authority of
/// sideslip.
pub const COORD_GAIN: f32 = 1.0;
/// Vertical damping: the pitch channel acts on the nose error minus this
/// many seconds of the nose's own pitch rate, easing off as the nose swings
/// in — the angle of attack lags the elevator, and without the lead the
/// nose sails past the aim as the pull relaxes [s]. (The nose's rate, not
/// the error's: mouse aim moves in steps, and differentiating those would
/// kick the elevator every frame.)
pub const PITCH_LEAD: f32 = 0.1;
/// Lateral integral action (the rudder's) works within this aim error
/// (fully inside `.0`, not at all beyond `.1`) [rad]; outside it the
/// integrator leaks away, so big maneuvers never wind it up.
pub const I_ZONE: (f32, f32) = (0.4f32.to_radians(), 1.2f32.to_radians());
/// The integrator only runs while the error creeps: fully below `.0`, not
/// at all above `.1` [rad/s]. A closing error is the P-term's business;
/// integrating it winds up and overshoots.
pub const I_CREEP: (f32, f32) = (0.5f32.to_radians(), 1.5f32.to_radians());
/// Leak rate of the integrator outside [`I_ZONE`] [1/s].
pub const I_LEAK: f32 = 3.0;
/// Lateral integral gain: sideslip per radian-second of nose error at
/// [`V_REF`] and sea level [1/s], scaled with the airframe's yaw response
/// (√ dynamic pressure) so slow flight doesn't overdrive it. Removes the
/// lag of the rudder's slip behind the moving path.
pub const I_YAW: f32 = 1.5;
/// Most sideslip the lateral integrator may add [rad].
pub const I_YAW_MAX: f32 = 2f32.to_radians();
/// Angle-of-attack protection keeps this far clear of the airframe's stall
/// angles [rad]. (The G limits are the airframe's too.)
pub const ALPHA_MARGIN: f32 = 2f32.to_radians();
/// A downward lift demand smaller than this is pushed for rather than
/// rolled over for [g].
pub const PUSH_MAX_G: f32 = 2.0;
/// Hysteresis between pushing and rolling over, in resulting bank [rad].
pub const PUSH_HYSTERESIS: f32 = 30f32.to_radians();
/// Ground avoidance looks this far ahead along the descent [s]…
pub const GROUND_LOOKAHEAD: f32 = 4.0;
/// …and starts pulling out when the predicted altitude falls below this [m].
pub const GROUND_SAFE: f32 = 80.0;
/// Climb angle the ground avoidance steers toward [rad].
pub const GROUND_ESCAPE: f32 = 5f32.to_radians();

#[cfg(not(target_arch = "wasm32"))]
/// The instructor's tuning constants, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    let mut table = tuning_table![
        PATH_GAIN,
        LATERAL_GAIN,
        PITCH_LEAD,
        MAX_PATH_RATE,
        ROLL_GAIN,
        ROLL_URGENT_ERROR,
        ROLL_RATE_MIN_CAP,
        ROLL_LEAD,
        COORD_GAIN,
        I_LEAK,
        I_YAW,
        I_YAW_MAX,
        ALPHA_MARGIN,
        PUSH_MAX_G,
        PUSH_HYSTERESIS,
        GROUND_LOOKAHEAD,
        GROUND_SAFE,
        GROUND_ESCAPE,
    ];
    table.push(("PRECISION_BAND.0", PRECISION_BAND.0));
    table.push(("PRECISION_BAND.1", PRECISION_BAND.1));
    table.push(("I_CREEP.0", I_CREEP.0));
    table.push(("I_CREEP.1", I_CREEP.1));
    table.push(("I_ZONE.0", I_ZONE.0));
    table.push(("I_ZONE.1", I_ZONE.1));
    table.push(("ROLL_MIN_LIFT.0", ROLL_MIN_LIFT.0));
    table.push(("ROLL_MIN_LIFT.1", ROLL_MIN_LIFT.1));
    table
}

/// Instructor memory (push-mode hysteresis, the lateral integrator). One
/// per piloted plane.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct Instructor {
    push: bool,
    /// Integrated lateral nose error near capture [rad·s].
    integral: f32,
    /// Last tick's nose error (lateral, vertical), for its rate of change.
    last_nose_error: Option<Vec2>,
    /// What the last [`Instructor::command`] saw and decided, for the
    /// flight recorder.
    pub debug: InstructorDebug,
}

/// The instructor's view of one tick (angles in radians, rates in rad/s,
/// accelerations in m/s²). Recorded every tick; see `recorder`. Fields
/// missing from older logs read as zero.
#[derive(Default, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstructorDebug {
    /// Aim error off the nose.
    pub err: f32,
    /// Share of the lateral error flown by banking (the rest: rudder).
    pub w: f32,
    /// Aim error in the flight-path frame: right of the path…
    pub err_lateral: f32,
    /// …and above the nose, in the lift plane.
    pub err_vertical: f32,
    /// Needed acceleration ⊥ the path: along the wing (right)…
    pub need_side: f32,
    /// …and along the current lift direction.
    pub need_lift: f32,
    pub push: bool,
    /// Ground avoidance raised the aim.
    pub ground: bool,
    pub bank: f32,
    pub roll_error: f32,
    pub roll_rate_cmd: f32,
    pub path_rate_cmd: f32,
    pub alpha_trim: f32,
    pub alpha_cmd: f32,
    pub beta_cmd: f32,
    /// Lateral nose error (aim right of the nose) and its integral
    /// [rad·s].
    pub nose_lateral: f32,
    pub integral_lateral: f32,
}

impl Instructor {
    /// Surface commands that fly the nose of `s` (an `a` airframe) onto the
    /// world-space unit direction `aim`, within the flight envelope if
    /// `protect` is set, for a tick of `dt` seconds. Everything it knows
    /// about the plane comes from `a`, so every airframe gets the same
    /// behavior, scaled to its own performance.
    pub fn command(
        &mut self,
        s: &FlightState,
        a: &Airframe,
        aim: Vec3,
        protect: bool,
        dt: f32,
    ) -> Surfaces {
        let raw_aim = aim;
        let aim = if protect { avoid_ground(s, aim) } else { aim };

        let forward = s.quat * Vec3::NEG_Z;
        let up = s.quat * Vec3::Y;
        let right = s.quat * Vec3::X;
        let speed = s.speed().max(1.0);
        let v_dir = s.vel.try_normalize().unwrap_or(forward);
        let sigma = density_ratio(s.pos.y);
        let pressure = (a.aero_k * sigma * speed * speed).max(1e-3);
        let eff = control_effectiveness(a, speed);

        // Flight-path frame: the path, the current lift direction (⊥ path,
        // in the symmetry plane) and the side direction (right).
        let lift_dir = right.cross(v_dir).try_normalize().unwrap_or(up);
        let side_dir = v_dir.cross(lift_dir);

        // Acceleration ⊥ the path that just holds it: cancel gravity and
        // the thrust's share across the path.
        let gravity = Vec3::new(0.0, -GRAVITY, 0.0);
        let across = gravity + forward * thrust(a, s.engine, sigma, speed);
        let hold = -(across - v_dir * across.dot(v_dir));
        let alpha_trim = (hold.dot(lift_dir) / pressure - a.cl0) / a.cl_alpha;

        // Aim error. Its direction around the path, its size from the path;
        // then the vertical part re-measured from the nose.
        let err = forward.angle_between(aim);
        let across_aim = Vec2::new(aim.dot(side_dir), aim.dot(lift_dir));
        let mut error = across_aim.normalize_or_zero() * v_dir.angle_between(aim);
        error.y -= s.alpha;
        let err_lateral = error.x;
        // The last few degrees laterally are the rudder's, wings level.
        let w = smoothstep(PRECISION_BAND.0, PRECISION_BAND.1, err_lateral.abs());
        error.x *= w;

        // Lateral integral action near capture, on the nose's error (the
        // nose sits -β off the path): what the pilot sees. It only runs
        // while that error creeps, i.e. on the tail the rudder's slip lags.
        let near = 1.0 - smoothstep(I_ZONE.0, I_ZONE.1, err);
        let nose_lateral = err_lateral + s.beta;
        let nose_error = Vec2::new(nose_lateral, error.y);
        let nose_rate = self
            .last_nose_error
            .map_or(Vec2::ZERO, |last| (nose_error - last) / dt.max(1e-4));
        self.last_nose_error = Some(nose_error);
        let creep = 1.0 - smoothstep(I_CREEP.0, I_CREEP.1, nose_rate.x.abs());
        self.integral = (self.integral * (-I_LEAK * (1.0 - near) * dt).exp()
            + nose_lateral * creep * near * dt)
            .clamp(-I_YAW_MAX / I_YAW, I_YAW_MAX / I_YAW);

        // Needed acceleration ⊥ the path, in (side, lift) components.
        let rate = Vec2::new(
            LATERAL_GAIN * error.x,
            PATH_GAIN * (error.y - PITCH_LEAD * s.omega.pitch),
        );
        let path_rate = rate.length().min(MAX_PATH_RATE);
        let turn = rate.normalize_or_zero() * path_rate * speed;
        let need = Vec2::new(hold.dot(side_dir), hold.dot(lift_dir)) + turn;

        // ── Roll: put the needed lift above the canopy, or below it and
        // push when that keeps the wings nearer upright and it is small.
        let bank = (-right.y).atan2(up.y);
        let roll_pull = need.x.atan2(need.y);
        let roll_push = wrap_angle(roll_pull - PI);
        let (bank_pull, bank_push) = (wrap_angle(bank + roll_pull), wrap_angle(bank + roll_push));
        let need_g = need.length() / GRAVITY;
        self.push = if self.push {
            need_g < PUSH_MAX_G + 0.5 && bank_push.abs() < bank_pull.abs() + PUSH_HYSTERESIS
        } else {
            need_g < PUSH_MAX_G && bank_push.abs() + PUSH_HYSTERESIS < bank_pull.abs()
        };
        let roll_error = if self.push { roll_push } else { roll_pull };
        let confidence = smoothstep(ROLL_MIN_LIFT.0, ROLL_MIN_LIFT.1, need_g);
        let full_rate = (a.roll_rate * eff * (sigma.sqrt() * speed / V_REF)).max(0.3);
        let cap = (full_rate * err / ROLL_URGENT_ERROR)
            .clamp(ROLL_RATE_MIN_CAP.min(full_rate), full_rate);
        let roll_rate = (ROLL_GAIN * roll_error * confidence).clamp(-cap, cap);
        let aileron = (roll_rate + ROLL_LEAD * (roll_rate - s.omega.roll)) / full_rate;

        // ── Pitch: the needed lift along the current lift direction →
        // AoA → elevator. Mid-roll that is only its projection, so the
        // plane unloads while the lift vector swings round.
        let lift_needed = need.y;
        let alpha = if protect {
            let lift = lift_needed.clamp(a.g_limit_neg * GRAVITY, a.g_limit * GRAVITY);
            ((lift / pressure - a.cl0) / a.cl_alpha).clamp(
                a.alpha_stall_neg + ALPHA_MARGIN,
                a.alpha_stall - ALPHA_MARGIN,
            )
        } else {
            // Unprotected: ask for whatever AoA the linear lift curve says,
            // up to the elevator's full authority (well past the stall).
            ((lift_needed / pressure - a.cl0) / a.cl_alpha)
                .clamp(-a.elevator_alpha_down, a.elevator_alpha_up)
        };
        let elevator = alpha
            / (eff
                * if alpha >= 0.0 {
                    a.elevator_alpha_up
                } else {
                    a.elevator_alpha_down
                });

        // ── Yaw ────────────────────────────────────────────────────────────
        // Precision: slip the nose onto the aim. The nose sits -β off the
        // path, so putting it on the aim takes β = -(lateral path error);
        // the side force of that slip then swings the path after the nose.
        let yaw_response = (sigma.sqrt() * speed / V_REF).clamp(0.3, 1.5);
        let beta_precise = (-err_lateral - I_YAW * yaw_response * self.integral)
            .clamp(-a.rudder_beta, a.rudder_beta);
        let rudder_precise = -beta_precise / (a.rudder_beta * eff);
        // Maneuvering: coordinate — cancel the ailerons' adverse yaw and
        // wash out sideslip, so the plane turns where it banks.
        let adverse = a.adverse_yaw / (a.yaw_freq * a.yaw_freq * a.rudder_beta);
        let rudder_coord =
            adverse * aileron.clamp(-1.0, 1.0) + COORD_GAIN * s.beta / (a.rudder_beta * eff);
        let rudder = (1.0 - w) * rudder_precise + w * rudder_coord;

        self.debug = InstructorDebug {
            err,
            w,
            err_lateral,
            err_vertical: error.y,
            need_side: need.x,
            need_lift: need.y,
            push: self.push,
            ground: aim != raw_aim,
            bank,
            roll_error,
            roll_rate_cmd: roll_rate,
            path_rate_cmd: path_rate,
            alpha_trim,
            alpha_cmd: alpha,
            beta_cmd: beta_precise * (1.0 - w),
            nose_lateral,
            integral_lateral: self.integral,
        };

        Surfaces {
            elevator: elevator.clamp(-1.0, 1.0),
            aileron: aileron.clamp(-1.0, 1.0),
            rudder: rudder.clamp(-1.0, 1.0),
        }
    }
}

/// Ground avoidance: when the plane is about to fly into the sea, raise the
/// aim toward a gentle climb along the current heading, the more urgently
/// the closer the predicted impact.
fn avoid_ground(s: &FlightState, aim: Vec3) -> Vec3 {
    let predicted = s.pos.y + s.vel.y.min(0.0) * GROUND_LOOKAHEAD;
    let danger = ((GROUND_SAFE - predicted) / GROUND_SAFE).clamp(0.0, 1.0);
    if danger == 0.0 {
        return aim;
    }
    let heading = Vec3::new(s.vel.x, 0.0, s.vel.z)
        .try_normalize()
        .unwrap_or_else(|| {
            Vec3::new(aim.x, 0.0, aim.z)
                .try_normalize()
                .unwrap_or(Vec3::NEG_Z)
        });
    let escape = (heading + Vec3::Y * GROUND_ESCAPE.tan()).normalize();
    if aim.y >= escape.y {
        return aim;
    }
    aim.lerp(escape, danger).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight::model::{MIN_ALTITUDE, step};
    use aces_protocol::aircraft::PLACEHOLDER_JET as JET;

    const DT: f32 = 1.0 / 60.0;

    /// Outcome of flying the instructor toward a fixed aim.
    struct Flight {
        state: FlightState,
        /// Largest |bank| seen [deg].
        max_bank: f32,
        max_g: f32,
        min_g: f32,
        ever_stalled: bool,
        min_altitude: f32,
    }

    fn bank_of(s: &FlightState) -> f32 {
        let right = s.quat * Vec3::X;
        let up = s.quat * Vec3::Y;
        (-right.y).atan2(up.y).to_degrees()
    }

    fn fly(mut state: FlightState, aim: Vec3, seconds: f32) -> Flight {
        let mut instructor = Instructor::default();
        let mut flight = Flight {
            state,
            max_bank: 0.0,
            max_g: 1.0,
            min_g: 1.0,
            ever_stalled: false,
            min_altitude: f32::MAX,
        };
        for _ in 0..(seconds / DT).round() as usize {
            let command = instructor.command(&state, &JET, aim, true, DT);
            step(&mut state, &JET, &command, 0.0, DT);
            flight.max_bank = flight.max_bank.max(bank_of(&state).abs());
            flight.max_g = flight.max_g.max(state.g_load);
            flight.min_g = flight.min_g.min(state.g_load);
            flight.ever_stalled |= state.stalled;
            flight.min_altitude = flight.min_altitude.min(state.pos.y);
        }
        flight.state = state;
        flight
    }

    fn cruise(speed: f32) -> FlightState {
        FlightState::new(Vec3::new(0.0, 5000.0, 0.0), Quat::IDENTITY, speed, 1.0)
    }

    /// A world direction `up` degrees above and `right` degrees right of
    /// the initial nose (-Z).
    fn off(up: f32, right: f32) -> Vec3 {
        Quat::from_rotation_y(-right.to_radians())
            * Quat::from_rotation_x(up.to_radians())
            * Vec3::NEG_Z
    }

    fn nose_error(s: &FlightState, aim: Vec3) -> f32 {
        s.forward().angle_between(aim).to_degrees()
    }

    /// Tuning aid: `cargo test -p aces-client trace -- --ignored --nocapture`
    /// prints a closed-loop trajectory.
    #[test]
    #[ignore]
    fn trace() {
        let scenarios = [
            ("6/6 @130", cruise(130.0), off(6.0, 6.0)),
            ("5 right @190", cruise(190.0), off(0.0, 5.0)),
            ("3 right @190", cruise(190.0), off(0.0, 3.0)),
            ("6 right @190", cruise(190.0), off(0.0, 6.0)),
            ("90 right @170", cruise(170.0), off(0.0, 90.0)),
            ("170 behind @170", cruise(170.0), off(0.0, 170.0)),
            ("80 down @170", cruise(170.0), off(-80.0, 5.0)),
        ];
        for (name, mut state, aim) in scenarios {
            println!("── {name}");
            let mut instructor = Instructor::default();
            for i in 0..(8.0 / DT) as usize {
                let c = instructor.command(&state, &JET, aim, true, DT);
                step(&mut state, &JET, &c, 0.0, DT);
                if i % 15 == 0 {
                    let d = instructor.debug;
                    println!(
                        "t {:5.2} err {:5.2} lat {:5.2} nose_lat {:5.2} vert {:5.2} beta {:5.2} bcmd {:5.2} w {:4.2} bank {:5.1} rud {:5.2} I {:6.3}",
                        i as f32 * DT,
                        nose_error(&state, aim),
                        d.err_lateral.to_degrees(),
                        d.nose_lateral.to_degrees(),
                        d.err_vertical.to_degrees(),
                        state.beta.to_degrees(),
                        d.beta_cmd.to_degrees(),
                        d.w,
                        bank_of(&state),
                        c.rudder,
                        d.integral_lateral.to_degrees(),
                    );
                }
            }
        }
    }

    /// Small sideways offsets (the case that oscillated in flight logs):
    /// the plane must settle onto the aim without rocking its wings.
    #[test]
    fn small_lateral_offsets_settle_without_rocking() {
        for (up, right, speed) in [
            (0.0, 3.0, 190.0),
            (0.0, 6.0, 190.0),
            (1.0, -4.0, 150.0),
            (-2.0, 8.0, 220.0),
            (0.0, 1.5, 120.0),
        ] {
            let aim = off(up, right);
            let mut instructor = Instructor::default();
            let mut state = cruise(speed);
            let (mut max_bank, mut bank_swings, mut last_sign) = (0.0f32, 0, 0.0f32);
            let mut roll_sq = 0.0;
            let n = (8.0 / DT) as usize;
            for _ in 0..n {
                let command = instructor.command(&state, &JET, aim, true, DT);
                step(&mut state, &JET, &command, 0.0, DT);
                let bank = bank_of(&state);
                max_bank = max_bank.max(bank.abs());
                if bank.abs() > 3.0 && bank.signum() != last_sign {
                    bank_swings += 1;
                    last_sign = bank.signum();
                }
                roll_sq += state.omega.roll.to_degrees().powi(2);
            }
            let roll_rms = (roll_sq / n as f32).sqrt();
            let err = nose_error(&state, aim);
            let case = format!("aim {up}°/{right}° at {speed} m/s");
            assert!(err < 0.5, "{case}: nose {err:.2}° off after 8 s");
            // One bank in and out, no rocking.
            assert!(bank_swings <= 1, "{case}: wings rocked {bank_swings} times");
            // A sideways path correction needs a bank; about 10° per degree
            // of error at these speeds is the physics, more is overreacting.
            assert!(
                max_bank < 5.0 + 10.0 * right.abs(),
                "{case}: banked {max_bank:.0}°"
            );
            assert!(roll_rms < 35.0, "{case}: roll rate rms {roll_rms:.0}°/s");
        }
    }

    /// End-game convergence: the last tenth of a degree is closed promptly
    /// (the lateral integrator removes the rudder slip's lag) without the
    /// nose swinging back out once it got close.
    #[test]
    fn nose_settles_onto_small_offsets() {
        for (up, right, speed, within) in [
            (0.0, 2.0, 190.0, 3.5),
            (0.0, 5.0, 190.0, 4.5),
            (2.0, -3.0, 220.0, 3.0),
            (3.0, 0.0, 190.0, 3.0),
        ] {
            let case = format!("aim {up}°/{right}° at {speed} m/s");
            let settle = settle(off(up, right), cruise(speed), 8.0);
            assert!(
                settle.within_tenth < within,
                "{case}: took {:.2} s to stay within 0.1°",
                settle.within_tenth
            );
            assert!(
                settle.err_at_6s < 0.05,
                "{case}: {:.3}° left at 6 s",
                settle.err_at_6s
            );
            assert!(
                settle.overshoot < 0.3,
                "{case}: swung back out to {:.2}°",
                settle.overshoot
            );
        }
    }

    /// Settling benchmark: `cargo test -p aces-client settling -- --ignored
    /// --nocapture`. For each offset: time until the nose stays within
    /// 0.25° and 0.1° of the aim, the error left after 6 s, and the worst
    /// overshoot once inside 0.25°.
    #[test]
    #[ignore]
    fn settling_report() {
        println!("case                      t<0.25°  t<0.1°  err@6s  overshoot");
        for (up, right, speed) in [
            (0.0, 2.0, 190.0),
            (0.0, 5.0, 190.0),
            (3.0, 0.0, 190.0),
            (-3.0, 0.0, 150.0),
            (2.0, -3.0, 220.0),
            (0.0, 20.0, 170.0),
            (6.0, 6.0, 130.0),
        ] {
            let settle = settle(off(up, right), cruise(speed), 12.0);
            println!(
                "{:<24} {:>7.2} {:>7.2} {:>7.3} {:>9.3}",
                format!("{up}°/{right}° @{speed}"),
                settle.within_quarter,
                settle.within_tenth,
                settle.err_at_6s,
                settle.overshoot
            );
        }
    }

    struct Settle {
        within_quarter: f32,
        within_tenth: f32,
        err_at_6s: f32,
        overshoot: f32,
    }

    /// Fly onto `aim` for `seconds` and measure how the nose settles.
    fn settle(aim: Vec3, mut state: FlightState, seconds: f32) -> Settle {
        let mut instructor = Instructor::default();
        let n = (seconds / DT) as usize;
        let mut errs = Vec::with_capacity(n);
        for _ in 0..n {
            let command = instructor.command(&state, &JET, aim, true, DT);
            step(&mut state, &JET, &command, 0.0, DT);
            errs.push(nose_error(&state, aim));
        }
        // Time after which the error never again exceeds `limit`.
        let settled = |limit: f32| {
            errs.iter()
                .rposition(|&e| e > limit)
                .map_or(0.0, |i| (i + 1) as f32 * DT)
        };
        let first_inside = errs.iter().position(|&e| e < 0.25).unwrap_or(n);
        Settle {
            within_quarter: settled(0.25),
            within_tenth: settled(0.1),
            err_at_6s: errs[((6.0 / DT) as usize).min(n - 1)],
            overshoot: errs[first_inside..].iter().copied().fold(0.0, f32::max),
        }
    }

    /// The instructor's core behaviors on every test airframe, each at a
    /// cruise speed scaled to its own stall speed: holds level, settles
    /// small offsets without rocking, turns onto a 90° offset, and keeps
    /// the G and AoA limits of that airframe.
    #[test]
    fn every_airframe_flies_the_same_way() {
        use crate::flight::model::test_airframes;
        let sigma_5000 = crate::flight::model::density_ratio(5000.0);
        for (name, a) in test_airframes::all() {
            // True airspeed ~3× the local 1 g stall speed.
            let cruise_speed = 3.0 * a.stall_speed() / sigma_5000.sqrt();
            let fly_on = |aim: Vec3, speed: f32, seconds: f32| -> (Flight, f32, usize) {
                let mut state = cruise(speed);
                let mut instructor = Instructor::default();
                let mut flight = Flight {
                    state,
                    max_bank: 0.0,
                    max_g: 1.0,
                    min_g: 1.0,
                    ever_stalled: false,
                    min_altitude: f32::MAX,
                };
                let (mut roll_sq, mut swings, mut last_sign) = (0.0, 0, 0.0f32);
                let n = (seconds / DT) as usize;
                for _ in 0..n {
                    let command = instructor.command(&state, &a, aim, true, DT);
                    step(&mut state, &a, &command, 0.0, DT);
                    let bank = bank_of(&state);
                    flight.max_bank = flight.max_bank.max(bank.abs());
                    flight.max_g = flight.max_g.max(state.g_load);
                    flight.min_g = flight.min_g.min(state.g_load);
                    flight.ever_stalled |= state.stalled;
                    roll_sq += state.omega.roll.to_degrees().powi(2);
                    if bank.abs() > 3.0 && bank.signum() != last_sign {
                        swings += 1;
                        last_sign = bank.signum();
                    }
                }
                flight.state = state;
                (flight, (roll_sq / n as f32).sqrt(), swings)
            };

            let (level, _, _) = fly_on(Vec3::NEG_Z, cruise_speed, 20.0);
            let err = nose_error(&level.state, Vec3::NEG_Z);
            assert!(err < 0.5, "{name}: level flight {err:.2}° off");
            assert!(
                level.max_bank < 2.0,
                "{name}: banked {:.1}° holding level",
                level.max_bank
            );

            for right in [3.0, 6.0] {
                let aim = off(0.0, right);
                let (flight, roll_rms, swings) = fly_on(aim, cruise_speed, 10.0);
                let err = nose_error(&flight.state, aim);
                assert!(
                    err < 0.5,
                    "{name}: {right}° offset, {err:.2}° off after 10 s"
                );
                assert!(swings <= 1, "{name}: {right}° offset rocked {swings} times");
                assert!(
                    roll_rms < 35.0,
                    "{name}: {right}° offset, roll rms {roll_rms:.0}°/s"
                );
            }

            let aim = off(0.0, 90.0);
            let (turn, _, _) = fly_on(aim, cruise_speed, 25.0);
            let err = nose_error(&turn.state, aim);
            assert!(err < 2.0, "{name}: 90° turn ended {err:.1}° off");
            assert!(
                turn.max_g < a.g_limit + 0.8,
                "{name}: pulled {:.1} g",
                turn.max_g
            );
            assert!(!turn.ever_stalled, "{name}: stalled in a protected turn");

            let slow = 0.6 * cruise_speed;
            let (reversal, _, _) = fly_on(off(10.0, 150.0), slow, 10.0);
            assert!(!reversal.ever_stalled, "{name}: stalled reversing slowly");
            assert!(
                reversal.max_g < a.g_limit + 0.8,
                "{name}: pulled {:.1} g",
                reversal.max_g
            );
        }
    }

    /// Hands off, aiming straight ahead: the plane trims itself to level
    /// flight — no porpoising, no creeping bank.
    #[test]
    fn holds_level_flight() {
        let aim = Vec3::NEG_Z;
        let flight = fly(cruise(150.0), aim, 30.0);
        let s = &flight.state;
        assert!(
            nose_error(s, aim) < 0.3,
            "nose {:.2}° off",
            nose_error(s, aim)
        );
        // The nose holds the horizon; the path rides the trim AoA below it.
        assert!(s.vel.y.abs() < 6.0, "climbing/sinking at {} m/s", s.vel.y);
        assert!(flight.max_bank < 1.0, "banked {:.1}°", flight.max_bank);
    }

    /// Small corrections are flown wings-level with rudder and elevator —
    /// the WT gun-aiming mode — and land precisely.
    #[test]
    fn precision_aim_stays_wings_level() {
        let aim = off(1.0, 1.5);
        let flight = fly(cruise(170.0), aim, 3.0);
        assert!(
            flight.max_bank < 8.0,
            "banked {:.1}° for a 1.8° correction",
            flight.max_bank
        );
        let err = nose_error(&flight.state, aim);
        assert!(err < 0.3, "nose {err:.2}° off after 3 s");
    }

    /// A large lateral offset is flown as bank-and-pull, and the wings come
    /// level again once the nose arrives.
    #[test]
    fn lateral_aim_banks_and_pulls() {
        let aim = off(0.0, 90.0);
        let flight = fly(cruise(170.0), aim, 12.0);
        assert!(
            flight.max_bank > 60.0,
            "only banked {:.1}°",
            flight.max_bank
        );
        assert!(
            flight.max_g > 4.0,
            "never pulled hard: {:.1} g",
            flight.max_g
        );
        let s = &flight.state;
        assert!(
            nose_error(s, aim) < 1.0,
            "nose {:.1}° off",
            nose_error(s, aim)
        );
        assert!(bank_of(s).abs() < 5.0, "still banked {:.1}°", bank_of(s));
    }

    /// Offset cursor above and to the side converges.
    #[test]
    fn converges_onto_offset_aim() {
        let aim = off(10.0, 30.0);
        let flight = fly(cruise(150.0), aim, 10.0);
        assert!(nose_error(&flight.state, aim) < 1.0);
    }

    /// Aim far above: pull up into it.
    #[test]
    fn converges_onto_aim_above() {
        let aim = off(60.0, 0.0);
        let flight = fly(cruise(200.0), aim, 10.0);
        assert!(nose_error(&flight.state, aim) < 1.5);
    }

    /// Aim almost behind: reverse the turn.
    #[test]
    fn converges_onto_aim_behind() {
        let aim = off(0.0, 170.0);
        let flight = fly(cruise(170.0), aim, 25.0);
        assert!(
            nose_error(&flight.state, aim) < 2.0,
            "{:.1}°",
            nose_error(&flight.state, aim)
        );
    }

    /// Aim slightly below: push onto it, don't roll inverted.
    #[test]
    fn small_drop_is_pushed_not_rolled() {
        let aim = off(-8.0, 0.0);
        let flight = fly(cruise(170.0), aim, 5.0);
        assert!(
            flight.max_bank < 30.0,
            "rolled {:.1}° to push 8°",
            flight.max_bank
        );
        assert!(flight.min_g < 0.5, "never unloaded: {:.1} g", flight.min_g);
        assert!(nose_error(&flight.state, aim) < 1.0);
    }

    /// Aim far below: roll over and pull through (split-S), don't bunt.
    #[test]
    fn steep_drop_rolls_over_and_pulls() {
        let aim = off(-80.0, 5.0);
        let flight = fly(cruise(170.0), aim, 12.0);
        assert!(
            flight.max_bank > 120.0,
            "never rolled over: {:.1}°",
            flight.max_bank
        );
        assert!(
            flight.min_g > JET.g_limit_neg - 0.5,
            "bunted at {:.1} g",
            flight.min_g
        );
        assert!(nose_error(&flight.state, aim) < 2.0);
    }

    /// Protection: a hard reversal at high speed respects the G limit, and
    /// a slow one never stalls.
    #[test]
    fn protection_limits_g_and_alpha() {
        let aim = off(10.0, 150.0);
        let fast = fly(cruise(260.0), aim, 6.0);
        assert!(fast.max_g < JET.g_limit + 0.8, "pulled {:.1} g", fast.max_g);
        assert!(
            fast.max_g > JET.g_limit - 1.5,
            "gave away turn performance: {:.1} g",
            fast.max_g
        );
        let slow = fly(cruise(90.0), aim, 10.0);
        assert!(!slow.ever_stalled, "stalled under protection");
    }

    /// With protection off, a hard pull at low speed stalls the plane —
    /// and switching protection back on recovers it.
    #[test]
    fn unprotected_pull_stalls_and_protection_recovers() {
        let aim = off(60.0, 0.0);
        let mut instructor = Instructor::default();
        let mut state = cruise(110.0);
        let mut stalled = false;
        for _ in 0..(4.0 / DT) as usize {
            let command = instructor.command(&state, &JET, aim, false, DT);
            step(&mut state, &JET, &command, 0.0, DT);
            stalled |= state.stalled;
        }
        assert!(stalled, "never stalled without protection");
        let flight = fly(state, Vec3::NEG_Z, 10.0);
        assert!(
            !flight.state.stalled,
            "protection did not recover the stall"
        );
        assert!(nose_error(&flight.state, Vec3::NEG_Z) < 5.0);
    }

    /// Aiming into the sea from a dive pulls out instead of crashing.
    #[test]
    fn ground_avoidance_pulls_out() {
        let dive = Quat::from_rotation_x(-50f32.to_radians());
        let state = FlightState::new(Vec3::new(0.0, 900.0, 0.0), dive, 220.0, 1.0);
        let aim = dive * Vec3::NEG_Z;
        let flight = fly(state, aim, 15.0);
        assert!(
            flight.min_altitude > MIN_ALTITUDE + 15.0,
            "hit the water: min altitude {:.1} m",
            flight.min_altitude
        );
    }

    /// Once captured, the nose holds the aim instead of hunting around it.
    #[test]
    fn capture_is_stable() {
        let aim = off(5.0, 5.0);
        let mut instructor = Instructor::default();
        let mut state = cruise(160.0);
        let mut worst = 0.0f32;
        for i in 0..(30.0 / DT) as usize {
            let command = instructor.command(&state, &JET, aim, true, DT);
            step(&mut state, &JET, &command, 0.0, DT);
            if i as f32 * DT > 10.0 {
                worst = worst.max(nose_error(&state, aim));
            }
        }
        assert!(worst < 0.5, "hunting up to {worst:.2}° after capture");
    }
}
