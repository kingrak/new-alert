//! Coverage for M6's deterministic skirmish AI (`ra-sim/src/ai.rs`), which had
//! zero tests before this suite. Drives everything through the public `World`
//! API (`World::new`/`spawn_unit`/`set_catalog`/`set_ai`/`tick`), per the
//! ra-tester charter's preference for testing through public APIs rather than
//! reaching into `ai.rs` internals.
//!
//! A small local catalog fixture (mirrors `world.rs`'s `m5_tests::catalog()`
//! pattern) gives each house: a construction yard (from its starting MCV), a
//! power plant, a refinery (which auto-spawns a free harvester on placement),
//! a war factory, and two armed vehicle types so the weighted-random
//! production draw actually has a choice to make.
//!
//! Covers:
//! - full-skirmish determinism (same seed twice) at each `Difficulty`,
//! - the AI's build-order policy (power -> refinery -> war factory),
//! - attack waves actually reaching the enemy's base,
//! - harder difficulties attacking sooner than easier ones,
//! - the weighted-random unit-production draw being seed-deterministic and
//!   seed-sensitive.
//!
//! Tests that only need a milestone tick (not the full per-tick hash chain)
//! use [`run_until`], which stops as soon as the milestone is observed rather
//! than always burning a fixed tick budget — debug-build pathfinding/targeting
//! scans dominate wall-clock here, so this keeps the suite fast without
//! weakening what is asserted.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildItem, BuildingProto, Catalog, Difficulty, EconRules, MoveStats, Passability,
    UnitProto, WarheadProfile, WeaponProfile, World,
};

// Building ids in the local fixture catalog.
const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
const B_PROC: u32 = 2;
const B_WEAP: u32 = 3;

// Unit-proto ids.
const U_MCV: u32 = 0;
const U_HARV: u32 = 1;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn weapon(damage: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof: 30,
        range: 5 * 256, // 5 cells
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 1,
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// A tiny catalog: FACT (construction yard) / POWR / PROC (refinery, free
/// harvester on placement) / WEAP (war factory), plus MCV/HARV/two armed
/// vehicle types (TANK, ARTY) so `produce_units`'s weighted-random pick has
/// more than one eligible outcome. Costs are small so build loops finish in a
/// few hundred ticks, keeping the suite fast.
fn catalog() -> Catalog {
    let bproto = |name: &str,
                  w: u8,
                  h: u8,
                  power: i32,
                  cost: i32,
                  prereq: Vec<u32>,
                  cy: bool,
                  refin: bool,
                  wf: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor: 0,
        power,
        cost,
        prereq,
        is_refinery: refin,
        is_construction_yard: cy,
        is_war_factory: wf,
        free_harvester_unit: if refin { Some(U_HARV) } else { None },
        sight: 5,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let uproto = |name: &str,
                  sprite_id: u32,
                  harv: bool,
                  deploys: Option<u32>,
                  weapon: Option<WeaponProfile>,
                  cost: i32,
                  prereq: Vec<u32>| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name: name.to_string(),
        sprite_id,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon,
        secondary: None,
        has_turret: weapon.is_some(),
        is_harvester: harv,
        deploys_to: deploys,
        cost,
        prereq,
        sight: 4,
    };
    Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 0, 100, vec![], true, false, false),
            bproto("POWR", 2, 2, 100, 30, vec![B_FACT], false, false, false),
            bproto("PROC", 3, 3, -30, 50, vec![B_POWR], false, true, false),
            bproto("WEAP", 3, 3, -20, 60, vec![B_POWR], false, false, true),
        ],
        units: vec![
            uproto("MCV", 0, false, Some(B_FACT), None, 100, vec![]),
            uproto("HARV", 1, true, None, None, 140, vec![]),
            uproto("TANK", 2, false, None, Some(weapon(25)), 80, vec![B_WEAP]),
            uproto("ARTY", 3, false, None, Some(weapon(40)), 90, vec![B_WEAP]),
        ],
        econ: EconRules::default(),
    }
}

fn home1() -> CellCoord {
    CellCoord::new(15, 15)
}
fn home2() -> CellCoord {
    CellCoord::new(110, 110)
}

/// Ample starting credits so the AI never stalls on funds within the tick
/// budgets used below (no ore field is modeled; this isolates AI build/attack
/// behavior from harvester economy, which is covered elsewhere).
const CREDITS: i32 = 6000;

/// A two-house AI-vs-AI skirmish: house 1 and house 2, each AI-controlled at
/// `difficulty`, MCVs planted far apart (opposite corners of the 128x128 map)
/// so an attack wave has to cross real distance. House 0 is left empty,
/// matching `world.rs`'s `m5_tests` convention of using house 0 as an unused
/// "neutral" slot.
fn skirmish(seed: u32, difficulty: Difficulty) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, CREDITS);
    w.spawn_unit(U_MCV, 1, home1(), Facing(0), 400, stats());
    w.spawn_unit(U_MCV, 2, home2(), Facing(0), 400, stats());
    w.set_ai(vec![
        AiPlayer::new(1, difficulty),
        AiPlayer::new(2, difficulty),
    ]);
    w
}

