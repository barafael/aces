//! Wire protocol and shared simulation constants for aces.
//!
//! Deliberately free of any bevy dependency (same pattern as gnils-protocol):
//! everything in here must compile anywhere the client does — native and
//! wasm32 — and define the types that cross the network as well as the
//! constants that flight physics and weapon logic share.

use serde::{Deserialize, Serialize};

/// Fixed simulation tick rate shared by flight physics and weapon logic
/// (Bevy `Time::<Fixed>` on the client, see PLAN.md "Own-plane authority").
pub const TICK_HZ: f64 = 60.0;

/// Rate at which peers broadcast their own plane's state snapshots on the
/// unreliable channel. Remote planes are interpolated between snapshots.
pub const SNAPSHOT_HZ: f32 = 20.0;

pub mod aircraft;
pub use aircraft::{AIRCRAFT, AIRCRAFT_COUNT, AircraftType, Airframe, Combat, Engine, aircraft};

// ── Lobby ───────────────────────────────────────────────────────────────────

/// A room member. Everyone in an aces room flies; there are no seats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerInfo {
    /// Stable within a session; the peer id's string form.
    pub peer: String,
    pub name: String,
    /// Index into [`AIRCRAFT`].
    pub aircraft: u8,
}

// ── Snapshots ───────────────────────────────────────────────────────────────

/// One peer's plane state, broadcast on the unreliable channel. Each peer is
/// authoritative over its own plane (PLAN.md "Netcode"); receivers
/// interpolate remote planes between snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaneSnapshot {
    /// Sender's monotonically increasing counter, for reorder detection.
    pub tick: u32,
    pub pos: [f32; 3],
    /// Orientation quaternion (x, y, z, w).
    pub rot: [f32; 4],
    /// World-space velocity [m/s], used for short-range extrapolation.
    pub vel: [f32; 3],
    /// Control-surface positions (elevator, aileron, rudder), quantized
    /// from [-1, 1] by [`quantize_surface`], so remote airframes animate.
    pub surfaces: [i8; 3],
    /// The sender's health, quantized from 0..=100 to a byte. 0 while dead.
    pub hp: u8,
    /// Landing-gear retraction progress, 0 = down … 255 = up
    /// ([`quantize_unit`]), so remote gear animates too.
    pub gear: u8,
    /// Throttle in percent (past 100 into the boost range), for the remote
    /// engines' exhaust.
    pub throttle: u8,
}

/// Pack a value in [0, 1] into a byte.
pub fn quantize_unit(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Unpack a byte from [`quantize_unit`].
pub fn dequantize_unit(value: u8) -> f32 {
    value as f32 / 255.0
}

/// Pack a control-surface position in [-1, 1] into a byte.
pub fn quantize_surface(value: f32) -> i8 {
    (value.clamp(-1.0, 1.0) * 127.0).round() as i8
}

/// Unpack a byte from [`quantize_surface`].
pub fn dequantize_surface(value: i8) -> f32 {
    (value as f32 / 127.0).clamp(-1.0, 1.0)
}

// ── Spawn points ────────────────────────────────────────────────────────────

/// Number of spawn slots on the starting ring.
pub const SPAWN_COUNT: usize = 8;
/// Radius of the spawn ring [m].
pub const SPAWN_RADIUS: f32 = 3000.0;
/// Altitude of the spawn ring [m].
pub const SPAWN_ALTITUDE: f32 = 600.0;

/// Spawn slot `index` (mod [`SPAWN_COUNT`]): position on a ring around the
/// origin, heading along the ring (counter-clockwise), so a fresh group of
/// players starts circling rather than head-on.
///
/// Returns `(position, yaw)`: yaw is the rotation about +Y that turns the
/// default forward (-Z) onto the flight direction.
pub fn spawn_point(index: usize) -> ([f32; 3], f32) {
    let a = (index % SPAWN_COUNT) as f32 * core::f32::consts::TAU / SPAWN_COUNT as f32;
    let pos = [
        a.cos() * SPAWN_RADIUS,
        SPAWN_ALTITUDE,
        a.sin() * SPAWN_RADIUS,
    ];
    // Tangent of the CCW circle at angle a is (-sin a, 0, cos a); solving
    // Ry(yaw) · -Z = tangent gives yaw = π - a.
    (pos, core::f32::consts::PI - a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surfaces_survive_quantization() {
        for v in [-1.0f32, -0.5, 0.0, 0.33, 1.0] {
            assert!((dequantize_surface(quantize_surface(v)) - v).abs() < 0.005);
        }
        assert_eq!(quantize_surface(7.0), 127);
    }

    #[test]
    fn spawn_points_are_on_the_ring_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..SPAWN_COUNT {
            let (pos, _yaw) = spawn_point(i);
            let r = (pos[0] * pos[0] + pos[2] * pos[2]).sqrt();
            assert!((r - SPAWN_RADIUS).abs() < 1.0, "radius {r} at {i}");
            assert_eq!(pos[1], SPAWN_ALTITUDE);
            assert!(
                seen.insert((pos[0].to_bits(), pos[2].to_bits())),
                "duplicate at {i}"
            );
        }
    }

    /// The yaw must face the flight direction: rotating -Z by yaw yields the
    /// CCW tangent.
    #[test]
    fn spawn_yaw_faces_the_tangent() {
        for i in 0..SPAWN_COUNT {
            let (_pos, yaw) = spawn_point(i);
            let a = i as f32 * core::f32::consts::TAU / SPAWN_COUNT as f32;
            // Ry(yaw) · -Z = (-sin yaw, 0, -cos yaw); with yaw = π - a this
            // is (-sin(π-a), 0, -cos(π-a)) = (-sin a, 0, cos a).
            assert!(((-yaw.sin()) - (-a.sin())).abs() < 1e-5);
            assert!(((-yaw.cos()) - a.cos()).abs() < 1e-5);
        }
    }
}
