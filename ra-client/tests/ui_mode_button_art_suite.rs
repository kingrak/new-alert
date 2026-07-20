//! M7.12 audit — SELL/REPAIR mode-button coverage beyond `ui_sell_repair.rs`
//! (which only exercises the **text-fallback** geometry, since its fixtures
//! never call `set_mode_button_art`). This suite adds:
//! 1. Hit-test alignment pins for **both** geometries — click inside the
//!    button rect arms the mode, just outside doesn't — for the text
//!    fallback (already implicit in `ui_sell_repair.rs`, re-derived here
//!    explicitly against the real rect math) and for the **art-installed**
//!    geometry (`appcore.rs`'s `mode_btn_art_dims`/`sell_button_rect`/
//!    `repair_button_rect`, `1780-1820`).
//! 2. Pressed-frame rendering, pinned via a compose-hash differential: arming
//!    a mode must change the composed pixels (frame 1 draws), and the same
//!    state composed twice must be byte-identical (determinism).
//! 3. Radar-present layout: with the sidebar radar enabled, the mode-button
//!    rect and the radar panel rect must be disjoint.
//!
//! **Regression guard for the M7.12 header-widening fix (ra-tester audit).**
//! With real (`hires.mix`-sized, 34×28) button art installed *and* the radar
//! panel enabled, the SELL/REPAIR buttons would vertically overlap the top of
//! the radar panel unless `sidebar_header_h()` (the radar's `y0`) is widened to
//! clear the taller art. The production fix (`appcore.rs`) makes
//! `sidebar_header_h()` take `text_h.max(1 + art_h + 1)` when
//! `mode_btn_art_dims()` reports installed art; with no art it stays the
//! font-derived 22px (text buttons are 9px and already fit), so no-asset
//! goldens are byte-identical. The radar-layout tests below read the header
//! height from the core's REAL geometry (`core.sidebar_header_h()`), so they
//! observe that fix directly: revert the art-widening and the real-asset test
//! fails (the buttons overlap the radar again) — verified in the M7.12
//! revert-sensitivity pass.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, MouseButton};
use ra_client::unit_render::{SpriteFrame, UnitSprite};

const VIEWPORT_W: u32 = 640;
const VIEWPORT_H: u32 = 400;
const SIDEBAR_W: i32 = 130; // ra_client::appcore::SIDEBAR_W

/// Deliberately non-square, non-`MODE_BTN_W`-sized fake art (20x16, 3 frames:
/// up / pressed / disabled) so the art-installed geometry math in this test
/// is manifestly independent of the text-fallback constants, not a
/// coincidental match.
fn fake_mode_button_art() -> UnitSprite {
    const W: u32 = 20;
    const H: u32 = 16;
    UnitSprite {
        frames: vec![
            SpriteFrame {
                width: W,
                height: H,
                pixels: vec![1u8; (W * H) as usize],
            },
            SpriteFrame {
                width: W,
                height: H,
                pixels: vec![2u8; (W * H) as usize],
            },
            SpriteFrame {
                width: W,
                height: H,
                pixels: vec![3u8; (W * H) as usize],
            },
        ],
    }
}

fn core_with_art(art_installed: bool, radar: bool) -> AppCore {
    let (mut world, _mcv) = support::synthetic_world_with_econ(0x5E11_0BA7, 5000);
    world
        .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(2, 2))
        .expect("own PROC spawns");
    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, support::econ_buildables());
    if art_installed {
        core.set_mode_button_art(Some(fake_mode_button_art()), Some(fake_mode_button_art()));
    }
    if radar {
        core.enable_radar();
    }
    core.handle(InputEvent::Resize {
        width: VIEWPORT_W,
        height: VIEWPORT_H,
    });
    core.set_camera(0.0, 0.0);
    core
}

fn click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
}

use ra_sim::coords::CellCoord;

// ===========================================================================
// 1. Hit-test alignment: text-fallback geometry (no art installed).
//    `sell_button_rect` = (vw-2-34, 1, vw-2, 1+9); `repair_button_rect`
//    stacks directly below it (`appcore.rs`'s `None` branch of both rects).
// ===========================================================================

