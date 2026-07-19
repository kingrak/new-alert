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
