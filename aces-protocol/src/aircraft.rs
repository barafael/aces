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

/// What an aircraft fights with: its toughness, gun, missiles and radar.
/// Weapons themselves (missile kinematics, seekers, countermeasures) are
/// the same for everyone; the aircraft decides how much it carries and
/// whether it can guide a radar missile at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Combat {
    /// Hit points of a fresh plane; [`STANDARD_COMBAT`] has 100.
    pub hp: f32,
    /// Damage per gun hit.
    pub gun_damage: f32,
    /// Gun shots per second (0: no gun).
    pub gun_rate: f32,
    /// Heat-seeking missiles carried.
    pub ir_missiles: u8,
    /// Radar-guided missiles carried.
    pub radar_missiles: u8,
    /// Range the radar locks a target from [m]; 0 without a fire-control
    /// radar (radar missiles cannot be guided then).
    pub radar_range: f32,
}

/// The baseline loadout every aircraft is measured against: a 20 mm-class
/// gun at 120 damage per second, 6 heat-seekers, 4 radar missiles and an
/// 8 km radar.
pub const STANDARD_COMBAT: Combat = Combat {
    hp: 100.0,
    gun_damage: 8.0,
    gun_rate: 15.0,
    ir_missiles: 6,
    radar_missiles: 4,
    radar_range: 8000.0,
};

impl Combat {
    /// Gun damage per second of trigger time.
    pub fn gun_dps(&self) -> f32 {
        self.gun_damage * self.gun_rate
    }
}

/// One selectable aircraft.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AircraftType {
    pub name: &'static str,
    pub airframe: Airframe,
    pub combat: Combat,
    /// glTF model (`.glb`), relative to the client's asset root; `None`
    /// shows the built-in low-poly placeholder airframe. The client needs
    /// per-model corrections (scale/orientation) — see its model fixups.
    pub model: Option<&'static str>,
    /// Attribution for a third-party model: its license requires it
    /// wherever the game is shared (shown in game and in `CREDITS.md`).
    pub credit: Option<Credit>,
}

/// Attribution for third-party art, as its Creative Commons license asks:
/// title, author, source and license.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credit {
    pub title: &'static str,
    pub author: &'static str,
    pub source: &'static str,
    /// Short license name, e.g. `CC BY 4.0`.
    pub license: &'static str,
    pub license_url: &'static str,
}

/// Both non-commercial models' license.
const CC_BY_NC_SA_4: (&str, &str) = (
    "CC BY-NC-SA 4.0",
    "https://creativecommons.org/licenses/by-nc-sa/4.0/",
);

/// The other models' license.
const CC_BY_4: (&str, &str) = ("CC BY 4.0", "https://creativecommons.org/licenses/by/4.0/");

