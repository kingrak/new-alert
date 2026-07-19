//! M7.6 unit cell occupancy invariant suite (QUIRKS Q5): the reservation layer
//! `ra-sim/src/occupancy.rs` + `world.rs::move_units` maintains over vehicles
//! and infantry. Complements `occupancy.rs`'s own colocated unit tests (which
//! exercise `UnitGrid` in isolation) with **integration-level** coverage
//! through the public `World`/`Command` API: property-tested random movement,
//! group dispersal, head-on corridor exchanges, and factory/harvester exit
//! contention — the scenarios item 2 of the M7.6 test plan calls out by name.
//!
//! Every test below checks the one-vehicle-per-cell invariant with a plain
//! function call (`vehicle_overlap_count`), not `debug_assert!` — so it holds
//! in release builds too, per the task brief ("release-mode check, not just
//! debug_assert").

use std::collections::BTreeMap;

use proptest::prelude::*;

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, HarvStatus, MoveStats, Passability, World};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// Count of *extra* vehicles sharing a cell (0 when every vehicle has its own
/// cell; N when N cells each hold one too many). Infantry are excluded — their
/// sub-cell sharing is legitimate and covered by `subcell_suite.rs`. A plain
/// scan over `world.units`, so this is a real (release-mode) check, not a
/// `debug_assert`.
fn vehicle_overlap_count(world: &World) -> usize {
    let mut counts: BTreeMap<(i32, i32), i32> = BTreeMap::new();
    for (_, u) in world.units.iter() {
        if !u.is_infantry() {
            let c = u.cell();
            *counts.entry((c.x, c.y)).or_insert(0) += 1;
        }
    }
    counts
        .values()
        .filter(|&&n| n > 1)
        .map(|&n| (n - 1) as usize)
        .sum()
}

fn assert_no_vehicle_overlap(world: &World, ctx: &str) {
    let excess = vehicle_overlap_count(world);
    assert_eq!(
        excess, 0,
        "{ctx}: {excess} cell(s) hold more than one vehicle — one-vehicle-per-cell violated"
    );
}

// ===========================================================================
// 1. Property test: random vehicle counts/positions/move orders over hundreds
//    of ticks never let two vehicles share a cell.
// ===========================================================================

const GRID_W: i32 = 14;
const GRID_H: i32 = 14;

/// `n` distinct cells on a `GRID_W`x`GRID_H` open grid (distinct starts avoid
/// baking a pre-existing overlap into the fixture itself — the invariant this
/// suite checks is that movement never *creates* one).
fn distinct_cells(n: usize) -> impl Strategy<Value = Vec<CellCoord>> {
    proptest::collection::hash_set(0u32..(GRID_W * GRID_H) as u32, n).prop_map(|set| {
        set.into_iter()
            .map(|idx| CellCoord::new((idx as i32) % GRID_W, (idx as i32) / GRID_W))
            .collect()
    })
}

fn any_cell() -> impl Strategy<Value = CellCoord> {
    (0..GRID_W, 0..GRID_H).prop_map(|(x, y)| CellCoord::new(x, y))
}

