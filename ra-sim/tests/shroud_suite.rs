//! The M6 shroud (per-house sticky fog-of-war) coverage suite — item 2 of the
//! M6 test plan. Complements `ra-sim/src/shroud.rs`'s three colocated unit
//! tests (`disabled_reads_all_explored`, `reveal_marks_a_disc`,
//! `reveal_is_sticky`) with scenario-level coverage over `World`'s public API
//! (spawning, movement, building placement, hashing) — the same "build a
//! `World`, feed commands, assert outcomes" style as
//! `ra-sim/tests/determinism.rs` and `ra-sim/tests/damage_matrix.rs`.
//!
//! Claims exercised here (DESIGN.md §4.9 M6, `shroud.rs` module docs):
//! - Per-house isolation: one house's exploration never leaks into another's,
//!   including at the hash level (shroud state is per-house, not global).
//! - The reveal shape is the exact octagonal disc `Sight_From` computes
//!   (`leptons_distance(center, cell) <= sight * 256`), not a square.
//! - Sticky semantics: once explored, a cell stays explored after the
//!   revealing unit leaves — no re-shrouding.
//! - Reveal-on-place covers a building's *entire* footprint, not just its
//!   center cell.
//! - The shroud folds into `World::state_hash` only when enabled, and is
//!   hash-sensitive to every additional reveal when it is.

use ra_data::buildings::{building_stats, footprint as real_footprint};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_sim::coords::{leptons_distance, CellCoord, Facing, LEPTONS_PER_CELL};
use ra_sim::{BuildingProto, Catalog, Command, MoveStats, Passability, World};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

// ---------------------------------------------------------------------
// 1. Per-house isolation.
// ---------------------------------------------------------------------

/// House A's units move around and reveal a region; house B — sharing the
/// same map, never touching that region — must never gain any of it. Driven
/// through real unit movement (not direct `Shroud::reveal` calls), so this
/// also exercises the movement system's per-tick incremental reveal.
#[test]
fn per_house_isolation_a_reveals_never_leak_to_b() {
    let mut world = World::new(Passability::all_passable(), 0xA11C_E001);
    world.enable_shroud();

    let a = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 256, stats(200, 10));
    // House B has a unit far away that never moves and never sights the
    // region A explores (sight 0 -> no reveal at all for B from this unit).
    world.spawn_unit(0, 2, CellCoord::new(90, 90), Facing(0), 256, stats(0, 10));

    world.set_unit_sight(a, 4);
    world.tick(&[Command::Move {
        unit: a,
        dest: CellCoord::new(10, 40),
        house: 1,
    }]);
    for _ in 0..150 {
        world.tick(&[]);
    }

    // Sanity: A actually explored a meaningful area along its path.
    assert!(
        world.shroud.explored_count(1) > 20,
        "house A should have revealed a non-trivial area while traveling"
    );
    assert!(
        world.shroud.is_explored(1, CellCoord::new(10, 25)),
        "house A should have revealed cells along its travel path"
    );

    // House B must have explored nothing at all: no reveal ever ran for it.
    assert_eq!(
        world.shroud.explored_count(2),
        0,
        "house B must not have gained any exploration from house A's movement"
    );
    // Spot-check every cell A explored is unexplored for B.
    for y in 6..44 {
        for x in 6..14 {
            let c = CellCoord::new(x, y);
            if world.shroud.is_explored(1, c) {
                assert!(
                    !world.shroud.is_explored(2, c),
                    "house B must not see cell {c:?} that only house A explored"
                );
            }
        }
    }
}

