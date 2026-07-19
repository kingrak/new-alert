//! M7.9 P0 — build-time fidelity audit.
//!
//! Player report: "builds feel too slow." The audit compared our
//! `Catalog::time_to_build` port against the reference `TechnoTypeClass::
//! Time_To_Build` (`techno.cpp:6777`) + `FactoryClass::Start` (`factory.cpp:432`)
//! and found **two missing pieces**, both of which slowed us down:
//!
//! 1. **`Rule.BuildSpeedBias`** (`[General] BuildSpeed`, rules.cpp:464) — stock
//!    RA ships `.8`. We were dropping it entirely, so every item took `1/0.8 =
//!    1.25×` too long.
//! 2. The **STEP_COUNT rate conversion** — `FactoryClass::Start` does
//!    `rate = Bound(T / STEP_COUNT, 1, 255)` and the factory then advances one of
//!    `STEP_COUNT (= 54)` stages every `rate` ticks, so the real build time is
//!    `rate × 54`, which truncates `T` down to a multiple of 54 (and floors any
//!    trivially-cheap item to 54 ticks).
//!
//! ### Derivation (full power, Normal-difficulty human, single factory)
//!
//! ```text
//! T   = round( round(Cost × 0.8) × (900/1000) )      // techno.cpp:6777, .8 bias
//! rate = Bound(T / 54, 1, 255)                        // factory.cpp:439
//! build = rate × 54  ticks   (÷ 15 Hz = seconds)      // factory.cpp:440 + StageClass
//! ```
//!
//! | item        | Cost | T    | rate | build ticks | seconds | BEFORE (buggy) |
//! |-------------|------|------|------|-------------|---------|----------------|
//! | POWR        |  300 |  216 |   4  |     **216** | 14.4 s  | 270  (18.0 s)  |
//! | WEAP        | 2000 | 1440 |  26  |    **1404** | 93.6 s  | 1800 (120.0 s) |
//! | 2TNK (unit) |  800 |  576 |  10  |     **540** | 36.0 s  | 720  (48.0 s)  |
//!
//! Units use the **same formula family** as buildings: with the stock
//! `UnitBuildPenalty = 100` (`fixed(100,100) = 1`, globals.cpp:669) the unit
//! branch of `Time_To_Build` collapses to the building branch, so 2TNK follows
//! the identical `Cost × 0.8 × 0.9` path (verified below).

use ra_sim::coords::CellCoord;
use ra_sim::{
    AiPlayer, BuildItem, BuildingProto, Catalog, Command, Difficulty, EconRules, Handicap,
    MoveStats, Passability, UnitProto, World,
};

// Building type ids.
const B_FACT: u32 = 0; // construction yard, cost 100
const B_POWR: u32 = 1; // power plant, cost 300, prereq FACT
const B_WEAP: u32 = 2; // war factory, cost 2000, prereq POWR
const B_SILO: u32 = 3; // ore silo, cost 150 (real rules.ini SILO cost), prereq FACT
                       // Unit-proto id.
const U_2TNK: u32 = 0; // war-factory product, cost 800, prereq WEAP

/// `.8` parsed to raw 16.16 (`0.8 × 65536 = 52428`, matching the reference
/// `fixed(".8")`), so this fixture's `Catalog::time_to_build` reproduces the
/// stock-asset build times exactly.
const BUILD_SPEED_BIAS_08: i32 = 52428;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn catalog() -> Catalog {
    let bproto =
        |name: &str, cost: i32, power: i32, prereq: Vec<u32>, cy: bool, wf: bool| BuildingProto {
            is_barracks: false,
            name: name.to_string(),
            foot_w: 2,
            foot_h: 2,
            max_health: 500,
            armor: 0,
            power,
            cost,
            prereq,
            is_refinery: false,
            is_construction_yard: cy,
            is_war_factory: wf,
            free_harvester_unit: None,
            sight: 4,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        };
    Catalog {
        buildings: vec![
            // Power values are all >= 0 (no drain), so the house is always at
            // full power and `build_time_scale()` snapshots (1, 1) — the
            // full-power case the P0 numbers are derived for.
            bproto("FACT", 100, 0, vec![], true, false),
            bproto("POWR", 300, 100, vec![B_FACT], false, false),
            bproto("WEAP", 2000, 0, vec![B_POWR], false, true),
            bproto("SILO", 150, 0, vec![B_FACT], false, false),
        ],
        units: vec![UnitProto {
            is_infantry: false,
            locomotor: 1,
            name: "2TNK".to_string(),
            sprite_id: 99,
            max_health: 400,
            stats: stats(),
            armor: 0,
            weapon: None,
            secondary: None,
            has_turret: false,
            is_harvester: false,
            deploys_to: None,
            cost: 800,
            prereq: vec![B_WEAP],
            sight: 2,
            passengers: 0,
        }],
        econ: EconRules {
            build_speed_bias_raw: BUILD_SPEED_BIAS_08,
            ..EconRules::default()
        },
    }
}

