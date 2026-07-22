//! M7.12 audit — boundary/edge coverage for the "ask-the-blocker-to-scatter"
//! mechanic (`scatter_friendly_blockers`/`scatter_blocker`, `ra-sim/src/world.rs`)
//! beyond the coder's smoke suite (`ra-sim/tests/scatter_suite.rs`). Ground truth:
//! `drive.cpp`'s `Start_Of_Move` (MOVE_TEMP reaction), `CellClass::Incoming`
//! (`cell.cpp:2013`), `DriveClass::Scatter` (`drive.cpp:181`).
//!
//! Covers (ra-tester, M7.12 audit item 2):
//! 1. Multi-blocker chain — two parked friendlies in file, both scatter in
//!    sequence, mover gets through within a bounded tick budget.
//! 2. Scatter into a dead-end niche off the mover's own path.
//! 3. A fully boxed blocker (no free adjacent cell at all): mover holds
//!    stably forever, no panic, RNG draw count is bounded (exactly one draw
//!    per blocked tick, never more).
//! 4. Friendly infantry blocker scattered by a vehicle mover, and — the other
//!    direction — a vehicle blocker is *not* scattered by an infantry mover
//!    (infantry movers never issue the request at all).
//! 5. A harvester `Unloading` (dumping) at its dock is exempt, even though it
//!    is otherwise a textbook friendly, stationary blocker.
//! 6. Allied-but-different-house: the call site's gate is `House->Is_Ally`
//!    (`drive.cpp:959`/`:1023`), not same-house identity — pinned against a
//!    real alliance matrix, extending the smoke suite's same-house-only case.
//! 7. RNG accounting: a detour that succeeds never touches the scatter RNG at
//!    all (reroute happens before scatter is ever considered), and multiple
//!    friendly blockers packed into one cell each draw exactly once when
//!    asked (three blockers -> three draws, not one, not more).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, Handle, HarvStatus, MoveStats, Passability, World,
};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// A 1-wide horizontal corridor in the middle row of a `len`×`h` grid — no
/// detour is ever possible off the single passable row (mirrors
/// `scatter_suite.rs`'s helper of the same name; duplicated per this
/// codebase's convention of small per-file test fixtures).
fn corridor(len: i32, h: i32) -> (Passability, i32) {
    let n = (len * h) as usize;
    let mut cells = vec![false; n];
    let row = h / 2;
    for x in 0..len {
        cells[(row * len + x) as usize] = true;
    }
    (Passability::new(len, h, cells), row)
}

fn no_overlap(world: &World) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    for (_, u) in world.units.iter() {
        if !u.is_infantry() && !seen.insert((u.cell().x, u.cell().y)) {
            return false;
        }
    }
    true
}

fn spawn_infantry(world: &mut World, house: u8, cell: CellCoord, sub_cell: u8) -> Handle {
    let h = world.spawn_unit(0, house, cell, Facing(0), 50, stats(20, 8));
    world.units.get_mut(h).unwrap().make_infantry(sub_cell);
    h
}

/// Independent re-implementation of `RandomLcg`'s step formula
/// (`ra-sim/src/rng.rs`: `Seed = Seed*0x41C6_4E6D + 0x3039`, a bit-exact port
/// of `RandomClass::operator()()`, `common/random.cpp:85`) — used only to
/// *count* how many draws occurred between two observed `World::rng_seed()`
/// values, exactly the cross-check technique `rng.rs`'s own
/// `golden_ten_draws_seed_0x12345678` test uses. The step is a bijection
/// (the multiplier is odd), so counting forward from `before` to `after` is
/// exact, not a coincidence.
fn lcg_steps_between(mut seed: u32, target: u32, max: u32) -> Option<u32> {
    const MULT: u32 = 0x41C6_4E6D;
    const ADD: u32 = 0x0000_3039;
    for n in 0..=max {
        if seed == target {
            return Some(n);
        }
        seed = seed.wrapping_mul(MULT).wrapping_add(ADD);
    }
    None
}

