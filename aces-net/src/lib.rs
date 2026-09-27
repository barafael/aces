//! P2P WebRTC networking for aces, following the gnils approach.
//!
//! Scaffolding reused from gnils: matchbox P2P with a public signaling
//! server, one reliable + one unreliable data channel, postcard-encoded
//! messages, pet names, room ids, host = lowest-sorted peer id, and
//! host-sequenced events applied only in canonical order.
//!
//! What differs from gnils (see PLAN.md "Netcode"): a flight sim moves
//! continuously, so peers are authoritative over their *own* planes —
//! - reliable channel: lobby (`Hello`, `Roster`, `Start`) and sequenced
//!   game events (`Game` submissions sequenced by the host into
//!   `Sequenced` rebroadcasts; empty event set until milestone 4),
//! - unreliable channel: [`NetMsg::Snapshot`] state broadcasts, which every
//!   receiver interpolates. Snapshots are fire-and-forget; the newest wins.
//!
//! The host is the roster authority; guests take its `Roster`/`Start`
//! broadcasts verbatim.

use aces_protocol::{PlaneSnapshot, PlayerInfo};
use bevy::prelude::*;
use matchbox_socket::{MessageLoopFuture, RtcIceServerConfig, WebRtcSocket, WebRtcSocketBuilder};
use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};

/// Signaling server URL. Overridable at build time via `MATCHBOX_SERVER`.
pub const SIGNALING_SERVER: &str = if let Some(s) = option_env!("MATCHBOX_SERVER") {
    s
} else {
    "wss://omdurman-matchbox.fly.dev"
};

/// Reliable, ordered channel: lobby traffic, sequenced game events.
pub const CH_RELIABLE: usize = 0;
/// Unreliable channel: plane snapshots (newest wins).
pub const CH_UNRELIABLE: usize = 1;

pub use matchbox_socket::{ChannelConfig, PeerId, PeerState};

// ── Game events (the only things that mutate a running game) ────────────────

/// Semantic in-game events, host-sequenced. Empty until milestone 4 (guns,
/// missile launches, damage claims); the sequencing plumbing exists so
/// variants can be added without reshaping the wire format.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum GameEvent {}

// ── Wire protocol ───────────────────────────────────────────────────────────

/// Top-level wire envelope.
///
/// Guests submit (`Hello`, `Game`), the host is the one authority, and what
/// everyone applies is only ever the host's answer (a `Roster`, a `Start`,
/// or a `Sequenced` event). Snapshots are peer-to-peer, no host involved.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum NetMsg {
    /// Introduce yourself on join (and again after renaming or changing
    /// aircraft in the lobby).
    Hello { name: String, aircraft: u8 },
    /// Host -> all: the full roster, whenever it changes.
    Roster(Vec<PlayerInfo>),
    /// Host -> all: the game begins. Carries the final roster and a seed for
    /// future deterministic world generation.
    Start { seed: u64, players: Vec<PlayerInfo> },
    /// Guest -> host: an unsequenced event submission. Never applied
    /// directly.
    Game(GameEvent),
    /// Host -> all (and host loopback): the canonical ordered form. The only
    /// form that is applied.
    Sequenced { seq: u32, event: GameEvent },
    /// Any -> all, unreliable: the sender's own plane state.
    Snapshot(PlaneSnapshot),
}

/// Encode a `NetMsg` for the wire. Returns `None` if encoding fails or would
/// produce a zero-length payload. WebRTC data channels may silently drop a
/// zero-byte payload, so we refuse to emit one entirely.
pub fn enc_msg(msg: &NetMsg) -> Option<Box<[u8]>> {
    match postcard::to_allocvec(msg) {
        Ok(v) if !v.is_empty() => Some(v.into_boxed_slice()),
        Ok(_) => {
            error!("postcard produced an empty NetMsg encoding; dropping");
            None
        }
        Err(e) => {
            error!("postcard encode failed: {e}");
            None
        }
    }
}

pub fn decode(raw: &[u8]) -> Option<NetMsg> {
    postcard::from_bytes(raw)
        .inspect_err(|e| warn!("matchbox decode error: {e}"))
        .ok()
}

// ── Net state ───────────────────────────────────────────────────────────────