/// A world with house 1 already owning FACT + POWR + WEAP (prereqs satisfied,
/// full power) and a big pile of credits so every installment is affordable.
fn world() -> World {
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    w.set_catalog(catalog());
    w.init_houses(2, 1_000_000);
    w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();
    w.spawn_building(B_WEAP, 1, CellCoord::new(30, 10)).unwrap();
    w
}

// ===========================================================================
// 1. Pure formula pins (Catalog::time_to_build), with the derivation above.
// ===========================================================================

#[test]
fn formula_matches_reference_full_power() {
    let c = catalog();
    // Full power => scale (1, 1).
    assert_eq!(c.time_to_build(300, 1, 1), 216, "POWR ($300) build ticks");
    assert_eq!(
        c.time_to_build(2000, 1, 1),
        1404,
        "WEAP ($2000) build ticks"
    );
    assert_eq!(c.time_to_build(800, 1, 1), 540, "2TNK ($800) build ticks");

    // Raw T (pre-STEP_COUNT), the `Cost × 0.8 × 0.9` intermediate.
    assert_eq!(c.build_time_base(300), 216);
    assert_eq!(c.build_time_base(2000), 1440);
    assert_eq!(c.build_time_base(800), 576);
}

#[test]
fn low_power_snapshot_scales_before_step_count() {
    let c = catalog();
    // ×4 at zero power (build_time_scale => (4,1)): 576 × 4 = 2304, /54 = 42,
    // rate 42 × 54 = 2268. (techno.cpp:6832 scales T, THEN factory.cpp divides.)
    assert_eq!(c.time_to_build(800, 4, 1), 2268);
    // ×1.5 at <full power (3,2): round(576 × 1.5) = 864, /54 = 16, 16 × 54 = 864.
    assert_eq!(c.time_to_build(800, 3, 2), 864);
}

#[test]
fn cheap_items_floor_to_one_step_count() {
    let c = catalog();
    // A trivially cheap item still costs a full STEP_COUNT (54 ticks): rate
    // floors to 1 (factory.cpp:439 Bound(.., 1, 255)).
    assert_eq!(c.time_to_build(10, 1, 1), 54);
    assert_eq!(c.time_to_build(1, 1, 1), 54);
}

// ===========================================================================
// 2. Headless end-to-end measurement: drive the real sim to completion and
//    count ticks. Proves the pinned numbers are what a player actually waits.
// ===========================================================================

/// Start producing `item` and tick the sim (empty commands) until the lane
/// completes, returning the number of `tick` calls it took (inclusive of the
/// StartProduction tick, on which `run_production` already advances one step).
fn measure_build_ticks(w: &mut World, item: BuildItem, is_unit: bool) -> u32 {
    w.tick(&[Command::StartProduction { house: 1, item }]);
    let mut ticks = 1u32;
    loop {
        let done = if is_unit {
            w.house(1).unwrap().unit_prod.is_none()
        } else {
            // Building lands in `ready_building` on completion.
            w.house(1).unwrap().ready_building.is_some()
        };
        if done {
            break;
        }
        w.tick(&[]);
        ticks += 1;
        assert!(ticks < 100_000, "build never completed (stuck lane?)");
    }
    ticks
}

#[test]
fn measured_powr_build_takes_216_ticks() {
    let mut w = world();
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Building(B_POWR), false),
        216,
        "POWR wall-clock = 216 ticks / 14.4 s (was 270 / 18.0 s)"
    );
}

#[test]
fn measured_weap_build_takes_1404_ticks() {
    let mut w = world();
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Building(B_WEAP), false),
        1404,
        "WEAP wall-clock = 1404 ticks / 93.6 s (was 1800 / 120.0 s)"
    );
}

#[test]
fn measured_2tnk_build_takes_540_ticks() {
    let mut w = world();
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Unit(U_2TNK), true),
        540,
        "2TNK wall-clock = 540 ticks / 36.0 s (was 720 / 48.0 s)"
    );
}

// ===========================================================================
// 3. Same-script-twice determinism (hash equality).
// ===========================================================================

