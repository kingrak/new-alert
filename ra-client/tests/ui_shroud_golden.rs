//! M6 shroud UI coverage (DESIGN.md §4.8 layer 4: golden frames) — the
//! client-side mirror of `ra-sim/tests/shroud_suite.rs`, driving the shroud
//! entirely through `AppCore`'s windowless seam (`handle`/`update`/
//! `compose_game`/`drain_commands`/`world()`), never a live window.
//!
//! **Which compose to use.** `AppCore::compose(Rect)` (used by
//! `ui_golden_frames.rs`) is the *raw* terrain+units viewport compositor —
//! it never draws the shroud overlay (see `appcore.rs::draw_shroud`, which is
//! only called from `compose_game`/`compose_camera`-when-sidebar-enabled).
//! So every test here drives the sidebar-enabled path and asserts on
//! `compose_game()`, the actual seam the real game view (and the windowed
//! shell, once a skirmish is booted) renders through.
//!
//! Synthetic tests always run (no real assets needed, same hand-built
//! terrain fixture `ui_golden_frames.rs` uses). The real-map variant loads a
//! real skirmish (`ra_client::assets::load_skirmish_from_dir`, shroud enabled
//! by construction — see `assets.rs`'s M6 skirmish loader) and skips cleanly
//! if the real archives are absent; they are present in this environment, so
//! it actually exercises the real path here.

mod support;

use ra_client::appcore::AppCore;
use ra_client::assets;
use ra_client::input::{InputEvent, MouseButton};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Handle, MoveStats, Passability, World};

const CELL_PIXELS: i32 = 24;

/// A shroud-enabled synthetic world: one house-1 unit with a modest sight
/// range, on the same hand-built synthetic terrain `ui_golden_frames.rs`
/// uses (via `support::synthetic_fixture`), so the goldens here need no real
/// assets.
fn synthetic_shroud_world(seed: u32) -> (World, Handle) {
    let mut world = World::new(Passability::all_passable(), seed);
    world.enable_shroud();
    let unit = world.spawn_unit(
        0,
        1,
        CellCoord::new(5, 5),
        Facing(0),
        256,
        MoveStats {
            max_speed: 25,
            rot: 10,
        },
    );
    world.set_unit_sight(unit, 4);
    (world, unit)
}

/// [`synthetic_shroud_world`] wrapped in an `AppCore` with the sidebar
/// enabled for house 1 (so `compose_game`'s `draw_shroud` engages — see the
/// module docs on why `compose_game`, not plain `compose`, is used
/// throughout this file).
fn synthetic_shroud_core(seed: u32) -> (AppCore, Handle) {
    let (raster, palette) = support::synthetic_fixture();
    let (world, unit) = synthetic_shroud_world(seed);
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, Vec::new());
    (core, unit)
}

/// Select `unit` and right-click-order it to `dest_cell`, draining the
/// emitted command, then step `ticks` virtual frames (~1 sim tick each).
/// Mirrors `support::run_select_and_move_script`'s click math, but selects
/// directly via `AppCore::select_units` (a handle is already known here) and
/// returns nothing — callers care about the resulting `compose_game()`
/// frame, not the hash chain.
fn move_unit_and_settle(core: &mut AppCore, unit: Handle, dest_cell: CellCoord, ticks: u32) {
    core.select_units(&[unit]);
    let r = core.camera_rect();
    let dest_vx = (dest_cell.x * CELL_PIXELS) as i64 + CELL_PIXELS as i64 / 2 - r.x;
    let dest_vy = (dest_cell.y * CELL_PIXELS) as i64 + CELL_PIXELS as i64 / 2 - r.y;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: dest_vx as i32,
        y: dest_vy as i32,
    });
    core.drain_commands();
    for _ in 0..ticks {
        core.update(67);
    }
}

/// Average `[r, g, b]` over the pixels in viewport rect `[x0,x1) x [y0,y1)`
/// of an RGBA `Frame`'s pixel buffer (row-major, 4 bytes/pixel).
fn avg_rgb(
    frame: &ra_client::compositor::RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
) -> [f64; 3] {
    let mut sum = [0f64; 3];
    let mut n = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y as usize) * (frame.width as usize) + x as usize) * 4;
            sum[0] += frame.pixels[i] as f64;
            sum[1] += frame.pixels[i + 1] as f64;
            sum[2] += frame.pixels[i + 2] as f64;
            n += 1.0;
        }
    }
    [sum[0] / n, sum[1] / n, sum[2] / n]
}