/// Squared-distance "in the opposing base" radius used to detect an attack
/// wave actually arriving (buildings place within a 13-cell spiral of the
/// construction yard, so 25 cells comfortably covers the whole base cluster
/// while staying well short of "anywhere on the map").
const INCURSION_RADIUS_SQ: i64 = 25 * 25;

#[derive(Default)]
struct RunOutcome {
    hashes: Vec<u64>,
    /// `first_owned[house_idx][building_id]` = first tick that house owned
    /// that building, if ever (house_idx 0 = house 1, house_idx 1 = house 2).
    first_owned: [[Option<u32>; 4]; 2],
    /// First tick any unit of one house came within [`INCURSION_RADIUS_SQ`] of
    /// the *other* house's starting base cell.
    first_incursion: Option<u32>,
}

/// Run a two-house AI skirmish up to `max_ticks`, instrumenting build order
/// and enemy-base incursion as it goes (both are transient facts that must be
/// observed during the run, not reconstructed from the final `World`).
/// Stops early once `done(&outcome)` returns true; pass `|_| false` to always
/// run the full budget (needed when the caller wants the complete hash
/// chain, e.g. determinism comparisons). Skips collecting the hash chain
/// when `keep_hashes` is false, since it is unused by milestone-only tests.
fn run_impl(
    seed: u32,
    difficulty: Difficulty,
    max_ticks: u32,
    keep_hashes: bool,
    mut done: impl FnMut(&RunOutcome) -> bool,
) -> RunOutcome {
    let mut w = skirmish(seed, difficulty);
    let mut outcome = RunOutcome::default();
    let h1 = home1();
    let h2 = home2();

    for t in 0..max_ticks {
        let hash = w.tick(&[]);
        if keep_hashes {
            outcome.hashes.push(hash);
        }

        for (idx, house) in [1u8, 2u8].into_iter().enumerate() {
            if let Some(hs) = w.house(house) {
                for (b, slot) in outcome.first_owned[idx].iter_mut().enumerate() {
                    if slot.is_none() && hs.owns_building(b as u32) {
                        *slot = Some(t);
                    }
                }
            }
        }

        if outcome.first_incursion.is_none() {
            for (_, u) in w.units.iter() {
                let target_home = match u.house {
                    1 => h2,
                    2 => h1,
                    _ => continue,
                };
                let dx = (u.cell().x - target_home.x) as i64;
                let dy = (u.cell().y - target_home.y) as i64;
                if dx * dx + dy * dy <= INCURSION_RADIUS_SQ {
                    outcome.first_incursion = Some(t);
                    break;
                }
            }
        }

        if done(&outcome) {
            break;
        }
    }

    outcome
}

/// Run the full `ticks` budget, collecting the per-tick hash chain — for
/// determinism comparisons, which must compare the *whole* chain.
fn run(seed: u32, difficulty: Difficulty, ticks: u32) -> RunOutcome {
    run_impl(seed, difficulty, ticks, true, |_| false)
}

/// Run up to `max_ticks`, stopping as soon as `done` is satisfied. No hash
/// chain is kept. `max_ticks` is a safety cap: if it is hit, the relevant
/// `RunOutcome` field(s) stay `None`/incomplete and the caller's assertion
/// fails with a clear "never happened within budget" message.
fn run_until(
    seed: u32,
    difficulty: Difficulty,
    max_ticks: u32,
    done: impl FnMut(&RunOutcome) -> bool,
) -> RunOutcome {
    run_impl(seed, difficulty, max_ticks, false, done)
}

// ---------------------------------------------------------------------------
// 1. Same-seed-twice full-skirmish determinism, at each difficulty.
// ---------------------------------------------------------------------------

