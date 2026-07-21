//! M7.17-B audit (ra-tester): the "zero re-pin" claim for the grown sidebar.
//!
//! M7.17-B added five cameos to the real-asset buildables strip — HPAD/AGUN/SAM
//! (structures column) and HELI/HIND (units column) — yet changed no pinned
//! `compose_game` golden. The coder's claim is that the new items land *below*
//! the visible fold of the default sidebar, so nothing that actually renders
//! moves. This suite confirms that claim against the REAL `build_content`
//! buildables list (loaded via the campaign path, the one that enables the real
//! sidebar):
//!
//!   (a) all five items are genuinely PRESENT in the shipped buildables list;
//!   (b) each sits at a column index at/below the visible fold — derived from the
//!       public scroll API (scroll a column to its bottom: the resulting
//!       `sidebar_scroll` == `column_len - visible_rows`, so `visible_rows` is
//!       recovered exactly, no private accessor needed). `draw_sidebar_column`
//!       only paints `items[scroll .. scroll+visible_rows]`, so an item whose
//!       index >= visible_rows contributes ZERO pixels at rest — surfacing it
//!       cannot move a strip golden;
//!   (c) they are nonetheless REACHABLE — scrolling down repaints the strip.
//!
//! Skips cleanly when the real assets are absent. Note: no repo golden actually
//! pins the real `compose_game` sidebar strip (the real-asset goldens pin
//! `compose()` raw terrain; the lone synthetic `compose_game` golden uses a
//! synthetic catalog), so "zero re-pin" also holds for that independent reason —
//! this test nails the stronger, load-bearing one (below-the-fold), which is
//! what makes surfacing the cameos safe if such a golden is ever added.
//!
//! Methodology note (testability): `AppCore::cameo_for` resolves a cameo by the
//! item's index into `buildables`, but `enable_sidebar` swaps `buildables`
//! without re-setting the parallel `cameo_sprites`. So a "remove items and diff
//! the frame" approach is INVALID — dropping an early item slides every later
//! item's cameo. (Not a runtime bug: production calls `enable_sidebar` once at
//! load with cameos installed in lockstep.) Hence the index-math proof below.

mod support;

use ra_client::assets;
use ra_client::input::InputEvent;
use ra_sim::BuildItem;

/// The five buildables M7.17-B surfaced, and which column each lives in.
const AIR_STRUCTS: [&str; 3] = ["HPAD", "AGUN", "SAM"];
const AIR_UNITS: [&str; 2] = ["HELI", "HIND"];

/// Recover a column's visible-row count from the public scroll API: scroll it to
/// the bottom, then `sidebar_scroll(col) == column_len - visible_rows`, i.e.
/// `visible_rows = column_len - max_scroll`. (`column_len` == the number of that
/// column's entries in `sidebar_items`.)
fn visible_rows(core: &mut ra_client::appcore::AppCore, col: u8, column_len: usize) -> usize {
    // Reset to top, then drive down well past any real page count.
    for _ in 0..256 {
        core.handle(InputEvent::SidebarScroll {
            column: col,
            up: true,
        });
    }
    for _ in 0..256 {
        core.handle(InputEvent::SidebarScroll {
            column: col,
            up: false,
        });
    }
    let max_scroll = core.sidebar_scroll(col as usize);
    // Restore to top for subsequent checks.
    for _ in 0..256 {
        core.handle(InputEvent::SidebarScroll {
            column: col,
            up: true,
        });
    }
    column_len.saturating_sub(max_scroll)
}

#[test]
fn aircraft_cameos_sit_below_the_visible_fold_and_are_reachable_by_scroll() {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: real assets not found under {}", dir.display());
        return;
    }
    // The campaign loader is the path that enables the real `build_content`
    // sidebar (scg01ea has no yard, so every item renders greyed but present —
    // exactly the strip whose golden the claim is about).
    let mission = assets::load_campaign_from_bytes(
        &std::fs::read(dir.join("main.mix")).unwrap(),
        &std::fs::read(dir.join("redalert.mix")).unwrap(),
        "scg01ea.ini",
        ra_sim::Difficulty::Normal,
    )
    .expect("load scg01ea");
    let mut core = mission.core;
    // A classic 640x400 game viewport.
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });

    // Per-column ordered name lists (describe_buildable always yields Some, so
    // sidebar_items is the complete declared list, structures-then-units).
    let items = core.sidebar_items();
    let structs: Vec<String> = items
        .iter()
        .filter(|it| matches!(it.item, BuildItem::Building(_)))
        .map(|it| it.name.clone())
        .collect();
    let units: Vec<String> = items
        .iter()
        .filter(|it| matches!(it.item, BuildItem::Unit(_)))
        .map(|it| it.name.clone())
        .collect();

    // (a) Present.
    for name in AIR_STRUCTS {
        assert!(
            structs.iter().any(|n| n == name),
            "structure `{name}` missing from sidebar"
        );
    }
    for name in AIR_UNITS {
        assert!(
            units.iter().any(|n| n == name),
            "unit `{name}` missing from sidebar"
        );
    }

    // (b) Below the fold. Recover each column's visible-row count exactly.
    let vis0 = visible_rows(&mut core, 0, structs.len());
    let vis1 = visible_rows(&mut core, 1, units.len());
    assert!(
        vis0 > 0 && vis1 > 0,
        "sidebar must show at least one row per column"
    );

    for name in AIR_STRUCTS {
        let idx = structs.iter().position(|n| n == name).unwrap();
        assert!(
            idx >= vis0,
            "structure `{name}` at column index {idx} is WITHIN the visible \
             window (0..{vis0}) — it renders at rest, so a compose_game strip \
             golden WOULD move; the below-the-fold claim is false"
        );
    }
    for name in AIR_UNITS {
        let idx = units.iter().position(|n| n == name).unwrap();
        assert!(
            idx >= vis1,
            "unit `{name}` at column index {idx} is WITHIN the visible window \
             (0..{vis1}) — the below-the-fold claim is false"
        );
    }

    // (c) Reachable: both columns overflow, and scrolling repaints the strip
    // (the below-fold rows, incl. the aircraft/AA cameos, draw once scrolled in).
    let at_rest = support::fnv1a(&core.compose_game().pixels);
    for _ in 0..64 {
        core.handle(InputEvent::SidebarScroll {
            column: 0,
            up: false,
        });
    }
    assert!(
        core.sidebar_scroll(0) > 0,
        "structures column must overflow (scrollable)"
    );
    let scrolled = support::fnv1a(&core.compose_game().pixels);
    assert_ne!(
        at_rest, scrolled,
        "scrolling must repaint the strip — the below-fold rows (incl. the new \
         aircraft/AA cameos) render only once scrolled into view"
    );

    eprintln!(
        "SIDEBAR FOLD AUDIT: {} structures (visible {vis0}), {} units (visible {vis1}); \
         HPAD@{} AGUN@{} SAM@{} | HELI@{} HIND@{} — all below the fold",
        structs.len(),
        units.len(),
        structs.iter().position(|n| n == "HPAD").unwrap(),
        structs.iter().position(|n| n == "AGUN").unwrap(),
        structs.iter().position(|n| n == "SAM").unwrap(),
        units.iter().position(|n| n == "HELI").unwrap(),
        units.iter().position(|n| n == "HIND").unwrap(),
    );
}
