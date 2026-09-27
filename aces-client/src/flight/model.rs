//! Flight model: a rigid-body airplane in the spirit of War Thunder's arcade
//! physics.
//!
//! Nothing here commands a rotation rate directly. The control surfaces
//! (moved by the [`instructor`](super::instructor) or the keyboard, through
//! rate-limited actuators) set up aerodynamic moments; the moments rotate the
//! airframe; the airframe's angle of attack produces the lift that actually
//! turns the flight path. So the nose leads and the velocity follows, hard
//! turns bleed speed, dives regain it — the energy game WT arcade is built
//! on — while the stall stays soft and recoverable.
//!
//! The simulation state is the plain value [`FlightState`], advanced by the
//! pure function [`step`], so the whole model is unit-testable without a
//! Bevy `App`. [`step_flight`] runs instructor + model on the fixed 60 Hz
//! tick and writes [`SimPose`]; rendering interpolates from there.
//!
//! Model, in specific forces (accelerations; no explicit mass):
//! - Lift ⊥ velocity in the plane's symmetry plane, from a lift curve that
//!   rounds off past the stall angle into flat-plate lift.
//! - Drag: parasitic + induced, wave drag approaching Mach 1, flat-plate
//!   drag past the stall and in sideslip.
//! - Side force from sideslip (fuselage and fin push the velocity back
//!   under the nose).
//! - Thrust along the nose, spooling after the throttle lever; above 100 %
//!   the afterburner.
//! - Pitch and yaw are statically stable: elevator and rudder set the angle
//!   of attack / sideslip the airframe weathervanes to, a damped oscillator
//!   whose stiffness scales with dynamic pressure. Ailerons accelerate the
//!   roll against roll damping and drag the nose the other way (adverse
//!   yaw). Rolling about the body axis under load turns angle of attack into
//!   sideslip, which the dihedral effect and weathervaning answer: planes
//!   slip and twist rather than fly on rails. High speed stiffens the
//!   controls; low speed makes them mushy.
//! - Stall: lift rounds off and collapses, drag soars, the nose breaks
//!   downward, buffet shakes the airframe, and the roll damping reverses so
//!   a wing drops. Let go and it recovers.
//! - Air density falls off with altitude.

use bevy::prelude::*;
use core::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::flight::input::FlightInput;
use crate::flight::instructor::Instructor;
use crate::flight::{Aircraft, AngularRates, LocalPlane, SimPose, Surfaces, Velocity};
pub use aces_protocol::Airframe;

// ── Universal constants (per-aircraft ones live in the `Airframe`) ─────────

/// Gravitational acceleration [m/s²].
pub const GRAVITY: f32 = 9.81;
/// Speed of sound [m/s].
pub const SPEED_OF_SOUND: f32 = 340.0;
/// Scale height of the exponential atmosphere [m].
pub const SCALE_HEIGHT: f32 = 10_400.0;
/// Speed the airframes' rotational constants are quoted at [m/s].
pub const V_REF: f32 = aces_protocol::aircraft::REFERENCE_SPEED;
/// Throttle range: 0..1 is dry power, 1..THROTTLE_MAX the boost
/// (afterburner or war emergency power).
pub const THROTTLE_MAX: f32 = 1.1;
/// Throttle lever speed [1/s].
pub const THROTTLE_RATE: f32 = 0.5;
/// Speed-independent angular damping [1/s], so a plane with no airspeed
/// left does not tumble forever.
pub const RATE_DAMP_FLOOR: f32 = 0.3;
/// Hard floor above the ocean; no damage yet (milestone 4).
pub const MIN_ALTITUDE: f32 = 3.0;

#[cfg(not(target_arch = "wasm32"))]
/// The model's universal tuning constants, by name.
pub fn tuning() -> Vec<(&'static str, f32)> {
    tuning_table![THROTTLE_RATE, RATE_DAMP_FLOOR]
}

