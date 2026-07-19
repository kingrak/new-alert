//! Game-over UI suite (M6 coverage item 5, client half): drives a synthetic
//! `AppCore`+`World` to Victory and Defeat through the real `handle`/`update`
//! seam, asserts the VICTORY/DEFEAT overlay actually paints pixels, and
//! monkey-checks that the UI stops emitting orders once the game is over.
//! Fully synthetic — no real assets needed, so nothing here is skip-gated
//! (DESIGN.md §4.8; per the M6 task spec, small synthetic worlds are
//! explicitly fine for this coverage item).
//!
//! **Structural findings, read before extending this file.**
//!
//! 1. `AppCore::compose` (the arbitrary-map-rect API the map-sweep/
//!    golden-frame suites use) only ever calls `draw_units` — it never calls
//!    `draw_game_over`. The VICTORY/DEFEAT banner is painted exclusively by
//!    `compose_game`, which `compose_camera` delegates to *only when the
//!    build sidebar is enabled*. So a headless caller using bare
//!    `compose(viewport)` (or `compose_camera()` without `enable_sidebar`)
//!    never sees the overlay at all, no matter the game state. This suite
//!    therefore drives `enable_sidebar` and asserts through
//!    `compose_game()`/`compose_camera()`, matching how the real windowed
//!    shell actually renders it — but flagged to ra-coder: any headless
//!    consumer of the bare `compose()` seam (a hypothetical spectator view, a
//!    thumbnail renderer, ...) would silently never show game-over.
//! 2. `AppCore` exposes `world()` (`&World`) but no `world_mut()` — there is
//!    no way to mutate the wrapped `World` (e.g. kill a unit to trigger a
//!    state transition mid-drive) through `AppCore`'s public seam at all.
//!    Every other UI suite in this crate works around this by building the
//!    desired `World` state *before* handing it to `AppCore::with_sim` (see
//!    `support::synthetic_world_for_selection_regression`'s doc comment for
//!    another instance of exactly this workaround). This suite does the same
//!    thing: elimination is pre-arranged on the raw `World` before it's
//!    wrapped, not performed mid-game through `AppCore`. The `game_over()`
//!    *resolution* itself still happens for real, inside a real `tick()`
//!    driven by `update()` — only the "kill the unit" step had to happen
//!    before wrapping. A `world_mut()` (or a narrower "kill house X" test
//!    seam) would remove the need for this workaround; flagged to ra-coder.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Difficulty, EconRules, GameOver, MoveStats, Passability,
    World,
};

const B_HUT: u32 = 0;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "HUT".to_string(),
            foot_w: 1,
            foot_h: 1,
            max_health: 100,
            armor: 0,
            power: 0,
            cost: 10,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
        }],
        units: vec![],
        econ: EconRules::default(),
    }
}

/// Which side to pre-eliminate when building a [`GameOverFixture`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Eliminate {
    /// House 2 (AI) starts with nothing — the world should resolve Victory.
    Ai,
    /// House 1 (player) starts with nothing — the world should resolve Defeat.
    Player,
}

/// A synthetic win/lose fixture wrapped in `AppCore`: house 1 is the
/// controlled player, house 2 is the sole `AiPlayer`. Per this file's module
/// doc (structural finding 2 — `AppCore` has no `world_mut()`), the losing
/// side's assets are omitted *before* the `World` is wrapped, rather than
/// removed mid-drive; `game_over()` still resolves for real, inside a real
/// `tick()`, the first time `AppCore::update` steps the sim.
struct GameOverFixture {
    core: AppCore,
    /// The surviving house's live unit (always present; the other house's
    /// assets don't exist in this fixture at all).
    survivor_unit_cell: CellCoord,
}

fn gameover_fixture(seed: u32, eliminate: Eliminate) -> GameOverFixture {
    let (raster, palette) = support::synthetic_fixture();
    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(catalog());
    world.init_houses(3, 1000);

    let survivor_unit_cell = CellCoord::new(25, 25);
    if eliminate != Eliminate::Player {
        world
            .spawn_building(B_HUT, 1, CellCoord::new(20, 20))
            .unwrap();
        world.spawn_unit(0, 1, survivor_unit_cell, Facing(0), 100, stats());
    }
    if eliminate != Eliminate::Ai {
        world
            .spawn_building(B_HUT, 2, CellCoord::new(60, 60))
            .unwrap();
        world.spawn_unit(0, 2, CellCoord::new(65, 65), Facing(0), 100, stats());
    }

    world.set_player_house(1);
    world.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    // Sidebar enabled: matches real game-mode rendering, and is required for
    // `compose_game`'s overlay to have a sane tactical width to centre in
    // (see the module doc's structural finding).
    core.enable_sidebar(1, Vec::new());

    GameOverFixture {
        core,
        survivor_unit_cell,
    }
}