/// A CC BY 4.0 model from Sketchfab.
const fn cc_by(title: &'static str, author: &'static str, source: &'static str) -> Option<Credit> {
    Some(Credit {
        title,
        author,
        source,
        license: CC_BY_4.0,
        license_url: CC_BY_4.1,
    })
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

// ── Aircraft airframes ──────────────────────────────────────────────────────
//
// Grounded in each type's real wing loading, thrust-to-weight, aspect ratio,
// G limit and handling reputation, then fitted to research-based game
// targets at 540 km/h near sea level (sustained turn with boost,
// instantaneous turn, top speed with boost; `cargo test -p aces-client
// performance -- --ignored --nocapture` prints them). The game compresses
// reality: specific thrust runs at ~0.8 of the real value, top speeds sit
// between ~880 and ~1250 km/h (real sea-level speeds squeezed toward the
// middle), roll rates at ~0.6 of the real ones. Every airframe struct-updates
// the placeholder, which already is an F-16C, so only the differences show.
/// MiG-15bis (1st generation, 1950). Real: 5.0 t, 20.6 m² (245 kg/m², the
/// lightest wing loading here), VK-1 26.5 kN without afterburner (T/W 0.54),
/// 1076 km/h at sea level, M0.92 limit, +8 g; controls "stuck in cement" above
/// M0.9; spin-prone after a stall; slow centrifugal engine.
/// Game: a subsonic gun fighter — tight low-speed turns and a hard-hitting
/// 37/23 mm battery against no missiles, no afterburner, the lowest top speed
/// and stiff controls at speed.
/// Sources: <https://en.wikipedia.org/wiki/Mikoyan-Gurevich_MiG-15>,
/// <https://www.koreanwaronline.com/arms/sabremig.htm>.
pub const MIG_15BIS: Airframe = Airframe {
    aero_k: 0.00226,
    cl0: 0.08,
    cl_alpha: 4.6,
    alpha_stall: 15.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    cd0: 0.020,
    k_induced: 0.088,
    wave_drag: 0.062,
    wave_mach: [0.72, 0.9],
    thrust_mil: 4.20,
    thrust_boost: 4.20,
    spool_up: 0.2,
    spool_down: 0.3,
    pitch_freq: 4.6,
    elevator_alpha_up: 23.0f32.to_radians(),
    yaw_freq: 3.0,
    roll_rate: 2.90,
    roll_tau: 0.24,
    adverse_yaw: 1.6,
    stiff_spread: 70.0,
    actuator_rate: 3.5,
    autorotation: 3.0,
    half_span: 5.04,
    wing_drop: 9.0,
    stall_pitch_break: 9.0f32.to_radians(),
    g_limit: 8.0,
    ..PLACEHOLDER_JET
};

/// MiG-19S (2nd generation, 1955). Real: 7.6 t, 25 m² (302 kg/m²), 2× RD-9B
/// 51.0 kN dry / 63.6 kN afterburner (T/W 0.86), M1.35 at altitude; "easily
/// out-turned the Phantom" in US tests; spin-prone; engines surge when the
/// throttle is slammed. 3× NR-30 30 mm; the S carried no missiles (the
/// Chinese J-6 carried PL-2 heat-seekers — hence its two).
/// Game: the best sustained turn of the early jets and the heaviest gun, but
/// slow spool, a departure-prone stall and only two heat-seekers, no radar.
/// Sources: <https://en.wikipedia.org/wiki/Mikoyan-Gurevich_MiG-19>,
/// <https://aerospaceweb.org/aircraft/fighter/mig19>.
pub const MIG_19S: Airframe = Airframe {
    aero_k: 0.00221,
    cl0: 0.08,
    cl_alpha: 4.6,
    alpha_stall: 15.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    k_induced: 0.136,
    wave_drag: 0.042,
    thrust_mil: 5.40,
    thrust_boost: 6.73,
    spool_up: 0.25,
    spool_down: 0.4,
    pitch_freq: 4.8,
    elevator_alpha_up: 23.0f32.to_radians(),
    yaw_freq: 3.2,
    roll_rate: 3.07,
    adverse_yaw: 1.4,
    stiff_speed: 200.0,
    stiff_spread: 110.0,
    autorotation: 2.8,
    half_span: 4.5,
    wing_drop: 8.0,
    stall_pitch_break: 9.0f32.to_radians(),
    g_limit: 8.0,
    ..PLACEHOLDER_JET
};

/// MiG-21bis (2nd/3rd generation, 1972). Real: 8.7 t, 23 m² delta (379 kg/m²),
/// R-25-300 40.2 kN dry / 69.6 kN afterburner / 97.4 kN emergency (boost here
/// blends in the emergency rating), 1300 km/h at sea level, 235 m/s climb;
/// instantaneous ~22 °/s but sustained ~12 °/s — the delta bleeds energy;
/// AoA limit 33°; unsealed tanks. GSh-23L, R-60 heat-seekers, R-3R radar
/// missiles, RP-22 radar (~30 km).
/// Game: fast and a strong climber with a good first turn, but the worst
/// energy bleed of the fighters and a fragile airframe.
/// Sources: <https://en.wikipedia.org/wiki/Mikoyan-Gurevich_MiG-21>,
/// <https://www.airandspaceforces.com/article/0610doughnut/>.
pub const MIG_21BIS: Airframe = Airframe {
    aero_k: 0.00194,
    cl0: 0.05,
    cl_alpha: 3.6,
    alpha_stall: 22.0f32.to_radians(),
    alpha_stall_neg: -14.0f32.to_radians(),
    stall_band: 10.0f32.to_radians(),
    k_induced: 0.209,
    wave_drag: 0.101,
    thrust_mil: 3.69,
    thrust_boost: 7.79,
    spool_up: 0.25,
    spool_down: 0.4,
    pitch_freq: 4.6,
    elevator_alpha_up: 30.0f32.to_radians(),
    yaw_freq: 3.0,
    roll_rate: 2.90,
    roll_tau: 0.24,
    adverse_yaw: 1.3,
    stiff_speed: 200.0,
    stiff_spread: 100.0,
    actuator_rate: 3.5,
    autorotation: 2.0,
    half_span: 3.58,
    wing_drop: 5.0,
    stall_pitch_break: 6.0f32.to_radians(),
    g_limit: 8.5,
    g_limit_neg: -2.5,
    ..PLACEHOLDER_JET
};

/// F-5A Freedom Fighter (2nd generation, 1964). Real: 5.2 t, 15.8 m²
/// (330 kg/m²), 2× J85-GE-13 24.2 kN dry / 36.3 kN afterburner (T/W 0.71),
/// M1.4 at altitude, +7.33/−3 g, "high roll rates", responsive J85s.
/// 2× M39 20 mm, 2 AIM-9 on the wingtips, no radar.
/// Game: small, quick-rolling and responsive — a nimble knife fighter, but
/// short on thrust and top speed, with two heat-seekers and no radar.
/// Sources: <https://www.globalsecurity.org/military/systems/aircraft/f-5-specs.htm>,
/// <https://archive.org/download/F5EFFlightManual/F-5E-F-Flight-Manual_text.pdf>.
pub const F_5A: Airframe = Airframe {
    aero_k: 0.00211,
    cl0: 0.08,
    cl_alpha: 4.6,
    alpha_stall: 15.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    k_induced: 0.125,
    wave_drag: 0.176,
    thrust_mil: 3.72,
    thrust_boost: 5.58,
    spool_up: 0.45,
    spool_down: 0.55,
    pitch_freq: 5.2,
    elevator_alpha_up: 23.0f32.to_radians(),
    yaw_freq: 3.4,
    roll_rate: 3.54,
    roll_tau: 0.18,
    adverse_yaw: 1.4,
    stiff_speed: 210.0,
    stiff_spread: 120.0,
    actuator_rate: 4.5,
    autorotation: 2.4,
    half_span: 3.85,
    wing_drop: 7.0,
    g_limit: 7.33,
    ..PLACEHOLDER_JET
};

/// T-38A Talon (supersonic trainer, 1961). Real: 5.4 t, 15.8 m² (339 kg/m²),
/// 2× J85-GE-5 23.8 kN dry / 34.3 kN afterburner, M1.3 at altitude, +7.33 g,
/// famously fast roll, poor slow-speed handling. Unarmed.
/// Game (arcade concession: an AT-38B-style gun pod and two heat-seekers):
/// the fastest roll and snappiest response of all, but the weakest gun,
/// a light airframe and a poor sustained turn.
/// Sources: <https://en.wikipedia.org/wiki/Northrop_T-38_Talon>,
/// <https://www.af.mil/About-Us/Fact-Sheets/Display/Article/104569/t-38-talon/>.
pub const T_38A: Airframe = Airframe {
    aero_k: 0.00201,
    cl0: 0.08,
    cl_alpha: 4.6,
    alpha_stall: 15.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    k_induced: 0.122,
    wave_drag: 0.104,
    thrust_mil: 3.55,
    thrust_boost: 5.12,
    spool_up: 0.45,
    spool_down: 0.55,
    elevator_alpha_up: 23.0f32.to_radians(),
    yaw_freq: 3.4,
    roll_rate: 3.90,
    roll_tau: 0.15,
    adverse_yaw: 1.4,
    stiff_speed: 220.0,
    actuator_rate: 5.0,
    autorotation: 2.5,
    half_span: 3.85,
    wing_drop: 7.0,
    g_limit: 7.33,
    ..PLACEHOLDER_JET
};

/// SR-71A Blackbird (strategic reconnaissance, 1966). Real: 30.6 t empty,
/// 69 t gross, 167 m² (412 kg/m²), 2× J58 222 kN dry / ~296 kN afterburner
/// (T/W 0.44), M3.2 cruise at 80,000 ft, +3.5 g, barely manoeuvrable.
/// Unarmed; the YF-12A interceptor prototype carried 3 AIM-47s and the
/// long-range ASG-18 radar.
/// Game (arcade concession: the YF-12A's radar and missiles): by far the
/// fastest and a long-range radar, but it hardly turns (3.5 g, ~8 °/s
/// sustained) and has no gun and no heat-seekers.
/// Sources: <https://en.wikipedia.org/wiki/Lockheed_SR-71_Blackbird>,
/// <https://en.wikipedia.org/wiki/Lockheed_YF-12>.
pub const SR_71: Airframe = Airframe {
    aero_k: 0.00145,
    cl0: 0.02,
    cl_alpha: 3.0,
    alpha_stall: 18.0f32.to_radians(),
    alpha_stall_neg: -14.0f32.to_radians(),
    stall_band: 10.0f32.to_radians(),
    cd0: 0.014,
    k_induced: 0.213,
    wave_drag: 0.017,
    wave_mach: [0.85, 1.2],
    thrust_mil: 2.96,
    thrust_boost: 3.95,
    spool_up: 0.25,
    spool_down: 0.35,
    pitch_freq: 3.0,
    elevator_alpha_up: 26.0f32.to_radians(),
    elevator_alpha_down: 10.0f32.to_radians(),
    yaw_freq: 2.4,
    roll_rate: 1.64,
    roll_tau: 0.4,
    adverse_yaw: 1.0,
    stiff_speed: 240.0,
    stiff_spread: 160.0,
    actuator_rate: 2.5,
    autorotation: 1.5,
    half_span: 8.47,
    wing_drop: 4.0,
    stall_pitch_break: 6.0f32.to_radians(),
    g_limit: 3.5,
    g_limit_neg: -1.0,
    ..PLACEHOLDER_JET
};

/// Super Étendard (3rd generation strike fighter, 1978). Real: 8.6 t, 28.4 m²
/// (303 kg/m²), Atar 8K-50 49 kN without afterburner (T/W 0.58), M0.98 at
/// low level, 100 m/s climb, +7 g. 2× DEFA 30 mm, 2 Magic heat-seekers;
/// the Agave radar is an anti-ship set with little air-to-air use.
/// Game: agile at low speed with heavy 30 mm guns, but no afterburner (it
/// cannot run or re-engage) and no radar missiles.
/// Sources: <https://en.wikipedia.org/wiki/Dassault-Breguet_Super_%C3%89tendard>,
/// <https://fr.wikipedia.org/wiki/Dassault_Super-%C3%89tendard>.
pub const SUPER_ETENDARD: Airframe = Airframe {
    aero_k: 0.00211,
    cl0: 0.08,
    cl_alpha: 4.6,
    alpha_stall: 15.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    cd0: 0.022,
    k_induced: 0.116,
    wave_drag: 0.045,
    wave_mach: [0.74, 0.92],
    thrust_mil: 4.56,
    thrust_boost: 4.56,
    spool_up: 0.3,
    spool_down: 0.45,
    pitch_freq: 4.8,
    elevator_alpha_up: 23.0f32.to_radians(),
    yaw_freq: 3.2,
    roll_rate: 2.99,
    roll_tau: 0.24,
    adverse_yaw: 1.3,
    stiff_speed: 200.0,
    stiff_spread: 110.0,
    actuator_rate: 3.8,
    autorotation: 2.4,
    half_span: 4.8,
    g_limit: 7.0,
    g_limit_neg: -3.2,
    ..PLACEHOLDER_JET
};

/// Su-25 (3rd generation attack aircraft, 1981). Real: 14.6 t, 30.1 m²
/// (485 kg/m²), 2× R-95Sh 80.4 kN without afterburner (T/W 0.56), 975 km/h
/// at sea level, 60 m/s climb, +6.5 g, ~13 °/s turn at 555 km/h; titanium
/// cockpit tub, 1050 kg of survivability measures. GSh-30-2, 2 R-60
/// heat-seekers for self defence, no radar.
/// Game: the toughest airframe and a devastating gun, but the slowest,
/// weakest climber, with no radar and two heat-seekers.
/// Sources: <https://en.wikipedia.org/wiki/Sukhoi_Su-25>,
/// <https://ru.wikipedia.org/wiki/Су-25>.
pub const SU_25: Airframe = Airframe {
    aero_k: 0.00163,
    cl0: 0.15,
    cl_alpha: 5.2,
    alpha_stall: 14.0f32.to_radians(),
    stall_band: 6.0f32.to_radians(),
    cd0: 0.030,
    k_induced: 0.097,
    wave_drag: 0.025,
    wave_mach: [0.62, 0.8],
    thrust_mil: 4.41,
    thrust_boost: 4.41,
    spool_up: 0.3,
    spool_down: 0.4,
    pitch_freq: 4.0,
    elevator_alpha_up: 22.0f32.to_radians(),
    yaw_freq: 3.0,
    roll_rate: 2.56,
    roll_tau: 0.3,
    adverse_yaw: 1.4,
    stiff_speed: 180.0,
    stiff_spread: 100.0,
    actuator_rate: 3.0,
    half_span: 7.18,
    g_limit: 6.5,
    ..PLACEHOLDER_JET
};

/// MiG-23MLD (3rd generation, 1982). Real: 14.8 t, 37.4 m² (397 kg/m²),
/// R-35-300 83.6 kN dry / 127.5 kN afterburner (T/W 0.88), 1400 km/h at sea
/// level, +8.5 g; instantaneous ~17-18 °/s, sustained ~14-15 °/s (ML manual);
/// "very unstable and liable to depart" at high AoA (HAVE PAD) but
/// tremendous acceleration. GSh-23L, R-24R radar and R-60 heat-seeking
/// missiles, Sapfir-23ML radar.
/// Game: a slashing interceptor — fast, accelerates hard, radar missiles —
/// that loses every sustained turning fight and departs if pushed.
/// Sources: <https://en.wikipedia.org/wiki/Mikoyan-Gurevich_MiG-23>,
/// <https://ru.wikipedia.org/wiki/МиГ-23>.
pub const MIG_23MLD: Airframe = Airframe {
    aero_k: 0.00162,
    cl_alpha: 4.8,
    alpha_stall: 17.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    k_induced: 0.145,
    wave_drag: 0.075,
    thrust_mil: 4.51,
    thrust_boost: 6.87,
    pitch_freq: 4.4,
    elevator_alpha_up: 25.0f32.to_radians(),
    yaw_freq: 3.0,
    roll_rate: 2.73,
    roll_tau: 0.26,
    adverse_yaw: 1.6,
    stiff_speed: 205.0,
    stiff_spread: 120.0,
    actuator_rate: 3.5,
    autorotation: 3.0,
    half_span: 5.5,
    wing_drop: 9.0,
    stall_pitch_break: 10.0f32.to_radians(),
    g_limit: 8.5,
    g_limit_neg: -2.5,
    ..PLACEHOLDER_JET
};

/// F-14A Tomcat (4th generation, 1974). Real: ~27 t, 52.5 m² of wing plus a
/// lifting body (93.6 m² effective), 2× TF30 110 kN dry / 186 kN afterburner
/// (T/W ~0.7), 1470 km/h at sea level, +6.5 g fleet limit; excellent low-speed
/// nose authority; adverse yaw and TF30 compressor stalls at high AoA.
/// M61, 2 AIM-9, AIM-7/AIM-54; the AWG-9 was the longest-ranged radar of
/// its day.
/// Game: the longest radar and most radar missiles, and the tightest
/// low-speed turn of the heavy jets, but underpowered (slow climb) and
/// prone to departing when pulled hard.
/// Sources: <https://en.wikipedia.org/wiki/Grumman_F-14_Tomcat>,
/// <https://en.wikipedia.org/wiki/Pratt_%26_Whitney_TF30>.
pub const F_14A: Airframe = Airframe {
    aero_k: 0.00200,
    cl_alpha: 4.8,
    alpha_stall: 17.0f32.to_radians(),
    stall_band: 7.0f32.to_radians(),
    cd0: 0.025,
    k_induced: 0.095,
    wave_drag: 0.014,
    thrust_mil: 3.26,
    thrust_boost: 5.51,
    spool_up: 0.3,
    spool_down: 0.45,
    pitch_freq: 4.6,
    elevator_alpha_up: 25.0f32.to_radians(),
    yaw_freq: 3.0,
    roll_rate: 2.79,
    roll_tau: 0.28,
    adverse_yaw: 2.0,
    stiff_speed: 210.0,
    actuator_rate: 3.5,
    autorotation: 2.8,
    wing_drop: 8.0,
    g_limit: 6.5,
    ..PLACEHOLDER_JET
};

/// F-15E Strike Eagle (4th generation, 1988). Real: ~26 t in combat with
/// conformal tanks, 56.5 m² (423-513 kg/m²), 2× F100-PW-229 158 kN dry /
/// 259 kN afterburner (T/W 0.91-1.11), 1296 km/h at sea level with conformal
/// tanks, +9 g, departure-resistant. M61, 4 AIM-120 + 4 AIM-9, APG-70/82.
/// Game: big engines, a long radar and the second-toughest airframe, but
/// heavy — a slow roll and a mediocre sustained turn.
/// Sources: <https://en.wikipedia.org/wiki/McDonnell_Douglas_F-15E_Strike_Eagle>.
pub const F_15E: Airframe = Airframe {
    aero_k: 0.00192,
    cd0: 0.024,
    k_induced: 0.129,
    wave_drag: 0.054,
    thrust_mil: 4.86,
    thrust_boost: 7.97,
    spool_up: 0.45,
    spool_down: 0.55,
    pitch_freq: 4.5,
    yaw_freq: 3.2,
    roll_rate: 2.75,
    roll_tau: 0.26,
    adverse_yaw: 1.0,
    stiff_speed: 225.0,
    stiff_spread: 150.0,
    autorotation: 1.6,
    half_span: 6.53,
    wing_drop: 4.0,
    stall_pitch_break: 7.0f32.to_radians(),
    ..PLACEHOLDER_JET
};

/// F-16C Block 50/52 (4th generation, 1991). Real: 12 t, 27.9 m² (431 kg/m²),
/// F110-GE-129 76.3 kN dry / 131.2 kN afterburner (T/W 1.1), 1482 km/h at
/// sea level, +9 g fly-by-wire, ~25° AoA limit; sustained 21.5 °/s and
/// instantaneous 24.9 °/s at sea level. M61, 2-4 AIM-120 + 2 AIM-9, APG-68.
/// Game: the benchmark turn-and-burn fighter (the placeholder jet was tuned
/// on it) — carefree handling, fast roll, strong sustained turn — with a
/// middling radar and two heat-seekers.
/// Sources: <https://en.wikipedia.org/wiki/General_Dynamics_F-16_Fighting_Falcon>,
/// <https://www.f-16.net/forum/viewtopic.php?p=501046>.
pub const F_16C: Airframe = Airframe {
    aero_k: 0.00205,
    k_induced: 0.121,
    wave_drag: 0.055,
    thrust_mil: 5.09,
    thrust_boost: 8.75,
    spool_up: 0.45,
    spool_down: 0.55,
    pitch_freq: 5.5,
    yaw_freq: 3.8,
    roll_rate: 3.37,
    roll_tau: 0.18,
    adverse_yaw: 0.8,
    stiff_speed: 230.0,
    stiff_spread: 150.0,
    actuator_rate: 5.0,
    autorotation: 1.6,
    half_span: 4.98,
    wing_drop: 4.0,
    stall_pitch_break: 7.0f32.to_radians(),
    ..PLACEHOLDER_JET
};

/// JAS 39C Gripen (4.5 generation, 1996). Real: 8.5 t, ~28 m² with the
/// canards, RM12 54 kN dry / 80.5 kN afterburner (T/W 0.97), M1.2 at sea
/// level, +9 g fly-by-wire, ~26° AoA limit; sustained ~20 °/s,
/// instantaneous ~30 °/s. BK-27, 4 AMRAAM/Meteor + 2 IRIS-T, PS-05/A.
/// Game: superb instantaneous turn and roll in a small, hard-to-hit jet, but
/// the least thrust of the modern fighters and a light airframe.
/// Sources: <https://en.wikipedia.org/wiki/Saab_JAS_39_Gripen>,
/// <https://www.saab.com/globalassets/products/aeronautics/gripen-c-series/gripen_c_factsheet.pdf>.
pub const GRIPEN_C: Airframe = Airframe {
    aero_k: 0.00190,
    cl0: 0.08,
    cl_alpha: 4.2,
    alpha_stall: 22.0f32.to_radians(),
    stall_band: 9.0f32.to_radians(),
    k_induced: 0.106,
    wave_drag: 0.091,
    thrust_mil: 5.08,
    thrust_boost: 7.58,
    spool_up: 0.45,
    spool_down: 0.55,
    pitch_freq: 6.0,
    elevator_alpha_up: 30.0f32.to_radians(),
    yaw_freq: 3.8,
    roll_rate: 3.35,
    roll_tau: 0.17,
    adverse_yaw: 0.8,
    stiff_speed: 225.0,
    stiff_spread: 150.0,
    actuator_rate: 5.0,
    autorotation: 1.5,
    half_span: 4.2,
    wing_drop: 3.5,
    stall_pitch_break: 6.0f32.to_radians(),
    ..PLACEHOLDER_JET
};

/// Eurofighter Typhoon Tranche 2/3 (4.5 generation, 2003). Real: 16 t,
/// 51.2 m² (312 kg/m²), 2× EJ200 120 kN dry / 180 kN afterburner (T/W 1.15),
/// supercruise ~M1.1-1.2, 1530 km/h at sea level, +9 g carefree fly-by-wire;
/// sustained ~22-23 °/s, instantaneous ~30 °/s. BK-27, 4 AMRAAM/Meteor +
/// 2 ASRAAM/IRIS-T, Captor radar, PIRATE IRST.
/// Game: the best energy fighter — top speed, climb, sustained turn and
/// radar — but big to hit and with only two heat-seekers.
/// Sources: <https://en.wikipedia.org/wiki/Eurofighter_Typhoon>.
pub const TYPHOON: Airframe = Airframe {
    aero_k: 0.00190,
    cl0: 0.08,
    cl_alpha: 4.2,
    alpha_stall: 22.0f32.to_radians(),
    stall_band: 9.0f32.to_radians(),
    k_induced: 0.106,
    wave_drag: 0.054,
    thrust_mil: 6.00,
    thrust_boost: 9.00,
    spool_up: 0.5,
    spool_down: 0.6,
    pitch_freq: 5.8,
    elevator_alpha_up: 30.0f32.to_radians(),
    yaw_freq: 3.8,
    roll_rate: 3.18,
    roll_tau: 0.18,
    adverse_yaw: 0.8,
    stiff_speed: 235.0,
    stiff_spread: 160.0,
    actuator_rate: 5.0,
    autorotation: 1.5,
    half_span: 5.48,
    wing_drop: 3.5,
    stall_pitch_break: 6.0f32.to_radians(),
    ..PLACEHOLDER_JET
};

/// Su-47 Berkut (5th generation demonstrator, 1997). Real: ~25.7 t normal
/// (28 t here as a notional armed fighter), 56 m² forward-swept wing,
/// 2× D-30F6 ~186 kN dry / ~306 kN afterburner, tested to ~M1.6, +9 g,
/// controllable at 45°+ AoA — the ailerons keep working. Never armed; the
/// notional fit is a GSh-30-1 and an internal bay of R-77s and short-range
/// heat-seekers.
/// Game: unmatched nose authority and a benign stall, with strong thrust,
/// but heavy, slow-rolling and with a middling top speed.
/// Sources: <https://en.wikipedia.org/wiki/Sukhoi_Su-47>,
/// <http://testpilot.ru/russia/sukhoi/s/37/s37.php>.
pub const SU_47: Airframe = Airframe {
    aero_k: 0.00163,
    cl_alpha: 4.8,
    alpha_stall: 24.0f32.to_radians(),
    stall_band: 12.0f32.to_radians(),
    k_induced: 0.098,
    wave_drag: 0.147,
    thrust_mil: 5.31,
    thrust_boost: 8.74,
    spool_up: 0.4,
    pitch_freq: 6.0,
    elevator_alpha_up: 34.0f32.to_radians(),
    yaw_freq: 3.6,
    roll_rate: 2.94,
    adverse_yaw: 1.0,
    stiff_speed: 215.0,
    stiff_spread: 140.0,
    actuator_rate: 4.5,
    autorotation: 1.2,
    half_span: 8.35,
    wing_drop: 3.0,
    stall_pitch_break: 4.0f32.to_radians(),
    ..PLACEHOLDER_JET
};

/// F/A-141F (fictional twin-engine naval strike fighter; the model is based
/// on concept art). Numbers after a Super Hornet/F-35C-class jet: 21 t,
/// ~46 m², ~124 kN dry / ~196 kN afterburner, +8 g naval limit.
/// Game: a well-rounded modern fighter with a long radar and a tough
/// airframe, but the naval weight dulls its climb and turn.
pub const FA_141F: Airframe = Airframe {
    aero_k: 0.00205,
    k_induced: 0.117,
    wave_drag: 0.044,
    thrust_mil: 4.72,
    thrust_boost: 7.47,
    spool_up: 0.45,
    spool_down: 0.55,
    pitch_freq: 5.2,
    yaw_freq: 3.6,
    roll_rate: 3.07,
    roll_tau: 0.2,
    adverse_yaw: 0.9,
    stiff_speed: 225.0,
    stiff_spread: 150.0,
    actuator_rate: 4.5,
    autorotation: 1.6,
    half_span: 6.8,
    wing_drop: 4.0,
    stall_pitch_break: 7.0f32.to_radians(),
    g_limit: 8.0,
    ..PLACEHOLDER_JET
};

/// The selectable aircraft, indexed by [`crate::PlayerInfo::aircraft`]. Each
/// has its own airframe (above) and combat stores: guns as damage per hit ×
/// shots per second against the standard 120 per second, missiles and radar
/// after the real type's armament (see the airframes' notes, including the
/// arcade concessions).
pub const AIRCRAFT: [AircraftType; 17] = [
    AircraftType {
        name: "Jet (placeholder)",
        airframe: PLACEHOLDER_JET,
        combat: STANDARD_COMBAT,
        model: None,
        credit: None,
    },
    AircraftType {
        name: "F-15E Strike Eagle",
        airframe: F_15E,
        combat: Combat {
            hp: 125.0,
            gun_damage: 8.0,
            gun_rate: 15.0,
            ir_missiles: 4,
            radar_missiles: 4,
            radar_range: 10_000.0,
        },
        model: Some("models/f-15e_strike_eagle_-_fighter_jet_-_free.glb"),
        credit: Some(Credit {
            title: "F-15E Strike Eagle - Fighter Jet - Free",
            author: "bohmerang",
            source: "https://sketchfab.com/3d-models/f-15e-strike-eagle-fighter-jet-free-fff7d75490474e9b964d90cc031c8d01",
            license: CC_BY_NC_SA_4.0,
            license_url: CC_BY_NC_SA_4.1,
        }),
    },
    AircraftType {
        name: "F/A-141F",
        airframe: FA_141F,
        combat: Combat {
            hp: 110.0,
            gun_damage: 8.0,
            gun_rate: 15.0,
            ir_missiles: 2,
            radar_missiles: 4,
            radar_range: 9_000.0,
        },
        model: Some("models/f__a-141f_fighter.glb"),
        credit: Some(Credit {
            title: "F / A-141F fighter",
            author: "小微流 (jiagoushi)",
            source: "https://sketchfab.com/3d-models/f-a-141f-fighter-bb4fa6ed9fef4a52b119e80748327276",
            license: CC_BY_NC_SA_4.0,
            license_url: CC_BY_NC_SA_4.1,
        }),
    },
    AircraftType {
        name: "MiG-19",
        airframe: MIG_19S,
        combat: Combat {
            hp: 100.0,
            gun_damage: 13.0,
            gun_rate: 13.0,
            ir_missiles: 2,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/mikoyan-gurevich_mig-19.glb"),
        credit: cc_by(
            "Mikoyan-gurevich mig-19",
            "Chenchanchong",
            "https://sketchfab.com/3d-models/mikoyan-gurevich-mig-19-056bde58c01345c59793aaac7e1764bc",
        ),
    },
    AircraftType {
        name: "MiG-23MLD",
        airframe: MIG_23MLD,
        combat: Combat {
            hp: 100.0,
            gun_damage: 8.0,
            gun_rate: 16.0,
            ir_missiles: 4,
            radar_missiles: 2,
            radar_range: 7_000.0,
        },
        model: Some("models/mig-23_mld.glb"),
        credit: cc_by(
            "Mig-23 MLD",
            "Tim Samedov (citizensnip)",
            "https://sketchfab.com/3d-models/mig-23-mld-7a13c91f07e042a685b4d265644fdc06",
        ),
    },
    AircraftType {
        name: "JAS 39 Gripen",
        airframe: GRIPEN_C,
        combat: Combat {
            hp: 90.0,
            gun_damage: 10.0,
            gun_rate: 13.0,
            ir_missiles: 2,
            radar_missiles: 4,
            radar_range: 8_500.0,
        },
        model: Some("models/jas39_gripen.glb"),
        credit: cc_by(
            "JAS39  Gripen",
            "helijah",
            "https://sketchfab.com/3d-models/jas39-gripen-a2b70c2f92af45d18d95f02b60621dbf",
        ),
    },
    AircraftType {
        name: "T-38 Talon",
        airframe: T_38A,
        combat: Combat {
            hp: 85.0,
            gun_damage: 6.0,
            gun_rate: 12.0,
            ir_missiles: 2,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/northrop_t-38_talon.glb"),
        credit: cc_by(
            "Northrop T-38 Talon",
            "helijah",
            "https://sketchfab.com/3d-models/northrop-t-38-talon-d5d22b4b37944f0e9bb1e77ede028f2f",
        ),
    },
    AircraftType {
        name: "Su-25",
        airframe: SU_25,
        combat: Combat {
            hp: 160.0,
            gun_damage: 14.0,
            gun_rate: 12.0,
            ir_missiles: 2,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/su-25.glb"),
        credit: cc_by(
            "Su-25",
            "tnikita",
            "https://sketchfab.com/3d-models/su-25-88b71eb848cf4418a95dff497c07cefc",
        ),
    },
    AircraftType {
        name: "MiG-21",
        airframe: MIG_21BIS,
        combat: Combat {
            hp: 85.0,
            gun_damage: 8.0,
            gun_rate: 16.0,
            ir_missiles: 4,
            radar_missiles: 2,
            radar_range: 4_000.0,
        },
        model: Some("models/mig-21_fishbed_-_cold_war_era_fighter_-_free.glb"),
        credit: cc_by(
            "MIG-21 Fishbed - cold war era fighter - free",
            "NETRUNNER_pl",
            "https://sketchfab.com/3d-models/mig-21-fishbed-cold-war-era-fighter-free-18927e007c3b47ed9e676f88b4adb578",
        ),
    },
    AircraftType {
        name: "F-14 Tomcat",
        airframe: F_14A,
        combat: Combat {
            hp: 115.0,
            gun_damage: 8.0,
            gun_rate: 15.0,
            ir_missiles: 2,
            radar_missiles: 6,
            radar_range: 11_000.0,
        },
        model: Some("models/f-14_tomcat.glb"),
        credit: cc_by(
            "F-14 TOMCAT",
            "Ryan.Qin",
            "https://sketchfab.com/3d-models/f-14-tomcat-082e081ecea94a6aaa8c7bb72ec9136b",
        ),
    },
    AircraftType {
        name: "F-16C Fighting Falcon",
        airframe: F_16C,
        combat: Combat {
            hp: 100.0,
            gun_damage: 8.0,
            gun_rate: 15.0,
            ir_missiles: 2,
            radar_missiles: 4,
            radar_range: 8_000.0,
        },
        model: Some("models/f16-c_falcon.glb"),
        credit: cc_by(
            "F16-C Falcon",
            "Carlos.Maciel",
            "https://sketchfab.com/3d-models/f16-c-falcon-4bc2ff75dc584af2afd0aa6bd8b79015",
        ),
    },
    AircraftType {
        name: "Eurofighter Typhoon",
        airframe: TYPHOON,
        combat: Combat {
            hp: 105.0,
            gun_damage: 10.0,
            gun_rate: 13.0,
            ir_missiles: 2,
            radar_missiles: 4,
            radar_range: 9_500.0,
        },
        model: Some("models/eurofighter_typhoon_game_prop.glb"),
        credit: cc_by(
            "Eurofighter Typhoon Game Prop",
            "robnewman76",
            "https://sketchfab.com/3d-models/eurofighter-typhoon-game-prop-01d9a26a89dc4a17a9fa4c4c1f7ac39f",
        ),
    },
    AircraftType {
        name: "SR-71 Blackbird",
        airframe: SR_71,
        combat: Combat {
            hp: 120.0,
            gun_damage: 0.0,
            gun_rate: 0.0,
            ir_missiles: 0,
            radar_missiles: 3,
            radar_range: 12_000.0,
        },
        model: Some("models/lockheed_sr-71_blackbird.glb"),
        credit: cc_by(
            "Lockheed SR-71 \"Blackbird\"",
            "KOG_THORNS (ioai25312)",
            "https://sketchfab.com/3d-models/lockheed-sr-71-blackbird-e2400e6119f5414c89e075654a82d30a",
        ),
    },
    AircraftType {
        name: "MiG-15",
        airframe: MIG_15BIS,
        combat: Combat {
            hp: 110.0,
            gun_damage: 18.0,
            gun_rate: 7.0,
            ir_missiles: 0,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/mig-15.glb"),
        credit: cc_by(
            "Mig-15",
            "Vermishel",
            "https://sketchfab.com/3d-models/mig-15-db02d092b7a344339a3e60d2dc0f6f39",
        ),
    },
    AircraftType {
        name: "F-5 Freedom Fighter",
        airframe: F_5A,
        combat: Combat {
            hp: 95.0,
            gun_damage: 7.0,
            gun_rate: 16.0,
            ir_missiles: 2,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/northrop_f-5_freedom_fighter.glb"),
        credit: cc_by(
            "Northrop F-5 Freedom Fighter",
            "Pan_Ar4ik (rave.Ar4ik)",
            "https://sketchfab.com/3d-models/northrop-f-5-freedom-fighter-8074c87c10fa47ef909bac55d21cc789",
        ),
    },
    AircraftType {
        name: "Super Étendard",
        airframe: SUPER_ETENDARD,
        combat: Combat {
            hp: 100.0,
            gun_damage: 12.0,
            gun_rate: 12.0,
            ir_missiles: 2,
            radar_missiles: 0,
            radar_range: 0.0,
        },
        model: Some("models/super-etendard.glb"),
        credit: cc_by(
            "super-etendard",
            "helijah",
            "https://sketchfab.com/3d-models/super-etendard-3589004316f54dba90ed7f34455eeeb2",
        ),
    },
    AircraftType {
        name: "Su-47 Berkut",
        airframe: SU_47,
        combat: Combat {
            hp: 110.0,
            gun_damage: 12.0,
            gun_rate: 11.0,
            ir_missiles: 4,
            radar_missiles: 4,
            radar_range: 9_000.0,
        },
        model: Some("models/su-47_berkut.glb"),
        credit: cc_by(
            "Su-47 Berkut",
            "Carlos.Maciel",
            "https://sketchfab.com/3d-models/su-47-berkut-4a2b1cecf13c4c9db7933ffd7fd67339",
        ),
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

    /// Every third-party model is credited — in the registry (shown in
    /// game) and in `CREDITS.md` — with its author, source and license.
    #[test]
    fn every_model_is_credited() {
        let credits_md = include_str!("../../CREDITS.md");
        for kind in AIRCRAFT {
            let Some(model) = kind.model else { continue };
            let credit = kind
                .credit
                .unwrap_or_else(|| panic!("{} ({model}) has no credit", kind.name));
            for part in [
                credit.title,
                credit.author,
                credit.source,
                credit.license,
                credit.license_url,
            ] {
                assert!(
                    credits_md.contains(part),
                    "CREDITS.md does not mention {part:?} for {}",
                    kind.name
                );
            }
        }
    }

    /// Every aircraft's combat stores make sense: hit points fit the
    /// snapshot's byte, a gun has damage, and radar missiles come with a
    /// radar to guide them.
    #[test]
    fn combat_is_consistent() {
        for kind in AIRCRAFT {
            let (c, name) = (kind.combat, kind.name);
            assert!((1.0..=255.0).contains(&c.hp), "{name}: {} hp", c.hp);
            assert!(c.gun_rate >= 0.0 && c.gun_damage >= 0.0, "{name}: gun");
            assert!(
                (c.gun_rate > 0.0) == (c.gun_damage > 0.0),
                "{name}: a gun needs both damage and a rate of fire"
            );
            assert!(
                c.radar_missiles == 0 || c.radar_range > 0.0,
                "{name}: radar missiles without a radar"
            );
        }
    }

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