// ---------------------------------------------------------------------
// Synthetic scenario (always runs, no real assets).
// ---------------------------------------------------------------------

/// (a) `compose_game()` does not panic on a shroud-enabled synthetic core,
/// with a unit that has moved and revealed a partial map.
#[test]
fn synthetic_compose_game_with_shroud_does_not_panic() {
    let (mut core, unit) = synthetic_shroud_core(0x5000_0001);
    move_unit_and_settle(&mut core, unit, CellCoord::new(10, 5), 40);
    let _frame = core.compose_game();
}

/// (b) Pinned regression hash of the composed frame after a fixed
/// select→move→settle script. A change here means either a real rendering
/// regression (terrain/unit/shroud compositing) or a deliberate change;
/// update the pin with a comment explaining why (same policy as
/// `ui_golden_frames.rs`).
#[test]
fn synthetic_shroud_frame_golden_hash() {
    let (mut core, unit) = synthetic_shroud_core(0x5000_0002);
    move_unit_and_settle(&mut core, unit, CellCoord::new(10, 5), 40);
    let frame = core.compose_game();
    assert_eq!(frame.width, 640);
    assert_eq!(frame.height, 400);
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0xbfef_0bbd_9db5_dab6,
        "synthetic shroud frame hash changed"
    );
}

/// (c) A shrouded (unexplored) region renders visibly differently from an
/// explored one: pure black (shroud's `fill_rect(..., [0, 0, 0])`) vs the
/// synthetic terrain's palette colours, which are never pure black (every
/// template fill index maps through `synthetic_palette` to `[i, 255-i,
/// 128]` — the blue channel is always 128, so no explored terrain pixel can
/// read as `[0, 0, 0]`). This is a meaningful pixel check, not just "some
/// pixel isn't equal to some other pixel".
#[test]
fn synthetic_shroud_paints_unexplored_cells_visibly_darker_than_explored() {
    let (mut core, unit) = synthetic_shroud_core(0x5000_0003);
    // A short, local move so the far corner of the default 640x400 (26x16
    // cell) viewport stays untouched by the unit's sight-4 disc the whole
    // time.
    move_unit_and_settle(&mut core, unit, CellCoord::new(7, 5), 15);
    let frame = core.compose_game();

    // Explored patch: right on top of the unit's starting cell (5,5), well
    // inside its sight-4 disc for the whole script.
    let explored = avg_rgb(
        &frame,
        5 * CELL_PIXELS,
        5 * CELL_PIXELS,
        6 * CELL_PIXELS,
        6 * CELL_PIXELS,
    );
    // Shrouded patch: a cell well within the *tactical* strip (sidebar takes
    // the rightmost `SIDEBAR_W` = 130px, so cell 19 at x=[456,480) is safely
    // clear of it) but 12+ cells from the unit's entire path -- never
    // revealed.
    let shrouded = avg_rgb(
        &frame,
        19 * CELL_PIXELS,
        14 * CELL_PIXELS,
        20 * CELL_PIXELS,
        15 * CELL_PIXELS,
    );

    assert_eq!(
        shrouded,
        [0.0, 0.0, 0.0],
        "an unexplored cell should render as solid black"
    );
    assert_ne!(
        explored,
        [0.0, 0.0, 0.0],
        "an explored terrain cell should never render as solid black"
    );
    // Concrete, not just "differs": the shrouded patch is strictly darker on
    // every channel (explored terrain's blue channel alone is always 128).
    for c in 0..3 {
        assert!(
            explored[c] > shrouded[c],
            "channel {c}: explored ({}) should be brighter than shrouded ({})",
            explored[c],
            shrouded[c]
        );
    }
}

/// (d) `compose_game()` is stable across two independent calls on the same
/// (unchanging) state -- no nondeterministic rendering (e.g. an
/// uninitialized buffer, iteration-order-dependent overlay).
#[test]
fn synthetic_shroud_frame_is_stable_across_repeat_compose() {
    let (mut core, unit) = synthetic_shroud_core(0x5000_0004);
    move_unit_and_settle(&mut core, unit, CellCoord::new(10, 5), 40);
    let a = core.compose_game();
    let b = core.compose_game();
    assert_eq!(
        a.pixels, b.pixels,
        "compose_game() is not deterministic across repeat calls on unchanged state"
    );
}

// ---------------------------------------------------------------------
// Real-map (skip-clean) variant.
// ---------------------------------------------------------------------

