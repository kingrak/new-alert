//! Pixel-driven menu flow (M7.8 coverage layer 3): drives
//! MainMenu -> SkirmishSetup -> InGame -> Paused -> InGame -> Paused ->
//! MainMenu -> quit purely through `InputEvent`s a real player could
//! generate — mouse clicks at *computed* button coordinates plus the Esc
//! key — never `App::select_map`/`App::start_game`/`App::config_mut` (those
//! are the scripted-drive/verification backdoors the other suites use
//! deliberately; this suite exists specifically to prove the same outcomes
//! are reachable without them, i.e. that the real click surface is
//! sufficient on its own).
//!
//! Coordinates are computed from the *same layout formulas* `menu.rs`'s
//! private `items_main_menu`/`items_setup`/`items_paused`/`items_gameover`
//! use (`ROW_H = 16`, `MAP_ROWS = 8`, the option-cycler column geometry,
//! ...) rather than probed/guessed — see the `layout` module below, whose
//! doc comments cite the exact `menu.rs` formula each constant/function
//! mirrors. This is deliberate coupling to `menu.rs`'s private geometry: if
//! that layout changes, this suite's coordinates must change with it — which
//! is the point, since a silent layout change that broke real mouse clicks
//! while leaving `select_map`-style tests green is exactly the gap this
//! suite exists to close.

mod support;

use ra_client::input::{InputEvent, Key};
use ra_client::menu::AppState;

/// A fixed 1024x768 viewport (matches `support::menu_app`), so every
/// coordinate below can be a literal computed once rather than a formula
/// re-evaluated per test.
const VW: i32 = 1024;
const VH: i32 = 768;

/// Coordinates mirroring `menu.rs`'s private layout math, one function per
/// `items_*` method it mirrors. Kept separate from `support` (which is
/// shared fixture *state*, not `menu.rs`-coupled *geometry*) since no other
/// suite needs pixel-exact button coordinates — the monkey suite fuzzes
/// coordinates and the golden-frame suite drives state with the
/// `select_map`/`start_game` backdoors.
mod layout {
    use super::{VH, VW};

    /// `items_main_menu`: `cx = viewport_w/2`, `bw = 260`, `bh = 36`,
    /// first item's `y = viewport_h/2 - 20`, `gap = bh + 14 = 50`. Returns a
    /// point inside the Nth item's rect (0 = SKIRMISH, 1 = CAMPAIGN
    /// (disabled), 2 = QUIT).
    pub fn main_menu_item(n: i32) -> (i32, i32) {
        let cx = VW / 2;
        let y0 = VH / 2 - 20 + n * (36 + 14);
        (cx, y0 + 36 / 2)
    }

    /// `items_setup`'s map-list rows: `list_x0 = 40`, `list_y0 = 84`,
    /// `ROW_H = 16`. Row `idx` (with `map_scroll == 0`, true throughout this
    /// suite — the 3-entry fixture list never overflows `MAP_ROWS = 8`).
    pub fn map_row(idx: i32) -> (i32, i32) {
        let list_x0 = 40;
        let list_y0 = 84;
        let row_h = 16;
        let y = list_y0 + idx * row_h;
        (list_x0 + 10, y + row_h / 2)
    }

    /// `items_setup`'s option cyclers: `oy` starts at
    /// `list_y0 + MAP_ROWS*ROW_H + 32 = 84 + 8*16 + 32 = 244`, each row
    /// `oh = 28` tall with `gap = oh + 8 = 36`, in order
    /// DIFFICULTY, HOUSE, COLOR, CREDITS (indices 0..=3).
    pub fn cycler(field_index: i32) -> (i32, i32) {
        let ox0 = 40;
        let ow = 300;
        let oh = 28;
        let oy = 244 + field_index * (oh + 8);
        (ox0 + ow / 2, oy + oh / 2)
    }

    /// `items_setup`'s START button: same `oy` progression as `cycler`, one
    /// more step past CREDITS (index 3) for CLASSIC RADAR (index 4), then
    /// START/BACK's row at index 5. Rect is `(ox0, oy, ox0+140, oy+oh+4)`.
    pub fn start_button() -> (i32, i32) {
        let ox0 = 40;
        let oh = 28;
        let oy = 244 + 5 * (oh + 8);
        (ox0 + 70, oy + (oh + 4) / 2)
    }

    /// `items_paused`: `cx = viewport_w/2`, `bw = 240`, `bh = 36`,
    /// first item's `y = viewport_h/2 - 30`, `gap = bh + 14 = 50`.
    /// 0 = RESUME, 1 = QUIT TO MENU.
    pub fn paused_item(n: i32) -> (i32, i32) {
        let cx = VW / 2;
        let y0 = VH / 2 - 30 + n * (36 + 14);
        (cx, y0 + 36 / 2)
    }
}