#[test]
fn build_measurement_is_deterministic() {
    let run = || {
        let mut w = world();
        let t = measure_build_ticks(&mut w, BuildItem::Unit(U_2TNK), true);
        (t, w.state_hash())
    };
    assert_eq!(run(), run(), "same script twice => identical ticks + hash");
}

// ===========================================================================
// 4. Independent re-derivation of the `.8`-BuildSpeed formula for a *different*
//    real-asset item (ra-tester audit, not part of the original M7.9 landing).
//
//    SILO (ore silo) costs **$150** in the real `redalert.mix` rules.ini
//    (`[SILO] Cost=150`, confirmed by extracting the actual asset — ground
//    truth, not the brief). Hand derivation, independent of the POWR/WEAP/2TNK
//    table above:
//
//    ```text
//    T    = round( round(150 × 0.8) × (900/1000) )
//         = round( 120 × 0.9 )
//         = round(108) = 108
//    rate = Bound(108 / 54, 1, 255) = 2
//    build = 2 × 54 = 108 ticks = 7.2 s
//    ```
// ===========================================================================

#[test]
fn silo_150_derivation_independently_matches_hand_computation() {
    let c = catalog();
    // Raw T (pre-STEP_COUNT): round(round(150 × 0.8) × 0.9) = round(120 × 0.9) = 108.
    assert_eq!(c.build_time_base(150), 108, "SILO ($150) raw T");
    // STEP_COUNT quantised: rate = Bound(108/54, 1, 255) = 2 => 2 × 54 = 108.
    assert_eq!(
        c.time_to_build(150, 1, 1),
        108,
        "SILO ($150) build ticks = 7.2 s"
    );
}

#[test]
fn measured_silo_build_takes_108_ticks() {
    let mut w = world();
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Building(B_SILO), false),
        108,
        "SILO wall-clock = 108 ticks / 7.2 s, independently re-derived from a \
         different real-asset cost ($150) than the POWR/WEAP/2TNK table"
    );
}

// ===========================================================================
// 5. End-to-end measurement at the full ×1.5 / ×2.5 / ×4 low-power multiplier
//    ladder (`House::build_time_scale`), driving the real sim to completion
//    (not just the pure-formula check `low_power_snapshot_scales_before_step_
//    count` above). `power_fraction()` reads `House::power_output`/`power_
//    drain` directly, so setting them on the fixture reproduces exactly the
//    same low-power classification the real economy would after taking drain
//    damage — the production code path is identical either way.
// ===========================================================================

/// `world()` with house 1's power fields overridden to `(output, drain)`
/// **before** production starts, so `build_time_scale()` snapshots the
/// corresponding discrete multiplier (techno.cpp:6819-6831):
///   `output >= drain` (or `drain == 0`) => ×1 (full)
///   `output == 0`                       => ×4
///   `output×2 < drain`                  => ×2.5
///   otherwise (`0 < output < drain`)    => ×1.5
fn world_with_power(output: i32, drain: i32) -> World {
    let mut w = world();
    w.houses[1].power_output = output;
    w.houses[1].power_drain = drain;
    w
}

#[test]
fn measured_2tnk_build_at_quarter_power_takes_2268_ticks() {
    // output = 0 => ×4. scaled = 576×4 = 2304, rate = Bound(2304/54,1,255) = 42,
    // build = 42×54 = 2268 (matches the pure-formula pin above).
    let mut w = world_with_power(0, 10);
    assert!(w.house(1).unwrap().low_power());
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Unit(U_2TNK), true),
        2268,
        "2TNK at ×4 (zero power): 2268 ticks / 151.2 s"
    );
}

#[test]
fn measured_2tnk_build_at_third_power_takes_2_5x_ticks() {
    // output=1, drain=3: output×2=2 < 3 => ×2.5. scaled = round(576×5/2) = 1440,
    // rate = Bound(1440/54,1,255) = 26, build = 26×54 = 1404.
    let mut w = world_with_power(1, 3);
    assert!(w.house(1).unwrap().low_power());
    assert_eq!(w.house(1).unwrap().build_time_scale(), (5, 2));
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Unit(U_2TNK), true),
        1404,
        "2TNK at ×2.5 (output < half drain): 1404 ticks / 93.6 s"
    );
}

