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
    BuildItem, BuildingProto, Catalog, Command, EconRules, MoveStats, Passability, UnitProto, World,
};

// Building type ids.
const B_FACT: u32 = 0; // construction yard, cost 100
const B_POWR: u32 = 1; // power plant, cost 300, prereq FACT
const B_WEAP: u32 = 2; // war factory, cost 2000, prereq POWR
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