/// The payload of a [`NetMsg::Start`], ready to be applied.
#[derive(Debug, Clone)]
pub struct GameStart {
    pub seed: u64,
    pub players: Vec<PlayerInfo>,
}

#[derive(Resource, Default)]
pub struct NetState {
    pub peers: Vec<PeerId>,
    pub my_id: Option<PeerId>,
    pub is_host: bool,
    /// The name this peer goes by in the roster. Survives `leave_room`.
    pub name: String,
    /// The aircraft this peer has selected (roster index).
    pub aircraft: u8,
    /// The lobby roster / in-game player list. On the host it is the
    /// authority; guests take the host's broadcasts verbatim.
    pub players: Vec<PlayerInfo>,
    /// Set once a `Start` has been applied (or locally when acting as host).
    pub start: Option<GameStart>,
    /// Peers we have already sent our [`NetMsg::Hello`] to. Without this a
    /// per-frame greet loop would flood the channel; disconnected peers may
    /// stay listed here — a reconnect arrives with a fresh id anyway.
    pub greeted: Vec<PeerId>,
    /// Host-only: the next canonical sequence number to assign.
    pub next_seq: u32,
    /// Highest sequence number applied locally, so a duplicate delivery is
    /// never applied twice. `None` until the first event.
    pub last_applied_seq: Option<u32>,
    /// All peers including `my_id`, in canonical sorted order.
    sorted_all: Vec<PeerId>,
}

impl NetState {
    /// A fresh state that already goes by `name`.
    pub fn with_name(name: String) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// Rebuild `sorted_all` from `peers` + `my_id`. Call after any mutation.
    pub fn refresh_sorted(&mut self) {
        self.sorted_all.clear();
        self.sorted_all.extend(self.peers.iter().copied());
        if let Some(me) = self.my_id {
            self.sorted_all.push(me);
        }
        self.sorted_all.sort();
    }

    /// Canonical sorted list of all peers including the local player.
    pub fn sorted_all(&self) -> &[PeerId] {
        &self.sorted_all
    }

    /// The canonical host: the lowest-sorted peer id across everyone. Re-derived
    /// on every peer change, so a guest is promoted automatically if the host
    /// disconnects.
    pub fn host_id(&self) -> Option<PeerId> {
        self.sorted_all.first().copied()
    }

    /// Is `peer` (a `PeerId`'s string form) this local peer?
    pub fn is_me(&self, peer: &str) -> bool {
        self.my_id.is_some_and(|id| id.to_string() == peer)
    }

    /// This peer's roster entry, if present.
    pub fn me(&self) -> Option<&PlayerInfo> {
        let id = self.my_id?.to_string();
        self.players.iter().find(|p| p.peer == id)
    }

    /// My slot index in the roster: the spawn point index too.
    pub fn my_index(&self) -> Option<usize> {
        let id = self.my_id?.to_string();
        self.players.iter().position(|p| p.peer == id)
    }

    /// Forget everything the current room's socket established — peers, id,
    /// host flag, roster, start, sequence counters — but keep the player's
    /// own name and aircraft choice, which identify the player.
    pub fn leave_room(&mut self) {
        let (name, aircraft) = (std::mem::take(&mut self.name), self.aircraft);
        *self = Self::with_name(name);
        self.aircraft = aircraft;
    }
}

// ── Room ids ────────────────────────────────────────────────────────────────

/// Why a room name was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomIdError {
    Empty,
    TooLong {
        len: usize,
    },
    /// The offending character, so the message can name it rather than
    /// saying "invalid".
    BadChar(char),
}

impl core::fmt::Display for RoomIdError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RoomIdError::Empty => write!(f, "a room name cannot be empty"),
            RoomIdError::TooLong { len } => write!(
                f,
                "a room name may be at most {} characters, got {len}",
                RoomId::MAX_LEN
            ),
            RoomIdError::BadChar(c) => write!(
                f,
                "'{c}' is not allowed in a room name; use letters, digits, '-' or '_'"
            ),
        }
    }
}

impl core::error::Error for RoomIdError {}

/// The room to join. Peers sharing a room id find each other.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct RoomId(pub String);

impl RoomId {
    /// Long enough for a descriptive name, short enough to type and to read
    /// back to someone over the phone.
    pub const MAX_LEN: usize = 40;

