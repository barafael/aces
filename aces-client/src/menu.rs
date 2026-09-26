//! Menu, lobby and in-game overlays: keyboard-driven UI over the 3D scene.
//!
//! Flow: Menu (`F` solo, `H` host, `J` join) → Lobby (roster, `1–5` aircraft
//! select, host `Enter` starts) → InGame (`Esc` leaves). Joining asks for a
//! room id with a minimal text field.
//!
//! For hands-free two-instance testing, launching with a room id as the CLI
//! argument (wasm: `?room=`) enables the auto flow: hosting/joining uses
//! that room, and the host starts once every connected peer has greeted and
//! the roster holds at least two players.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::CursorOptions;

use aces_net::{
    broadcast_reliable, close_socket, new_seed, open_socket_for, random_room, GameStart,
    MatchboxSocket, NetMsg, NetState, RoomId,
};
use aces_protocol::AIRCRAFT_COUNT;

use crate::net::{publish_roster, upsert_player};
use crate::{NetworkMode, Phase};

/// How long the host waits after the last roster change before an
/// auto-start, so a joining peer's roster broadcast has settled [s].
const AUTO_START_SETTLE: f32 = 2.0;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JoinDraft>()
            .add_systems(Startup, spawn_ui)
            .add_systems(
                Update,
                (
                    auto_enter_lobby.run_if(in_state(Phase::Menu)),
                    update_menu.run_if(in_state(Phase::Menu)),
                    update_lobby.run_if(in_state(Phase::Lobby)),
                    update_in_game.run_if(in_state(Phase::InGame)),
                    update_ui_text,
                    update_cursor_visibility,
                ),
            );
    }
}

/// State of the join-room text field.
#[derive(Resource, Default)]
struct JoinDraft {
    active: bool,
    text: String,
    /// Why the last attempt was refused, shown under the field.
    error: Option<String>,
}

#[derive(Component)]
struct MenuText;

#[derive(Component)]
struct LobbyText;

#[derive(Component)]
struct GameHint;

/// The embedded UI font (GNU FreeSans Bold, via gnils). Bevy's built-in
/// default font is a small Fira Mono subset that lacks — – … etc.
#[derive(Resource, Clone)]
struct UiFont(Handle<Font>);

fn spawn_ui(mut commands: Commands, assets: Res<AssetServer>) {
    let font = UiFont(assets.load("fonts/FreeSansBold.ttf"));
    let text_font = TextFont {
        font: font.0.clone().into(),
        font_size: FontSize::Px(26.0),
        ..default()
    };
    let text_color = TextColor(Color::srgb(0.9, 0.93, 1.0));
    commands.insert_resource(font);

    commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|parent| {
            parent
                .spawn((
                    Node {
                        padding: UiRect::all(px(28.0)),
                        border_radius: BorderRadius::all(px(10.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.05, 0.07, 0.12, 0.82)),
                ))
                .with_children(|parent| {
                    parent.spawn((
                        MenuText,
                        Text::default(),
                        text_font.clone(),
                        text_color,
                    ));
                    parent.spawn((
                        LobbyText,
                        Text::default(),
                        text_font.clone(),
                        text_color,
                        Visibility::Hidden,
                    ));
                    parent.spawn((
                        GameHint,
                        Text::new("Esc — leave"),
                        TextFont {
                            font_size: FontSize::Px(22.0),
                            ..default()
                        },
                        TextColor(Color::srgba(0.9, 0.93, 1.0, 0.7)),
                        Visibility::Hidden,
                    ));
                });
        });
}

/// Enter the lobby with `room`: open the socket and switch modes.
fn enter_lobby(commands: &mut Commands, mode: &mut NetworkMode, room: String) {
    info!(%room, "entering the lobby");
    *mode = NetworkMode::Net;
    open_socket_for(commands, room);
}

/// Host action: everyone in the roster starts flying.
fn host_start(socket: &mut MatchboxSocket, net: &mut NetState) {
    let players = net.players.clone();
    let seed = new_seed();
    info!(?seed, "host starts the game");
    broadcast_reliable(
        socket,
        &net.peers,
        &NetMsg::Start {
            seed,
            players: players.clone(),
        },
    );
    net.players = players.clone();
    net.start = Some(GameStart { seed, players });
}