/// `n` vehicles' distinct starts, plus two independent waves of (possibly
/// colliding, possibly repeated) destinations — the second wave re-routes
/// mid-flight, maximising the chance of induced head-on/contention scenarios.
fn scenario() -> impl Strategy<Value = (Vec<CellCoord>, Vec<CellCoord>, Vec<CellCoord>)> {
    (3usize..=7).prop_flat_map(|n| {
        (
            distinct_cells(n),
            proptest::collection::vec(any_cell(), n),
            proptest::collection::vec(any_cell(), n),
        )
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// However many vehicles, wherever they start, whatever they're ordered to
    /// do (including a mid-flight re-route to a second random destination): at
    /// no point during the run does any cell ever hold two vehicles.
    #[test]
    fn vehicles_never_overlap_over_random_moves((starts, wave1, wave2) in scenario()) {
        let mut world = World::new(Passability::all_passable(), 0xF00D_CAFE);
        let handles: Vec<Handle> = starts
            .iter()
            .map(|&c| world.spawn_unit(0, 1, c, Facing(0), 400, stats(28, 8)))
            .collect();
        prop_assert_eq!(vehicle_overlap_count(&world), 0, "sanity: distinct starts");

        let cmds1: Vec<Command> = handles
            .iter()
            .zip(&wave1)
            .map(|(&unit, &dest)| Command::Move { unit, dest, house: 1 })
            .collect();
        world.tick(&cmds1);
        prop_assert_eq!(vehicle_overlap_count(&world), 0, "tick 0 (wave1 issued)");
        for t in 1..60 {
            world.tick(&[]);
            prop_assert_eq!(vehicle_overlap_count(&world), 0, "tick {}", t);
        }

        let cmds2: Vec<Command> = handles
            .iter()
            .zip(&wave2)
            .map(|(&unit, &dest)| Command::Move { unit, dest, house: 1 })
            .collect();
        world.tick(&cmds2);
        prop_assert_eq!(vehicle_overlap_count(&world), 0, "tick 60 (wave2 issued)");
        for t in 61..120 {
            world.tick(&[]);
            prop_assert_eq!(vehicle_overlap_count(&world), 0, "tick {}", t);
        }
    }
}

// ===========================================================================
// 2. Group dispersal: N vehicles ordered to one destination cell settle into
//    N distinct cells, all within a reasonable radius of the destination
//    (`pick_dest`'s `Adjust_Dest`-style ring spiral, QUIRKS Q5).
// ===========================================================================

#[test]
fn group_move_to_one_cell_disperses_to_distinct_nearby_cells() {
    for &n in &[2usize, 5, 9] {
        let mut world = World::new(Passability::all_passable(), 0xD15C_0001);
        let dest = CellCoord::new(40, 40);
        // Spread starts around the destination so every unit's direct path
        // would otherwise converge on the exact same cell.
        let handles: Vec<Handle> = (0..n)
            .map(|i| {
                let c = CellCoord::new(38 + i as i32, 38);
                world.spawn_unit(0, 1, c, Facing(0), 400, stats(30, 10))
            })
            .collect();
        let cmds: Vec<Command> = handles
            .iter()
            .map(|&unit| Command::Move {
                unit,
                dest,
                house: 1,
            })
            .collect();
        world.tick(&cmds);
        for t in 0..200 {
            world.tick(&[]);
            assert_no_vehicle_overlap(&world, &format!("n={n} tick={t}"));
        }

        // Every unit finished moving (arrived somewhere, not stuck oscillating).
        for &h in &handles {
            let u = world.units.get(h).unwrap();
            assert!(!u.is_moving(), "n={n}: unit should have finished its path");
        }

        // Distinct end cells (the one-vehicle-per-cell invariant, restated as
        // the group's outcome) and each within a reasonable radius of dest —
        // "packed adjacently, not stacked" per QUIRKS Q5. `pick_dest` spirals
        // out ring by ring, so for `n` units the worst case is bounded by the
        // ring whose cell count first reaches `n` (ring r has 8r cells);
        // give a generous slack factor on top of that bound.
        let mut ends = std::collections::BTreeSet::new();
        let mut max_dist = 0i32;
        for &h in &handles {
            let c = world.units.get(h).unwrap().cell();
            assert!(
                ends.insert((c.x, c.y)),
                "n={n}: two units ended on the same cell — dispersal failed"
            );
            let d = (c.x - dest.x).abs().max((c.y - dest.y).abs());
            max_dist = max_dist.max(d);
        }
        assert!(
            max_dist <= 4,
            "n={n}: dispersal settled a unit {max_dist} cells from the destination — too far"
        );
    }
}

// ===========================================================================
// 3/4. Head-on corridor exchange (item 2 of the M7.6 plan, verbatim: "two
//    units passing in 2-wide corridor succeed; 1-wide documented-wait
//    behavior pinned").
// ===========================================================================

/// A horizontal corridor `width` rows tall (centred vertically) across a
/// `len`-cell-wide open strip, walled (impassable) above and below.
fn corridor_grid(len: i32, total_h: i32, width: i32) -> Passability {
    let n = (len * total_h) as usize;
    let mut cells = vec![false; n];
    let band_top = (total_h - width) / 2;
    for y in band_top..(band_top + width) {
        for x in 0..len {
            cells[(y * len + x) as usize] = true;
        }
    }
    Passability::new(len, total_h, cells)
}

const CORRIDOR_LEN: i32 = 20;
const CORRIDOR_H: i32 = 5;

/// Unequal speeds (24 vs. 17 leptons/tick — plausible real-game asymmetry,
/// e.g. a tank vs. a slower vehicle) so the two units are never in lockstep:
/// see `known_bug_symmetric_two_wide_corridor_head_on_livelocks` below for
/// what happens when they *are* perfectly symmetric.
#[test]
fn two_wide_corridor_head_on_exchange_succeeds() {
    let band_top = (CORRIDOR_H - 2) / 2;
    let row = band_top; // either of the two passable rows works as a start row
    let mut world = World::new(corridor_grid(CORRIDOR_LEN, CORRIDOR_H, 2), 0xC0AA_0002);
    let left = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(0), 400, stats(24, 8));
    let right = world.spawn_unit(
        1,
        1,
        CellCoord::new(CORRIDOR_LEN - 2, row),
        Facing(128),
        400,
        stats(17, 8),
    );
    // Destinations one cell short of the opponent's *spawn* cell (not the
    // opponent's spawn cell itself) — issuing both `Move` commands in the same
    // tick means `pick_dest` sees the other unit still parked at its spawn
    // cell at command-issue time, and would otherwise disperse a destination
    // that exactly coincides with it (a test-fixture artifact, not the thing
    // under test). Still requires a full corridor traversal and a genuine
    // pass-through in the middle.
    world.tick(&[
        Command::Move {
            unit: left,
            dest: CellCoord::new(CORRIDOR_LEN - 3, row),
            house: 1,
        },
        Command::Move {
            unit: right,
            dest: CellCoord::new(2, row),
            house: 1,
        },
    ]);
    for t in 0..400 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("2-wide tick {t}"));
    }
    let ul = world.units.get(left).unwrap();
    let ur = world.units.get(right).unwrap();
    assert!(
        !ul.is_moving(),
        "left unit should have finished (2-wide corridor)"
    );
    assert!(
        !ur.is_moving(),
        "right unit should have finished (2-wide corridor)"
    );
    assert_eq!(ul.cell(), CellCoord::new(CORRIDOR_LEN - 3, row));
    assert_eq!(ur.cell(), CellCoord::new(2, row));
}

