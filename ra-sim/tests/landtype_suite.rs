//! M7.6 land-type passability suite (QUIRKS Q6): per-locomotor terrain
//! passability replacing the M3 water-only stand-in.
//!
//! Covers item 4 of the M7.6 test plan:
//! - a mask-correctness table transcribed from the real `rules.ini`
//!   (skip cleanly without real assets, same policy as `damage_matrix.rs`),
//! - the "Foot-vs-Track divergence case" ask: real `rules.ini` turns out to
//!   have **none** (documented below, not invented),
//! - a property test that A* never routes any locomotor through a cell that
//!   locomotor cannot enter, over per-locomotor grids (`Passability::per_locomotor`,
//!   which `astar_properties.rs`'s existing property suite does not exercise —
//!   it only builds grids via the single-mask `Passability::new`),
//! - a synthetic case that positively demonstrates per-locomotor divergence
//!   *works* in the engine (a corridor only `Track` can enter), compensating
//!   for real `rules.ini` not exercising it.
//!
//! Per-theater *template* `land_control` spot-checks against real map content
//! (a known cell resolving to `LAND_ROCK`/`LAND_WATER` etc.) live in
//! `ra-client/tests/ui_landtype_realmap_suite.rs` instead of here: resolving
//! a template id + icon to a `LandType` is client-side (`TileSet::land_type`,
//! `ra-client/src/terrain.rs`), not something `ra-sim`/`ra-data` alone can do
//! without duplicating that lookup.

use std::path::PathBuf;

use proptest::prelude::*;

use ra_data::landtype::{LandCosts, LandType, LOCO_FOOT, LOCO_TRACK, LOCO_WHEEL};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_sim::coords::{CellCoord, Locomotor};
use ra_sim::path::{find_path, Passability};

fn assets_dir() -> PathBuf {
    std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"))
}

/// Load the real `rules.ini` from `redalert.mix` -> `local.mix`, or `None`
/// (skip) if the archive isn't present — same helper shape as
/// `damage_matrix.rs::load_rules`.
fn load_rules() -> Option<Ini> {
    let dir = assets_dir();
    if !dir.join("redalert.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             redalert.mix into assets/ to run this test)",
            dir.display()
        );
        return None;
    }
    let bytes = std::fs::read(dir.join("redalert.mix")).ok()?;
    let redalert = MixArchive::parse(&bytes).ok()?;
    let local = redalert.open_nested("local.mix").ok()?;
    let rules_bytes = local.get("rules.ini")?;
    Some(Ini::parse(&String::from_utf8_lossy(rules_bytes)))
}

// ===========================================================================
// 1. Mask-correctness table, transcribed from the real `rules.ini` land
//    sections (`[Clear]/[Road]/[Water]/[Rock]/[Wall]/[Ore]/[Beach]/[Rough]/
//    [River]`), cross-checked against the file so a rules.ini rebalance
//    fails loudly here rather than silently validating a stale table.
// ===========================================================================

/// One row: section name, the real `Foot=`/`Track=`/`Wheel=` percentages
/// (read back and asserted against `rules.ini` below), and the expected
/// `LandCosts::passable` boolean per locomotor (`percentage != 0`).
struct Row {
    section: &'static str,
    land: LandType,
    pct: [i32; 3], // [Foot, Track, Wheel]
}