#[cfg(not(target_arch = "wasm32"))]
/// An airframe's numbers, by name (the names the constants had before
/// airframes became data, so old flight logs still compare).
pub fn airframe_tuning(a: &Airframe) -> Vec<(&'static str, f32)> {
    vec![
        ("AERO_K", a.aero_k),
        ("CL0", a.cl0),
        ("CL_ALPHA", a.cl_alpha),
        ("ALPHA_STALL", a.alpha_stall),
        ("ALPHA_STALL_NEG", a.alpha_stall_neg),
        ("STALL_BAND", a.stall_band),
        ("CL_PLATE", a.cl_plate),
        ("CD0", a.cd0),
        ("K_INDUCED", a.k_induced),
        ("CD_PLATE", a.cd_plate),
        ("CY_BETA", a.cy_beta),
        ("WAVE_DRAG", a.wave_drag),
        ("WAVE_MACH.0", a.wave_mach[0]),
        ("WAVE_MACH.1", a.wave_mach[1]),
        ("THRUST_MIL", a.thrust_mil),
        ("THRUST_AB", a.thrust_boost),
        ("THRUST_LAPSE", a.thrust_lapse),
        ("THRUST_DENSITY_EXPONENT", a.thrust_density_exponent),
        ("SPOOL_UP", a.spool_up),
        ("SPOOL_DOWN", a.spool_down),
        ("PITCH_FREQ", a.pitch_freq),
        ("PITCH_DAMPING", a.pitch_damping),
        ("ELEVATOR_ALPHA_UP", a.elevator_alpha_up),
        ("ELEVATOR_ALPHA_DOWN", a.elevator_alpha_down),
        ("YAW_FREQ", a.yaw_freq),
        ("YAW_DAMPING", a.yaw_damping),
        ("RUDDER_BETA", a.rudder_beta),
        ("ROLL_RATE", a.roll_rate),
        ("ROLL_TAU", a.roll_tau),
        ("DIHEDRAL", a.dihedral),
        ("ROLL_FROM_YAW", a.roll_from_yaw),
        ("ADVERSE_YAW", a.adverse_yaw),
        ("STIFF_SPEED", a.stiff_speed),
        ("STIFF_SPREAD", a.stiff_spread),
        ("ACTUATOR_RATE", a.actuator_rate),
        ("AUTOROTATION", a.autorotation),
        ("AUTOROTATION_HELIX", a.autorotation_helix),
        ("HALF_SPAN", a.half_span),
        ("WING_DROP", a.wing_drop),
        ("STALL_PITCH_BREAK", a.stall_pitch_break),
        ("BUFFET_FREQUENCY", a.buffet_frequency),
        ("BUFFET_AMPLITUDE", a.buffet_amplitude),
        ("G_LIMIT", a.g_limit),
        ("G_LIMIT_NEG", a.g_limit_neg),
    ]
}

// ── Aerodynamic helpers ─────────────────────────────────────────────────────

/// Hermite smoothstep of `x` between `edge0` and `edge1`.
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Wrap an angle into (-π, π].
pub fn wrap_angle(angle: f32) -> f32 {
    let wrapped = (angle + PI).rem_euclid(TAU) - PI;
    if wrapped == -PI { PI } else { wrapped }
}

/// Air density relative to sea level at `altitude` [m].
pub fn density_ratio(altitude: f32) -> f32 {
    (-altitude.max(0.0) / SCALE_HEIGHT).exp()
}

/// Control effectiveness in [0, 1]: full up to the airframe's stiffening
/// speed, then the surfaces get heavy (the WT "controls lock up at high
/// speed" feel).
pub fn control_effectiveness(a: &Airframe, speed: f32) -> f32 {
    let over = (speed - a.stiff_speed).max(0.0) / a.stiff_spread;
    1.0 / (1.0 + over * over)
}

/// Specific thrust [m/s²] at `throttle` in [0, THROTTLE_MAX], density ratio
/// `sigma` and airspeed `speed`.
pub fn thrust(a: &Airframe, throttle: f32, sigma: f32, speed: f32) -> f32 {
    let dry = throttle.min(1.0) * a.thrust_mil;
    let boost =
        ((throttle - 1.0) / (THROTTLE_MAX - 1.0)).clamp(0.0, 1.0) * (a.thrust_boost - a.thrust_mil);
    let lapse = (1.0 - a.thrust_lapse * speed / V_REF).max(0.0);
    (dry + boost) * sigma.powf(a.thrust_density_exponent) * lapse
}

/// How far into the stall `alpha` is: 0 = attached flow, 1 = flat plate.
fn stall_blend(a: &Airframe, alpha: f32) -> f32 {
    let positive = smoothstep(a.alpha_stall, a.alpha_stall + a.stall_band, alpha);
    let negative = smoothstep(
        -a.alpha_stall_neg,
        -a.alpha_stall_neg + a.stall_band,
        -alpha,
    );
    positive.max(negative)
}

/// Lift coefficient at any angle of attack: linear while the flow is
/// attached, blending into flat-plate lift through the stall band, so the
/// peak rounds off instead of falling off a cliff.
pub fn lift_coefficient(a: &Airframe, alpha: f32) -> f32 {
    let linear = a.cl0 + a.cl_alpha * alpha;
    let plate = a.cl_plate * (2.0 * alpha).sin();
    linear + (plate - linear) * stall_blend(a, alpha)
}

/// Drag coefficient at angle of attack `alpha`, sideslip `beta` and `mach`.
pub fn drag_coefficient(a: &Airframe, alpha: f32, beta: f32, mach: f32) -> f32 {
    let cl = lift_coefficient(a, alpha);
    let wave = a.wave_drag * smoothstep(a.wave_mach[0], a.wave_mach[1], mach);
    let plate = stall_blend(a, alpha) * alpha.sin().powi(2) + beta.sin().powi(2);
    a.cd0 + wave + a.k_induced * cl * cl + a.cd_plate * plate
}

// ── Simulation state ────────────────────────────────────────────────────────