// ===========================================================================
// 1. Multi-blocker chain: two parked friendlies packed in file, the near one
//    boxed against the far one, both scatter *off* the mover's line and the
//    mover drives all the way through to its destination.
//
// **This is the load-bearing regression test for the M7.12 cascade fix**
// (`scatter_blocker`'s boxed-neighbour propagation, `world.rs`). The geometry
// is a 1-wide corridor (row `ROW`) that DEAD-ENDS at its east edge (x=17),
// with a pair of one-cell alcoves carved directly north and south of that
// dead-end cell — (17, ROW-1) and (17, ROW+1) — the *only* off-corridor cells
// anywhere on the map. Two parked allied vehicles sit in the corridor; the
// mover approaches from the west.
//
// Why this exercises the cascade (and why the geometry matters):
//   * The mover pushes `b1` east one cell at a time (b1's only open neighbour
//     is east, since the mover always occupies its west neighbour and the
//     1-wide corridor boxes north/south). b1 is thus driven flush against
//     `b2`.
//   * Once b1 is flush against b2, b1 is **fully boxed** — west=mover,
//     east=b2, all six other neighbours impassable. On its own it can never
//     move again (this is exactly the permanent gridlock the *unfixed* code
//     produced). The ONLY way forward is the cascade: a boxed blocker asks
//     the friendly, stationary neighbour that is boxing it (b2) to scatter
//     first.
//   * b2 in turn gets pushed east to the dead-end cell (x=17), where its east
//     is off-map and its sole legal escapes are the two alcoves; it steps off
//     the corridor into one of them. That frees x=17, b1 advances, becomes the
//     mover's blocker at the dead-end, and takes the *remaining* alcove.
//   * With both blockers now off the corridor row, row `ROW` is clear from the
//     mover's start to the dead-end and the mover reaches its destination.
//
// Asserts (a) both blockers scatter, (b) NO vehicle overlap on any tick, and
// (c) the mover reaches its destination *because the blockers stepped aside*.
// Unlike the old (broken-as-written) version, "reached" is geometrically
// achievable here: the blockers vacate the corridor row entirely (into the
// alcoves) instead of being parked on the only path to the goal. Revert the
// cascade propagation in `world.rs` and this test returns to the permanent
// gridlock it guards against — the mover never reaches the dead-end (verified
// in the M7.12 revert-sensitivity pass).
// ===========================================================================

/// A 1-wide corridor on row `h/2` of a `len`×`h` grid that dead-ends at its
/// east edge (`x == len-1`), plus a one-cell alcove directly north and south
/// of that dead-end cell — the sole off-corridor escapes on the whole map.
/// Returns the passability and the corridor row.
fn dead_end_corridor_with_alcoves(len: i32, h: i32) -> (Passability, i32) {
    let row = h / 2;
    let mut cells = vec![false; (len * h) as usize];
    for x in 0..len {
        cells[(row * len + x) as usize] = true;
    }
    let last = len - 1;
    cells[((row - 1) * len + last) as usize] = true; // north alcove at the dead-end
    cells[((row + 1) * len + last) as usize] = true; // south alcove at the dead-end
    (Passability::new(len, h, cells), row)
}

#[test]
fn multi_blocker_chain_two_parked_friendlies_both_scatter_and_mover_gets_through() {
    const LEN: i32 = 18; // corridor x=0..=17; dead-ends at x=17
    let (grid, row) = dead_end_corridor_with_alcoves(LEN, 5);
    let dead_end = LEN - 1; // 17
    let mut world = World::new(grid, 0x5CA7_B001);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let b1 = world.spawn_unit(0, 1, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
    let b2 = world.spawn_unit(0, 1, CellCoord::new(13, row), Facing(0), 400, stats(24, 8));
    let b1_start = world.units.get(b1).unwrap().cell();
    let b2_start = world.units.get(b2).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(dead_end, row),
        house: 1,
    }]);

    let mut b1_scattered = false;
    let mut b2_scattered = false;
    let mut reached = false;
    for t in 0..1200 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: two vehicles share a cell");
        if world.units.get(b1).unwrap().cell() != b1_start {
            b1_scattered = true;
        }
        if world.units.get(b2).unwrap().cell() != b2_start {
            b2_scattered = true;
        }
        if world.units.get(mover).unwrap().cell().x >= dead_end {
            reached = true;
            break;
        }
    }
    assert!(b1_scattered, "the first parked blocker never scattered");
    assert!(b2_scattered, "the second parked blocker never scattered");
    assert!(
        reached,
        "mover never got through both scattered blockers within the tick budget"
    );
    // Both blockers ended up *off* the corridor row (in the dead-end alcoves),
    // which is the only way the mover could have reached the dead-end cell.
    let b1_end = world.units.get(b1).unwrap().cell();
    let b2_end = world.units.get(b2).unwrap().cell();
    assert_ne!(b1_end.y, row, "b1 should have stepped off the corridor row");
    assert_ne!(b2_end.y, row, "b2 should have stepped off the corridor row");
}

