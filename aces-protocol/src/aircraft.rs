//! Aircraft types: everything that makes one plane fly differently from
//! another.
//!
//! An aircraft is data. [`Airframe`] holds every number the client's flight
//! model and instructor read — aerodynamics, engine, control response,
//! stall behavior, structural limits — and the flight code itself has no
//! per-plane constants. Adding a plane means adding an entry to
//! [`AIRCRAFT`] (and, from milestone 6, its model).
//!
//! Units are SI with angles in radians. "Specific" forces are accelerations
//! (the model has no explicit mass). Rotational constants are quoted at
//! [`REFERENCE_SPEED`] at sea level; the model scales them with dynamic
//! pressure.

use serde::{Deserialize, Serialize};

/// Speed the airframes' rotational constants are quoted at [m/s].
pub const REFERENCE_SPEED: f32 = 150.0;

/// What drives the plane. Decides what the throttle's boost range is
/// called (afterburner vs war emergency power); the thrust curve itself is
/// in the [`Airframe`] numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Jet,
    Propeller,
}

/// The flight characteristics of one aircraft type.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Airframe {
    // ── Aerodynamics ────────────────────────────────────────────────────────
    /// Aerodynamic acceleration per unit coefficient at sea level, ½·ρ₀·S/m
    /// [1/m]: any aerodynamic acceleration is `aero_k · σ · V² · C`. The
    /// inverse wing loading — bigger means lower stall speed, tighter turns.
    pub aero_k: f32,
    /// Lift at zero angle of attack.
    pub cl0: f32,
    /// Lift curve slope [1/rad].
    pub cl_alpha: f32,
    /// Angle of attack where the lift curve breaks [rad].
    pub alpha_stall: f32,
    /// Negative-g stall angle [rad].
    pub alpha_stall_neg: f32,
    /// Width of the soft transition from attached to flat-plate flow [rad].
    pub stall_band: f32,
    /// Flat-plate lift amplitude, `CL = cl_plate · sin 2α` deep in the stall.
    pub cl_plate: f32,
    /// Parasitic drag coefficient.
    pub cd0: f32,
    /// Induced drag factor, `CD_i = k_induced · CL²`: what makes hard turns
    /// cost speed.
    pub k_induced: f32,
    /// Flat-plate drag (post-stall `sin²α`, sideslip `sin²β`).
    pub cd_plate: f32,
    /// Side force per radian of sideslip (negative: it opposes the slip).
    pub cy_beta: f32,
    /// Extra drag coefficient once the transonic drag rise is complete.
    pub wave_drag: f32,
    /// Mach range over which the wave drag rises.
    pub wave_mach: [f32; 2],

    // ── Engine ──────────────────────────────────────────────────────────────
    pub engine: Engine,
    /// Specific thrust at 100 % throttle, sea level, standing still [m/s²].
    pub thrust_mil: f32,
    /// Specific thrust at full boost (afterburner / WEP), same conditions
    /// [m/s²].
    pub thrust_boost: f32,
    /// Fraction of static thrust lost per [`REFERENCE_SPEED`] of airspeed:
    /// 0 for a jet, around 0.3 for a propeller.
    pub thrust_lapse: f32,
    /// Thrust ∝ σ^this with air density ratio σ.
    pub thrust_density_exponent: f32,
    /// Engine spool-up speed [throttle units per second].
    pub spool_up: f32,
    /// Engine spool-down speed [throttle units per second].
    pub spool_down: f32,

    // ── Control response (quoted at REFERENCE_SPEED, sea level) ────────────
    /// Pitch (short-period) natural frequency [rad/s].
    pub pitch_freq: f32,
    /// Pitch damping ratio.
    pub pitch_damping: f32,
    /// Angle of attack the airframe trims to at full up elevator [rad];
    /// beyond the stall, only the instructor's protection keeps it from
    /// getting there.
    pub elevator_alpha_up: f32,
    /// Angle of attack at full down elevator (magnitude) [rad].
    pub elevator_alpha_down: f32,
    /// Yaw (weathervane) natural frequency [rad/s].
    pub yaw_freq: f32,
    /// Yaw damping ratio.
    pub yaw_damping: f32,
    /// Sideslip the airframe trims to at full rudder [rad].
    pub rudder_beta: f32,
    /// Steady roll rate at full aileron [rad/s].
    pub roll_rate: f32,
    /// Roll-mode time constant [s].
    pub roll_tau: f32,
    /// Dihedral effect: roll acceleration per radian of sideslip [1/s²] —
    /// sideslip rolls the plane away from the relative wind, which is what
    /// lets the rudder bank the plane.
    pub dihedral: f32,
    /// Roll due to yaw rate [1/s]: yawing right speeds up the left wing,
    /// which lifts more and rolls the plane right.
    pub roll_from_yaw: f32,
    /// Adverse yaw: yaw acceleration per unit aileron [rad/s²]. Rolling
    /// right drags the nose left, so uncoordinated rolls slip.
    pub adverse_yaw: f32,
    /// Above this speed the controls start to stiffen [m/s].
    pub stiff_speed: f32,
    /// How quickly they stiffen past `stiff_speed` [m/s].
    pub stiff_spread: f32,
    /// Control-surface actuator speed [full deflections per second].
    pub actuator_rate: f32,

    // ── Stall ───────────────────────────────────────────────────────────────
    /// Past the stall the roll damping collapses and reverses (the
    /// down-going wing sits deeper in the stall and loses more lift): for
    /// small roll rates at full stall the damping is `1 - autorotation` of
    /// normal, so a wing drops and the plane rolls off into a spin unless
    /// the pilot lets go.
    pub autorotation: f32,
    /// The autorotative push saturates once the roll's helix angle
    /// `p·(b/2)/V` reaches this [rad]: spins settle at a finite rate.
    pub autorotation_helix: f32,
    /// Half wingspan [m].
    pub half_span: f32,
    /// Wing drop at full stall [rad/s²]: the wing that stalls first (the
    /// windward one in a sideslip, or the retreating one when yawing) loses
    /// its lift and drops.
    pub wing_drop: f32,
    /// Nose-down pitch break at full stall, as an equivalent AoA [rad]: the
    /// stalled wing's lift moves aft and drops the nose.
    pub stall_pitch_break: f32,
    /// Buffet (airframe shake) frequency while stalled [rad/s].
    pub buffet_frequency: f32,
    /// Buffet amplitude as an angular acceleration [rad/s²].
    pub buffet_amplitude: f32,

    // ── Structure ───────────────────────────────────────────────────────────
    /// Load the instructor's protection holds the plane to [g].
    pub g_limit: f32,
    /// Negative load limit [g].
    pub g_limit_neg: f32,
}