/// Everything the flight model needs and produces; a plain value so tests
/// can run it headlessly, and the local plane's component in the game.
#[derive(Component, Clone, Copy, Debug)]
pub struct FlightState {
    pub pos: Vec3,
    pub quat: Quat,
    /// World-space velocity [m/s].
    pub vel: Vec3,
    /// Body angular velocity [rad/s].
    pub omega: AngularRates,
    /// Actual control-surface positions (actuator-lagged).
    pub surfaces: Surfaces,
    /// Throttle lever: 0..1 dry, up to [`THROTTLE_MAX`] with boost.
    pub throttle: f32,
    /// What the engine actually delivers, spooling after the lever.
    pub engine: f32,
    /// Angle of attack [rad], positive with the nose above the velocity.
    pub alpha: f32,
    /// Sideslip [rad], positive with the velocity right of the nose.
    pub beta: f32,
    /// Load factor along the plane's up axis [g] (1 in level flight).
    pub g_load: f32,
    pub stalled: bool,
    pub buffet_phase: f32,
}

impl Default for FlightState {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            quat: Quat::IDENTITY,
            vel: Vec3::ZERO,
            omega: AngularRates::default(),
            surfaces: Surfaces::default(),
            throttle: 0.0,
            engine: 0.0,
            alpha: 0.0,
            beta: 0.0,
            g_load: 1.0,
            stalled: false,
            buffet_phase: 0.0,
        }
    }
}

impl FlightState {
    /// A plane at `pos` flying along its nose at `speed`.
    pub fn new(pos: Vec3, quat: Quat, speed: f32, throttle: f32) -> Self {
        Self {
            pos,
            quat,
            vel: quat * Vec3::NEG_Z * speed,
            throttle,
            engine: throttle,
            ..default()
        }
    }

    pub fn forward(&self) -> Vec3 {
        self.quat * Vec3::NEG_Z
    }

    pub fn speed(&self) -> f32 {
        self.vel.length()
    }
}

/// Aerodynamic angles `(alpha, beta)` of `vel` seen from attitude `quat`.
pub fn aero_angles(quat: Quat, vel: Vec3) -> (f32, f32) {
    let speed = vel.length();
    if speed < 0.5 {
        return (0.0, 0.0);
    }
    let v = quat.inverse() * vel;
    ((-v.y).atan2(-v.z), (v.x / speed).clamp(-1.0, 1.0).asin())
}

/// Move `current` toward `target` by at most `max_step`.
fn slew(current: f32, target: f32, max_step: f32) -> f32 {
    current + (target.clamp(-1.0, 1.0) - current).clamp(-max_step, max_step)
}