    /// Validate a room name handed over by a share link or typed by a player.
    ///
    /// The name is interpolated into the signaling URL's path, so it is
    /// restricted to characters that need no escaping: letters, digits, `-`
    /// and `_`. That rules out `/` (which would silently redirect the socket
    /// to a different path), whitespace (invisible in a name two people are
    /// trying to match) and non-ASCII (homoglyph/normalisation mismatch).
    pub fn parse(name: &str) -> Result<Self, RoomIdError> {
        if name.is_empty() {
            return Err(RoomIdError::Empty);
        }
        if name.chars().count() > Self::MAX_LEN {
            return Err(RoomIdError::TooLong {
                len: name.chars().count(),
            });
        }
        if let Some(c) = name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            return Err(RoomIdError::BadChar(c));
        }
        Ok(Self(name.to_string()))
    }
}

/// Room adjectives: the memorable half of a generated room name.
const ROOM_ADJECTIVES: &[&str] = &[
    "amber", "brave", "brisk", "calm", "clever", "cozy", "dandy", "eager", "fussy", "gentle",
    "glad", "happy", "hazy", "jolly", "keen", "lively", "lucky", "mellow", "merry", "mild",
    "noble", "plucky", "quiet", "rapid", "rustic", "shiny", "silent", "smart", "snug", "spry",
    "stellar", "sunny", "swift", "tender", "tidy", "upbeat", "valiant", "vivid", "warm", "witty",
];

/// Room nouns: the second half of a generated room name. Deliberately
/// distinct from [`PET_NAMES`], so "room otter" and "player otter" never meet.
const ROOM_NOUNS: &[&str] = &[
    "anchor", "basin", "beacon", "birch", "blossom", "canyon", "cedar", "cliff", "cinder", "coral",
    "dawn", "delta", "dune", "fjord", "fern", "forest", "harbor", "horizon", "island", "ivy",
    "lagoon", "lantern", "moss", "meadow", "mesa", "mist", "orbit", "orchid", "pine", "pond",
    "ripple", "reef", "river", "shore", "spring", "star", "summit", "tundra", "valley", "zephyr",
];

/// A freshly generated room id: a petname — "swift-harbor-42" — that survives
/// being said aloud across a table, plus two digits. The word pair alone
/// gives 1600 rooms; the digits lift the space past 160k.
pub fn random_room() -> String {
    let adjective = ROOM_ADJECTIVES[rand::random_range(0..ROOM_ADJECTIVES.len())];
    let noun = ROOM_NOUNS[rand::random_range(0..ROOM_NOUNS.len())];
    let digits: u32 = rand::random_range(0..100);
    format!("{adjective}-{noun}-{digits:02}")
}

/// Short pet names a player is born with.
const PET_NAMES: &[&str] = &[
    "otter", "falcon", "maple", "ember", "comet", "panda", "lynx", "heron", "quail", "gecko",
    "koala", "raven", "tiger", "bison", "crane", "dingo", "eagle", "ibex", "orca", "yak", "hare",
    "moth", "wren", "toad", "newt", "elk", "fox", "owl", "bee", "finch", "mole", "starling",
    "puffin", "badger", "marten", "vole",
];

/// The pet name a session goes by until the player picks their own.
pub fn petname() -> String {
    PET_NAMES[rand::random_range(0..PET_NAMES.len())].to_string()
}

// ── Socket resource ─────────────────────────────────────────────────────────

/// A [`WebRtcSocket`] as a Bevy resource. Same shape as bevy_matchbox's
/// `MatchboxSocket`, but implemented against `matchbox_socket` directly.
#[derive(Resource)]
pub struct MatchboxSocket(WebRtcSocket);

impl Deref for MatchboxSocket {
    type Target = WebRtcSocket;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for MatchboxSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<WebRtcSocketBuilder> for MatchboxSocket {
    fn from(builder: WebRtcSocketBuilder) -> Self {
        Self::from(builder.build())
    }
}

impl From<(WebRtcSocket, MessageLoopFuture)> for MatchboxSocket {
    fn from((socket, message_loop_fut): (WebRtcSocket, MessageLoopFuture)) -> Self {
        spawn_message_loop(message_loop_fut);
        MatchboxSocket(socket)
    }
}

/// Spawn the matchbox message-loop future so it keeps running for the
/// lifetime of the socket.
///
/// On native, `webrtc-rs` depends on a live tokio runtime for timers and I/O;
/// spawning on a dedicated multi-threaded runtime fixes ICE-connects-but-
/// channels-never-open issues (see gnils-net for the full story).
///
/// On WASM the browser provides WebRTC, so we use `IoTaskPool`.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_message_loop(fut: MessageLoopFuture) {
    use std::sync::OnceLock;
    use tokio::runtime::Runtime;
    use tokio::task::JoinHandle;