/// One selectable aircraft.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AircraftType {
    pub name: &'static str,
    pub airframe: Airframe,
    /// glTF model (`.glb`), relative to the client's asset root; `None`
    /// shows the built-in low-poly placeholder airframe. The client needs
    /// per-model corrections (scale/orientation) — see its model fixups.
    pub model: Option<&'static str>,
}

/// The placeholder jet everything was tuned on: a ~16 m span fighter,
/// 1 g stall ~206 km/h, ~1020 km/h dry / ~1080 km/h with afterburner.
pub const PLACEHOLDER_JET: Airframe = Airframe {
    aero_k: 0.0020,
    cl0: 0.10,
    cl_alpha: 5.0,
    alpha_stall: 16f32.to_radians(),
    alpha_stall_neg: -12f32.to_radians(),
    stall_band: 8f32.to_radians(),
    cl_plate: 1.2,
    cd0: 0.021,
    k_induced: 0.12,
    cd_plate: 1.3,
    cy_beta: -0.9,
    wave_drag: 0.06,
    wave_mach: [0.78, 1.0],

    engine: Engine::Jet,
    thrust_mil: 5.0,
    thrust_boost: 8.5,
    thrust_lapse: 0.0,
    thrust_density_exponent: 0.7,
    spool_up: 0.35,
    spool_down: 0.5,

    pitch_freq: 5.0,
    pitch_damping: 0.7,
    elevator_alpha_up: 24f32.to_radians(),
    elevator_alpha_down: 14f32.to_radians(),
    yaw_freq: 3.5,
    yaw_damping: 0.7,
    rudder_beta: 8f32.to_radians(),
    roll_rate: 3.2,
    roll_tau: 0.22,
    dihedral: 6.0,
    roll_from_yaw: 1.5,
    adverse_yaw: 1.2,
    stiff_speed: 190.0,
    stiff_spread: 130.0,
    actuator_rate: 4.0,

    autorotation: 2.2,
    autorotation_helix: 0.15,
    half_span: 8.0,
    wing_drop: 6.0,
    stall_pitch_break: 8f32.to_radians(),
    buffet_frequency: 30.0,
    buffet_amplitude: 0.6,

    g_limit: 9.0,
    g_limit_neg: -3.0,
};