#[test]
fn text_fallback_hit_test_arms_inside_and_misses_just_outside() {
    const W: i32 = 34;
    const H: i32 = 9;
    let x1 = VIEWPORT_W as i32 - 2;
    let x0 = x1 - W;
    let sell_rect = (x0, 1, x1, 1 + H);
    let repair_rect = (x0, 1 + H + 1, x1, 1 + H + 1 + H);

    // SELL: centre hits, arms.
    let mut core = core_with_art(false, false);
    assert!(!core.sell_mode());
    click(
        &mut core,
        (sell_rect.0 + sell_rect.2) / 2,
        (sell_rect.1 + sell_rect.3) / 2,
    );
    assert!(
        core.sell_mode(),
        "click at the SELL rect centre must arm sell mode"
    );

    // Just outside the SELL rect's left edge (x0-1): must miss (arm nothing).
    let mut core = core_with_art(false, false);
    click(&mut core, sell_rect.0 - 1, (sell_rect.1 + sell_rect.3) / 2);
    assert!(
        !core.sell_mode(),
        "one pixel left of the SELL rect must not arm it"
    );

    // Just outside the SELL rect's bottom edge (y1, exclusive): must miss.
    let mut core = core_with_art(false, false);
    click(&mut core, (sell_rect.0 + sell_rect.2) / 2, sell_rect.3);
    assert!(!core.sell_mode(), "the SELL rect's y1 must be exclusive");

    // REPAIR: centre hits, arms.
    let mut core = core_with_art(false, false);
    assert!(!core.repair_mode());
    click(
        &mut core,
        (repair_rect.0 + repair_rect.2) / 2,
        (repair_rect.1 + repair_rect.3) / 2,
    );
    assert!(
        core.repair_mode(),
        "click at the REPAIR rect centre must arm repair mode"
    );

    // Just above the REPAIR rect (inside the 1px gap between the two
    // stacked buttons, i.e. neither rect): must arm neither.
    let mut core = core_with_art(false, false);
    click(
        &mut core,
        (repair_rect.0 + repair_rect.2) / 2,
        repair_rect.1 - 1,
    );
    assert!(
        !core.sell_mode() && !core.repair_mode(),
        "the 1px gap between stacked buttons must hit neither"
    );
}

// ===========================================================================
// 2. Hit-test alignment: art-installed geometry (side-by-side, native size).
// ===========================================================================

#[test]
fn art_installed_hit_test_arms_inside_and_misses_just_outside() {
    const W: i32 = 20;
    const H: i32 = 16;
    let x1 = VIEWPORT_W as i32 - 2;
    let sell_rect = (x1 - W, 1, x1, 1 + H);
    let repair_x1 = sell_rect.0 - 1;
    let repair_rect = (repair_x1 - W, 1, repair_x1, 1 + H);

    let mut core = core_with_art(true, false);
    assert!(!core.sell_mode());
    click(
        &mut core,
        (sell_rect.0 + sell_rect.2) / 2,
        (sell_rect.1 + sell_rect.3) / 2,
    );
    assert!(
        core.sell_mode(),
        "click at the art SELL rect centre must arm sell mode"
    );

    let mut core = core_with_art(true, false);
    click(&mut core, sell_rect.2, (sell_rect.1 + sell_rect.3) / 2);
    assert!(
        !core.sell_mode(),
        "the art SELL rect's x1 must be exclusive"
    );

    let mut core = core_with_art(true, false);
    assert!(!core.repair_mode());
    click(
        &mut core,
        (repair_rect.0 + repair_rect.2) / 2,
        (repair_rect.1 + repair_rect.3) / 2,
    );
    assert!(
        core.repair_mode(),
        "click at the art REPAIR rect centre must arm repair mode"
    );

    // The 1px gap between the two side-by-side art buttons hits neither.
    let mut core = core_with_art(true, false);
    // repair_x1 itself is sell_rect.0 - 1, i.e. the gap column.
    click(&mut core, repair_x1, (repair_rect.1 + repair_rect.3) / 2);
    assert!(
        !core.sell_mode() && !core.repair_mode(),
        "the 1px gap between the side-by-side art buttons must hit neither"
    );

    // Sanity: the art rects are NOT where the text-fallback rects would be
    // (different size/layout entirely), so this is really exercising the
    // art geometry, not accidentally re-testing test 1.
    assert_ne!(
        sell_rect,
        (VIEWPORT_W as i32 - 2 - 34, 1, VIEWPORT_W as i32 - 2, 1 + 9)
    );
}

// ===========================================================================
// 3. Pressed-frame rendering, pinned via a compose-hash differential.
// ===========================================================================

#[test]
fn arming_sell_mode_changes_the_composed_frame_deterministically() {
    let mut core = core_with_art(true, false);

    // The sidebar (and its mode buttons) only render through `compose_game`
    // — plain `compose(Rect)` is the terrain-only camera view and never
    // touches the sidebar overlay at all (`appcore.rs:1130` vs `:1165`).
    let unarmed_a = core.compose_game();
    let unarmed_b = core.compose_game();
    assert_eq!(
        support::fnv1a(&unarmed_a.pixels),
        support::fnv1a(&unarmed_b.pixels),
        "composing the same (unarmed) state twice must be byte-identical"
    );

    core.toggle_sell_mode();
    let armed_a = core.compose_game();
    let armed_b = core.compose_game();
    assert_eq!(
        support::fnv1a(&armed_a.pixels),
        support::fnv1a(&armed_b.pixels),
        "composing the same (armed) state twice must be byte-identical"
    );

    assert_ne!(
        support::fnv1a(&unarmed_a.pixels),
        support::fnv1a(&armed_a.pixels),
        "arming sell mode must change the composed frame (pressed-frame art must actually render)"
    );
}

