//! aces — flight combat simulator client.
//!
//! Phases: Menu (solo / host / join) → Lobby (roster, aircraft select) →
//! InGame (flight + combat). See PLAN.md for the architecture.

mod flight;
mod hud;
mod menu;
mod net;
mod weapons;
mod world;

use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::window::WindowResolution;

/// High-level app phases.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Phase {
    #[default]
    Menu,
    Lobby,
    InGame,
}

/// Whether this session flies solo or over the network.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    #[default]
    Solo,
    Net,
}

/// The room this instance was launched for (CLI arg / `#room=` fragment),
/// enabling the hands-free auto host/join/start flow.
#[derive(Resource, Default, Debug, Clone)]
pub struct AutoRoom(pub Option<String>);

/// Asset folder for [`AssetPlugin`]: absolute in native dev builds (binaries
/// run from any directory still find the repo assets), relative otherwise.
/// Never absolute on the web: the browser fetches assets over HTTP next to
/// the page, and a filesystem path there fails every load (trunk answers it
/// with the index page).
fn asset_root() -> String {
    #[cfg(all(debug_assertions, not(target_arch = "wasm32")))]
    if let Some(dir) = option_env!("CARGO_MANIFEST_DIR") {
        return format!("{dir}/assets");
    }
    "assets".to_string()
}

fn main() {
    // `aces-client replay <log.jsonl> …` re-flies a flight log instead of
    // starting the game (see flight::replay).
    #[cfg(not(target_arch = "wasm32"))]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("replay") {
            if let Err(err) = flight::replay::cli(&args[2..]) {
                eprintln!("replay: {err}");
                std::process::exit(1);
            }
            return;
        }
    }

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "aces".into(),
                        resolution: WindowResolution::new(1600, 900),
                        resizable: true,
                        // Web: let the canvas grow with the browser viewport.
                        // Ignored natively; required so wasm fills the page.
                        canvas: Some("#game".into()),
                        fit_canvas_to_parent: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(AssetPlugin {
                    // Web dev servers answer the optional `<asset>.meta`
                    // probe with the index page; parsing that as meta fails
                    // and rejects every asset (gray screen). Same as gnils.
                    meta_check: AssetMetaCheck::Never,
                    // Dev builds embed the crate's asset dir (absolute), so
                    // the binary runs from any working directory; release
                    // installs keep the portable "assets" folder next to the
                    // executable.
                    file_path: asset_root(),
                    ..default()
                }),
        )
        // The fixed tick the flight model runs at.
        .insert_resource(Time::<Fixed>::from_hz(aces_protocol::TICK_HZ))
        .insert_resource(GlobalAmbientLight {
            color: Color::WHITE,
            brightness: 400.0,
            ..default()
        })
        // App state and session mode.
        .init_state::<Phase>()
        .init_resource::<NetworkMode>()
        .insert_resource(AutoRoom(aces_net::arg_room()))
        .insert_resource(aces_net::NetState::with_name(aces_net::petname()))
        .add_systems(Startup, world::setup_world)
        .add_plugins((
            flight::FlightPlugin,
            net::ClientNetPlugin,
            menu::MenuPlugin,
            hud::HudPlugin,
            weapons::WeaponsPlugin,
        ))
        .run();
}