/// The selectable aircraft, indexed by [`crate::PlayerInfo::aircraft`].
/// Every airframe is still the placeholder jet — the models arrived before
/// their numbers did; split the airframes as each plane gets tuned.
pub const AIRCRAFT: [AircraftType; 4] = [
    AircraftType {
        name: "Jet (placeholder)",
        airframe: PLACEHOLDER_JET,
        model: None,
    },
    AircraftType {
        name: "F-15E Strike Eagle",
        airframe: PLACEHOLDER_JET,
        model: Some("models/f-15e_strike_eagle_-_fighter_jet_-_free.glb"),
    },
    AircraftType {
        name: "F/A-141F",
        airframe: PLACEHOLDER_JET,
        model: Some("models/f__a-141f_fighter.glb"),
    },
    AircraftType {
        name: "MiG-19",
        airframe: PLACEHOLDER_JET,
        model: Some("models/mikoyan-gurevich_mig-19.glb"),
    },
];

/// Number of selectable aircraft.
pub const AIRCRAFT_COUNT: u8 = AIRCRAFT.len() as u8;

/// Aircraft `index`; unknown indices (e.g. from a newer peer) fall back to
/// the first.
pub fn aircraft(index: u8) -> &'static AircraftType {
    AIRCRAFT.get(usize::from(index)).unwrap_or(&AIRCRAFT[0])
}

impl Airframe {
    /// 1 g stall speed at sea level [m/s], from the lift curve's peak at the
    /// stall angle (the soft stall band adds a little on top).
    pub fn stall_speed(&self) -> f32 {
        let cl_max = self.linear_lift(self.alpha_stall);
        (9.81 / (self.aero_k * cl_max)).sqrt()
    }

    /// Lift coefficient of the attached-flow (linear) lift curve at `alpha`.
    pub fn linear_lift(&self, alpha: f32) -> f32 {
        self.cl0 + self.cl_alpha * alpha
    }

    /// Its inverse: the angle of attack for lift coefficient `cl`.
    pub fn alpha_for_lift(&self, cl: f32) -> f32 {
        (cl - self.cl0) / self.cl_alpha
    }

    /// Angle of attack per unit of elevator deflection in the direction of
    /// `sign` (pull for positive): the flight model maps elevator → AoA
    /// with it, the instructor AoA → elevator.
    pub fn elevator_alpha(&self, sign: f32) -> f32 {
        if sign >= 0.0 {
            self.elevator_alpha_up
        } else {
            self.elevator_alpha_down
        }
    }

    /// The throttle's boost range name: `AB` (afterburner) or `WEP`.
    pub fn boost_label(&self) -> &'static str {
        match self.engine {
            Engine::Jet => "AB",
            Engine::Propeller => "WEP",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_aircraft_fall_back_to_the_first() {
        assert_eq!(aircraft(0).name, AIRCRAFT[0].name);
        assert_eq!(aircraft(200).name, AIRCRAFT[0].name);
    }

    /// Every airframe is internally consistent: a lift curve, a stall
    /// above cruise AoA, thrust that the boost only adds to, sane limits.
    #[test]
    fn airframes_are_consistent() {
        for kind in AIRCRAFT {
            let a = kind.airframe;
            let name = kind.name;
            assert!(a.aero_k > 0.0 && a.cl_alpha > 0.0, "{name}: no lift");
            assert!(
                a.alpha_stall > 0.0 && a.alpha_stall_neg < 0.0,
                "{name}: stall angles"
            );
            assert!(
                a.elevator_alpha_up > a.alpha_stall,
                "{name}: cannot be stalled"
            );
            assert!(
                a.thrust_boost >= a.thrust_mil && a.thrust_mil > 0.0,
                "{name}: thrust"
            );
            assert!(a.g_limit > 1.0 && a.g_limit_neg < 0.0, "{name}: g limits");
            assert!(
                (20.0..120.0).contains(&a.stall_speed()),
                "{name}: stall speed {}",
                a.stall_speed()
            );
        }
    }
}