    /// A global multi-threaded tokio runtime dedicated to the matchbox
    /// message loop. Created once, reused for every socket.
    static MATCHBOX_RUNTIME: OnceLock<Runtime> = OnceLock::new();

    let runtime = MATCHBOX_RUNTIME.get_or_init(|| {
        // webrtc-rs uses rustls for DTLS. rustls 0.23 requires a process-level
        // CryptoProvider to be installed before any config is built.
        let _ = rustls::crypto::ring::default_provider().install_default();

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("matchbox")
            .build()
            .expect("failed to build matchbox tokio runtime")
    });

    // Detach the JoinHandle so it runs in the background. The runtime lives
    // for 'static and the task completes when the socket closes.
    let _handle: JoinHandle<()> = runtime.spawn(async move {
        let _ = fut.await;
    });
}

#[cfg(target_arch = "wasm32")]
fn spawn_message_loop(fut: MessageLoopFuture) {
    use bevy::tasks::IoTaskPool;
    IoTaskPool::get().spawn(fut).detach();
}

/// Build a `MatchboxSocket` for the given room, keeping ICE config and
/// channel layout in one place. The room is deliberately *not* completed by
/// the matchmaker (`?next=…`): rooms accept everyone who asks.
pub fn build_socket(room: &str) -> MatchboxSocket {
    let url = format!("{SIGNALING_SERVER}/{room}");
    info!(%room, %url, "opening matchbox socket");

    let ice_config = RtcIceServerConfig {
        urls: vec![
            "stun:stun.l.google.com:19302".to_string(),
            "stun:stun1.l.google.com:19302".to_string(),
        ],
        username: None,
        credential: None,
    };

    let builder = WebRtcSocketBuilder::new(&url)
        .ice_server(ice_config)
        .reconnect_attempts(None) // unlimited reconnection attempts
        .add_reliable_channel() // channel 0: lobby + sequenced events
        .add_unreliable_channel(); // channel 1: snapshots

    MatchboxSocket::from(builder)
}

/// Register the `RoomId` resource and open a socket for `room` in one call.
/// Deferred via `Commands`, so the socket is available to systems on the
/// following frame.
pub fn open_socket_for(commands: &mut Commands, room: String) {
    commands.insert_resource(RoomId(room.clone()));
    commands.insert_resource(build_socket(&room));
}

/// Close the current room: drop socket + room id, reset the net state (but
/// keep the player's name and aircraft).
pub fn close_socket(commands: &mut Commands, net: &mut NetState) {
    commands.remove_resource::<MatchboxSocket>();
    commands.remove_resource::<RoomId>();
    net.leave_room();
}

/// Random 64-bit seed for a fresh game. Host-generated, committed in
/// [`NetMsg::Start`].
pub fn new_seed() -> u64 {
    rand::random()
}

/// The room id this instance belongs to. Native: first CLI arg (a fresh
/// generated room otherwise). Wasm: the `?room=` URL parameter — a share
/// link —, generated into the URL if absent.
pub fn room_id() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        use web_sys::wasm_bindgen::JsValue;
        let win = web_sys::window().expect("window always available");
        let href = win.location().href().ok().unwrap_or_default();

        if let Ok(url) = web_sys::Url::new(&href)
            && let Some(id) = url.search_params().get("room")
            && !id.is_empty()
        {
            return id;
        }

        let new_id = random_room();

        if let Ok(url) = web_sys::Url::new(&href) {
            url.search_params().set("room", &new_id);
            if let Ok(history) = win.history() {
                let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url.href()));
            }
        }

        new_id
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        match std::env::args().nth(1).as_deref() {
            Some(arg) if RoomId::parse(arg).is_ok() => arg.to_string(),
            Some(arg) => {
                warn!("ignoring invalid room name {arg:?}; using a generated room");
                random_room()
            }
            None => random_room(),
        }
    }
}