/// **Known bug (report to ra-coder, not fixed here): two vehicles with
/// *identical* speed meeting head-on in a corridor wide enough to pass (2
/// rows) can livelock forever instead of passing.**
///
/// Minimal repro: same `corridor_grid` as the (passing) asymmetric-speed test
/// above, but both units given the *same* `stats(24, 8)`. Traced with
/// `eprintln!` per tick during diagnosis: once adjacent, both units detect
/// they're blocked on the same tick, both compute the *same* detour (step
/// into the corridor's other row) via `find_path_avoiding`, both move there
/// together, then on the *next* tick both are blocked again (now occupying
/// each other's just-vacated row) and both detour *back* — repeating forever.
/// From roughly tick 90 onward in the repro, `left` oscillates
/// `(9,1) <-> (9,2)` and `right` oscillates `(10,1) <-> (10,2)` in lockstep,
/// `path_len` oscillating between two values, never trending toward either
/// destination, for the remaining 300+ ticks of a 400-tick run (confirmed it
/// does not resolve given far more ticks either).
///
/// **Root cause (diagnosis, not a fix):** `move_units`'s per-unit re-route
/// (`world.rs`, the `is_blocked`/`find_path_avoiding` block) has no
/// tie-breaking between two units that are simultaneously blocked by each
/// other and compute mirror-image detours. The original engine's
/// `drive.cpp` "ask the blocker to scatter" radio protocol (deliberately
/// simplified away here per QUIRKS Q5 deviation #1) implicitly broke this
/// symmetry by making one unit passive (the one asked to scatter) while the
/// other proceeds; the re-route-around approach has no equivalent asymmetry,
/// so two units with identical speed/rotation/position-parity can stay
/// exactly in phase indefinitely. Breaking the tie deterministically (e.g.
/// by handle/slot order — the lower-slot unit re-routes, the higher-slot
/// unit holds this tick) would fix it without reintroducing RNG.
///
/// **Live-game impact:** real skirmishes rarely have two vehicles of
/// *exactly* equal speed meet *exactly* head-on in a *exactly*-2-wide
/// corridor at *exactly* the same phase — `two_wide_corridor_head_on_exchange_succeeds`
/// above shows any speed asymmetry breaks the tie and passing succeeds. But
/// same-type unit mirrors (two tanks of the same house, or two AI-controlled
/// units on a symmetric map) are a plausible real scenario, so this is a
/// live traffic-jam risk, not a purely theoretical one — worth fixing before
/// relying on "2-wide corridors always work."
///
/// **FIXED in M7.7 (P0a).** `move_units` now breaks the symmetry
/// deterministically by slot order: when a *moving* vehicle with a lower handle
/// index blocks this one, the higher-index unit **yields** (holds one tick)
/// instead of re-routing in lock-step, so only the lower-index unit detours and
/// the pair passes. Un-ignored; both units now arrive.
#[test]
fn known_bug_symmetric_two_wide_corridor_head_on_livelocks() {
    let band_top = (CORRIDOR_H - 2) / 2;
    let row = band_top;
    let mut world = World::new(corridor_grid(CORRIDOR_LEN, CORRIDOR_H, 2), 0xC0AA_0003);
    let left = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(0), 400, stats(24, 8));
    let right = world.spawn_unit(
        1,
        1,
        CellCoord::new(CORRIDOR_LEN - 2, row),
        Facing(128),
        400,
        stats(24, 8), // identical speed to `left` — the livelock trigger
    );
    world.tick(&[
        Command::Move {
            unit: left,
            dest: CellCoord::new(CORRIDOR_LEN - 2, row),
            house: 1,
        },
        Command::Move {
            unit: right,
            dest: CellCoord::new(1, row),
            house: 1,
        },
    ]);
    for t in 0..400 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("2-wide symmetric tick {t}"));
    }
    let ul = world.units.get(left).unwrap();
    let ur = world.units.get(right).unwrap();
    // The livelock resolves: both units settle (stop oscillating).
    assert!(
        !ul.is_moving(),
        "left unit should have finished (2-wide corridor)"
    );
    assert!(
        !ur.is_moving(),
        "right unit should have finished (2-wide corridor)"
    );
    // They crossed to the far side of the corridor — `left` (started x=1) is now
    // in the right half, `right` (started x=18) in the left half. We assert the
    // *crossing*, not the exact ordered cell, because each unit was ordered onto
    // the cell the *other* one occupied at command time; `pick_dest`'s group
    // dispersal (QUIRKS Q5) therefore resolves the target to an adjacent free
    // cell (left 18→17, right 1→0 for this seed), which is correct RA behaviour
    // (you cannot order a unit onto an occupied cell). The point of this test is
    // that the head-on symmetry no longer livelocks — both reach the far side.
    let lc = ul.cell();
    let rc = ur.cell();
    assert!(
        lc.x > CORRIDOR_LEN / 2,
        "left should have crossed to the right half, got {lc:?}"
    );
    assert!(
        rc.x < CORRIDOR_LEN / 2,
        "right should have crossed to the left half, got {rc:?}"
    );
    assert_ne!(lc, rc, "the two units must not share a cell");
}