#[test]
fn measured_2tnk_build_at_two_thirds_power_takes_1_5x_ticks() {
    // output=2, drain=3: output×2=4, not < 3 => ×1.5 (matches the pure-formula
    // pin: round(576×1.5)=864, rate=Bound(864/54,1,255)=16, build=864).
    let mut w = world_with_power(2, 3);
    assert!(w.house(1).unwrap().low_power());
    assert_eq!(w.house(1).unwrap().build_time_scale(), (3, 2));
    assert_eq!(
        measure_build_ticks(&mut w, BuildItem::Unit(U_2TNK), true),
        864,
        "2TNK at ×1.5 (partial power): 864 ticks / 57.6 s"
    );
}

#[test]
fn power_ladder_is_monotonic_end_to_end() {
    // Sanity: the four multipliers order strictly (full < 1.5x < 2.5x < 4x),
    // measured end to end, not just as pure-formula ratios.
    let full = measure_build_ticks(&mut world(), BuildItem::Unit(U_2TNK), true);
    let x15 = measure_build_ticks(&mut world_with_power(2, 3), BuildItem::Unit(U_2TNK), true);
    let x25 = measure_build_ticks(&mut world_with_power(1, 3), BuildItem::Unit(U_2TNK), true);
    let x4 = measure_build_ticks(&mut world_with_power(0, 10), BuildItem::Unit(U_2TNK), true);
    assert!(full < x15, "full ({full}) should beat ×1.5 ({x15})");
    assert!(x15 < x25, "×1.5 ({x15}) should beat ×2.5 ({x25})");
    assert!(x25 < x4, "×2.5 ({x25}) should beat ×4 ({x4})");
    assert_eq!((full, x15, x25, x4), (540, 864, 1404, 2268));
}

// ===========================================================================
// 6. Difficulty `BuildTime`/`Cost` handicap (M7.9 P2a): applied to an
//    AI-controlled house, and verified **absent** on a human house at Normal
//    — measured on the same item, same tick, same catalog. Raw 16.16 values
//    below are the real `redalert.mix` rules.ini `[Easy]` section (`BuildTime=
//    .8`, `Cost=.8`), which our label→section inversion (QUIRKS Q15) assigns
//    to a **Hard** AI (the "strong" opponent).
// ===========================================================================

/// `.8` in raw 16.16 (`round(0.8 × 65536)`, truncated like the reference's own
/// `fixed` parse — same constant as [`BUILD_SPEED_BIAS_08`] since it is the
/// same real rules.ini value).
const HANDICAP_08: i32 = BUILD_SPEED_BIAS_08;

fn catalog_with_hard_handicap() -> Catalog {
    let hard = Handicap {
        build_time: HANDICAP_08,
        cost: HANDICAP_08,
        ..Handicap::default()
    };
    Catalog {
        econ: EconRules {
            difficulty: [Handicap::default(), Handicap::default(), hard],
            ..catalog().econ
        },
        ..catalog()
    }
}

/// Two houses on the identical catalog/fixture: house 1 carries the `Hard`
/// difficulty's `.8` BuildTime/Cost handicap (assigned exactly the way
/// `World::set_ai` assigns it — `house.handicap = catalog.difficulty_handicap
/// (Difficulty::Hard)` — see `hard_ai_handicap_is_applied_and_neutral_human_is_
/// exact` above, which pins that `set_ai` really does this); house 2 is left
/// as a human on the implicit neutral (Normal) handicap. Both own FACT/POWR/
/// WEAP and start with equal credits. Deliberately **not** wired through
/// `World::set_ai` (which would also attach a live `AiPlayer` that issues its
/// *own* production/attack/repair commands every tick) — this measurement
/// wants only the handicap's effect on a single, manually-issued production
/// order, isolated from the AI's autonomous decisions (covered separately by
/// `ai_suite.rs` / `ui_ai_vs_ai.rs`).
fn world_ai_vs_human() -> World {
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    w.set_catalog(catalog_with_hard_handicap());
    w.init_houses(3, 1_000_000);
    for house in [1u8, 2u8] {
        let (fx, wx) = if house == 1 { (10, 30) } else { (60, 80) };
        w.spawn_building(B_FACT, house, CellCoord::new(fx, 10))
            .unwrap();
        w.spawn_building(B_POWR, house, CellCoord::new(fx + 5, 10))
            .unwrap();
        w.spawn_building(B_WEAP, house, CellCoord::new(wx, 10))
            .unwrap();
    }
    w.houses[1].handicap = w.catalog.difficulty_handicap(Difficulty::Hard);
    w
}

