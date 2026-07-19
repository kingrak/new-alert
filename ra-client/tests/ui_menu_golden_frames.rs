//! Menu golden-frame suite (M7.8 coverage layer 2, DESIGN.md §4.8 layer 4
//! applied to the pre-game `App`): pinned `App::compose()` RGBA hashes for
//! `MainMenu`, `SkirmishSetup`, the `Paused` overlay, and the `GameOver`
//! overlay. Entirely asset-free (`support::MenuSynthFactory`/
//! `support::MenuGameOverFactory` build tiny synthetic `World`s with a
//! hand-rolled 16x16 all-zero raster — no archives read anywhere in this
//! file), so unlike the real-scenario goldens in `ui_golden_frames.rs`, none
//! of these skip: they always run, on every OS target, in CI.
//!
//! Compositing is integer-only, so these are tolerance-free regression pins
//! (same policy as `ui_golden_frames.rs`): any hash change means either a
//! genuine compositing/layout bug or a deliberate rendering change that
//! should update the pin with a comment explaining why. Every pin below was
//! derived once via a throwaway run of this exact test against the current
//! (correct, human-reviewed) code, per that file's established convention.

mod support;

use ra_client::input::{InputEvent, Key};
use ra_client::menu::AppState;

/// `MainMenu` is the very first frame a fresh `App` composes — no clicks, no
/// setup needed.
#[test]
fn main_menu_frame_hash() {
    let a = support::menu_app();
    assert_eq!(a.state(), AppState::MainMenu);
    let f = a.compose();
    assert_eq!((f.width, f.height), (1024, 768));
    assert_eq!(
        support::fnv1a(&f.pixels),
        0x21f5_8661_6a78_ced9,
        "MainMenu frame hash changed"
    );
}

/// `SkirmishSetup` with the default selections (map 0 "Alpha", NORMAL,
/// GREECE, GOLD, 5000, classic radar on) against the fixed 3-entry synthetic
/// map list — pins the map-list rows, the placeholder preview cross (empty
/// `RgbaImage`), the metadata line, and every option cycler's label text in
/// one frame.
#[test]
fn skirmish_setup_default_frame_hash() {
    let mut a = support::menu_app();
    support::menu_click(&mut a, 512, 382); // SKIRMISH (see ui_menu_state_machine.rs's identical click point)
    assert_eq!(a.state(), AppState::SkirmishSetup);
    let f = a.compose();
    assert_eq!((f.width, f.height), (1024, 768));
    assert_eq!(
        support::fnv1a(&f.pixels),
        0xceb1_c9f0_be04_ffcb,
        "SkirmishSetup default-selection frame hash changed"
    );
}

/// The `Paused` overlay over a running synthetic game: start a game from the
/// menu, tick a fixed number of times (deterministic — fixed seed, fixed
/// dt), then Esc into `Paused` and pin the dimmed frame + RESUME/QUIT TO MENU
/// buttons. Exercises `compose_paused`'s `compose_ingame` + `dim` +
/// `draw_items` composition together.
#[test]
fn paused_overlay_frame_hash() {
    let mut a = support::menu_app();
    support::menu_click(&mut a, 512, 382); // -> SkirmishSetup
    a.select_map(0);
    a.start_game();
    assert_eq!(a.state(), AppState::InGame);
    for _ in 0..5 {
        a.update(67);
    }
    a.handle(InputEvent::KeyDown(Key::Menu)); // Esc -> Paused
    assert_eq!(a.state(), AppState::Paused);
    let f = a.compose();
    assert_eq!((f.width, f.height), (1024, 768));
    assert_eq!(
        support::fnv1a(&f.pixels),
        0x73ba_6fb7_89a4_2c0a,
        "Paused overlay frame hash changed"
    );
}

/// The `GameOver` overlay: `support::MenuGameOverFactory` builds a world
/// whose AI house starts with nothing, so the very first `update()` resolves
/// Victory and `App::update` transitions to `GameOver` automatically. Pins
/// `compose_gameover`'s `compose_ingame` (carrying the VICTORY banner) +
/// CONTINUE button composition.
#[test]
fn gameover_overlay_frame_hash() {
    let mut a = ra_client::menu::App::new(
        support::menu_synth_maps(),
        Box::new(support::MenuGameOverFactory),
    );
    a.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    support::menu_click(&mut a, 512, 382); // -> SkirmishSetup
    a.select_map(0);
    a.start_game();
    assert_eq!(a.state(), AppState::InGame);
    a.update(67);
    assert_eq!(
        a.state(),
        AppState::GameOver,
        "MenuGameOverFactory's empty AI house should resolve Victory on the first tick"
    );
    let f = a.compose();
    assert_eq!((f.width, f.height), (1024, 768));
    assert_eq!(
        support::fnv1a(&f.pixels),
        0x8bfb_478a_810d_f155,
        "GameOver overlay frame hash changed"
    );
}