// ===========================================================================
// 4. Radar-present layout: the mode-button rect and the radar panel rect
//    must be disjoint (buttons live in the header, above the radar).
// ===========================================================================

fn rects_disjoint(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.2 <= b.0 || b.2 <= a.0 || a.3 <= b.1 || b.3 <= a.1
}

#[test]
fn text_fallback_buttons_never_overlap_the_radar_panel() {
    const W: i32 = 34;
    const H: i32 = 9;
    let x1 = VIEWPORT_W as i32 - 2;
    let sell_rect = (x1 - W, 1, x1, 1 + H);
    let repair_rect = (x1 - W, 1 + H + 1, x1, 1 + H + 1 + H);

    let core = core_with_art(false, true);
    assert!(core.has_radar(), "test setup: radar should be active");
    // radar_rect = (tactical_width+2, sidebar_header_h(), RADAR_SIZE). `y0` is
    // read from the core's REAL geometry (public `sidebar_header_h()`), NOT a
    // hardcoded constant, so the assertion tracks the production layout exactly.
    // With no art installed this is the font-derived 22px header.
    let tactical_w = VIEWPORT_W as i32 - SIDEBAR_W;
    const RADAR_SIZE: i32 = 120;
    let header_h = core.sidebar_header_h();
    let radar_rect = (
        tactical_w + 2,
        header_h,
        tactical_w + 2 + RADAR_SIZE,
        header_h + RADAR_SIZE,
    );

    assert!(
        rects_disjoint(sell_rect, radar_rect),
        "text SELL button {sell_rect:?} overlaps the radar panel {radar_rect:?}"
    );
    assert!(
        rects_disjoint(repair_rect, radar_rect),
        "text REPAIR button {repair_rect:?} overlaps the radar panel {radar_rect:?}"
    );
}

/// **Regression guard for the header-widening fix (see module doc).** Using
/// real-asset-sized (34×28) art — not this suite's default 20×16 fake, to
/// match the shipped `SELL.SHP`/`REPAIR.SHP` dimensions exactly — with the
/// radar enabled, the button rects must NOT overlap the radar panel. The radar
/// rect's `y0` is read from the core's real geometry (`sidebar_header_h()`), so
/// this observes the production art-widening: with the fix, `y0` is pushed to
/// `1 + 28 + 1 = 30`, clearing the 34×28 buttons (which occupy y=1..29); revert
/// the widening and `y0` falls back to 22, the buttons overlap, and this fails.
#[test]
fn real_asset_sized_buttons_must_not_overlap_the_radar_panel() {
    const W: i32 = 34;
    const H: i32 = 28; // hires.mix SELL.SHP/REPAIR.SHP native size
    let art = UnitSprite {
        frames: vec![
            SpriteFrame {
                width: W as u32,
                height: H as u32,
                pixels: vec![1u8; (W * H) as usize],
            },
            SpriteFrame {
                width: W as u32,
                height: H as u32,
                pixels: vec![2u8; (W * H) as usize],
            },
            SpriteFrame {
                width: W as u32,
                height: H as u32,
                pixels: vec![3u8; (W * H) as usize],
            },
        ],
    };
    let (mut world, _mcv) = support::synthetic_world_with_econ(0x5E11_0BA8, 5000);
    world
        .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(2, 2))
        .expect("own PROC spawns");
    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, support::econ_buildables());
    core.set_mode_button_art(Some(art.clone()), Some(art));
    core.enable_radar();
    core.handle(InputEvent::Resize {
        width: VIEWPORT_W,
        height: VIEWPORT_H,
    });
    assert!(core.has_radar(), "test setup: radar should be active");

    let x1 = VIEWPORT_W as i32 - 2;
    let sell_rect = (x1 - W, 1, x1, 1 + H);
    let repair_x1 = sell_rect.0 - 1;
    let repair_rect = (repair_x1 - W, 1, repair_x1, 1 + H);

    // Read the radar panel's real top edge from the core (NOT a hardcoded 22):
    // this is exactly what the production art-widening fix moves. With 34×28 art
    // installed the fixed `sidebar_header_h()` returns 30, so the radar clears
    // the buttons; the stale hardcoded 22 that the original pin used could never
    // have observed that change.
    let tactical_w = VIEWPORT_W as i32 - SIDEBAR_W;
    const RADAR_SIZE: i32 = 120;
    let header_h = core.sidebar_header_h();
    let radar_rect = (
        tactical_w + 2,
        header_h,
        tactical_w + 2 + RADAR_SIZE,
        header_h + RADAR_SIZE,
    );

    assert!(
        rects_disjoint(sell_rect, radar_rect),
        "real-asset-sized SELL button {sell_rect:?} overlaps the radar panel {radar_rect:?} — \
         sidebar_header_h()={header_h} did not clear the real (34x28) art"
    );
    assert!(
        rects_disjoint(repair_rect, radar_rect),
        "real-asset-sized REPAIR button {repair_rect:?} overlaps the radar panel {radar_rect:?}"
    );
}
