//! Menu, lobby and in-game overlays: keyboard-driven UI over the 3D scene.
//!
//! Flow: Menu (`F` solo, `H` host, `J` join) → Lobby (roster, `←`/`→`
//! cycle the aircraft when there is a choice, host `Enter` starts) → InGame
//! (`Esc` leaves). Joining asks for a room id with a minimal text field.
//!
//! For hands-free two-instance testing, launching with a room id as the CLI
//! argument (wasm: `?room=`) enables the auto flow: hosting/joining uses
//! that room, and the host starts once every connected peer has greeted and
//! the roster holds at least two players.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

use aces_net::{
    GameStart, MatchboxSocket, NetMsg, NetState, RoomId, broadcast_reliable, close_socket,
    new_seed, open_socket_for, random_room,
};
use aces_protocol::{AIRCRAFT, AIRCRAFT_COUNT, aircraft};

use crate::hud::set_text;
use crate::net::{publish_roster, upsert_player};
use crate::{NetworkMode, Phase};

/// How long the host waits after the last roster change before an
/// auto-start, so a joining peer's roster broadcast has settled [s].
const AUTO_START_SETTLE: f32 = 2.0;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JoinDraft>()
            .init_resource::<CreditsOpen>()
            .add_systems(Startup, spawn_ui)
            .add_systems(
                Update,
                (
                    auto_enter_lobby.run_if(in_state(Phase::Menu)),
                    (toggle_credits, update_menu.run_if(credits_closed))
                        .chain()
                        .run_if(in_state(Phase::Menu)),
                    update_lobby.run_if(in_state(Phase::Lobby)),
                    update_in_game.run_if(in_state(Phase::InGame)),
                    update_ui_text,
                    update_panel_visibility,
                    update_cursor,
                ),
            );
    }
}

/// Hide the menu rectangle while flying: the phase system blanks the texts,
/// but the panel's background would otherwise stay on screen.
fn update_panel_visibility(
    phase: Res<State<Phase>>,
    mut panel: Single<&mut Visibility, With<MenuPanel>>,
) {
    panel.set_if_neq(if phase.get() == &Phase::InGame {
        Visibility::Hidden
    } else {
        Visibility::Visible
    });
}

/// The main menu's credits screen: the page shown, `None` while closed.
#[derive(Resource, Default)]
struct CreditsOpen(Option<usize>);

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

/// The rounded rectangle behind the menu/lobby texts; hidden while flying
/// (its children's text content is blanked by phase, but the background
/// itself would otherwise stay on screen).
#[derive(Component)]
struct MenuPanel;

#[derive(Component)]
struct LobbyText;

#[derive(Component)]
struct GameHint;

/// The embedded UI font (GNU FreeSans Bold, via gnils), for menu and HUD.
/// Bevy's built-in default font is a small Fira Mono subset that lacks — –
/// … etc.
pub const UI_FONT: &str = "fonts/FreeSansBold.ttf";