/// A **1-wide** corridor gives neither unit anywhere to detour: the
/// find_path_avoiding re-route (QUIRKS Q5 deviation #1: "a true 1-wide
/// corridor with no detour just waits") cannot find an alternate cell, so
/// both units hold in place once they meet, forever, with no panic and no
/// overlap. This test pins that *behavior class* (stuck, not swapped, no
/// crash) rather than exact coordinates, since the precise meeting cell is an
/// implementation detail of speed/rounding, not the invariant under test.
#[test]
fn one_wide_corridor_head_on_is_a_documented_wait_not_a_swap() {
    let band_top = (CORRIDOR_H - 1) / 2;
    let row = band_top;
    let mut world = World::new(corridor_grid(CORRIDOR_LEN, CORRIDOR_H, 1), 0xC0AA_0001);
    let left = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(0), 400, stats(24, 8));
    let right = world.spawn_unit(
        1,
        1,
        CellCoord::new(CORRIDOR_LEN - 2, row),
        Facing(128),
        400,
        stats(24, 8),
    );
    world.tick(&[
        Command::Move {
            unit: left,
            dest: CellCoord::new(CORRIDOR_LEN - 2, row),
            house: 1,
        },
        Command::Move {
            unit: right,
            dest: CellCoord::new(1, row),
            house: 1,
        },
    ]);
    let mut cells_over_time = Vec::new();
    for t in 0..200 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("1-wide tick {t}"));
        cells_over_time.push((
            world.units.get(left).unwrap().cell(),
            world.units.get(right).unwrap().cell(),
        ));
    }

    let (lc, rc) = *cells_over_time.last().unwrap();
    // Neither swapped past the other: left never reaches the far (right-side)
    // destination and vice versa.
    assert_ne!(
        lc,
        CellCoord::new(CORRIDOR_LEN - 2, row),
        "left must not have swept past the block"
    );
    assert_ne!(
        rc,
        CellCoord::new(1, row),
        "right must not have swept past the block"
    );
    // Still distinct cells (restates the invariant, specifically for this
    // scenario) and genuinely stuck — position unchanged over the last 20
    // ticks, not merely still crawling toward each other.
    assert_ne!(lc, rc);
    let (lc_earlier, rc_earlier) = cells_over_time[cells_over_time.len() - 20];
    assert_eq!(
        lc, lc_earlier,
        "left should have settled (documented wait), not be inching forever"
    );
    assert_eq!(
        rc, rc_earlier,
        "right should have settled (documented wait), not be inching forever"
    );
    // And they never actually reached each other's start (no phasing through).
    let gap = (lc.x - rc.x).abs();
    assert!(
        gap >= 1,
        "units must not occupy the same or passed-through cells"
    );
}