/// Advance the flight state of airframe `a` by `dt` seconds with the control surfaces
/// commanded to `command` and the throttle lever moving by `throttle_delta`
/// (-1, 0 or 1).
pub fn step(s: &mut FlightState, a: &Airframe, command: &Surfaces, throttle_delta: f32, dt: f32) {
    s.throttle = (s.throttle + throttle_delta * THROTTLE_RATE * dt).clamp(0.0, THROTTLE_MAX);
    s.engine += (s.throttle - s.engine).clamp(-a.spool_down * dt, a.spool_up * dt);

    // Actuators: surfaces travel at a finite speed toward the command.
    let max_step = a.actuator_rate * dt;
    s.surfaces.elevator = slew(s.surfaces.elevator, command.elevator, max_step);
    s.surfaces.aileron = slew(s.surfaces.aileron, command.aileron, max_step);
    s.surfaces.rudder = slew(s.surfaces.rudder, command.rudder, max_step);

    let speed = s.speed();
    let sigma = density_ratio(s.pos.y);
    // Dynamic pressure relative to V_REF at sea level, and its square root.
    let qr = sigma * (speed / V_REF).powi(2);
    let sq = qr.sqrt();
    let eff = control_effectiveness(a, speed);

    let (alpha, beta) = aero_angles(s.quat, s.vel);
    let alpha_rate = wrap_angle(alpha - s.alpha) / dt;
    let beta_rate = (beta - s.beta) / dt;
    s.alpha = alpha;
    s.beta = beta;
    s.stalled = !(a.alpha_stall_neg..=a.alpha_stall).contains(&alpha);
    let stall = stall_blend(a, alpha);
    // Wing stall effects (drop, autorotation) need forward-ish flow over
    // the wing; in a tail slide there is nothing to autorotate.
    let wing_stall = stall * alpha.cos().max(0.0);

    // ── Moments → angular accelerations ────────────────────────────────────
    // Pitch/yaw: weathervane toward the commanded AoA/sideslip, damped
    // against the nose moving relative to the flight path (α̇, β̇).
    let elevator = s.surfaces.elevator;
    let alpha_cmd = eff
        * elevator
        * if elevator >= 0.0 {
            a.elevator_alpha_up
        } else {
            a.elevator_alpha_down
        };
    let beta_cmd = -s.surfaces.rudder * a.rudder_beta * eff;
    // The stall break drops the nose (positive AoA) or raises it (negative).
    // It fades out toward ±90° and is absent in reverse flow, so tail-first
    // flight stays unstable.
    let pitch_break = if alpha.abs() < FRAC_PI_2 {
        -a.stall_pitch_break * stall * (2.0 * alpha).sin()
    } else {
        0.0
    };
    let mut pitch_acc = a.pitch_freq * a.pitch_freq * qr * (alpha_cmd + pitch_break - alpha).sin()
        - 2.0 * a.pitch_damping * a.pitch_freq * sq * alpha_rate
        - RATE_DAMP_FLOOR * s.omega.pitch;
    let yaw_acc = a.yaw_freq * a.yaw_freq * qr * (beta - beta_cmd).sin()
        + 2.0 * a.yaw_damping * a.yaw_freq * sq * beta_rate
        - a.adverse_yaw * qr * eff * s.surfaces.aileron
        - RATE_DAMP_FLOOR * s.omega.yaw;
    // Roll: aileron against roll damping (which a stall undoes), plus the
    // dihedral effect.
    let roll_saturation = (a.autorotation_helix * speed / a.half_span).max(0.1);
    let autorotation =
        a.autorotation * wing_stall * roll_saturation * (s.omega.roll / roll_saturation).tanh();
    let mut roll_acc = (a.roll_rate / a.roll_tau) * qr * eff * s.surfaces.aileron
        - (sq / a.roll_tau) * (s.omega.roll - autorotation)
        - a.dihedral * qr * beta
        + a.roll_from_yaw * sq * s.omega.yaw
        - RATE_DAMP_FLOOR * s.omega.roll;
    // Wing drop: which wing stalls first follows the sideslip and the yaw
    // rate; with neither, the left one (no airframe is rigged perfectly).
    let asymmetry = (20.0 * beta + 3.0 * s.omega.yaw - 0.3).clamp(-1.0, 1.0);
    roll_acc += a.wing_drop * qr * wing_stall * asymmetry;

    // Buffet: airframe shake while stalled, deterministic in phase.
    if s.stalled {
        s.buffet_phase += a.buffet_frequency * dt;
        pitch_acc += a.buffet_amplitude * s.buffet_phase.sin();
        roll_acc += 0.7 * a.buffet_amplitude * (1.7 * s.buffet_phase).sin();
    }

    // ── Forces ─────────────────────────────────────────────────────────────
    let forward = s.quat * Vec3::NEG_Z;
    let up = s.quat * Vec3::Y;
    let right = s.quat * Vec3::X;

    let gravity = Vec3::new(0.0, -GRAVITY, 0.0);
    let mut acc = gravity + forward * thrust(a, s.engine, sigma, speed);
    if speed > 0.5 {
        let v_dir = s.vel / speed;
        let pressure = a.aero_k * sigma * speed * speed;
        // Lift ⊥ velocity, in the symmetry plane (⊥ the wing span).
        let lift_dir = right.cross(v_dir).normalize_or_zero();
        let side_dir = v_dir.cross(lift_dir);
        acc += lift_dir * (pressure * lift_coefficient(a, alpha));
        acc += side_dir * (pressure * a.cy_beta * beta);
        acc -= v_dir * (pressure * drag_coefficient(a, alpha, beta, speed / SPEED_OF_SOUND));
    }
    s.g_load = (acc - gravity).dot(up) / GRAVITY;

    // ── Integrate (semi-implicit Euler) ────────────────────────────────────
    s.omega.pitch += pitch_acc * dt;
    s.omega.yaw += yaw_acc * dt;
    s.omega.roll += roll_acc * dt;
    s.vel += acc * dt;
    s.pos += s.vel * dt;

    // Body rates → local rotation axis: pitch about +X (nose up), yaw about
    // -Y (nose right), roll about -Z (right wing down). Renormalize so
    // rounding error never accumulates into a non-unit quaternion.
    let omega_local = Vec3::new(s.omega.pitch, -s.omega.yaw, -s.omega.roll);
    s.quat = (s.quat * Quat::from_scaled_axis(omega_local * dt)).normalize();

    // Water is not lethal yet: skim along the surface instead.
    if s.pos.y < MIN_ALTITUDE {
        s.pos.y = MIN_ALTITUDE;
        s.vel.y = s.vel.y.max(0.0);
    }
}

// ── ECS shim ────────────────────────────────────────────────────────────────

/// Everything the flight model touches on the local aircraft.
type PlaneParts = (
    &'static Aircraft,
    &'static mut FlightState,
    &'static mut Instructor,
    &'static mut SimPose,
    &'static mut Velocity,
    &'static mut Surfaces,
    &'static mut TickTelemetry,
);