/// Leave the room (or the solo session) and go back to the menu.
fn leave_room(commands: &mut Commands, net: &mut NetState, mode: &mut NetworkMode) {
    if *mode == NetworkMode::Net {
        close_socket(commands, net);
    }
    *mode = NetworkMode::Solo;
}

/// Launched with a room argument: skip the menu entirely and join the room.
/// Host election happens in the mesh (lowest peer id); the elected host
/// auto-starts once the room has settled (see `update_lobby`).
fn auto_enter_lobby(
    mut commands: Commands,
    auto_room: Res<crate::AutoRoom>,
    mut mode: ResMut<NetworkMode>,
    mut next: ResMut<NextState<Phase>>,
) {
    if let Some(room) = auto_room.0.clone() {
        info!(%room, "auto-joining the room from the launch argument");
        enter_lobby(&mut commands, &mut mode, room);
        next.set(Phase::Lobby);
    }
}

fn update_menu(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<KeyboardInput>,
    mut mode: ResMut<NetworkMode>,
    mut draft: ResMut<JoinDraft>,
    mut next: ResMut<NextState<Phase>>,
    auto_room: Res<crate::AutoRoom>,
) {
    if draft.active {
        for event in key_events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match (&event.logical_key, event.key_code) {
                (Key::Enter, _) => match RoomId::parse(draft.text.trim()) {
                    Ok(room) => {
                        draft.active = false;
                        draft.error = None;
                        enter_lobby(&mut commands, &mut mode, room.0);
                        next.set(Phase::Lobby);
                    }
                    Err(e) => draft.error = Some(e.to_string()),
                },
                (Key::Backspace, _) => {
                    draft.text.pop();
                }
                (Key::Escape, _) => {
                    draft.active = false;
                    draft.error = None;
                }
                _ => {
                    if let Some(text) = &event.text {
                        for c in text.chars() {
                            // Room ids are ASCII by construction.
                            if draft.text.chars().count() < RoomId::MAX_LEN
                                && (c.is_ascii_alphanumeric() || c == '-' || c == '_')
                            {
                                draft.text.push(c);
                            }
                        }
                    }
                }
            }
        }
    } else if keys.just_pressed(KeyCode::KeyF) {
        info!("flying solo");
        *mode = NetworkMode::Solo;
        next.set(Phase::InGame);
    } else if keys.just_pressed(KeyCode::KeyH) {
        let room = auto_room.0.clone().unwrap_or_else(random_room);
        enter_lobby(&mut commands, &mut mode, room);
        next.set(Phase::Lobby);
    } else if keys.just_pressed(KeyCode::KeyJ) {
        draft.active = true;
        draft.text = auto_room.0.clone().unwrap_or_default();
        draft.error = None;
    }
}

#[allow(clippy::too_many_arguments)]
fn update_lobby(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut net: ResMut<NetState>,
    mut mode: ResMut<NetworkMode>,
    mut socket: Option<ResMut<MatchboxSocket>>,
    mut next: ResMut<NextState<Phase>>,
    auto_room: Res<crate::AutoRoom>,
    mut settle: Local<(f32, (usize, bool))>,
) {
    // Aircraft select re-greets, which is how the change propagates.
    let select = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
    ]
    .iter()
    .position(|k| keys.just_pressed(*k));
    if let Some(slot) = select {
        let aircraft = slot as u8;
        if aircraft < AIRCRAFT_COUNT && net.aircraft != aircraft {
            net.aircraft = aircraft;
            net.greeted.clear();
            if net.is_host {
                // The host's own entry refreshes on its next greet; make it
                // immediate so the roster the host starts with is correct.
                let me = net.my_id.map(|id| id.to_string()).unwrap_or_default();
                let (name, aircraft) = (net.name.clone(), net.aircraft);
                let peers = net.peers.clone();
                if let Some(socket) = socket.as_mut()
                    && upsert_player(&mut net, &me, name, aircraft)
                {
                    publish_roster(socket, &peers, &net);
                }
            }
        }
    }

    if keys.just_pressed(KeyCode::Escape) {
        leave_room(&mut commands, &mut net, &mut mode);
        next.set(Phase::Menu);
        return;
    }

    let Some(socket) = socket.as_mut() else {
        return;
    };

    // Reset the auto-start settle timer whenever the room picture changes.
    let everyone_greeted = net.peers.iter().all(|p| net.greeted.contains(p));
    let room_picture = (net.players.len(), everyone_greeted);
    if room_picture != settle.1 {
        *settle = (0.0, room_picture);
    }
    settle.0 += time.delta_secs();

    // Host: manual start…
    let manual_start = keys.just_pressed(KeyCode::Enter) && net.is_host;
    // …or the hands-free auto flow for launched-with-a-room sessions.
    let auto_start = auto_room.0.is_some()
        && net.is_host
        && net.players.len() >= 2
        && everyone_greeted
        && settle.0 > AUTO_START_SETTLE;

    // The phase transition applies on the frame *after* `net.start` is set;
    // without the guard the auto-start would fire again in between (with a
    // fresh seed).
    if (manual_start || auto_start) && net.start.is_none() {
        host_start(socket, &mut net);
    }
}