// ===========================================================================
// 5. Factory exit respects occupancy under contention.
// ===========================================================================

use ra_sim::{BuildItem, BuildingProto, Catalog, EconRules, UnitProto};

const B_WEAP: u32 = 0;
const U_TANK: u32 = 0;
const U_TANK_SPRITE: u32 = 77;

fn factory_catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "WEAP".into(),
            foot_w: 2,
            foot_h: 2,
            max_health: 500,
            armor: 0,
            power: 0,
            cost: 20, // cheap: time_to_build = 20*900/1000 = 18 ticks
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: true,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 1,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
        }],
        units: vec![UnitProto {
            is_infantry: false,
            locomotor: 1,
            name: "TANK".into(),
            sprite_id: U_TANK_SPRITE,
            max_health: 400,
            stats: stats(24, 8),
            armor: 0,
            weapon: None,
            secondary: None,
            has_turret: false,
            is_harvester: false,
            deploys_to: None,
            cost: 20,
            prereq: vec![],
            sight: 2,
        }],
        econ: EconRules::default(),
    }
}

/// The war factory's full adjacency ring (south-of-centre first, matching
/// `factory_exit_ring`'s own geometry — recomputed here from the public
/// footprint fields, not by reaching into the private function).
fn factory_ring(tl: CellCoord, w: i32, h: i32) -> Vec<CellCoord> {
    let mut ring = vec![CellCoord::new(tl.x + w / 2, tl.y + h)]; // south of centre, preferred
    for x in (tl.x - 1)..=(tl.x + w) {
        for y in (tl.y - 1)..=(tl.y + h) {
            let on_ring = x == tl.x - 1 || x == tl.x + w || y == tl.y - 1 || y == tl.y + h;
            if on_ring {
                let c = CellCoord::new(x, y);
                if !ring.contains(&c) {
                    ring.push(c);
                }
            }
        }
    }
    ring
}

fn spawned_tank_count(world: &World) -> usize {
    world
        .units
        .iter()
        .filter(|(_, u)| u.type_id == U_TANK_SPRITE)
        .count()
}