/// Transcribed directly from the real `rules.ini` (read with `radump extract
/// assets/redalert.mix local.mix -> rules.ini` during derivation, values
/// copied into this table by hand, then verified against the live file by
/// `mask_correctness_table_matches_real_rules_ini` below on every run).
fn rows() -> [Row; 9] {
    [
        Row {
            section: "Clear",
            land: LandType::Clear,
            pct: [90, 80, 60],
        },
        Row {
            section: "Road",
            land: LandType::Road,
            pct: [100, 100, 100],
        },
        Row {
            section: "Water",
            land: LandType::Water,
            pct: [0, 0, 0],
        },
        Row {
            section: "Rock",
            land: LandType::Rock,
            pct: [0, 0, 0],
        },
        Row {
            section: "Wall",
            land: LandType::Wall,
            pct: [0, 0, 0],
        },
        Row {
            section: "Ore",
            land: LandType::Ore,
            pct: [90, 70, 50],
        },
        Row {
            section: "Beach",
            land: LandType::Beach,
            pct: [80, 70, 40],
        },
        Row {
            section: "Rough",
            land: LandType::Rough,
            pct: [80, 70, 40],
        },
        Row {
            section: "River",
            land: LandType::River,
            pct: [0, 0, 0],
        },
    ]
}

#[test]
fn mask_correctness_table_matches_real_rules_ini() {
    let Some(rules) = load_rules() else { return };
    for row in rows() {
        for (i, key) in ["Foot", "Track", "Wheel"].iter().enumerate() {
            let got = rules.get_int(row.section, key);
            assert_eq!(
                got,
                Some(row.pct[i] as i64),
                "[{}] {}= drifted from the hand-transcribed table (expected {}%)",
                row.section,
                key,
                row.pct[i]
            );
        }
    }
}

#[test]
fn mask_correctness_table_matches_land_costs_passability() {
    let Some(rules) = load_rules() else { return };
    let costs = LandCosts::from_rules(&rules);
    for row in rows() {
        let expected = [row.pct[0] != 0, row.pct[1] != 0, row.pct[2] != 0];
        let got = [
            costs.passable(row.land, LOCO_FOOT),
            costs.passable(row.land, LOCO_TRACK),
            costs.passable(row.land, LOCO_WHEEL),
        ];
        assert_eq!(
            got, expected,
            "[{}] passability mismatch: rules.ini pct={:?} -> expected passable {:?}, got {:?}",
            row.section, row.pct, expected, got
        );
    }
    // Restate the hard-blocking claim from QUIRKS Q6 explicitly: rock/water/
    // wall/river block every ground locomotor.
    for land in [
        LandType::Water,
        LandType::Rock,
        LandType::Wall,
        LandType::River,
    ] {
        for loco in [LOCO_FOOT, LOCO_TRACK, LOCO_WHEEL] {
            assert!(
                !costs.passable(land, loco),
                "{land:?} should be impassable to every locomotor (loco={loco})"
            );
        }
    }
}

// ===========================================================================
// 2. "Foot-vs-Track divergence case if any land class differs — find one; if
//    none, document."  Finding: **none exists** in the real rules.ini.
// ===========================================================================

/// Every one of the 9 real land sections has Foot/Track/Wheel percentages
/// that are either **all zero** (Water/Rock/Wall/River — hard-blocking, see
/// the table above) or **all nonzero** (Clear/Road/Ore/Beach/Rough — merely
/// speed-reducing under the deferred-modelling simplification QUIRKS Q6
/// documents: "only impassability... is modelled"). Since our engine's
/// `LandCosts::passable` collapses each column to a boolean
/// (`percentage != 0`), and every real section's three columns agree on
/// zero-vs-nonzero, **no land class in the shipped rules.ini produces a
/// Foot-vs-Track/Wheel *passability* divergence** — a vehicle-passable cell
/// is always foot-passable and vice versa, today. This is a real finding,
/// not a gap in this suite: it means `two_wide_corridor...`-style
/// same-locomotor-only terrain tests can't be built from real content, and
/// item 4's synthetic divergence test below exists specifically to prove the
/// *mechanism* itself works despite real data never exercising it.
#[test]
fn no_real_rules_ini_land_class_diverges_foot_vs_vehicle_passability() {
    let Some(rules) = load_rules() else { return };
    let costs = LandCosts::from_rules(&rules);
    for row in rows() {
        let foot = costs.passable(row.land, LOCO_FOOT);
        let track = costs.passable(row.land, LOCO_TRACK);
        let wheel = costs.passable(row.land, LOCO_WHEEL);
        assert_eq!(
            foot, track,
            "found an unexpected Foot-vs-Track divergence at [{}] — update the doc comment, \
             this was believed not to exist in the shipped rules.ini",
            row.section
        );
        assert_eq!(
            foot, wheel,
            "found an unexpected Foot-vs-Wheel divergence at [{}] — update the doc comment",
            row.section
        );
    }
}