/// One fixed tick of piloted flight: the instructor turns the aim into
/// surface commands, the keyboard overrides single axes, and the model
/// advances. Returns the surface command. The game and the flight-log
/// replay both fly through here, so a replay reproduces the game exactly.
pub fn fly_tick(
    state: &mut FlightState,
    airframe: &Airframe,
    instructor: &mut Instructor,
    input: &FlightInput,
    dt: f32,
) -> Surfaces {
    let aim = input.aim.try_normalize().unwrap_or(state.forward());
    let mut command = instructor.command(state, airframe, aim, !input.limiter_off, dt);
    if let Some(roll) = input.roll_override {
        command.aileron = roll;
    }
    if let Some(yaw) = input.yaw_override {
        command.rudder = yaw;
    }
    step(state, airframe, &command, input.throttle_delta, dt);
    command
}

/// What the last fixed tick fed the flight model, for the flight recorder
/// (native only; the browser build writes it for nobody).
#[derive(Component, Default, Clone, Copy, Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct TickTelemetry {
    pub input: FlightInput,
    pub command: Surfaces,
    pub dt: f32,
}

/// The ECS side of [`fly_tick`] for the local plane.
pub fn step_flight(
    time: Res<Time>,
    input: Res<FlightInput>,
    mut plane: Single<PlaneParts, With<LocalPlane>>,
) {
    let (aircraft, state, instructor, pose, velocity, surfaces, telemetry) = &mut *plane;

    let dt = time.delta_secs();
    let command = fly_tick(state, &aircraft.airframe, instructor, &input, dt);
    **telemetry = TickTelemetry {
        input: *input,
        command,
        dt,
    };

    pose.previous = pose.current;
    pose.current = (state.pos, state.quat);
    velocity.0 = state.vel;
    **surfaces = state.surfaces;
}

/// Deliberately different airframes for tests: the flight model and the
/// instructor must handle every plane that could be added later, not only
/// the one they were tuned on.
#[cfg(test)]
pub mod test_airframes {
    use super::Airframe;
    use aces_protocol::Engine;
    use aces_protocol::aircraft::PLACEHOLDER_JET as JET;