/// Count pixels in `frame` matching `rgb` exactly (ignoring alpha).
fn count_pixels(frame: &ra_client::appcore::Frame, rgb: [u8; 3]) -> usize {
    frame
        .pixels
        .chunks_exact(4)
        .filter(|px| px[0] == rgb[0] && px[1] == rgb[1] && px[2] == rgb[2])
        .count()
}

const VICTORY_RGB: [u8; 3] = [120, 240, 120];
const DEFEAT_RGB: [u8; 3] = [240, 90, 90];

/// Step the sim `n` virtual frames (~1 tick each, `TICKS_PER_SECOND` = 15).
fn step(core: &mut AppCore, n: u32) {
    for _ in 0..n {
        core.update(67);
    }
}

// ---------------------------------------------------------------------
// 1. Drive to Victory / Defeat; assert the overlay actually paints pixels.
// ---------------------------------------------------------------------

#[test]
fn victory_paints_the_victory_overlay() {
    // AI (house 2) starts with nothing: house 1 wins the moment
    // `update_game_over` first runs, on the very first ticked `update()`.
    let mut f = gameover_fixture(0x0FF1_CE01, Eliminate::Ai);

    assert_eq!(f.core.game_over(), GameOver::Ongoing, "before any update()");
    let pre = f.core.compose_game();
    assert_eq!(count_pixels(&pre, VICTORY_RGB), 0);
    assert_eq!(count_pixels(&pre, DEFEAT_RGB), 0);

    step(&mut f.core, 3);

    assert_eq!(f.core.game_over(), GameOver::Victory);
    let post = f.core.compose_game();
    assert!(
        count_pixels(&post, VICTORY_RGB) > 0,
        "compose_game() after Victory should paint at least one VICTORY-green pixel"
    );
    assert_eq!(
        count_pixels(&post, DEFEAT_RGB),
        0,
        "a Victory frame must not also contain DEFEAT-red pixels"
    );

    // compose_camera() delegates to compose_game() when the sidebar is
    // enabled, so it must show the same thing.
    let via_camera = f.core.compose_camera();
    assert!(count_pixels(&via_camera, VICTORY_RGB) > 0);
}

#[test]
fn defeat_paints_the_defeat_overlay() {
    let mut f = gameover_fixture(0x0FF1_CE02, Eliminate::Player);

    step(&mut f.core, 3);

    assert_eq!(f.core.game_over(), GameOver::Defeat);
    let post = f.core.compose_game();
    assert!(
        count_pixels(&post, DEFEAT_RGB) > 0,
        "compose_game() after Defeat should paint at least one DEFEAT-red pixel"
    );
    assert_eq!(count_pixels(&post, VICTORY_RGB), 0);
}

// ---------------------------------------------------------------------
// 2. After game over: does the UI still let you select/order units?
// ---------------------------------------------------------------------

/// **Finding.** `AppCore::accepting_orders()` (`self.world.game_over() ==
/// GameOver::Ongoing`) gates `issue_order`, `deploy_selected`, `place_at`, and
/// `sidebar_click` — every *order-emitting* input path. It does **not** gate
/// selection: a left-drag/click still updates `self.selected` after the game
/// ends (`finish_selection` has no such check). So: yes, you can still select
/// units after the game ends; no, you cannot issue them any further orders
/// through input events. `drain_commands()` after further input stays empty
/// either way. Pinned here so a future change to either half of that split
/// shows up as an intentional diff.
#[test]
fn after_game_over_selection_still_works_but_no_orders_are_emitted() {
    let mut f = gameover_fixture(0x0FF1_CE03, Eliminate::Ai);
    step(&mut f.core, 3);
    assert_eq!(f.core.game_over(), GameOver::Victory);
    f.core.drain_commands(); // discard whatever the setup emitted

    // Centre the camera on the surviving player unit first — the default
    // camera sits at the map origin, far from a unit spawned near cell
    // (25,25), so an un-recentred click would land in empty terrain (or the
    // sidebar strip) rather than on the unit.
    let sc = f.survivor_unit_cell;
    let tw = f.core.tactical_width();
    f.core
        .set_camera((sc.x * 24 - tw as i32 / 2) as f32, (sc.y * 24 - 200) as f32);

    // Box-select the surviving player unit.
    let r = f.core.camera_rect();
    let (px, py) = (
        (sc.x * 24 - r.x as i32).max(0),
        (sc.y * 24 - r.y as i32).max(0),
    );
    f.core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: (px - 30).max(0),
        y: (py - 30).max(0),
    });
    f.core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: px + 30,
        y: py + 30,
    });
    assert!(
        !f.core.selected_handles().is_empty(),
        "FINDING: selection should still work after game_over() != Ongoing (no gate on \
         finish_selection)"
    );

    // Right-click to issue a move order: must be a no-op post-game-over.
    f.core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: px + 60,
        y: py + 60,
    });
    f.core.handle(InputEvent::MouseUp {
        button: MouseButton::Right,
        x: px + 60,
        y: py + 60,
    });
    assert!(
        f.core.drain_commands().is_empty(),
        "issue_order must not emit a Move/Attack once accepting_orders() is false"
    );

    // Deploy key: also gated.
    f.core.handle(InputEvent::KeyDown(Key::Deploy));
    f.core.handle(InputEvent::KeyUp(Key::Deploy));
    assert!(f.core.drain_commands().is_empty());

    // Sidebar click: also gated (row is empty here, but the gate itself is
    // checked first regardless of what's in the sidebar).
    f.core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: 600,
        y: 50,
    });
    f.core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: 600,
        y: 50,
    });
    assert!(f.core.drain_commands().is_empty());
}