#[test]
fn factory_exit_waits_while_the_whole_ring_is_blocked_then_uses_the_freed_cell() {
    let mut world = World::new(Passability::all_passable(), 0xFEED_0001);
    world.set_catalog(factory_catalog());
    world.init_houses(2, 10_000);
    let weap_tl = CellCoord::new(30, 30);
    world.spawn_building(B_WEAP, 1, weap_tl).unwrap();

    // Block every ring cell with a vehicle of a *different* house (occupancy
    // is house-agnostic: any vehicle blocks any other vehicle's entry).
    let ring = factory_ring(weap_tl, 2, 2);
    let blockers: Vec<Handle> = ring
        .iter()
        .map(|&c| world.spawn_unit(9, 9, c, Facing(0), 300, stats(20, 8)))
        .collect();
    assert_no_vehicle_overlap(&world, "ring fully blocked, before production");

    world.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Unit(U_TANK),
    }]);
    // Run well past the completion time (18 ticks) while every ring cell
    // stays occupied: no tank must spawn — the exit-blocked retry must hold
    // the finished production rather than spawn it into an occupied cell.
    for t in 0..200 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("ring blocked tick {t}"));
        assert_eq!(
            spawned_tank_count(&world),
            0,
            "tick {t}: a tank spawned while every exit cell was still blocked"
        );
    }
    assert!(
        world.house(1).unwrap().unit_prod.is_some(),
        "production should still be pending (done, but exit-blocked) rather than lost"
    );

    // Free exactly the south-of-centre cell (the preferred exit): move that
    // one blocker far away.
    let south = ring[0];
    let south_blocker = blockers
        .iter()
        .zip(&ring)
        .find(|(_, &c)| c == south)
        .map(|(&h, _)| h)
        .unwrap();
    world.tick(&[Command::Move {
        unit: south_blocker,
        dest: CellCoord::new(0, 0),
        house: 9,
    }]);
    let mut spawned_at = None;
    for t in 0..200 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("post-free tick {t}"));
        if spawned_tank_count(&world) > 0 && spawned_at.is_none() {
            spawned_at = Some(t);
        }
    }
    assert_eq!(
        spawned_tank_count(&world),
        1,
        "exactly one tank should spawn once a ring cell freed up"
    );
    assert!(
        spawned_at.is_some(),
        "the tank should have spawned within the run"
    );
    assert!(
        world.house(1).unwrap().unit_prod.is_none(),
        "the lane should have cleared once the tank spawned"
    );
    let tank = world
        .units
        .iter()
        .find(|(_, u)| u.type_id == U_TANK_SPRITE)
        .map(|(h, _)| h)
        .unwrap();
    assert_eq!(
        world.units.get(tank).unwrap().cell(),
        south,
        "the tank should exit onto the freed south-of-centre cell (preferred exit)"
    );
}

// ===========================================================================
// 6. Harvester dock respects occupancy under contention: two harvesters
//    heading to the same refinery at once never end up sharing a cell.
// ===========================================================================

const B_PROC: u32 = 0;

fn refinery_catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "PROC".into(),
            foot_w: 2,
            foot_h: 2,
            max_health: 500,
            armor: 0,
            power: 0,
            cost: 50,
            prereq: vec![],
            is_refinery: true,
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

#[test]
fn two_harvesters_heading_to_the_same_refinery_never_share_a_dock_cell() {
    let mut world = World::new(Passability::all_passable(), 0xDA0C_0001);
    world.set_catalog(refinery_catalog());
    world.init_houses(2, 1000);
    let refinery_tl = CellCoord::new(30, 30);
    world.spawn_building(B_PROC, 1, refinery_tl).unwrap();

    // Two harvesters, symmetric distance from the refinery, both already
    // holding cargo and about to FindHome on the same tick.
    let a = world.spawn_unit(0, 1, CellCoord::new(25, 25), Facing(0), 400, stats(26, 8));
    let b = world.spawn_unit(1, 1, CellCoord::new(35, 25), Facing(0), 400, stats(26, 8));
    for h in [a, b] {
        world.set_unit_harvester(h, true);
        let u = world.units.get_mut(h).unwrap();
        u.harvest.cargo = 10;
        u.harvest.status = HarvStatus::FindHome;
    }

    for t in 0..150 {
        world.tick(&[]);
        assert_no_vehicle_overlap(&world, &format!("dock contention tick {t}"));
    }
    // Both should have made it to some cell adjacent to (or reachable near)
    // the refinery, and — the actual invariant under test — distinct cells.
    let ca = world.units.get(a).unwrap().cell();
    let cb = world.units.get(b).unwrap().cell();
    assert_ne!(
        ca, cb,
        "two harvesters must not have docked on the same cell"
    );
}
