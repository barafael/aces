//! aces — flight combat simulator client.
//!
//! Solo flight with War Thunder arcade-style mouse aim (see PLAN.md):
//! the instructor flies the nose onto the cursor, a HUD shows both markers,
//! and the ocean grid gives spatial reference. Weapons and networking arrive
//! in later milestones.

mod flight;
mod hud;
mod world;

use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::window::WindowResolution;

fn main() {
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
        .add_systems(Startup, world::setup_world)
        .add_plugins((flight::FlightPlugin, hud::HudPlugin))
        .run();
}