// ---------------------------------------------------------------------
// 3. Monkey-ish: a modest fixed sequence of varied InputEvents post-game-over.
// ---------------------------------------------------------------------

/// A fixed (not proptest-random — "modest sequence" per the task, not a full
/// fuzz suite) but deliberately varied sequence of ~40 `InputEvent`s: clicks
/// both inside the tactical area and the sidebar strip, drags, key taps,
/// mouse moves in/out of bounds, and a resize. Exercises the same event
/// vocabulary `ui_monkey.rs`'s proptest strategies do, just hand-rolled.
fn varied_input_sequence() -> Vec<InputEvent> {
    let mut ev = Vec::new();
    for i in 0..8i32 {
        let x = 40 + i * 47;
        let y = 30 + i * 23;
        ev.push(InputEvent::MouseMoved { x, y });
        ev.push(InputEvent::MouseDown {
            button: if i % 2 == 0 {
                MouseButton::Left
            } else {
                MouseButton::Right
            },
            x,
            y,
        });
        ev.push(InputEvent::MouseUp {
            button: if i % 2 == 0 {
                MouseButton::Left
            } else {
                MouseButton::Right
            },
            x: x + 15,
            y: y + 15,
        });
    }
    // Sidebar-area clicks.
    for i in 0..5i32 {
        let x = 520 + i * 20;
        let y = 40 + i * 25;
        ev.push(InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
        });
        ev.push(InputEvent::MouseUp {
            button: MouseButton::Left,
            x,
            y,
        });
    }
    // Key taps.
    for k in [Key::Left, Key::Right, Key::Up, Key::Down, Key::Deploy] {
        ev.push(InputEvent::KeyDown(k));
        ev.push(InputEvent::KeyUp(k));
    }
    ev.push(InputEvent::MouseLeft);
    ev.push(InputEvent::Resize {
        width: 300,
        height: 200,
    });
    ev.push(InputEvent::MouseMoved { x: -50, y: -50 });
    ev.push(InputEvent::MouseMoved { x: 9000, y: 9000 });
    ev
}

#[test]
fn post_game_over_monkey_sequence_never_panics_and_leaks_no_command() {
    for (label, eliminate_ai) in [("victory", true), ("defeat", false)] {
        let eliminate = if eliminate_ai {
            Eliminate::Ai
        } else {
            Eliminate::Player
        };
        let mut f = gameover_fixture(0x0FF1_CE04, eliminate);
        step(&mut f.core, 3);
        assert_ne!(
            f.core.game_over(),
            GameOver::Ongoing,
            "setup ({label}): should be terminal"
        );
        f.core.drain_commands();

        let alive_house = if eliminate_ai { 1u8 } else { 2u8 };
        let dead_house = if eliminate_ai { 2u8 } else { 1u8 };

        for ev in varied_input_sequence() {
            f.core.handle(ev);
            // Interleave a virtual-time step so `update()` also gets fuzzed
            // alongside raw input, like the ui_monkey.rs convention.
            f.core.update(33);
            for cmd in f.core.drain_commands() {
                // Any command that slipped through despite the game being
                // over must still be well-formed: it must never reference
                // the eliminated house's (nonexistent) assets.
                let house = match cmd {
                    ra_sim::Command::Move { house, .. }
                    | ra_sim::Command::Stop { house, .. }
                    | ra_sim::Command::Attack { house, .. }
                    | ra_sim::Command::Deploy { house, .. }
                    | ra_sim::Command::StartProduction { house, .. }
                    | ra_sim::Command::CancelProduction { house, .. }
                    | ra_sim::Command::PlaceBuilding { house, .. }
                    | ra_sim::Command::Sell { house, .. } => house,
                };
                assert_ne!(
                    house, dead_house,
                    "({label}) drained {cmd:?} references the eliminated house {dead_house}"
                );
                assert_eq!(
                    house, alive_house,
                    "({label}) drained {cmd:?} was not scoped to the controlled house"
                );
            }
        }
        // No panic reaching here is itself the main assertion (implicit);
        // one final compose to confirm the seam stays usable post-sequence.
        let _ = f.core.compose_game();
    }
}