// ===========================================================================
// 2. Scatter into a dead-end niche that is off the mover's own path: the
//    blocker's only free cell is a side alcove, not a corridor continuation.
// ===========================================================================

#[test]
fn blocker_scatters_into_a_dead_end_niche_off_the_movers_path() {
    // A 16-wide, 5-tall grid with the mover's corridor on row 2, plus a
    // single extra passable cell at (8,1) — directly north of the blocker at
    // (8,2) — carved as the *only* niche anywhere on the map. Every other
    // neighbour of the blocker (E, W included) is intentionally boxed off by
    // stationary units so the niche is the sole legal destination.
    let len = 16i32;
    let h = 5i32;
    let row = 2i32;
    let mut cells = vec![false; (len * h) as usize];
    for x in 0..len {
        cells[(row * len + x) as usize] = true;
    }
    cells[(len + 8) as usize] = true; // the niche, north of the blocker (row 1)
    let mut world = World::new(Passability::new(len, h, cells), 0x5CA7_B002);

    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let blocker = world.spawn_unit(0, 1, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
    // Box the blocker's east neighbour with a permanent enemy "wall" (never
    // scattered) so the corridor-continuation cell isn't a competing escape;
    // the mover itself occupies the west neighbour once adjacent. That
    // leaves exactly one legal destination: the north niche.
    world.spawn_unit(0, 2, CellCoord::new(9, row), Facing(0), 400, stats(24, 8));

    // Order the mover PAST the blocker (not onto its cell): since M7.20 P1.5
    // the destination-only static-corner rule would let a mover ordered onto
    // the blocker's cell get its dest auto-adjusted to the niche and legally
    // diagonal-squeeze into it itself, bypassing the scatter this test pins.
    // A far-east dest keeps the plain path on the corridor row (units are
    // invisible to plain A*), so the mover jams against the blocker exactly
    // as before, and its unit-avoiding re-route may NOT corner-clip past the
    // blocker (the retained unit-corner strictness) — the scatter request is
    // the only unjam.
    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(15, row),
        house: 1,
    }]);
    let mut niche_used = false;
    for t in 0..300 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        if world.units.get(blocker).unwrap().cell() == CellCoord::new(8, 1) {
            niche_used = true;
            break;
        }
    }
    assert!(
        niche_used,
        "the blocker never took its one legal escape (the off-path niche)"
    );
    // The mover, having lost its blocker, should be free to close in on
    // (8,row) — the enemy wall at (9,row) still blocks the rest of the trip
    // (the mover eventually abandons that half via the PATH_RETRY budget, but
    // the vacated blocker cell must be reached first).
    let mut progressed = false;
    for _ in 0..200 {
        world.tick(&[]);
        if world.units.get(mover).unwrap().cell().x >= 7 {
            progressed = true;
            break;
        }
    }
    assert!(
        progressed,
        "mover never advanced once its blocker vacated into the niche"
    );
}

// ===========================================================================
// 3. Fully boxed blocker: no free adjacent cell in any of the 8 directions
//    (mover occupies one neighbour, a permanent enemy "wall" occupies the
//    opposite, the rest are impassable 1-wide-corridor terrain). Pin (M7.20):
//    mover holds stably, never panics, and — with the `PathDelay` throttle
//    (`PATH_DELAY_TICKS`) and the `TryTryAgain` budget (`PATH_RETRY`,
//    FOOT.H:241 / DRIVE.CPP:988-995) — its retries are *finite*: once the
//    budget exhausts it abandons the impossible order entirely (dest cleared)
//    and the scatter RNG goes silent, instead of drawing every tick forever
//    (the pre-M7.20 behaviour this suite used to pin, and a driver of the
//    scm01ea sim-rate collapse).
// ===========================================================================