    pub fn all() -> Vec<(&'static str, Airframe)> {
        vec![
            ("jet", JET),
            (
                "heavy",
                Airframe {
                    aero_k: 0.0014,
                    roll_rate: 1.6,
                    roll_tau: 0.35,
                    pitch_freq: 3.5,
                    yaw_freq: 2.5,
                    thrust_mil: 3.5,
                    thrust_boost: 6.0,
                    stiff_speed: 170.0,
                    g_limit: 6.5,
                    half_span: 11.0,
                    ..JET
                },
            ),
            (
                "nimble",
                Airframe {
                    aero_k: 0.0028,
                    roll_rate: 4.8,
                    roll_tau: 0.15,
                    pitch_freq: 6.5,
                    yaw_freq: 4.5,
                    stiff_speed: 160.0,
                    stiff_spread: 100.0,
                    g_limit: 11.0,
                    half_span: 5.0,
                    ..JET
                },
            ),
            (
                "prop",
                Airframe {
                    engine: Engine::Propeller,
                    aero_k: 0.0032,
                    cd0: 0.026,
                    k_induced: 0.09,
                    alpha_stall: 15f32.to_radians(),
                    wave_drag: 0.12,
                    wave_mach: [0.6, 0.8],
                    thrust_mil: 5.5,
                    thrust_boost: 6.3,
                    thrust_lapse: 0.35,
                    thrust_density_exponent: 1.0,
                    spool_up: 1.0,
                    spool_down: 1.0,
                    roll_rate: 2.4,
                    roll_tau: 0.3,
                    stiff_speed: 130.0,
                    stiff_spread: 80.0,
                    g_limit: 8.0,
                    half_span: 5.5,
                    ..JET
                },
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aces_protocol::aircraft::PLACEHOLDER_JET as JET;

    const DT: f32 = 1.0 / 60.0;

    fn cruise_state() -> FlightState {
        FlightState::new(Vec3::new(0.0, 1000.0, 0.0), Quat::IDENTITY, 150.0, 1.0)
    }

    fn run(state: &mut FlightState, command: Surfaces, seconds: f32) {
        for _ in 0..(seconds / DT).round() as usize {
            step(state, &JET, &command, 0.0, DT);
        }
    }

    fn pull(elevator: f32) -> Surfaces {
        Surfaces {
            elevator,
            ..default()
        }
    }

    /// The lift curve is linear below the stall, peaks softly just past it
    /// and sags into flat-plate lift.
    #[test]
    fn lift_curve_stalls_softly() {
        let cl_low = lift_coefficient(&JET, 4f32.to_radians());
        let cl_stall = lift_coefficient(&JET, JET.alpha_stall);
        let cl_deep = lift_coefficient(&JET, 28f32.to_radians());
        assert!((cl_stall - cl_low) > 0.9, "lift curve too flat");
        assert!((1.4..1.7).contains(&cl_stall), "CLmax off: {cl_stall}");
        assert!(
            cl_deep < cl_stall * 0.75,
            "no lift loss past the stall: {cl_deep}"
        );
        // No cliff: one degree past the stall still has most of the lift.
        assert!(lift_coefficient(&JET, JET.alpha_stall + 1f32.to_radians()) > cl_stall * 0.97);
    }

    /// The elevator trims the airframe to an angle of attack: a modest
    /// pull settles near its commanded AoA without oscillating wildly.
    #[test]
    fn elevator_commands_angle_of_attack() {
        let mut state = cruise_state();
        run(&mut state, pull(0.25), 1.5);
        let target = 0.25 * JET.elevator_alpha_up;
        assert!(
            (state.alpha - target).abs() < 1.5f32.to_radians(),
            "alpha {:.1}° vs commanded {:.1}°",
            state.alpha.to_degrees(),
            target.to_degrees()
        );
        assert!(
            state.g_load > 2.0,
            "a 6° pull at 150 m/s must load up, g={}",
            state.g_load
        );
    }

    /// A full pull without the instructor's protection stalls the wing…
    #[test]
    fn unprotected_full_pull_stalls() {
        let mut state = cruise_state();
        run(&mut state, pull(1.0), 3.0);
        assert!(
            state.stalled,
            "never stalled; alpha={:.1}°",
            state.alpha.to_degrees()
        );
    }

    /// …and centering the stick recovers: the nose weathervanes back into
    /// the airflow and the plane flies again.
    #[test]
    fn stall_recovers_hands_off() {
        let mut state = cruise_state();
        run(&mut state, pull(1.0), 3.0);
        run(&mut state, Surfaces::default(), 6.0);
        assert!(
            !state.stalled,
            "did not recover; alpha={:.1}°",
            state.alpha.to_degrees()
        );
        assert!(
            state.speed() > 60.0,
            "speed {} after recovery",
            state.speed()
        );
    }

    /// Full aileron at the reference speed settles near the rated roll rate,
    /// right wing down.
    #[test]
    fn aileron_rolls_at_rated_rate() {
        let mut state = cruise_state();
        let right_before = state.quat * Vec3::X;
        run(
            &mut state,
            Surfaces {
                aileron: 1.0,
                ..default()
            },
            0.2,
        );
        let up_after = state.quat * Vec3::Y;
        assert!(up_after.dot(right_before) > 0.02, "rolled the wrong way");
        run(
            &mut state,
            Surfaces {
                aileron: 1.0,
                ..default()
            },
            0.6,
        );
        assert!(
            (state.omega.roll - JET.roll_rate).abs() < 0.2 * JET.roll_rate,
            "roll rate {} vs rated {}",
            state.omega.roll,
            JET.roll_rate
        );
    }

    /// Stalled with the stick held back, the airframe does not sit still:
    /// the buffet seeds a roll that the collapsed roll damping lets grow
    /// (a wing drops) while the plane sinks.
    #[test]
    fn held_stall_drops_a_wing() {
        let mut state = cruise_state();
        state.vel = state.forward() * 90.0;
        let mut max_roll_rate = 0.0f32;
        let mut max_sink = 0.0f32;
        for _ in 0..(6.0 / DT) as usize {
            step(&mut state, &JET, &pull(1.0), 0.0, DT);
            max_roll_rate = max_roll_rate.max(state.omega.roll.abs());
            max_sink = max_sink.max(-state.vel.y);
        }
        assert!(
            max_roll_rate > 0.5,
            "no wing drop: {max_roll_rate:.2} rad/s"
        );
        assert!(
            max_sink > 10.0,
            "a stalled wing must sink: {max_sink:.1} m/s"
        );
    }

    /// Rolling hard without rudder slips the nose the "wrong" way (adverse
    /// yaw) before weathervaning pulls it back.
    #[test]
    fn uncoordinated_roll_slips() {
        let mut state = cruise_state();
        let mut max_beta = 0.0f32;
        for _ in 0..(1.0 / DT) as usize {
            step(
                &mut state,
                &JET,
                &Surfaces {
                    aileron: 1.0,
                    ..default()
                },
                0.0,
                DT,
            );
            max_beta = max_beta.max(state.beta);
        }
        // Rolling right yaws the nose left: the air comes from the right.
        assert!(
            max_beta > 1f32.to_radians(),
            "no slip: {:.2}°",
            max_beta.to_degrees()
        );
        assert!(
            max_beta < 8f32.to_radians(),
            "slip runaway: {:.2}°",
            max_beta.to_degrees()
        );
    }

    /// The engine spools: slamming the throttle does not deliver thrust
    /// instantly.
    #[test]
    fn engine_spools_after_the_lever() {
        let mut state = FlightState::new(Vec3::new(0.0, 1000.0, 0.0), Quat::IDENTITY, 150.0, 0.2);
        for _ in 0..60 {
            step(&mut state, &JET, &Surfaces::default(), 1.0, DT);
        }
        assert!(state.engine < state.throttle, "no spool lag");
        for _ in 0..(4.0 / DT) as usize {
            step(&mut state, &JET, &Surfaces::default(), 1.0, DT);
        }
        assert!((state.engine - state.throttle).abs() < 1e-4);
    }

    /// Right rudder yaws the nose right and, through the dihedral effect,
    /// banks the plane right as well.
    #[test]
    fn rudder_yaws_and_banks() {
        let mut state = cruise_state();
        run(
            &mut state,
            Surfaces {
                rudder: 1.0,
                ..default()
            },
            1.0,
        );
        let forward = state.forward();
        assert!(forward.x > 0.05, "nose did not swing right: {forward:?}");
        assert!((state.quat * Vec3::X).y < -0.05, "no rudder roll");
    }

    /// Energy: a sustained hard pull bleeds speed; a dive at idle gains it.
    #[test]
    fn turns_bleed_and_dives_gain_energy() {
        let mut state = cruise_state();
        state.vel = state.forward() * 200.0;
        run(&mut state, pull(0.55), 3.0);
        assert!(
            state.speed() < 185.0,
            "a hard pull kept {} m/s",
            state.speed()
        );

        let dive = Quat::from_rotation_x(-45f32.to_radians());
        let mut state = FlightState::new(Vec3::new(0.0, 3000.0, 0.0), dive, 120.0, 0.0);
        run(&mut state, Surfaces::default(), 5.0);
        assert!(
            state.speed() > 145.0,
            "dive only reached {} m/s",
            state.speed()
        );
    }

    /// The afterburner is worth having, and the throttle stays in range.
    #[test]
    fn afterburner_adds_thrust() {
        assert!(thrust(&JET, THROTTLE_MAX, 1.0, 0.0) > thrust(&JET, 1.0, 1.0, 0.0) * 1.5);
        assert!(thrust(&JET, 0.5, 1.0, 0.0) < thrust(&JET, 1.0, 1.0, 0.0));
        assert!(thrust(&JET, 1.0, density_ratio(8000.0), 0.0) < thrust(&JET, 1.0, 1.0, 0.0) * 0.8);

        let mut state = cruise_state();
        for _ in 0..600 {
            step(&mut state, &JET, &Surfaces::default(), 1.0, DT);
        }
        assert_eq!(state.throttle, THROTTLE_MAX);
    }

    /// Actuators are rate limited: a step command takes 1/JET.actuator_rate
    /// seconds to reach full deflection.
    #[test]
    fn surfaces_move_at_actuator_speed() {
        let mut state = cruise_state();
        run(&mut state, pull(1.0), 0.5 / JET.actuator_rate);
        assert!(
            (state.surfaces.elevator - 0.5).abs() < 0.05,
            "{}",
            state.surfaces.elevator
        );
        run(&mut state, pull(1.0), 1.0 / JET.actuator_rate);
        assert_eq!(state.surfaces.elevator, 1.0);
    }

    /// A tail slide (zero airspeed pointing straight up) must not blow up:
    /// the plane falls, weathervanes nose-down and flies again.
    #[test]
    fn tail_slide_recovers() {
        let up = Quat::from_rotation_x(89f32.to_radians());
        let mut state = FlightState::new(Vec3::new(0.0, 3000.0, 0.0), up, 5.0, 0.0);
        run(&mut state, Surfaces::default(), 15.0);
        assert!(state.quat.is_finite() && state.vel.is_finite());
        assert!(
            state.forward().y < 0.0,
            "nose never dropped: {:?}",
            state.forward()
        );
        assert!(
            state.speed() > 60.0,
            "never regained speed: {}",
            state.speed()
        );
    }

    /// Long sessions of constant manoeuvring must not let the attitude
    /// quaternion drift away from unit length.
    #[test]
    fn attitude_stays_normalized() {
        let mut state = cruise_state();
        let command = Surfaces {
            elevator: 0.3,
            aileron: 0.7,
            rudder: 0.4,
        };
        run(&mut state, command, 600.0);
        assert!(
            (state.quat.length() - 1.0).abs() < 1e-5,
            "quat length drifted to {}",
            state.quat.length()
        );
    }

    /// Top speed in level flight at 500 m on airframe `a` at `throttle`.
    fn top_speed(a: &Airframe, throttle: f32) -> f32 {
        let mut state =
            FlightState::new(Vec3::new(0.0, 500.0, 0.0), Quat::IDENTITY, 150.0, throttle);
        for _ in 0..(240.0 / DT) as usize {
            // Trim alpha each step so lift balances gravity.
            let speed = state.speed();
            let cl = GRAVITY / (a.aero_k * density_ratio(state.pos.y) * speed * speed);
            state.quat = Quat::from_rotation_x((cl - a.cl0) / a.cl_alpha);
            state.vel = Vec3::NEG_Z * speed;
            state.pos.y = 500.0;
            step(&mut state, a, &Surfaces::default(), 0.0, DT);
        }
        state.speed()
    }

    /// Design aid: `cargo test -p aces-client performance -- --ignored
    /// --nocapture` prints the envelope of every aircraft (and the test
    /// airframes, for comparison) — check a new airframe's numbers here.
    #[test]
    #[ignore]
    fn performance_report() {
        println!(
            "{:<22} {:>6} {:>6} {:>6} {:>6} {:>7} {:>7}",
            "aircraft [km/h, deg/s]", "stall", "top", "boost", "roll", "turn", "turn+"
        );
        let registry = aces_protocol::AIRCRAFT.iter().map(|k| (k.name, k.airframe));
        let variants = test_airframes::all();
        for (name, a) in registry.chain(variants) {
            // Steady roll rate at full aileron, 150 m/s at sea level.
            let mut state =
                FlightState::new(Vec3::new(0.0, 100.0, 0.0), Quat::IDENTITY, 150.0, 1.0);
            for _ in 0..(1.5 / DT) as usize {
                let command = Surfaces {
                    aileron: 1.0,
                    ..default()
                };
                step(&mut state, &a, &command, 0.0, DT);
                state.vel = state.forward() * 150.0;
            }
            // Turn rates at 150 m/s, sea level: sustained (thrust = drag,
            // dry and boost) from the drag polar, capped by the G limit and
            // the lift the protected AoA gives.
            let q = a.aero_k * 150.0 * 150.0;
            let cl_protected = a.cl0 + a.cl_alpha * (a.alpha_stall - 2f32.to_radians());
            let sustained = |throttle: f32| {
                let cd_max = thrust(&a, throttle, 1.0, 150.0) / q;
                let cl = ((cd_max - a.cd0) / a.k_induced)
                    .max(0.0)
                    .sqrt()
                    .min(cl_protected);
                let n = (q * cl / GRAVITY).min(a.g_limit);
                ((n * n - 1.0).max(0.0).sqrt() * GRAVITY / 150.0).to_degrees()
            };
            println!(
                "{:<22} {:>6.0} {:>6.0} {:>6.0} {:>6.0} {:>7.1} {:>7.1}",
                name,
                a.stall_speed() * 3.6,
                top_speed(&a, 1.0) * 3.6,
                top_speed(&a, THROTTLE_MAX) * 3.6,
                state.omega.roll.to_degrees(),
                sustained(1.0),
                sustained(THROTTLE_MAX),
            );
        }
        println!("\nheld stall, placeholder jet:");

        // A held full-back-stick stall at low speed, then hands off.
        let mut state = FlightState::new(Vec3::new(0.0, 3000.0, 0.0), Quat::IDENTITY, 100.0, 0.6);
        for i in 0..(14.0 / DT) as usize {
            let command = if i as f32 * DT < 7.0 {
                pull(1.0)
            } else {
                Surfaces::default()
            };
            step(&mut state, &JET, &command, 0.0, DT);
            if i % 30 == 0 {
                println!(
                    "t {:4.1} v {:4.0} alt {:5.0} a {:6.1} b {:6.1} p {:6.2} q {:5.2} r {:5.2} fwd.y {:5.2} stall {}",
                    i as f32 * DT,
                    state.speed(),
                    state.pos.y,
                    state.alpha.to_degrees(),
                    state.beta.to_degrees(),
                    state.omega.roll,
                    state.omega.pitch,
                    state.omega.yaw,
                    state.forward().y,
                    state.stalled
                );
            }
        }
    }

    /// Every airframe stalls under an unprotected full pull, recovers
    /// hands-off, and survives a tail slide.
    #[test]
    fn every_airframe_stalls_and_recovers() {
        for (name, a) in test_airframes::all() {
            let speed = 2.0 * a.stall_speed();
            let mut state =
                FlightState::new(Vec3::new(0.0, 3000.0, 0.0), Quat::IDENTITY, speed, 1.0);
            let mut stalled = false;
            for _ in 0..(4.0 / DT) as usize {
                step(&mut state, &a, &pull(1.0), 0.0, DT);
                stalled |= state.stalled;
            }
            assert!(stalled, "{name}: never stalled");
            for _ in 0..(10.0 / DT) as usize {
                step(&mut state, &a, &Surfaces::default(), 0.0, DT);
            }
            assert!(!state.stalled, "{name}: did not recover");
            assert!(
                state.speed() > 1.2 * a.stall_speed(),
                "{name}: {} m/s",
                state.speed()
            );

            let up = Quat::from_rotation_x(89f32.to_radians());
            let mut state = FlightState::new(Vec3::new(0.0, 3000.0, 0.0), up, 5.0, 0.0);
            for _ in 0..(15.0 / DT) as usize {
                step(&mut state, &a, &Surfaces::default(), 0.0, DT);
            }
            assert!(
                state.quat.is_finite() && state.vel.is_finite(),
                "{name}: blew up"
            );
            assert!(state.forward().y < 0.0, "{name}: nose never dropped");
        }
    }

    #[test]
    fn wrap_angle_stays_in_range() {
        for a in [-7.0f32, -PI, -1.0, 0.0, 1.0, PI, 4.0, 9.5] {
            let w = wrap_angle(a);
            assert!(w > -PI - 1e-6 && w <= PI + 1e-6, "{a} → {w}");
            assert!(((w - a) / TAU - ((w - a) / TAU).round()).abs() < 1e-4);
        }
    }
}