#[test]
fn determinism_holds_at_each_difficulty() {
    const TICKS: u32 = 2000;
    for &d in &[Difficulty::Easy, Difficulty::Normal, Difficulty::Hard] {
        let a = run(0xC0FF_EE01, d, TICKS);
        let b = run(0xC0FF_EE01, d, TICKS);
        assert_eq!(
            a.hashes, b.hashes,
            "{d:?}: hash chain diverged between two runs of the identical seed/setup"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The AI actually builds, in the real `next_structure` priority order:
//    power (when absent) -> refinery (when none) -> war factory.
// ---------------------------------------------------------------------------

#[test]
fn ai_builds_in_next_structure_priority_order() {
    const MAX_TICKS: u32 = 2000;
    let r = run_until(0xB0B0_B0B0, Difficulty::Normal, MAX_TICKS, |o| {
        o.first_owned[0][B_WEAP as usize].is_some() && o.first_owned[1][B_WEAP as usize].is_some()
    });
    for (house_idx, house_label) in [(0, "house 1"), (1, "house 2")] {
        let owned = &r.first_owned[house_idx];
        let powr = owned[B_POWR as usize]
            .unwrap_or_else(|| panic!("{house_label}: power plant never built"));
        let proc =
            owned[B_PROC as usize].unwrap_or_else(|| panic!("{house_label}: refinery never built"));
        let weap = owned[B_WEAP as usize]
            .unwrap_or_else(|| panic!("{house_label}: war factory never built"));
        assert!(
            powr < proc,
            "{house_label}: power plant (tick {powr}) should precede the refinery (tick {proc})"
        );
        assert!(
            proc < weap,
            "{house_label}: refinery (tick {proc}) should precede the war factory (tick {weap})"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. The AI actually attacks: units cross the map and enter the enemy base.
// ---------------------------------------------------------------------------

#[test]
fn ai_launches_attacks_that_reach_the_enemy_base() {
    // Hard: attack_interval = 35s = 525 ticks (+-50% jitter), min_force = 2.
    // Safety-cap budget covering a couple of retry windows plus build-up time
    // (observed first incursion around tick 1200 for this fixture/seed).
    const MAX_TICKS: u32 = 3000;
    let r = run_until(0x1234_5678, Difficulty::Hard, MAX_TICKS, |o| {
        o.first_incursion.is_some()
    });
    r.first_incursion.unwrap_or_else(|| {
        panic!(
            "no unit of either house ever entered the enemy base radius within {MAX_TICKS} ticks"
        )
    });
}

// ---------------------------------------------------------------------------
// 4. Per-difficulty behavior differs (while each is individually
//    deterministic, per test 1): Hard attacks sooner than Easy.
// ---------------------------------------------------------------------------

#[test]
fn harder_difficulty_reaches_the_enemy_base_sooner() {
    const MAX_TICKS: u32 = 4000;
    let easy = run_until(0xACE1_ACE1, Difficulty::Easy, MAX_TICKS, |o| {
        o.first_incursion.is_some()
    });
    let hard = run_until(0xACE1_ACE1, Difficulty::Hard, MAX_TICKS, |o| {
        o.first_incursion.is_some()
    });
    let easy_tick = easy
        .first_incursion
        .unwrap_or_else(|| panic!("easy AI never reached the enemy base within {MAX_TICKS} ticks"));
    let hard_tick = hard
        .first_incursion
        .unwrap_or_else(|| panic!("hard AI never reached the enemy base within {MAX_TICKS} ticks"));
    assert!(
        hard_tick < easy_tick,
        "hard (tick {hard_tick}) should reach the enemy base sooner than easy (tick {easy_tick}), \
         consistent with Hard's shorter attack_interval and smaller min_force"
    );
}

// ---------------------------------------------------------------------------
// 5. Weighted-random unit production: same seed -> same sequence of unit
//    types queued; a different seed can (and here does) change it.
// ---------------------------------------------------------------------------

/// The sequence of unit-proto ids house 1's war factory *starts* producing,
/// up to `max_ticks` or until `want` entries have been collected (whichever
/// comes first) — one entry per new `StartProduction(Unit)`, in order.
fn unit_production_sequence(seed: u32, max_ticks: u32, want: usize) -> Vec<u32> {
    let mut w = skirmish(seed, Difficulty::Normal);
    let mut seq = Vec::new();
    let mut prev: Option<u32> = None;
    for _ in 0..max_ticks {
        w.tick(&[]);
        let cur = w
            .house(1)
            .and_then(|hs| hs.unit_prod)
            .and_then(|p| match p.item {
                BuildItem::Unit(id) => Some(id),
                BuildItem::Building(_) => None,
            });
        if let Some(id) = cur {
            if cur != prev {
                seq.push(id);
                if seq.len() >= want {
                    break;
                }
            }
        }
        prev = cur;
    }
    seq
}

#[test]
fn weighted_random_unit_production_is_seed_deterministic_and_seed_sensitive() {
    const MAX_TICKS: u32 = 2000;
    const WANT: usize = 6;
    let a1 = unit_production_sequence(0x1111_1111, MAX_TICKS, WANT);
    let a2 = unit_production_sequence(0x1111_1111, MAX_TICKS, WANT);
    assert_eq!(
        a1, a2,
        "same seed must produce the identical unit-type sequence"
    );
    assert!(
        a1.len() >= 3,
        "expected at least a few units queued in {MAX_TICKS} ticks, got {}: {a1:?}",
        a1.len()
    );

    let b = unit_production_sequence(0x2222_2222, MAX_TICKS, WANT);
    assert_ne!(
        a1, b,
        "a different seed should (at least for this seed pair) draw a different unit-type sequence, \
         proving the pick is live and seed-derived rather than accidentally constant"
    );
}

// ===========================================================================
// M7.7 coverage: full ground roster (infantry, defenses, DOME/tech, walls-
// adjacent buildings) exercising chunk A (vehicle weighting — already covered
// above via TANK/ARTY), chunk B (defense tier / build-order priority), and
// chunk C (DOME radar-dome build priority + offensive-infantry filter).
// `ai.rs`/`house.rs` have zero colocated unit tests, so this is the only
// coverage for these code paths; see the ra-tester report for what was
// found while reading the source to derive each expectation below.
// ===========================================================================

// Building ids in the full-roster fixture (see `full_roster_catalog`).
const FR_FACT: u32 = 0;
const FR_POWR: u32 = 1;
const FR_PROC: u32 = 2;
const FR_WEAP: u32 = 3;
const FR_BARR: u32 = 4;
const FR_DOME: u32 = 5;
const FR_PBOX: u32 = 6;
const FR_TSLA: u32 = 7;
const FR_ATEK: u32 = 8;
const FR_BUILDING_COUNT: usize = 9;

// Unit-proto ids in the full-roster fixture. Ids 2/3 (TANK/ARTY) are the
// armed vehicles that give the weighted-random vehicle pick a real choice
// (chunk A, already covered by `weighted_random_unit_production_is_seed_*`
// against the smaller fixture above) — referenced only positionally here.
const FR_U_MCV: u32 = 0;
const FR_U_HARV: u32 = 1;
const FR_U_E1: u32 = 4;
const FR_U_MEDIC: u32 = 5;
const FR_U_ENGINEER: u32 = 6;

/// The M7.7 ground roster: construction yard / power / refinery / war
/// factory / barracks / radar dome / two base-defense tiers (a cheap `PBOX`
/// and a stronger `TSLA` gated behind an `ATEK`-like tech building that the
/// AI never builds on its own — `next_structure` has no role/name branch for
/// it, only `DOME` gets that treatment, `ai.rs:254-268`) / vehicles / three
/// infantry archetypes: an offensive rifleman (`E1`), a medic (heal weapon,
/// non-positive damage), and an unarmed engineer (`weapon: None`). The last
/// two exist specifically to exercise the `AI_Infantry`-derived filter added
/// in M7.7 chunk C (`ai.rs` `produce_units`'s infantry lane, `p.weapon.map(|w|
/// w.damage > 0)`), which is completely untested prior to this suite.
fn full_roster_catalog() -> Catalog {
    let bproto = |name: &str,
                  w: u8,
                  h: u8,
                  power: i32,
                  cost: i32,
                  prereq: Vec<u32>,
                  cy: bool,
                  refin: bool,
                  wf: bool,
                  barracks: bool,
                  weapon: Option<WeaponProfile>| BuildingProto {
        is_barracks: barracks,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor: 0,
        power,
        cost,
        prereq,
        is_refinery: refin,
        is_construction_yard: cy,
        is_war_factory: wf,
        free_harvester_unit: if refin { Some(FR_U_HARV) } else { None },
        sight: 5,
        sprite_id: 0,
        weapon,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let uproto = |name: &str,
                  sprite_id: u32,
                  is_infantry: bool,
                  harv: bool,
                  deploys: Option<u32>,
                  weapon: Option<WeaponProfile>,
                  cost: i32,
                  prereq: Vec<u32>| UnitProto {
        is_infantry,
        locomotor: if is_infantry { 0 } else { 1 },
        name: name.to_string(),
        sprite_id,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon,
        secondary: None,
        has_turret: weapon.is_some(),
        is_harvester: harv,
        deploys_to: deploys,
        cost,
        prereq,
        sight: 4,
    };
    Catalog {
        buildings: vec![
            bproto(
                "FACT",
                3,
                3,
                0,
                100,
                vec![],
                true,
                false,
                false,
                false,
                None,
            ),
            bproto(
                "POWR",
                2,
                2,
                100,
                30,
                vec![FR_FACT],
                false,
                false,
                false,
                false,
                None,
            ),
            bproto(
                "PROC",
                3,
                3,
                -30,
                50,
                vec![FR_POWR],
                false,
                true,
                false,
                false,
                None,
            ),
            bproto(
                "WEAP",
                3,
                3,
                -20,
                60,
                vec![FR_POWR],
                false,
                false,
                true,
                false,
                None,
            ),
            bproto(
                "BARR",
                2,
                2,
                -10,
                40,
                vec![FR_POWR],
                false,
                false,
                false,
                true,
                None,
            ),
            bproto(
                "DOME",
                2,
                2,
                -30,
                50,
                vec![FR_POWR],
                false,
                false,
                false,
                false,
                None,
            ),
            // Cheap defense: always buildable once a war factory is up.
            bproto(
                "PBOX",
                1,
                1,
                0,
                25,
                vec![],
                false,
                false,
                false,
                false,
                Some(weapon(10)),
            ),
            // Strong defense: gated behind ATEK, which the AI never builds on
            // its own (no role/name branch picks it) — only reachable in
            // these tests via a direct `World::spawn_building`.
            bproto(
                "TSLA",
                1,
                1,
                -10,
                150,
                vec![FR_ATEK],
                false,
                false,
                false,
                false,
                Some(weapon(100)),
            ),
            bproto(
                "ATEK",
                2,
                2,
                -10,
                75,
                vec![FR_DOME],
                false,
                false,
                false,
                false,
                None,
            ),
        ],
        units: vec![
            uproto("MCV", 0, false, false, Some(FR_FACT), None, 100, vec![]),
            uproto("HARV", 1, false, true, None, None, 140, vec![]),
            uproto(
                "TANK",
                2,
                false,
                false,
                None,
                Some(weapon(25)),
                80,
                vec![FR_WEAP],
            ),
            uproto(
                "ARTY",
                3,
                false,
                false,
                None,
                Some(weapon(40)),
                90,
                vec![FR_WEAP],
            ),
            // Offensive infantry: positive-damage weapon, admitted by the filter.
            uproto(
                "E1",
                4,
                true,
                false,
                None,
                Some(weapon(15)),
                30,
                vec![FR_BARR],
            ),
            // Medic: a "heal" weapon modeled as non-positive damage, excluded by
            // the filter (`ai.rs`: `w.damage > 0`).
            uproto(
                "MEDIC",
                5,
                true,
                false,
                None,
                Some(weapon(-10)),
                40,
                vec![FR_BARR],
            ),
            // Engineer: unarmed, excluded by the filter (`weapon.map(...)`
            // is `None` -> `unwrap_or(false)`).
            uproto("ENGINEER", 6, true, false, None, None, 40, vec![FR_BARR]),
        ],
        econ: EconRules::default(),
    }
}

fn full_roster_skirmish(seed: u32, difficulty: Difficulty) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(full_roster_catalog());
    w.init_houses(3, CREDITS);
    w.spawn_unit(FR_U_MCV, 1, home1(), Facing(0), 400, stats());
    w.spawn_unit(FR_U_MCV, 2, home2(), Facing(0), 400, stats());
    w.set_ai(vec![
        AiPlayer::new(1, difficulty),
        AiPlayer::new(2, difficulty),
    ]);
    w
}

#[derive(Default)]
struct FullRosterOutcome {
    hashes: Vec<u64>,
    first_owned: [[Option<u32>; FR_BUILDING_COUNT]; 2],
}

/// Drive a full-roster two-house AI skirmish for `ticks`, recording the
/// per-tick hash chain and the first tick each house owned each building
/// type — mirrors `run`/`run_impl` above but sized for the bigger roster.
fn run_full_roster(seed: u32, difficulty: Difficulty, ticks: u32) -> FullRosterOutcome {
    let mut w = full_roster_skirmish(seed, difficulty);
    let mut outcome = FullRosterOutcome::default();
    for t in 0..ticks {
        let hash = w.tick(&[]);
        outcome.hashes.push(hash);
        for (idx, house) in [1u8, 2u8].into_iter().enumerate() {
            if let Some(hs) = w.house(house) {
                for (b, slot) in outcome.first_owned[idx].iter_mut().enumerate() {
                    if slot.is_none() && hs.owns_building(b as u32) {
                        *slot = Some(t);
                    }
                }
            }
        }
    }
    outcome
}

// ---------------------------------------------------------------------------
// 6. Full-roster determinism, same seed twice, at every difficulty. Catches
//    nondeterminism newly introduced anywhere in M7.7 (radar-dome pick,
//    defense-tier scan, infantry filter, vehicle weighting) that the smaller
//    pre-M7.7 fixture above cannot reach because it has no barracks/DOME/
//    defense/infantry buildings at all.
// ---------------------------------------------------------------------------

#[test]
fn full_roster_determinism_holds_at_each_difficulty() {
    // Smaller than the pre-M7.7 fixture's 2000-tick budget: the full roster's
    // extra combat (base defenses firing, more units alive at once) makes
    // each tick noticeably more expensive in a debug build. 900 ticks (~1
    // in-game minute) still covers several `next_structure`/`produce_units`
    // decision cycles per house (DECIDE_PERIOD = 15 ticks) plus the start of
    // combat, which is where nondeterminism (HashMap iteration order, etc.)
    // would most likely show up.
    const TICKS: u32 = 900;
    for &d in &[Difficulty::Easy, Difficulty::Normal, Difficulty::Hard] {
        let a = run_full_roster(0xF0F0_0001, d, TICKS);
        let b = run_full_roster(0xF0F0_0001, d, TICKS);
        assert_eq!(
            a.hashes, b.hashes,
            "{d:?}: full-roster hash chain diverged between two runs of the identical seed/setup"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. Build-order priority over the FULL roster, derived by reading
//    `next_structure` (`ai.rs:213-307`) rather than assumed from the M7.7-B/C
//    commit messages:
//
//      1) power (`ai.rs:229-233`)
//      2) refinery, if none owned (`ai.rs:235-239`)
//      3) war factory (`ai.rs:241-245`)
//      3b) barracks, once the war factory is up (`ai.rs:247-253`)
//      3b2) DOME (radar), once the war factory is up — checked AFTER
//           barracks in source order (`ai.rs:257-268`)
//      3c) base defense (strongest buildable), also gated on the war
//          factory, checked AFTER DOME (`ai.rs:276-294`)
//
//    So the naive read of the M7.7-B commit message ("AI defense tier")
//    could suggest defenses come right after the war factory; the actual
//    code interleaves barracks and the radar dome ahead of any defense
//    structure. This test pins that observed (not assumed) order. TSLA is
//    excluded from natural build order here (its ATEK prereq is never
//    self-built by the AI, see `full_roster_catalog`'s doc comment), so the
//    only defense either house can complete via ordinary play is PBOX.
// ---------------------------------------------------------------------------

#[test]
fn ai_builds_full_roster_in_next_structure_priority_order() {
    const MAX_TICKS: u32 = 6000;
    let mut w = full_roster_skirmish(0xB0B0_0002, Difficulty::Normal);
    let mut outcome = FullRosterOutcome::default();
    for t in 0..MAX_TICKS {
        w.tick(&[]);
        for (idx, house) in [1u8, 2u8].into_iter().enumerate() {
            if let Some(hs) = w.house(house) {
                for (b, slot) in outcome.first_owned[idx].iter_mut().enumerate() {
                    if slot.is_none() && hs.owns_building(b as u32) {
                        *slot = Some(t);
                    }
                }
            }
        }
        if outcome.first_owned[0][FR_PBOX as usize].is_some()
            && outcome.first_owned[1][FR_PBOX as usize].is_some()
        {
            break;
        }
    }

    for (house_idx, house_label) in [(0, "house 1"), (1, "house 2")] {
        let owned = &outcome.first_owned[house_idx];
        let powr =
            owned[FR_POWR as usize].unwrap_or_else(|| panic!("{house_label}: POWR never built"));
        let proc =
            owned[FR_PROC as usize].unwrap_or_else(|| panic!("{house_label}: PROC never built"));
        let weap =
            owned[FR_WEAP as usize].unwrap_or_else(|| panic!("{house_label}: WEAP never built"));
        let barr =
            owned[FR_BARR as usize].unwrap_or_else(|| panic!("{house_label}: BARR never built"));
        let dome =
            owned[FR_DOME as usize].unwrap_or_else(|| panic!("{house_label}: DOME never built"));
        let pbox =
            owned[FR_PBOX as usize].unwrap_or_else(|| panic!("{house_label}: PBOX never built"));
        // ATEK is never self-built by the AI: no role/name branch selects it
        // (only "DOME" gets a name match, `ai.rs:257-268`).
        assert!(
            owned[FR_ATEK as usize].is_none(),
            "{house_label}: AI built ATEK, but next_structure has no branch that selects it \
             (a role/name match must have been added without updating this test's premise)"
        );
        assert!(
            owned[FR_TSLA as usize].is_none(),
            "{house_label}: AI built TSLA despite its ATEK prereq never being self-built"
        );
        assert!(
            powr < proc,
            "{house_label}: POWR ({powr}) should precede PROC ({proc})"
        );
        assert!(
            proc < weap,
            "{house_label}: PROC ({proc}) should precede WEAP ({weap})"
        );
        assert!(
            weap < barr,
            "{house_label}: WEAP ({weap}) should precede BARR ({barr})"
        );
        assert!(
            barr < dome,
            "{house_label}: BARR ({barr}) should precede DOME ({dome}) — barracks (3b) is \
             checked before the radar dome (3b2) in next_structure's source order"
        );
        assert!(
            dome < pbox,
            "{house_label}: DOME ({dome}) should precede the first base defense ({pbox}) — \
             the radar dome (3b2) is checked before base defense (3c) in next_structure's \
             source order"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. Defense-tier preference: when a stronger defense (TSLA) is ALREADY
//    buildable (its tech prereq pre-granted), `next_structure`'s reverse
//    catalog scan (`ai.rs:280-287`, "we prefer the *strongest* buildable
//    defense … reverse catalog order") must pick it over the cheap PBOX —
//    and PBOX should never get built while TSLA remains available, since the
//    scan always finds TSLA first and stops there.
// ---------------------------------------------------------------------------

#[test]
fn ai_prefers_the_strongest_buildable_defense() {
    const MAX_TICKS: u32 = 6000;
    let mut w = full_roster_skirmish(0xADEF_0003, Difficulty::Normal);
    // Pre-grant house 1 an ATEK far off in a corner of the map (off the
    // natural base-placement spiral) so TSLA is buildable from the start,
    // without going through production (the AI itself never builds ATEK —
    // see the roster doc comment).
    w.spawn_building(FR_ATEK, 1, CellCoord::new(2, 2));

    let mut first_tsla: Option<u32> = None;
    let mut first_pbox: Option<u32> = None;
    for t in 0..MAX_TICKS {
        w.tick(&[]);
        if first_tsla.is_none() {
            if let Some(hs) = w.house(1) {
                if hs.owns_building(FR_TSLA) {
                    first_tsla = Some(t);
                }
                if hs.owns_building(FR_PBOX) {
                    first_pbox.get_or_insert(t);
                }
            }
        }
        if first_tsla.is_some() {
            break;
        }
    }

    first_tsla.unwrap_or_else(|| {
        panic!("house 1 never built TSLA within {MAX_TICKS} ticks despite ATEK being pre-granted")
    });
    assert!(
        first_pbox.is_none(),
        "house 1 built PBOX (tick {first_pbox:?}) before/instead of the stronger \
         already-buildable TSLA — the reverse-catalog-order 'prefer strongest' logic in \
         next_structure (ai.rs:280-287) is not behaving as documented"
    );
}

// ---------------------------------------------------------------------------
// 9. Offensive-infantry filter (M7.7 chunk C): the barracks lane must only
//    ever queue infantry with a positive-damage weapon (E1 here). The medic
//    (heal weapon, non-positive damage) and the unarmed engineer must never
//    be self-built by the skirmish AI, across several seeds.
// ---------------------------------------------------------------------------

/// The sequence of infantry-proto ids house 1's barracks *starts* producing.
fn infantry_production_sequence(seed: u32, max_ticks: u32, want: usize) -> Vec<u32> {
    let mut w = full_roster_skirmish(seed, Difficulty::Normal);
    let mut seq = Vec::new();
    let mut prev: Option<u32> = None;
    for _ in 0..max_ticks {
        w.tick(&[]);
        let cur = w
            .house(1)
            .and_then(|hs| hs.infantry_prod)
            .and_then(|p| match p.item {
                BuildItem::Unit(id) => Some(id),
                BuildItem::Building(_) => None,
            });
        if let Some(id) = cur {
            if cur != prev {
                seq.push(id);
                if seq.len() >= want {
                    break;
                }
            }
        }
        prev = cur;
    }
    seq
}

#[test]
fn ai_never_builds_non_offensive_infantry() {
    const MAX_TICKS: u32 = 6000;
    const WANT: usize = 6;
    let mut any_nonempty = false;
    for seed in [0x1EE1_0001u32, 0x1EE1_0002, 0x1EE1_0003, 0x1EE1_0004] {
        let seq = infantry_production_sequence(seed, MAX_TICKS, WANT);
        if !seq.is_empty() {
            any_nonempty = true;
        }
        for id in &seq {
            assert_eq!(
                *id, FR_U_E1,
                "seed {seed:#x}: AI queued infantry id {id} (expected only E1={FR_U_E1}); \
                 MEDIC={FR_U_MEDIC} and ENGINEER={FR_U_ENGINEER} must be filtered out by the \
                 offensive-infantry filter (ai.rs produce_units, `w.damage > 0`), sequence={seq:?}"
            );
        }
    }
    assert!(
        any_nonempty,
        "no seed among the tried set ever queued any infantry within {MAX_TICKS} ticks — \
         weakens this test to a tautology; widen the seed set or tick budget"
    );
}

// ---------------------------------------------------------------------------
// 10. "AI radar-awareness" finding: despite the M7.7-C commit message
//    ("AI radar + offensive-infantry filter"), reading `ai.rs` end to end
//    shows the ONLY radar-related AI change is that `next_structure` now
//    builds a DOME building (tested above) — there is no shroud/visibility
//    gate anywhere in `launch_attack`/`nearest_enemy_target`, and
//    `World::apply`'s `Command::Attack` handling (`world.rs:840-872`) has no
//    shroud check either. This test pins that the AI attacks and lands hits
//    on a totally unexplored (never revealed to it) enemy base once a
//    shroud is enabled, proving the AI's targeting is fully omniscient
//    regardless of whether it has actually "seen" the enemy via radar or
//    scouting. This does not contradict DESIGN.md §3.10 ("difficulty is
//    stat handicap, not information cheating" — a statement about
//    difficulty tiers, not about baseline omniscience) but it does mean the
//    commit's "AI radar-awareness" description is misleading: nothing in the
//    AI actually *reads* shroud state. Flagged for ra-coder to confirm
//    intent; not weakened to hide it either way.
// ---------------------------------------------------------------------------

#[test]
fn ai_attacks_the_enemy_base_even_when_it_is_never_shroud_revealed() {
    const MAX_TICKS: u32 = 3000;
    let mut w = full_roster_skirmish(0x5AD0_5AD0, Difficulty::Hard);
    w.enable_shroud();

    let mut incursion_tick: Option<u32> = None;
    for t in 0..MAX_TICKS {
        w.tick(&[]);
        // House 1 must never have explored house 2's home cell (no scout
        // ever got there, no building of house 1's ever had it in sight
        // range) for this to prove omniscient targeting rather than "the AI
        // happened to see it first."
        assert!(
            !w.shroud.is_explored(1, home2()),
            "tick {t}: house 1 explored house 2's home cell — this test's premise (never \
             revealed) no longer holds, rerun with a setup that keeps it shrouded"
        );
        if incursion_tick.is_none() {
            for (_, u) in w.units.iter() {
                if u.house == 1 {
                    let dx = (u.cell().x - home2().x) as i64;
                    let dy = (u.cell().y - home2().y) as i64;
                    if dx * dx + dy * dy <= INCURSION_RADIUS_SQ {
                        incursion_tick = Some(t);
                        break;
                    }
                }
            }
        }
        if incursion_tick.is_some() {
            break;
        }
    }
    incursion_tick.unwrap_or_else(|| {
        panic!(
            "house 1 never reached house 2's (permanently shrouded-to-it) base within \
             {MAX_TICKS} ticks — either the omniscient-targeting premise is wrong or the \
             tick budget is too small"
        )
    });
}

// ---------------------------------------------------------------------------
// M7.10 showcase: log one composed-team lifecycle (composition → staging →
// attacking → dissolve) and one economic-reflex event (building repair). Runs
// a real Hard skirmish; the AI drives everything through the normal command
// pipeline. Run with `--nocapture` to read the narrative log.
// ---------------------------------------------------------------------------
#[test]
fn showcase_composed_team_lifecycle_and_repair() {
    let mut w = skirmish(0x5C0E_7A10, Difficulty::Hard);
    let mut logged_form = false;
    let mut logged_attack = false;
    let mut logged_dissolve = false;
    let mut logged_repair = false;
    let mut prev: Option<(usize, usize, bool, bool)> = None;

    for t in 0..16000u32 {
        w.tick(&[]);

        // --- Composed-team lifecycle (house 1's AI) ---
        let (enemy, summary, caps) = {
            let ai = w.ai().iter().find(|a| a.house() == 1).unwrap();
            (ai.enemy(), ai.team_summary(), ai.caps())
        };
        match (prev, summary) {
            (None, Some((n, init, staging, harass))) => {
                eprintln!(
                    "[t={t}] TEAM FORMED: {n} members (init {init}), phase={}, harass={harass}, \
                     enemy=house {:?}, caps(units={},bldgs={})",
                    if staging { "Staging" } else { "Attacking" },
                    enemy,
                    caps.0,
                    caps.1
                );
                logged_form = true;
            }
            (Some((_, _, true, _)), Some((n, _, false, _))) => {
                eprintln!("[t={t}] TEAM ATTACKING: {n} members committed to the objective");
                logged_attack = true;
            }
            (Some(_), None) => {
                eprintln!("[t={t}] TEAM DISSOLVED (wiped out or decimated → survivors retreat)");
                logged_dissolve = true;
            }
            _ => {}
        }
        prev = summary;

        // Force a decimation once the team is attacking, to exercise the
        // dissolve/retreat path (survivors below half the starting size fall back
        // to base). Remove all-but-one attacking member in one blow.
        if logged_attack && !logged_dissolve {
            let attackers: Vec<_> = w
                .units
                .iter()
                .filter(|(_, u)| u.house == 1 && u.target.is_some())
                .map(|(h, _)| h)
                .collect();
            if attackers.len() >= 2 {
                for &h in attackers.iter().take(attackers.len() - 1) {
                    w.units.remove(h);
                }
                eprintln!(
                    "[t={t}] (injected) wiped {} of {} attacking members to trigger a retreat",
                    attackers.len() - 1,
                    attackers.len()
                );
            }
        }

        // --- Economic reflex: damage a building, watch the AI repair it ---
        if t == 400 {
            w.set_house_credits(1, 20000); // ensure it can afford repairs
            let victim = w
                .buildings
                .iter()
                .find(|(_, b)| b.house == 1 && b.is_alive() && !b.is_wall)
                .map(|(h, _)| h);
            if let Some(h) = victim {
                let mx = w.buildings.get(h).unwrap().max_health;
                w.buildings.get_mut(h).unwrap().health = mx / 3;
                eprintln!("[t={t}] (injected) damaged a house-1 building to 1/3 strength");
            }
        }
        if !logged_repair {
            if let Some((_, b)) = w
                .buildings
                .iter()
                .find(|(_, b)| b.house == 1 && b.is_repairing)
            {
                eprintln!(
                    "[t={t}] ECONOMIC REFLEX: AI toggled repair — building healing ({}/{} hp)",
                    b.health, b.max_health
                );
                logged_repair = true;
            }
        }

        if logged_form && logged_attack && logged_dissolve && logged_repair {
            break;
        }
    }

    assert!(logged_form, "the AI should form at least one composed team");
    assert!(
        logged_attack,
        "a formed team should reach the Attacking phase"
    );
    assert!(
        logged_dissolve,
        "a decimated team should dissolve (survivors retreat)"
    );
    assert!(
        logged_repair,
        "the AI should repair the damaged building (economic reflex)"
    );
}