/// The room the instance was *launched* for, if any: native the first CLI
/// arg, wasm the `?room=` URL parameter. Unlike [`room_id`] this never
/// generates a room — `None` means "the user did not name one". The menu
/// uses it to prefill hosting/joining and to enable the hands-free
/// auto-start flow for two-instance testing.
pub fn arg_room() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let win = web_sys::window()?;
        let href = win.location().href().ok()?;
        let url = web_sys::Url::new(&href).ok()?;
        url.search_params().get("room").filter(|id| !id.is_empty())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::args()
            .nth(1)
            .filter(|arg| RoomId::parse(arg).is_ok())
    }
}

/// The shareable invite for `room`: on the web the page URL carrying the
/// `?room=` parameter, native the bare room code.
pub fn invite_url(room: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        use web_sys::wasm_bindgen::JsValue;
        let win = web_sys::window()?;
        let href = win.location().href().ok()?;
        let url = web_sys::Url::new(&href).ok()?;
        url.search_params().set("room", room);
        if let Some(history) = win.history().ok() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url.href()));
        }
        Some(url.href())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = room;
        None
    }
}

// ── Broadcast helpers ───────────────────────────────────────────────────────

/// Send a message to every peer on the reliable channel.
pub fn broadcast_reliable(socket: &mut MatchboxSocket, peers: &[PeerId], msg: &NetMsg) -> bool {
    if peers.is_empty() {
        return false;
    }
    let Some(encoded) = enc_msg(msg) else {
        return false;
    };
    let channel = socket.channel_mut(CH_RELIABLE);
    for &peer in peers {
        let _ = channel.try_send(encoded.clone(), peer);
    }
    true
}