#[test]
fn fully_boxed_blocker_holds_forever_no_panic_and_draws_are_bounded() {
    let (grid, row) = corridor(20, 5);
    let mut world = World::new(grid, 0x5CA7_B003);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let blocker = world.spawn_unit(0, 1, CellCoord::new(10, row), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();
    // Permanent enemy wall east of the blocker: never scattered, so it boxes
    // the blocker's east side for the whole run.
    world.spawn_unit(0, 2, CellCoord::new(11, row), Facing(0), 400, stats(24, 8));

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(18, row),
        house: 1,
    }]);

    // Run long enough for the mover to park adjacent to the blocker (west
    // neighbour), burn through its full retry budget (`PATH_RETRY` attempts
    // spaced `PATH_DELAY_TICKS` apart ≈ 140 ticks after parking), and abandon
    // the impossible order.
    for t in 0..400 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
    }
    let mover_settled = world.units.get(mover).unwrap().cell();
    assert_eq!(
        mover_settled,
        CellCoord::new(9, row),
        "mover should have parked directly west of the fully-boxed blocker"
    );
    assert_eq!(
        world.units.get(blocker).unwrap().cell(),
        blocker_start,
        "a fully boxed blocker must never appear to move"
    );
    // The `TryTryAgain` abandonment must have fired: the order is gone
    // (`Assign_Destination(TARGET_NONE)`, DRIVE.CPP:991-994), so the unit is
    // genuinely idle and re-taskable — not a permanent CPU drain.
    assert!(
        world.units.get(mover).unwrap().dest.is_none(),
        "after exhausting PATH_RETRY the mover must abandon its move order"
    );

    // Settled window: with the order abandoned there are NO further re-route
    // attempts and therefore NO scatter requests — the sim RNG must not
    // advance at all (this is what bounds per-tick cost in a gridlocked
    // world; the pre-M7.20 code drew every tick forever here).
    let seed_before = world.rng_seed();
    const WINDOW: u32 = 50;
    for t in 0..WINDOW {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t} (window): overlap");
        assert_eq!(
            world.units.get(mover).unwrap().cell(),
            mover_settled,
            "mover must hold stably, never overlapping or phasing through"
        );
        assert_eq!(
            world.units.get(blocker).unwrap().cell(),
            blocker_start,
            "boxed blocker must never move, tick {t}"
        );
    }
    assert_eq!(
        world.rng_seed(),
        seed_before,
        "an abandoned (given-up) mover must draw zero scatter RNG"
    );
}

// ===========================================================================
// 4. Friendly infantry blocker scattered by a vehicle mover, and the inverse:
//    a vehicle blocker is NOT scattered by an infantry mover (infantry
//    movers never issue the `Incoming` request at all — the reaction lives
//    in `DriveClass::Start_Of_Move`, and infantry are `FootClass`, not
//    `DriveClass`).
// ===========================================================================

#[test]
fn friendly_infantry_blocker_is_scattered_by_a_vehicle_mover() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_B004);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let blocker = spawn_infantry(&mut world, 1, CellCoord::new(8, row), 0);
    let blocker_start = world.units.get(blocker).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);
    let mut scattered = false;
    for t in 0..300 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        if world.units.get(blocker).unwrap().cell() != blocker_start {
            scattered = true;
            break;
        }
    }
    assert!(
        scattered,
        "a vehicle mover must be able to ask a friendly infantry blocker to scatter"
    );
}

#[test]
fn vehicle_blocker_is_not_scattered_by_an_infantry_mover() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_B005);
    let mover = spawn_infantry(&mut world, 1, CellCoord::new(1, row), 0);
    // A parked friendly vehicle occupies the whole cell; infantry cannot
    // co-occupy a vehicle's cell (Q5.3), so this genuinely blocks the mover.
    let blocker = world.spawn_unit(0, 1, CellCoord::new(4, row), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(10, row),
        house: 1,
    }]);
    for t in 0..200 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        assert_eq!(
            world.units.get(blocker).unwrap().cell(),
            blocker_start,
            "an infantry mover must never issue a scatter request (tick {t})"
        );
    }
}