// ===========================================================================
// 3. Property test: A* never routes any locomotor through a cell that
//    locomotor cannot enter (`Passability::per_locomotor`, not exercised by
//    `astar_properties.rs`'s existing single-mask-only property suite).
// ===========================================================================

fn per_locomotor_grid_and_endpoints(
) -> impl Strategy<Value = (Passability, Locomotor, CellCoord, CellCoord)> {
    (6i32..14, 6i32..14).prop_flat_map(|(w, h)| {
        let n = (w * h) as usize;
        let mask = || proptest::collection::vec(proptest::bool::weighted(0.7), n);
        let coord = (0i32..w, 0i32..h).prop_map(|(x, y)| CellCoord::new(x, y));
        let loco = prop_oneof![
            Just(Locomotor::Foot),
            Just(Locomotor::Track),
            Just(Locomotor::Wheel),
        ];
        (
            mask(),
            mask(),
            mask(),
            Just((w, h)),
            loco,
            coord.clone(),
            coord,
        )
            .prop_map(move |(foot, track, wheel, (w, h), loco, start, goal)| {
                (
                    Passability::per_locomotor(w, h, foot, track, wheel),
                    loco,
                    start,
                    goal,
                )
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_star_never_steps_on_a_cell_impassable_for_its_locomotor(
        (grid, loco, start, goal) in per_locomotor_grid_and_endpoints()
    ) {
        if let Some(path) = find_path(&grid, start, goal, loco) {
            for cell in &path {
                prop_assert!(
                    grid.is_passable_loco(*cell, loco),
                    "path stepped onto a cell impassable for {:?}: {:?}", loco, cell
                );
            }
        }
    }
}

// ===========================================================================
// 4. Synthetic per-locomotor divergence: a corridor only `Track` may enter
//    (real rules.ini has none, per test 2 above) — proves the *mechanism*.
// ===========================================================================

#[test]
fn track_only_corridor_is_reachable_for_track_and_unreachable_for_foot_and_wheel() {
    let w = 10;
    let h = 3;
    let n = (w * h) as usize;
    // Row 1 (the middle row) is the only route from x=0 to x=9; it is
    // passable for every locomotor EXCEPT one gate cell (5,1), which is
    // Track-only. Rows 0 and 2 are impassable for everyone (walls).
    let mut all_but_gate = vec![false; n];
    let mut track_only_gate = vec![false; n];
    for x in 0..w {
        let i = (w + x) as usize; // row index 1 (the middle row)
        all_but_gate[i] = true;
        track_only_gate[i] = x == 5; // only the gate cell is Track-exclusive
    }
    let foot = all_but_gate
        .iter()
        .zip(&track_only_gate)
        .map(|(&a, &g)| a && !g)
        .collect::<Vec<_>>();
    let wheel = foot.clone();
    let track = all_but_gate; // Track can use every corridor cell, including the gate

    let grid = Passability::per_locomotor(w, h, foot, track, wheel);
    let start = CellCoord::new(0, 1);
    let goal = CellCoord::new(9, 1);

    assert!(
        find_path(&grid, start, goal, Locomotor::Track).is_some(),
        "Track should be able to cross the gate cell"
    );
    assert!(
        find_path(&grid, start, goal, Locomotor::Foot).is_none(),
        "Foot should be blocked by the Track-exclusive gate (no detour exists — walled both sides)"
    );
    assert!(
        find_path(&grid, start, goal, Locomotor::Wheel).is_none(),
        "Wheel should be blocked by the Track-exclusive gate (no detour exists — walled both sides)"
    );
}
