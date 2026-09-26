//! Wire protocol and shared simulation constants for aces.
//!
//! Deliberately free of any bevy dependency (same pattern as gnils-protocol):
//! everything in here must compile anywhere the client does — native and
//! wasm32 — and define the types that cross the network as well as the
//! constants that flight physics and weapon logic share.

/// Fixed simulation tick rate shared by flight physics and weapon logic
/// (Bevy `Time::<Fixed>` on the client, see PLAN.md "Own-plane authority").
pub const TICK_HZ: f64 = 60.0;