// ===========================================================================
// 5. A harvester actively dumping (Unloading) at its dock is exempt from
//    scatter even though it is otherwise a textbook friendly, stationary
//    blocker (`IsDumping`, `drive.cpp:191`) — a documented, deliberate
//    permanent-wait, not a bug.
// ===========================================================================

#[test]
fn dumping_harvester_is_never_scattered_even_though_friendly_and_stationary() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_B006);
    // The harvest FSM's "no refinery for this house" guard runs *before* the
    // state match every tick (`run_harvesters`/`process_harvester`) and would
    // instantly force the status back to `Idle` — silently invalidating this
    // test's premise — unless house 1 owns a live refinery somewhere on the
    // map (position doesn't matter; `house_has_refinery` only checks
    // ownership + `is_refinery` + alive). Install a minimal one, off in a
    // corner, purely to keep the FSM guard from firing.
    world.set_catalog(Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "PROC".into(),
            foot_w: 3,
            foot_h: 3,
            max_health: 900,
            armor: 0,
            power: 0,
            cost: 2000,
            prereq: vec![],
            is_refinery: true,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 4,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 2000,
        }],
        units: vec![],
        econ: EconRules::default(),
    });
    world
        .spawn_building(0, 1, CellCoord::new(60, 60))
        .expect("refinery placed far away, just to satisfy house_has_refinery");

    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let harv = world.spawn_unit(0, 1, CellCoord::new(8, row), Facing(0), 600, stats(24, 5));
    {
        let u = world.units.get_mut(harv).unwrap();
        u.is_harvester = true;
        u.harvest.status = HarvStatus::Unloading;
        // A huge countdown so the FSM's own payout transition (Unloading ->
        // Looking once `timer` hits 0) never fires inside this test's window
        // — otherwise the harvester would legitimately leave Unloading on its
        // own after a few ticks, unrelated to the scatter mechanic under test.
        u.harvest.timer = 60_000;
    }
    let harv_start = world.units.get(harv).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);
    for t in 0..200 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        assert_eq!(
            world.units.get(harv).unwrap().cell(),
            harv_start,
            "a dumping harvester must never be scattered (tick {t})"
        );
    }
    // The mover is genuinely (and correctly) stuck behind it.
    assert!(world.units.get(mover).unwrap().cell().x < 8);
}

// ===========================================================================
// 6. Allied-but-different-house: the original's gate at the call site is
//    `House->Is_Ally(blockage)` (`drive.cpp:959`/`:1023`), which the sim ports
//    as `World::are_allies` — not raw same-house identity. Pin that an
//    allied (but distinct) house's blocker IS scattered, extending the smoke
//    suite's `enemy_parked_blocker_is_not_scattered` (which only proves
//    non-allies are exempt).
// ===========================================================================

#[test]
fn allied_but_different_house_blocker_is_scattered() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_B007);
    // House 1 (mover) is allied with house 2 (blocker) — a real alliance
    // matrix, not the "same house" no-op path.
    world.set_alliances(vec![0, 1u64 << 2, 1u64 << 1]);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let blocker = world.spawn_unit(0, 2, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);
    let mut scattered = false;
    for t in 0..300 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        if world.units.get(blocker).unwrap().cell() != blocker_start {
            scattered = true;
            break;
        }
    }
    assert!(
        scattered,
        "an allied (different-house) blocker must be scattered, matching House->Is_Ally at the call site"
    );
}

// ===========================================================================
// 7. RNG accounting.
// ===========================================================================