fn spawn_ui(mut commands: Commands, assets: Res<AssetServer>) {
    let font: Handle<Font> = assets.load(UI_FONT);
    let text_font = TextFont {
        font: font.into(),
        font_size: FontSize::Px(26.0),
        ..default()
    };
    let text_color = TextColor(Color::srgb(0.9, 0.93, 1.0));

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
                    MenuPanel,
                    Node {
                        padding: UiRect::all(px(28.0)),
                        border_radius: BorderRadius::all(px(10.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.05, 0.07, 0.12, 0.82)),
                ))
                .with_children(|parent| {
                    parent.spawn((MenuText, Text::default(), text_font.clone(), text_color));
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
                            ..text_font.clone()
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

#[allow(clippy::too_many_arguments)]
fn update_menu(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut key_events: MessageReader<KeyboardInput>,
    mut mode: ResMut<NetworkMode>,
    mut draft: ResMut<JoinDraft>,
    mut next: ResMut<NextState<Phase>>,
    auto_room: Res<crate::AutoRoom>,
    mut net: ResMut<NetState>,
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
    } else if let Some(step) = aircraft_step(&keys) {
        net.aircraft = (net.aircraft + step) % AIRCRAFT_COUNT;
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

/// `←`/`→` step through the aircraft (as an offset to add, modulo the
/// count), in the main menu and the lobby.
fn aircraft_step(keys: &ButtonInput<KeyCode>) -> Option<u8> {
    if keys.just_pressed(KeyCode::ArrowRight) {
        Some(1)
    } else if keys.just_pressed(KeyCode::ArrowLeft) {
        Some(AIRCRAFT_COUNT - 1)
    } else {
        None
    }
}

/// The aircraft choice line: "← / → — aircraft: MiG-21 (9/17)".
fn aircraft_line(selected: u8) -> String {
    format!(
        "← / → — aircraft: {} ({}/{AIRCRAFT_COUNT})",
        aircraft(selected).name,
        selected + 1,
    )
}

/// `K` opens and closes the credits screen (`Esc` closes it too), `←`/`→`
/// turn its pages — from the main menu, not while typing a room id.
fn toggle_credits(
    keys: Res<ButtonInput<KeyCode>>,
    draft: Res<JoinDraft>,
    mut credits: ResMut<CreditsOpen>,
) {
    if draft.active {
        return;
    }
    let pages = credits_pages();
    credits.0 = match credits.0 {
        None if keys.just_pressed(KeyCode::KeyK) => Some(0),
        Some(_) if keys.any_just_pressed([KeyCode::KeyK, KeyCode::Escape]) => None,
        Some(page) if keys.just_pressed(KeyCode::ArrowRight) => Some((page + 1) % pages),
        Some(page) if keys.just_pressed(KeyCode::ArrowLeft) => Some((page + pages - 1) % pages),
        open => open,
    };
}

/// Run condition: the main menu (not the credits screen) takes input.
fn credits_closed(credits: Res<CreditsOpen>) -> bool {
    credits.0.is_none()
}

/// Model credits per credits page: as many as fit the window's height.
const CREDITS_PER_PAGE: usize = 6;

/// Number of credits pages.
fn credits_pages() -> usize {
    AIRCRAFT
        .iter()
        .filter(|kind| kind.credit.is_some())
        .count()
        .div_ceil(CREDITS_PER_PAGE)
        .max(1)
}

/// Credits page `page`: third-party assets as their licenses ask (title,
/// author, source, license), the licenses' URLs under each page's models
/// and the font on the last page. `CREDITS.md` has the same.
fn credits_text(page: usize) -> String {
    let pages = credits_pages();
    let mut text = format!(
        "aces — credits ({}/{pages})\n\nAircraft models from Sketchfab (scaled, rotated\nand re-centred in game):\n",
        page + 1
    );
    let credits: Vec<_> = AIRCRAFT
        .iter()
        .filter_map(|kind| kind.credit)
        .skip(page * CREDITS_PER_PAGE)
        .take(CREDITS_PER_PAGE)
        .collect();
    for credit in &credits {
        text.push_str(&format!(
            "\n{} — {}, {}\n{}\n",
            credit.title, credit.author, credit.license, credit.source
        ));
    }
    let mut licenses: Vec<_> = credits
        .iter()
        .map(|credit| (credit.license, credit.license_url))
        .collect();
    licenses.sort_unstable();
    licenses.dedup();
    text.push('\n');
    for (license, url) in licenses {
        text.push_str(&format!("{license}: {url}\n"));
    }
    if page + 1 == pages {
        text.push_str(
            "\nFont: FreeSans Bold (GNU FreeFont),\nFree Software Foundation — GNU GPL\n",
        );
    }
    text.push_str("\n← / → — page    K / Esc — back");
    text
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
    if let Some(step) = aircraft_step(&keys) {
        let aircraft = (net.aircraft + step) % AIRCRAFT_COUNT;
        if net.aircraft != aircraft {
            net.aircraft = aircraft;
            net.greeted.clear();
            if net.is_host {
                // The host's own entry refreshes on its next greet; make it
                // immediate so the roster the host starts with is correct.
                let me = net.my_peer().to_string();
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
    draft: Res<JoinDraft>,
    credits: Res<CreditsOpen>,
    mut menu: Query<&mut Text, With<MenuText>>,
    mut lobby: Query<
        (&mut Text, &mut Visibility),
        (With<LobbyText>, Without<MenuText>, Without<GameHint>),
    >,
    mut hint: Query<&mut Visibility, (With<GameHint>, Without<MenuText>, Without<LobbyText>)>,
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

    // Every write re-shapes text and re-lays out the UI: build the wanted
    // state, then touch only what differs.
    let (menu_text, lobby, hint) = match phase.get() {
        Phase::Menu => {
            let text = if let Some(page) = credits.0 {
                credits_text(page)
            } else if draft.active {
                format!(
                    "aces — join a room\n\nroom: {}_\n\nEnter — join\nEsc — cancel{}",
                    draft.text,
                    draft
                        .error
                        .as_ref()
                        .map(|e| format!("\n\n{e}"))
                        .unwrap_or_default()
                )
            } else {
                let room = auto_room.0.as_deref();
                format!(
                    "aces\n\n{}\n\nF — fly solo\nH — host {}\nJ — join {}\nK — credits",
                    aircraft_line(net.aircraft),
                    room.unwrap_or("a room"),
                    room.unwrap_or("a room"),
                )
            };
            (text, None, false)
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
                    "{}. {} — {}{}\n",
                    index + 1,
                    player.name,
                    aircraft(player.aircraft).name,
                    tags
                ));
            }
            lines.push('\n');
            if AIRCRAFT_COUNT > 1 {
                lines.push_str(&aircraft_line(net.aircraft));
                lines.push('\n');
            }
            if net.is_host {
                lines.push_str("Enter — start\n");
            }
            lines.push_str("Esc — leave");
            (String::new(), Some(lines), false)
        }
        Phase::InGame => (String::new(), None, *mode == NetworkMode::Net),
    };

    set_text(&mut menu, &menu_text);
    if let Some(lines) = &lobby {
        set_text(&mut lobby_text, lines);
    }
    let shown = |visible| {
        if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        }
    };
    lobby_vis.set_if_neq(shown(lobby.is_some()));
    hint_vis.set_if_neq(shown(hint));
}

/// In flight the mouse steers the aim: the cursor is hidden and locked to
/// the window. Browsers only grant pointer lock from a user gesture, so a
/// click re-requests it. Menus get the cursor back. Writes only on change,
/// so the window backend is not asked to re-grab every frame.
fn update_cursor(
    phase: Res<State<Phase>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut cursor: Single<&mut CursorOptions>,
) {
    let in_game = phase.get() == &Phase::InGame;
    let grab_mode = if in_game {
        CursorGrabMode::Locked
    } else {
        CursorGrabMode::None
    };
    let regrab = in_game && buttons.just_pressed(MouseButton::Left);
    if cursor.grab_mode != grab_mode || cursor.visible == in_game || regrab {
        cursor.grab_mode = grab_mode;
        cursor.visible = !in_game;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The in-game credits carry every model's full attribution.
    #[test]
    fn credits_screen_lists_every_credit() {
        let text: String = (0..credits_pages()).map(credits_text).collect();
        for credit in AIRCRAFT.iter().filter_map(|kind| kind.credit) {
            for part in [
                credit.title,
                credit.author,
                credit.source,
                credit.license,
                credit.license_url,
            ] {
                assert!(text.contains(part), "credits screen lacks {part:?}");
            }
        }
        assert!(text.contains("GNU GPL"), "font credit missing");
    }
}