/// Broadcast a message to every peer on the unreliable channel. Send failures
/// are silently dropped — the next sample supersedes.
pub fn broadcast_unreliable(socket: &mut MatchboxSocket, peers: &[PeerId], msg: &NetMsg) {
    if peers.is_empty() {
        return;
    }
    let Some(encoded) = enc_msg(msg) else {
        return;
    };
    let channel = socket.channel_mut(CH_UNRELIABLE);
    for &peer in peers {
        let _ = channel.try_send(encoded.clone(), peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aces_protocol::PlaneSnapshot;

    #[test]
    fn lobby_and_snapshot_messages_round_trip() {
        let players = vec![PlayerInfo {
            peer: "peer-a".into(),
            name: "otter".into(),
            aircraft: 2,
        }];
        let start = NetMsg::Start {
            seed: 42,
            players: players.clone(),
        };
        let snapshot = NetMsg::Snapshot(PlaneSnapshot {
            tick: 7,
            pos: [1.0, 2.0, 3.0],
            rot: [0.0, 0.0, 0.0, 1.0],
            vel: [0.0, 0.0, -150.0],
            surfaces: [12, -127, 64],
        });
        for msg in [
            NetMsg::Hello {
                name: "otter".into(),
                aircraft: 1,
            },
            NetMsg::Roster(players),
            start,
            snapshot,
        ] {
            let bytes = enc_msg(&msg).expect("encodes");
            let back = decode(&bytes).expect("decodes");
            assert_eq!(format!("{back:?}"), format!("{msg:?}"));
        }
    }

    #[test]
    fn decoding_garbage_yields_none() {
        assert!(decode(&[0xff, 0xff, 0xff, 0xff]).is_none());
    }

    /// The empty event set cannot decode a `Game`/`Sequenced` payload — and
    /// that must fail gracefully to `None`, not panic.
    #[test]
    fn decoding_the_empty_event_fails_gracefully() {
        // postcard enum tag 3 = Game, followed by nothing it can decode.
        assert!(decode(&[3]).is_none());
    }

    #[test]
    fn my_index_and_me_need_the_roster() {
        let mut net = NetState::default();
        assert!(net.me().is_none());
        assert_eq!(net.my_index(), None);

        net.my_id = Some(PeerId(uuid::Uuid::nil()));
        net.players = vec![
            PlayerInfo {
                peer: "other".into(),
                name: "falcon".into(),
                aircraft: 0,
            },
            PlayerInfo {
                peer: net.my_id.unwrap().to_string(),
                name: "otter".into(),
                aircraft: 3,
            },
        ];
        assert_eq!(net.me().map(|p| p.name.as_str()), Some("otter"));
        assert_eq!(net.my_index(), Some(1));
        assert!(net.is_me(&PeerId(uuid::Uuid::nil()).to_string()));
        assert!(!net.is_me("other"));
    }

    /// Leaving the room must forget everything the old room's socket
    /// established but the player's own name and aircraft.
    #[test]
    fn leaving_a_room_forgets_everything_but_name_and_aircraft() {
        let mut net = NetState {
            is_host: true,
            next_seq: 12,
            last_applied_seq: Some(11),
            name: "ada".into(),
            aircraft: 4,
            players: vec![PlayerInfo {
                peer: "p".into(),
                name: "p".into(),
                aircraft: 0,
            }],
            start: Some(GameStart {
                seed: 1,
                players: vec![],
            }),
            greeted: vec![PeerId(uuid::Uuid::nil())],
            ..NetState::default()
        };

        net.leave_room();

        assert_eq!(net.name, "ada", "the player's name is not per-room");
        assert_eq!(net.aircraft, 4, "the aircraft choice is not per-room");
        assert!(!net.is_host, "host status belongs to the old room");
        assert!(
            net.players.is_empty(),
            "the roster was assigned by the old host"
        );
        assert!(net.start.is_none(), "the start belongs to the old room");
        assert!(net.peers.is_empty());
        assert_eq!(net.my_id, None, "the id came from the old socket");
        assert_eq!(net.next_seq, 0);
        assert_eq!(net.last_applied_seq, None, "or the first event looks stale");
        assert!(net.greeted.is_empty());
    }

    #[test]
    fn generated_rooms_are_petnames_and_valid_room_ids() {
        for _ in 0..50 {
            let room = random_room();
            let parts: Vec<&str> = room.split('-').collect();
            assert_eq!(parts.len(), 3, "{room}");
            assert!(ROOM_ADJECTIVES.contains(&parts[0]), "{room}");
            assert!(ROOM_NOUNS.contains(&parts[1]), "{room}");
            assert_eq!(parts[2].len(), 2, "{room}");
            assert!(parts[2].bytes().all(|c| c.is_ascii_digit()), "{room}");
            // A generated room must survive the round trip through the join
            // field without edits.
            assert_eq!(RoomId::parse(&room).unwrap().0, room);
        }
    }

    #[test]
    fn petnames_come_from_the_list() {
        assert!(PET_NAMES.contains(&petname().as_str()));
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["a", "game", "Room_7", "my-game-2", "ABC123"] {
            assert!(RoomId::parse(name).is_ok(), "{name} should be accepted");
        }
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(RoomId::parse(""), Err(RoomIdError::Empty));
    }

    #[test]
    fn an_overlong_name_is_refused() {
        let long = "a".repeat(RoomId::MAX_LEN + 1);
        assert_eq!(
            RoomId::parse(&long),
            Err(RoomIdError::TooLong {
                len: RoomId::MAX_LEN + 1
            })
        );
        assert!(RoomId::parse(&"a".repeat(RoomId::MAX_LEN)).is_ok());
    }

    /// A slash would redirect the socket to a different URL path, and
    /// whitespace is invisible in a name two people are trying to match.
    #[test]
    fn characters_that_would_change_the_url_are_refused() {
        for (name, bad) in [
            ("a/b", '/'),
            ("a b", ' '),
            ("a?b", '?'),
            ("a#b", '#'),
            ("a:b", ':'),
        ] {
            assert_eq!(RoomId::parse(name), Err(RoomIdError::BadChar(bad)));
        }
    }

    #[test]
    fn non_ascii_is_refused() {
        assert_eq!(RoomId::parse("café"), Err(RoomIdError::BadChar('é')));
    }

    #[test]
    fn every_error_explains_itself() {
        assert!(RoomIdError::Empty.to_string().contains("empty"));
        let long = RoomIdError::TooLong { len: 99 }.to_string();
        assert!(long.contains("99"), "{long}");
        assert!(long.contains(&RoomId::MAX_LEN.to_string()), "{long}");
        let bad = RoomIdError::BadChar('/').to_string();
        assert!(bad.contains('/'), "must name the character: {bad}");
    }
}