fn update_in_game(
    keys: Res<ButtonInput<KeyCode>>,
    mut net: ResMut<NetState>,
    mut mode: ResMut<NetworkMode>,
    mut commands: Commands,
    mut next: ResMut<NextState<Phase>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        leave_room(&mut commands, &mut net, &mut mode);
        next.set(Phase::Menu);
    }
}

/// Refresh the three text blocks and toggle their visibility by phase.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_ui_text(
    phase: Res<State<Phase>>,
    net: Res<NetState>,
    mode: Res<NetworkMode>,
    room: Option<Res<RoomId>>,
    auto_room: Res<crate::AutoRoom>,
    draft: ResMut<JoinDraft>,
    mut menu: Query<&mut Text, With<MenuText>>,
    mut lobby: Query<
        (&mut Text, &mut Visibility),
        (With<LobbyText>, Without<MenuText>, Without<GameHint>),
    >,
    mut hint: Query<
        &mut Visibility,
        (With<GameHint>, Without<MenuText>, Without<LobbyText>),
    >,
) {
    let Ok(mut menu) = menu.single_mut() else {
        return;
    };
    let Ok((mut lobby_text, mut lobby_vis)) = lobby.single_mut() else {
        return;
    };
    let Ok(mut hint_vis) = hint.single_mut() else {
        return;
    };

    match phase.get() {
        Phase::Menu => {
            if draft.active {
                **menu = format!(
                    "aces — join a room\n\nroom: {}_\n\nEnter — join\nEsc — cancel{}",
                    draft.text,
                    draft
                        .error
                        .as_ref()
                        .map(|e| format!("\n\n{e}"))
                        .unwrap_or_default()
                );
            } else {
                let mut text = String::from("aces\n\nF — fly solo\nH — host a room\nJ — join a room");
                if let Some(room) = &auto_room.0 {
                    text = format!("aces\n\nF — fly solo\nH — host {room}\nJ — join {room}");
                }
                **menu = text;
            }
            *lobby_vis = Visibility::Hidden;
            *hint_vis = Visibility::Hidden;
        }
        Phase::Lobby => {
            let mut lines = String::from("lobby");
            if let Some(room) = room.as_ref() {
                lines.push_str(&format!(" — {}", room.0));
            }
            lines.push_str("\n\n");
            if net.players.is_empty() {
                lines.push_str("connecting…\n");
            }
            for (index, player) in net.players.iter().enumerate() {
                let mut tags = Vec::new();
                if net.is_me(&player.peer) {
                    tags.push("you");
                }
                if index == 0 {
                    tags.push("host");
                }
                let tags = if tags.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", tags.join(", "))
                };
                lines.push_str(&format!(
                    "{}. {} — aircraft {}{}\n",
                    index + 1,
                    player.name,
                    player.aircraft + 1,
                    tags
                ));
            }
            lines.push_str(&format!(
                "\n1–{} — aircraft (currently {})\n",
                AIRCRAFT_COUNT,
                net.aircraft + 1
            ));
            if net.is_host {
                lines.push_str("Enter — start\n");
            }
            lines.push_str("Esc — leave");
            **lobby_text = lines;
            *lobby_vis = Visibility::Visible;
            **menu = String::new();
            *hint_vis = Visibility::Hidden;
        }
        Phase::InGame => {
            **menu = String::new();
            *lobby_vis = Visibility::Hidden;
            *hint_vis = if *mode == NetworkMode::Net {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
    }
}

fn update_cursor_visibility(phase: Res<State<Phase>>, mut cursor: Single<&mut CursorOptions>) {
    cursor.visible = phase.get() != &Phase::InGame;
}