#[test]
fn set_ai_assigns_the_catalog_handicap_and_leaves_the_human_neutral() {
    // The real wiring path (`World::set_ai`): confirms `set_ai` really does copy
    // `catalog.difficulty_handicap(Hard)` onto the AI house, and that a house
    // with no AI installed (the human) is untouched.
    let mut w = World::new(Passability::all_passable(), 1);
    w.set_catalog(catalog_with_hard_handicap());
    w.init_houses(3, 1_000_000);
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Hard)]);

    let h1 = w.house(1).unwrap();
    assert_eq!(h1.handicap.build_time, HANDICAP_08);
    assert_eq!(h1.handicap.cost, HANDICAP_08);
    assert!(!h1.handicap.is_neutral());
    // House 2 (human, no AI ever assigned) keeps the all-1.0 neutral handicap —
    // the human-on-Normal case must be a byte-exact no-op.
    let h2 = w.house(2).unwrap();
    assert!(
        h2.handicap.is_neutral(),
        "a house with no AI must keep the neutral (Normal) handicap"
    );
}

#[test]
fn build_time_bias_speeds_up_the_ai_and_leaves_the_human_at_baseline() {
    let mut w = world_ai_vs_human();
    // Credits *before* the very first tick (so the total spend at completion
    // is exactly comparable to the snapshotted `Production::cost`).
    let starting_credits = (w.house(1).unwrap().credits, w.house(2).unwrap().credits);
    // Both start producing the identical item (2TNK, $800) on the same tick.
    let hash = w.tick(&[
        Command::StartProduction {
            house: 1,
            item: BuildItem::Unit(U_2TNK),
        },
        Command::StartProduction {
            house: 2,
            item: BuildItem::Unit(U_2TNK),
        },
    ]);
    let _ = hash;

    // Baseline (unbiased, full power): time_to_build(800,1,1) = 540 ticks,
    // matching `measured_2tnk_build_takes_540_ticks` above.
    let human_total = w.house(2).unwrap().unit_prod.unwrap().total_ticks;
    assert_eq!(
        human_total, 540,
        "human on Normal (neutral handicap) must build at the exact unbiased rate"
    );
    let human_cost = w.house(2).unwrap().unit_prod.unwrap().cost;
    assert_eq!(human_cost, 800, "human is charged the raw, unbiased cost");

    // Hard AI: BuildTime bias (`.8`) is applied to the *already STEP_COUNT-
    // quantised* 540, rounding to nearest 16.16: round(540 × 0.8) = 432.
    // Cost bias scales the credits charged (not the build time, which uses the
    // raw cost per the reference): round(800 × 0.8) = 640.
    let ai_total = w.house(1).unwrap().unit_prod.unwrap().total_ticks;
    assert_eq!(
        ai_total, 432,
        "Hard AI's BuildTime handicap (.8) must shorten the build: \
         round(540 × 0.8) = 432 ticks"
    );
    let ai_cost = w.house(1).unwrap().unit_prod.unwrap().cost;
    assert_eq!(
        ai_cost, 640,
        "Hard AI's Cost handicap (.8) must cheapen the build: round(800 × 0.8) = 640"
    );
    assert!(
        ai_total < human_total,
        "the handicapped AI must build strictly faster than the neutral human"
    );

    // Drive both lanes to completion and confirm the *measured* wall-clock
    // ticks (counting from the StartProduction tick, already ticked once above)
    // and total credits spent match the snapshot exactly (end-to-end, not just
    // the snapshotted total_ticks/cost fields).
    let mut ai_done_at = None;
    let mut human_done_at = None;
    for t in 2..3000u32 {
        w.tick(&[]);
        if ai_done_at.is_none() && w.house(1).unwrap().unit_prod.is_none() {
            ai_done_at = Some(t);
        }
        if human_done_at.is_none() && w.house(2).unwrap().unit_prod.is_none() {
            human_done_at = Some(t);
        }
        if ai_done_at.is_some() && human_done_at.is_some() {
            break;
        }
    }
    assert_eq!(
        ai_done_at,
        Some(432),
        "AI lane wall-clock: completes on the 432nd tick total"
    );
    assert_eq!(
        human_done_at,
        Some(540),
        "human lane wall-clock: completes on the 540th tick total"
    );
    let credits_after = (w.house(1).unwrap().credits, w.house(2).unwrap().credits);
    assert_eq!(
        starting_credits.0 - credits_after.0,
        640,
        "AI must have spent exactly the biased cost (640) in total, start to finish"
    );
    assert_eq!(
        starting_credits.1 - credits_after.1,
        800,
        "human must have spent exactly the raw cost (800) in total, start to finish"
    );
}