/// Two otherwise-identical worlds where the *same region* is revealed, but by
/// a different house in each — the state (and therefore the hash) must
/// differ, proving the shroud is genuinely keyed per-house, not just "some
/// house explored something".
#[test]
fn hash_sensitivity_shroud_is_per_house_not_just_any_house() {
    let region = CellCoord::new(64, 64);

    let mut world_a_reveals = World::new(Passability::all_passable(), 42);
    world_a_reveals.enable_shroud();
    world_a_reveals.reveal_shroud(1, region, 5);

    let mut world_b_reveals = World::new(Passability::all_passable(), 42);
    world_b_reveals.enable_shroud();
    world_b_reveals.reveal_shroud(2, region, 5);

    assert_ne!(
        world_a_reveals.state_hash(),
        world_b_reveals.state_hash(),
        "revealing the same region by a different house must change the hash \
         (shroud state is per-house, not a shared 'explored by someone' bit)"
    );
    // Direct state check as well, not just the hash.
    assert!(world_a_reveals.shroud.is_explored(1, region));
    assert!(!world_a_reveals.shroud.is_explored(2, region));
    assert!(world_b_reveals.shroud.is_explored(2, region));
    assert!(!world_b_reveals.shroud.is_explored(1, region));
}

// ---------------------------------------------------------------------
// 2. Sight-disc shape pin: the exact octagonal metric, not a few spot checks.
// ---------------------------------------------------------------------

/// Reveal a known radius at a known center on a `World`'s shroud (via the
/// public `reveal_shroud`), then assert `is_explored` against an
/// independently-computed expectation (`leptons_distance` applied directly to
/// each candidate cell) for *every* cell in a bounding box comfortably larger
/// than the reveal radius — so both "did we reveal too little" (a missing
/// cell inside the disc) and "did we reveal too much" (an extra cell outside
/// it, e.g. a square instead of an octagon) are caught, not just a few
/// boundary spot checks.
#[test]
fn reveal_shroud_disc_matches_leptons_distance_formula_over_full_bounding_box() {
    let mut world = World::new(Passability::all_passable(), 1);
    world.enable_shroud();

    let center = CellCoord::new(64, 64);
    let sight: u8 = 3;
    world.reveal_shroud(1, center, sight);

    let reach = sight as i32 * LEPTONS_PER_CELL;
    let margin = 3; // comfortably outside the radius on every side
    let mut checked = 0u32;
    for dy in -(sight as i32 + margin)..=(sight as i32 + margin) {
        for dx in -(sight as i32 + margin)..=(sight as i32 + margin) {
            let c = CellCoord::new(center.x + dx, center.y + dy);
            let expected = leptons_distance(center.center(), c.center()) <= reach;
            let actual = world.shroud.is_explored(1, c);
            assert_eq!(
                expected, actual,
                "cell {c:?} (offset {dx},{dy}): expected explored={expected}, got {actual}"
            );
            checked += 1;
        }
    }
    // Sanity: the bounding box was big enough to include cells outside the
    // disc too (otherwise this test could vacuously pass by only ever
    // checking "inside" cells).
    assert_eq!(checked, (2 * (sight as i32 + margin) + 1).pow(2) as u32);
    assert!(
        !world.shroud.is_explored(
            1,
            CellCoord::new(center.x + sight as i32 + margin, center.y)
        ),
        "sanity: the margin cell on the axis should be outside the disc"
    );
}

/// A small, fully hand-computed pin independent of `leptons_distance`
/// entirely: at radius 1 the octagonal disc is a "plus" (center + 4
/// orthogonal neighbours), and explicitly excludes the 4 diagonal
/// neighbours. A future refactor that quietly turned this into a square (8-
/// neighbourhood) would still pass the formula-based test above only if it
/// also broke `leptons_distance` consistently with it — this test pins the
/// shape by hand, so a divergence between "the formula" and "what the
/// original engine actually does at radius 1" would still be caught.
#[test]
fn reveal_radius_one_is_a_plus_shape_not_a_square() {
    let mut world = World::new(Passability::all_passable(), 1);
    world.enable_shroud();
    world.reveal_shroud(1, CellCoord::new(50, 50), 1);

    for (x, y) in [(50, 50), (49, 50), (51, 50), (50, 49), (50, 51)] {
        assert!(
            world.shroud.is_explored(1, CellCoord::new(x, y)),
            "({x},{y}) should be explored at radius 1"
        );
    }
    for (x, y) in [(49, 49), (51, 49), (49, 51), (51, 51)] {
        assert!(
            !world.shroud.is_explored(1, CellCoord::new(x, y)),
            "diagonal neighbour ({x},{y}) must NOT be explored at radius 1 \
             (the disc is octagonal, not a square)"
        );
    }
}