/// Real skirmish scenario name — the same default the windowed shell's `M6
/// FIRST PLAYABLE` boot path uses (`ra-client/src/bin/ra-client.rs::
/// cmd_window`), a real multiplayer map.
const SKIRMISH_SCENARIO: &str = "scm01ea.ini";

#[test]
fn real_skirmish_compose_game_map_sweep_with_shroud_no_panic() {
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy main.mix/redalert.mix \
             into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let game = match assets::load_skirmish_from_dir(
        &support::assets_dir(),
        SKIRMISH_SCENARIO,
        5000,
        ra_sim::Difficulty::Normal,
    ) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: could not load real skirmish '{SKIRMISH_SCENARIO}': {e}");
            return;
        }
    };
    let mut core = game.core;
    assert!(
        core.world().shroud.is_enabled(),
        "sanity: the M6 skirmish loader must enable the shroud"
    );

    // Drive a few ticks so the player's MCV has revealed some terrain and the
    // AI is doing whatever it does.
    for _ in 0..20 {
        core.update(67);
    }

    // A small map sweep: a handful of viewport rects/corners, with shroud
    // active, none of which may panic.
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });
    let sweep_positions: [(f32, f32); 5] = [
        (0.0, 0.0),
        (3000.0, 3000.0), // far corner
        (
            (game.player_start.x * CELL_PIXELS) as f32,
            (game.player_start.y * CELL_PIXELS) as f32,
        ),
        (
            (game.ai_start.x * CELL_PIXELS) as f32,
            (game.ai_start.y * CELL_PIXELS) as f32,
        ),
        (1500.0, 100.0),
    ];
    let mut frames = Vec::new();
    for &(x, y) in &sweep_positions {
        core.set_camera(x, y);
        let frame = core.compose_game();
        assert_eq!(frame.width, 640);
        assert_eq!(frame.height, 400);
        frames.push(frame.pixels);
    }

    // Pinned golden: fnv1a of each sweep position's frame, in order. Derived
    // once against the real assets (same "computed once, read back, pinned"
    // policy as every other golden hash in this repo -- see
    // `ui_golden_frames.rs`'s doc comment).
    //
    // **Re-pinned for M7.** `compose_game` gained the M7 game-surface layers:
    // real ore/gem overlay art, the client animation layer, and the **radar
    // minimap panel** (now installed by `load_skirmish`). The radar draws the
    // current *camera view-box*, which differs per sweep position, so the frames
    // no longer collapse to two classes. Positions 1 and 3 still hash identically
    // (their clamped cameras coincide); position 2 (player start) additionally
    // shows the explored terrain/ore under `draw_shroud`, which paints against
    // `player_house`'s own exploration only -- the same per-house isolation
    // `shroud_suite.rs` pins at the sim level, now visible at the pixel level.
    //
    // **Re-pinned for M7.6** (infantry + land-type passability): `load_skirmish`
    // now builds the per-locomotor land-type passability grid (rock/cliff/river
    // impassable) instead of the water-only M3 stand-in, so the two houses' MCVs
    // start-position pathing and the shroud they reveal differ; the composed
    // game-surface frames change accordingly. A rendering-only, coordinator-
    // authorised re-pin (QUIRKS Q5/Q6). Re-derived deterministically (read once).
    //
    // **Re-pinned for M7.7 Chunk A** (two-strip scrolling sidebar + the P1
    // ground-roster additions): `draw_sidebar` was redrawn as two independently-
    // scrolling columns (structures | units), and the units column now lists the
    // fuller vehicle roster (3TNK/4TNK/ARTY/V2RL/APC/TRUK/MNLY) — enough to
    // overflow the strip, so its scroll arrows now draw too. All of this is
    // sidebar **rendering**: the sim (shroud, pathing, camera, unit spawns) is
    // untouched, so map-sweep positions 1 and 3 still hash *identically* to each
    // other, confirming only the sidebar strip moved. Coordinator-authorised;
    // re-derived deterministically (read back once).
    let golden: [u64; 5] = [
        0x3b12_32c7_c132_360e,
        0x153a_2655_33f1_b912,
        0x1fe2_a65b_2e40_7c0a,
        0x153a_2655_33f1_b912,
        0x7bba_075a_3e92_e37e,
    ];
    let got: Vec<u64> = frames.iter().map(|p| support::fnv1a(p)).collect();
    assert_eq!(
        got, golden,
        "real skirmish shroud map-sweep frame hashes changed -- either a real regression \
         or a deliberate change; update the pin with a comment"
    );
}
