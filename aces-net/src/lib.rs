//! P2P WebRTC networking for aces, following the gnils approach.
//!
//! Scaffolding reused from gnils: matchbox P2P with a public signaling
//! server, one reliable + one unreliable data channel, pet names, room ids,
//! host = lowest-sorted peer id, host-sequenced events applied only in
//! canonical order.
//!
//! What changes versus gnils (see PLAN.md "Netcode"): a flight sim moves
//! continuously, so peers are authoritative over their *own* planes and
//! broadcast snapshots on the unreliable channel; the host-sequenced reliable
//! stream carries only the events that must be agreed on (missile launches,
//! damage claims, kills). This crate gets those pieces in milestone 3; for
//! now it pins the socket stack and the channel layout.

/// Signaling server URL. Overridable at build time via `MATCHBOX_SERVER`.
pub const SIGNALING_SERVER: &str = if let Some(s) = option_env!("MATCHBOX_SERVER") {
    s
} else {
    "wss://omdurman-matchbox.fly.dev"
};

/// Reliable, ordered channel: lobby traffic, sequenced game events.
pub const CH_RELIABLE: usize = 0;
/// Unreliable channel: plane/missile snapshots, ephemeral effects.
pub const CH_UNRELIABLE: usize = 1;