// ---------------------------------------------------------------------
// 3. Sticky semantics via real unit movement (not direct `reveal` calls).
// ---------------------------------------------------------------------

/// A unit travels straight down a column, passing through and beyond a
/// checkpoint cell, then continues well past it. The checkpoint must remain
/// explored long after the unit — and its sight disc — have moved away: RA1
/// has no re-shrouding (`shroud.rs` module docs).
#[test]
fn shroud_reveal_is_sticky_after_unit_moves_away() {
    let mut world = World::new(Passability::all_passable(), 7);
    world.enable_shroud();

    let unit = world.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(200, 10));
    world.set_unit_sight(unit, 3);

    let checkpoint = CellCoord::new(5, 20);
    // The spawn-time reveal (radius 3 around (5,5)) does not reach the
    // checkpoint at y=20, so any exploration there must come from the
    // movement system's incremental per-tick reveal.
    assert!(!world.shroud.is_explored(1, checkpoint));

    world.tick(&[Command::Move {
        unit,
        dest: CellCoord::new(5, 60),
        house: 1,
    }]);
    for _ in 0..40 {
        world.tick(&[]);
    }
    assert!(
        world.shroud.is_explored(1, checkpoint),
        "the unit should have revealed the checkpoint on its way past it"
    );

    // Keep going well past the checkpoint and past the destination.
    for _ in 0..60 {
        world.tick(&[]);
    }
    let final_cell = world.units.get(unit).unwrap().cell();
    assert!(
        final_cell.y >= 55,
        "unit should have reached near its destination by now, at y={}",
        final_cell.y
    );
    // The unit's current sight disc (radius 3) is nowhere near y=20 anymore.
    assert!(
        leptons_distance(final_cell.center(), checkpoint.center()) > 3 * LEPTONS_PER_CELL,
        "sanity: the unit's current position should be well outside sight range of the checkpoint"
    );
    assert!(
        world.shroud.is_explored(1, checkpoint),
        "sticky: the checkpoint must remain explored long after the unit left (no re-shrouding)"
    );
}

// ---------------------------------------------------------------------
// 4. Reveal-on-place: a building's *full* footprint, not just its center.
// ---------------------------------------------------------------------

fn proto(name: &str, foot_w: u8, foot_h: u8, sight: u8) -> BuildingProto {
    BuildingProto {
        name: name.to_string(),
        foot_w,
        foot_h,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 100,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight,
        sprite_id: 0,
    }
}

/// An asymmetric 4x3 footprint (larger than any footprint the real catalog
/// currently models — max is 3x3, see `ra-data/src/buildings.rs`) with a
/// sight range that comfortably covers every corner: every footprint cell,
/// not just `center_cell()`, must read as explored immediately after
/// `spawn_building`.
#[test]
fn spawn_building_reveals_full_footprint_when_sight_covers_it() {
    let mut world = World::new(Passability::all_passable(), 1);
    world.enable_shroud();
    let mut catalog = Catalog::new();
    catalog.buildings.push(proto("BIG", 4, 3, 3));
    world.set_catalog(catalog);

    let top_left = CellCoord::new(50, 50);
    let handle = world
        .spawn_building(0, 1, top_left)
        .expect("spawn should succeed");
    let cells: Vec<CellCoord> = world.buildings.get(handle).unwrap().footprint().collect();
    assert_eq!(cells.len(), 12, "sanity: 4x3 footprint is 12 cells");
    for c in cells {
        assert!(
            world.shroud.is_explored(1, c),
            "footprint cell {c:?} should be explored immediately after placement"
        );
    }
}