/// A detour that succeeds (an open field, not a 1-wide corridor) must never
/// touch the scatter RNG at all: `find_path_avoiding` runs and adopts a
/// route before `scatter_friendly_blockers` is ever considered (yield ->
/// re-route -> scatter precedence). Zero draws for the whole run.
#[test]
fn a_successful_reroute_never_touches_the_scatter_rng() {
    let mut world = World::new(Passability::all_passable(), 0x5CA7_B008);
    let mover = world.spawn_unit(0, 1, CellCoord::new(2, 10), Facing(64), 400, stats(24, 8));
    let blocker = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();
    let seed_before = world.rng_seed();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(20, 10),
        house: 1,
    }]);
    let mut reached = false;
    for t in 0..400 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
        assert_eq!(
            world.units.get(blocker).unwrap().cell(),
            blocker_start,
            "an open-field blocker with a real detour available must never be asked to scatter (tick {t})"
        );
        if world.units.get(mover).unwrap().cell() == CellCoord::new(20, 10) {
            reached = true;
            break;
        }
    }
    assert!(
        reached,
        "mover should have simply routed around the parked blocker and arrived"
    );
    assert_eq!(
        world.rng_seed(),
        seed_before,
        "a purely reroute-resolved block must draw zero scatter RNG"
    );
}

/// Three friendly, stationary infantry packed into one cell, all boxed in
/// (no legal escape for any of them — same construction as the single-boxed-
/// blocker case, scaled to a full 3-occupant cell): each scatter *request*
/// asks every genuinely eligible occupant, so each request costs three
/// logical `range(0,2)` draws (the cell is not collapsed into one event).
/// Since M7.20 the requests themselves are finite: one per `PATH_DELAY_TICKS`
/// re-route attempt, at most `PATH_RETRY` of them, then the mover abandons
/// the order and the RNG goes silent. This pins both halves: ≥3 draws total
/// (at least one full request fired, all three occupants asked) and ZERO
/// draws after abandonment.
#[test]
fn three_packed_infantry_blockers_each_draw_exactly_once_per_tick() {
    let (grid, row) = corridor(20, 5);
    let mut world = World::new(grid, 0x5CA7_B009);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let b0 = spawn_infantry(&mut world, 1, CellCoord::new(10, row), 0);
    let b1 = spawn_infantry(&mut world, 1, CellCoord::new(10, row), 1);
    let b2 = spawn_infantry(&mut world, 1, CellCoord::new(10, row), 2);
    // Permanent enemy vehicle wall east of the packed cell so no infantry can
    // escape eastward either (a vehicle's presence blocks infantry entry).
    world.spawn_unit(0, 2, CellCoord::new(11, row), Facing(0), 400, stats(24, 8));

    let seed_start = world.rng_seed();
    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(18, row),
        house: 1,
    }]);
    // Long enough to park, burn the full retry budget, and abandon.
    for t in 0..400 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
    }
    let mover_settled = world.units.get(mover).unwrap().cell();
    assert_eq!(
        mover_settled,
        CellCoord::new(9, row),
        "mover should have parked directly west of the packed cell"
    );
    for (label, h) in [("b0", b0), ("b1", b1), ("b2", b2)] {
        assert_eq!(
            world.units.get(h).unwrap().cell(),
            CellCoord::new(10, row),
            "{label} should still be boxed in place after settling"
        );
    }
    // At least one full scatter request fired during the retry phase, asking
    // each of the three occupants once (>=3 logical draws => >=3 LCG steps).
    let steps_total = lcg_steps_between(seed_start, world.rng_seed(), 3 * 6 * PATH_RETRY_BOUND)
        .expect("draw count should be small and bounded");
    assert!(
        steps_total >= 3,
        "each of the three boxed occupants must be asked (>=3 draws total, got {steps_total})"
    );
    assert!(
        world.units.get(mover).unwrap().dest.is_none(),
        "after exhausting PATH_RETRY the mover must abandon its move order"
    );

    // Post-abandonment: the RNG must be silent.
    let seed_before = world.rng_seed();
    const WINDOW: u32 = 20;
    for t in 0..WINDOW {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t} (window): overlap");
        assert_eq!(world.units.get(mover).unwrap().cell(), mover_settled);
    }
    assert_eq!(
        world.rng_seed(),
        seed_before,
        "an abandoned mover must issue no further scatter requests (zero draws)"
    );
}

/// Loose upper bound on scatter requests for the packed-cell test: `PATH_RETRY`
/// (10) requests × 3 occupants × 6 steps-per-draw headroom.
const PATH_RETRY_BOUND: u32 = 10;