fn click(a: &mut ra_client::menu::App, (x, y): (i32, i32)) {
    support::menu_click(a, x, y);
}

/// The real-user-path drive: menu -> setup (select a *non-default* map and
/// change the difficulty via its cycler, both by click) -> start -> pause ->
/// resume -> pause again -> quit to menu -> quit the app. Every state
/// transition and every setup selection is verified against the same public
/// getters the other suites use (`App::state`/`config`/`core`/
/// `quit_requested`) — only the *drive* is click-only, not the assertions.
#[test]
fn pixel_driven_menu_to_pause_resume_quit_flow() {
    let mut a = support::menu_app();
    assert_eq!(a.state(), AppState::MainMenu);

    // MainMenu -> SkirmishSetup: click SKIRMISH (item 0).
    click(&mut a, layout::main_menu_item(0));
    assert_eq!(a.state(), AppState::SkirmishSetup);
    assert_eq!(a.config().map, 0, "starts on the default map (Alpha)");

    // Click map row 1 ("Bravo (4P)", MapSource::Archive) — a non-default
    // selection, purely by clicking the row, never `select_map`.
    click(&mut a, layout::map_row(1));
    assert_eq!(
        a.maps()[a.config().map].filename,
        "bravo.ini",
        "clicking map row 1 selected Bravo purely through the click path"
    );

    // Click the DIFFICULTY cycler (index 0) once: NORMAL(1) -> HARD(2).
    click(&mut a, layout::cycler(0));
    assert_eq!(
        a.config().difficulty,
        2,
        "one click cycled Difficulty to HARD"
    );

    // Click the HOUSE cycler (index 1) once: GREECE(0) -> USSR(1).
    click(&mut a, layout::cycler(1));
    assert_eq!(a.config().house, 1, "one click cycled House to USSR");

    // Click START.
    click(&mut a, layout::start_button());
    assert_eq!(a.state(), AppState::InGame, "START button entered the game");
    assert!(a.last_error().is_none());

    // The clicked selections actually reached the built World.
    let core = a.core().expect("in-game core");
    assert_eq!(
        core.world().player_house(),
        Some(2),
        "USSR (house index 2) threaded through purely via clicks"
    );
    assert_eq!(
        core.world().ai_difficulty(0),
        Some(ra_sim::Difficulty::Hard),
        "HARD threaded through purely via clicks"
    );

    // Play a few virtual ticks so pausing has something real to freeze.
    for _ in 0..10 {
        a.update(67);
    }
    let t_running = a.core().unwrap().world().tick_count();
    assert!(t_running > 0, "sim advanced before pausing");

    // InGame -> Paused: Esc. There is no in-game pause *button* to click —
    // Esc is the real (and only) user action that opens the pause overlay,
    // so this is still the genuine user path, just a keypress rather than a
    // click for this one edge (see `menu.rs`'s `handle_ingame`).
    a.handle(InputEvent::KeyDown(Key::Menu));
    assert_eq!(a.state(), AppState::Paused);
    for _ in 0..10 {
        a.update(67);
    }
    assert_eq!(
        a.core().unwrap().world().tick_count(),
        t_running,
        "sim frozen while paused"
    );

    // Paused -> InGame: click RESUME (item 0).
    click(&mut a, layout::paused_item(0));
    assert_eq!(a.state(), AppState::InGame);
    for _ in 0..10 {
        a.update(67);
    }
    assert!(
        a.core().unwrap().world().tick_count() > t_running,
        "sim resumed after clicking RESUME"
    );

    // InGame -> Paused again, then Paused -> MainMenu: click QUIT TO MENU
    // (item 1).
    a.handle(InputEvent::KeyDown(Key::Menu));
    assert_eq!(a.state(), AppState::Paused);
    click(&mut a, layout::paused_item(1));
    assert_eq!(a.state(), AppState::MainMenu);
    assert!(
        a.core().is_none(),
        "quitting to menu purely via click dropped the World"
    );
    assert!(!a.quit_requested(), "quit-to-menu is not app-quit");

    // MainMenu -> app quit: click QUIT (item 2).
    click(&mut a, layout::main_menu_item(2));
    assert!(
        a.quit_requested(),
        "clicking QUIT on the main menu set the quit flag"
    );
    assert_eq!(
        a.state(),
        AppState::MainMenu,
        "quitting does not itself change state — the shell reads quit_requested() and exits"
    );
}