/// **Finding, not a fix.** `World::spawn_building` (`world.rs`, "Reveal the
/// shroud around the new structure (building.cpp:1140)") reveals a single
/// octagonal disc centered on the building's `center_cell()`, using the
/// building's `sight`. That is correct for every building the real catalog
/// currently models (see `real_catalog_buildings_footprint_fully_covered_by_sight`
/// below — verified against the real `rules.ini` shipped with this repo's
/// assets: every one of the 10 modelled building types' `Sight=` covers its
/// full footprint today). But the reveal is structurally a *center-out disc*,
/// not a footprint walk — so a large-footprint / small-sight combination can
/// leave footprint corners dark. `ra-data/src/buildings.rs`'s own footprint
/// table doc comments a `BSIZE_55` (5x5) shape from the original engine that
/// this milestone doesn't model yet; this test builds a synthetic 5x5/sight-1
/// building to demonstrate the gap concretely, so it is caught the moment
/// such content is added. This is a *theoretical* gap today (no live catalog
/// building triggers it), reported to ra-coder as a design note on
/// `spawn_building`'s reveal, not a bug fix.
#[test]
fn spawn_building_center_disc_can_leave_large_footprint_corners_dark_when_sight_is_undersized() {
    let mut world = World::new(Passability::all_passable(), 1);
    world.enable_shroud();
    let mut catalog = Catalog::new();
    catalog.buildings.push(proto("HUGE", 5, 5, 1)); // sight=1: far too small for a 5x5 footprint
    world.set_catalog(catalog);

    let top_left = CellCoord::new(50, 50);
    let handle = world
        .spawn_building(0, 1, top_left)
        .expect("spawn should succeed");
    let b = world.buildings.get(handle).unwrap();
    let center = b.center_cell();
    assert_eq!(center, CellCoord::new(52, 52));

    // The center cell itself is explored (sight >= 1 covers the center).
    assert!(world.shroud.is_explored(1, center));

    // But the footprint's far corners (offset (2,2) etc. from the center)
    // are NOT — demonstrating the gap. If this assertion ever starts
    // failing because `spawn_building` was changed to walk the full
    // footprint, that's the fix landing; update/remove this test then.
    let corners = [
        CellCoord::new(50, 50),
        CellCoord::new(54, 50),
        CellCoord::new(50, 54),
        CellCoord::new(54, 54),
    ];
    let mut any_dark = false;
    for c in corners {
        if !world.shroud.is_explored(1, c) {
            any_dark = true;
        }
    }
    assert!(
        any_dark,
        "expected at least one footprint corner to be left unrevealed by the undersized-sight \
         center-disc reveal (if this now fails, spawn_building's reveal was fixed to cover the \
         full footprint -- update this characterization test)"
    );
}

/// Real-catalog check (skip-clean without assets, but this repo's `assets/`
/// directory has real archives, so this actually runs): for every building
/// short name this milestone models a footprint for
/// (`ra_data::buildings::footprint`), resolve its real `Sight=` from the
/// shipped `rules.ini` and confirm `spawn_building`'s center-disc reveal
/// covers every footprint cell. This is the live-catalog half of the finding
/// above: it demonstrates the gap is theoretical today, not live.
#[test]
fn real_catalog_buildings_footprint_fully_covered_by_sight() {
    let dir = std::env::var("RA_ASSETS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"));
    if !dir.join("redalert.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy redalert.mix into \
             assets/ to run this test)",
            dir.display()
        );
        return;
    }
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("read redalert.mix");
    let redalert = MixArchive::parse(&redalert_bytes).expect("parse redalert.mix");
    let local = redalert.open_nested("local.mix").expect("open local.mix");
    let rules_bytes = local.get("rules.ini").expect("rules.ini present");
    let rules = Ini::parse(&String::from_utf8_lossy(rules_bytes));

    let names = [
        "FACT", "PROC", "POWR", "APWR", "WEAP", "FIX", "SILO", "BARR", "TENT", "DOME",
    ];
    let mut checked_any = false;
    for name in names {
        let Some((foot_w, foot_h)) = real_footprint(name) else {
            continue;
        };
        let Some(bstats) = building_stats(&rules, name) else {
            continue;
        };
        checked_any = true;

        let mut world = World::new(Passability::all_passable(), 1);
        world.enable_shroud();
        let mut catalog = Catalog::new();
        catalog
            .buildings
            .push(proto(name, foot_w, foot_h, bstats.sight));
        world.set_catalog(catalog);

        let handle = world
            .spawn_building(0, 1, CellCoord::new(50, 50))
            .unwrap_or_else(|| panic!("{name}: spawn_building should succeed"));
        let cells: Vec<CellCoord> = world.buildings.get(handle).unwrap().footprint().collect();
        for c in cells {
            assert!(
                world.shroud.is_explored(1, c),
                "{name}: real Sight={} does not cover footprint cell {c:?} of its real {}x{} \
                 footprint -- this WOULD be a live bug (unlike the synthetic gap demo above)",
                bstats.sight,
                foot_w,
                foot_h
            );
        }
    }
    assert!(
        checked_any,
        "sanity: rules.ini should have resolved at least one modelled building's stats"
    );
}

// ---------------------------------------------------------------------
// 5. Hash sensitivity when the shroud is enabled.
// ---------------------------------------------------------------------

/// Two otherwise-identical enabled-shroud worlds, one with one extra reveal
/// (a scout peeking at one more cell) — the hash must differ.
#[test]
fn hash_sensitivity_one_extra_reveal_changes_hash() {
    let mut base = World::new(Passability::all_passable(), 99);
    base.enable_shroud();
    base.reveal_shroud(1, CellCoord::new(20, 20), 2);

    let mut with_extra_peek = base.clone();
    with_extra_peek.reveal_shroud(1, CellCoord::new(80, 80), 1);

    assert_ne!(
        base.state_hash(),
        with_extra_peek.state_hash(),
        "one extra revealed cell must change the state hash"
    );
}

// ---------------------------------------------------------------------
// 6. Shroud-disabled worlds are unaffected: identical to each other over a
// movement script, and the M3/M4/M5 golden hash chains in `determinism.rs`
// (verified separately -- see the test report) stay pinned unchanged, since
// `Shroud::hash_into` folds in zero bytes while disabled.
// ---------------------------------------------------------------------

#[test]
fn disabled_shroud_worlds_hash_identically_across_a_movement_script() {
    fn build(seed: u32) -> World {
        let mut world = World::new(Passability::all_passable(), seed);
        // Deliberately never call `enable_shroud()`.
        world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 256, stats(30, 10));
        world.spawn_unit(0, 2, CellCoord::new(60, 60), Facing(0), 256, stats(20, 8));
        world
    }
    let mut wa = build(0x5EED);
    let mut wb = build(0x5EED);

    let script: Vec<Vec<Command>> = {
        let ha = wa.units.handles()[0];
        let hb = wa.units.handles()[1];
        let mut log = vec![Vec::new(); 60];
        log[0].push(Command::Move {
            unit: ha,
            dest: CellCoord::new(10, 40),
            house: 1,
        });
        log[5].push(Command::Move {
            unit: hb,
            dest: CellCoord::new(30, 60),
            house: 2,
        });
        log
    };

    let mut chain_a = Vec::new();
    let mut chain_b = Vec::new();
    for cmds in &script {
        chain_a.push(wa.tick(cmds));
        chain_b.push(wb.tick(cmds));
    }
    assert_eq!(
        chain_a, chain_b,
        "two never-enabled-shroud worlds run through the same movement script must \
         hash identically at every tick"
    );
    assert_eq!(wa.state_hash(), wb.state_hash());
}
