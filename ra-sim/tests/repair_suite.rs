//! Audit coverage (ra-tester, post-M7.9 P1): **building self-repair** depth —
//! full-cycle economics (damaged → full, exact credits charged at every step),
//! the insolvency stop/resume cycle, and the destroyed-mid-repair edge case.
//! Drives everything through the public `World`/`Command` API, sim-level (the
//! UI-facing sell/repair-mode monkey coverage lives in
//! `ra-client/tests/ui_sell_repair.rs`).
//!
//! **M7.9.1 audit fix.** The formula constants pinned here are the *real*
//! `redalert.mix` rules.ini overrides — `RepairStep=7`, `RepairPercent=20%`
//! (`= 1/5`) — not the reference's compile-time defaults (`RepairStep(5)`,
//! `RepairPercent(1,4)`, `rules.cpp:221-222`) the M7.9 landing had pinned by
//! mistake (same category of bug as the P0 `BuildSpeedBias` miss). See
//! `ra-sim/src/world.rs`'s `BREPAIR_STEP`/`BREPAIR_PERCENT_NUM`/`_DEN` and
//! QUIRKS Q14's audit correction note.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, MoveStats, Passability, WarheadProfile,
    WeaponProfile, World,
};

const B_WEAP: u32 = 0; // war factory, cost 2000, max_health 500

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn catalog() -> Catalog {
    let bproto = |name: &str, cost: i32, cy: bool, wf: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 3,
        foot_h: 3,
        max_health: 500,
        armor: 0,
        power: 0,
        cost,
        prereq: vec![],
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
        buildings: vec![bproto("WEAP", 2000, false, true)],
        units: vec![],
        econ: EconRules::default(),
    }
}

fn world(credits: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0xBEEF_5EED);
    w.set_catalog(catalog());
    w.init_houses(2, credits);
    w
}

/// Ticks between repair steps (`REPAIR_INTERVAL`, world.rs — kept in sync
/// here for the hand-computed step counts below; a change to that constant
/// would need this comment, not this value, updated).
const REPAIR_INTERVAL: u32 = 15;

// ===========================================================================
// 1. Full repair cycle: damaged -> full, exact total credits charged.
//
// WEAP: cost=2000, max_health=500. Repair_Cost per step (techno.cpp:6907):
//   denom = max_health / RepairStep = 500 / 7 = 71
//   step_cost = (Cost / denom) * RepairPercent = (2000/71) * 1/5 = 28 * 1/5 = 5
// Healing 7 hp/step from 100 -> 500 needs ceil(400/7) = 58 steps (57 steps
// reaches 499; the 58th clamps to exactly 500 and auto-stops).
//   total credits = 58 * 5 = 290
// ===========================================================================

/// M7.5 P0: the four repair magnitudes are no longer world.rs module constants —
/// they live in `EconRules` (loaded from rules.ini in the real client). This guard
/// pins the *source of truth* these hand-computed expectations depend on, so a
/// future edit to the defaults (the exact drift the promotion guards against) trips
/// here instead of silently shifting the numbers below.
#[test]
fn repair_constants_come_from_econrules_not_a_module_const() {
    let e = EconRules::default();
    assert_eq!(e.brepair_step, 7, "RepairStep (stock rules.ini)");
    assert_eq!(
        (e.brepair_percent_num, e.brepair_percent_den),
        (1, 5),
        "RepairPercent 20%"
    );
    assert_eq!(e.urepair_step, 10, "URepairStep (stock rules.ini)");
    assert_eq!(
        (e.urepair_percent_num, e.urepair_percent_den),
        (20, 100),
        "URepairPercent 20%"
    );
}

#[test]
fn repair_full_cycle_heals_to_max_at_exact_hand_computed_cost() {
    let mut w = world(10_000);
    let b = w.spawn_building(B_WEAP, 1, CellCoord::new(10, 10)).unwrap();
    w.buildings.get_mut(b).unwrap().health = 100;
    let credits_before = w.house_credits(1);

    w.tick(&[Command::Repair {
        house: 1,
        building: b,
    }]);
    assert!(w.buildings.get(b).unwrap().is_repairing);

    // 58 steps * REPAIR_INTERVAL ticks/step, plus slack.
    let budget = 58 * REPAIR_INTERVAL + REPAIR_INTERVAL;
    let mut steps_seen = 0u32;
    let mut last_health = 100u16;
    for _ in 0..budget {
        w.tick(&[]);
        let now = w.buildings.get(b).unwrap().health;
        if now != last_health {
            steps_seen += 1;
            last_health = now;
        }
        if !w.buildings.get(b).unwrap().is_repairing {
            break;
        }
    }

    let b_ref = w.buildings.get(b).unwrap();
    assert_eq!(
        b_ref.health, 500,
        "must heal to exactly full health, no overshoot"
    );
    assert!(
        !b_ref.is_repairing,
        "must auto-stop repairing at full health"
    );
    assert_eq!(steps_seen, 58, "58 × 7hp steps to close a 400hp gap");

    let spent = credits_before - w.house_credits(1);
    assert_eq!(
        spent, 290,
        "total repair cost must equal exactly 58 steps × 5 credits (the cited \
         Repair_Cost formula, real rules.ini RepairStep=7/RepairPercent=20%)"
    );
}

// ===========================================================================
// 2. Insolvency mid-repair: stalls (toggle clears, no negative credits) when
//    the house can't afford the next step, then resumes once refunded and
//    re-armed.
// ===========================================================================

#[test]
fn repair_stops_at_insolvency_and_resumes_once_funded() {
    // Exactly 10 steps' worth (10 × 5 = 50) plus 2 leftover — the 11th step
    // (needs 5) must fail and clear the toggle, not go negative.
    let mut w = world(52);
    let b = w.spawn_building(B_WEAP, 1, CellCoord::new(10, 10)).unwrap();
    w.buildings.get_mut(b).unwrap().health = 100;

    w.tick(&[Command::Repair {
        house: 1,
        building: b,
    }]);
    for _ in 0..(20 * REPAIR_INTERVAL) {
        w.tick(&[]);
        assert!(w.house_credits(1) >= 0, "credits must never go negative");
        if !w.buildings.get(b).unwrap().is_repairing {
            break;
        }
    }
    assert!(
        !w.buildings.get(b).unwrap().is_repairing,
        "repair must stall (auto-off) once the house can't afford a step"
    );
    assert_eq!(
        w.buildings.get(b).unwrap().health,
        100 + 10 * 7,
        "exactly 10 affordable steps must have landed before the 11th stalled"
    );
    assert_eq!(
        w.house_credits(1),
        2,
        "the leftover 2 credits must be untouched"
    );

    // Fund the house and re-arm repair: it must resume from where it left off.
    w.set_house_credits(1, 10_000);
    w.tick(&[Command::Repair {
        house: 1,
        building: b,
    }]);
    assert!(
        w.buildings.get(b).unwrap().is_repairing,
        "re-issuing Command::Repair after a stall must re-arm it"
    );
    for _ in 0..(60 * REPAIR_INTERVAL) {
        w.tick(&[]);
        if !w.buildings.get(b).unwrap().is_repairing {
            break;
        }
    }
    assert_eq!(
        w.buildings.get(b).unwrap().health,
        500,
        "once funded again, repair must resume and finish healing to full"
    );
}

// ===========================================================================
// 3. Sell a building while it is mid-repair: clean removal, refund based on
//    (post-repair) cost, no dangling repair state, no panic on later ticks.
// ===========================================================================

#[test]
fn sell_mid_repair_removes_the_building_cleanly() {
    let mut w = world(10_000);
    let b = w.spawn_building(B_WEAP, 1, CellCoord::new(10, 10)).unwrap();
    w.buildings.get_mut(b).unwrap().health = 100;
    w.tick(&[Command::Repair {
        house: 1,
        building: b,
    }]);
    // Let a few repair steps land first.
    for _ in 0..(5 * REPAIR_INTERVAL) {
        w.tick(&[]);
    }
    assert!(w.buildings.get(b).unwrap().is_repairing);
    let credits_before_sell = w.house_credits(1);

    w.tick(&[Command::Sell {
        house: 1,
        building: b,
    }]);
    assert!(
        w.buildings.get(b).is_none(),
        "the building must be gone immediately after selling"
    );
    let refund_pct = w.catalog.econ.refund_percent;
    let expected_refund = 2000 * refund_pct / 100;
    assert_eq!(
        w.house_credits(1),
        credits_before_sell + expected_refund,
        "refund is RefundPercent × the (unbiased) build cost, unaffected by \
         being mid-repair"
    );

    // No dangling repair state: many more ticks must not panic, and the house
    // must not be charged again for a building that no longer exists.
    let credits_after_sell = w.house_credits(1);
    for _ in 0..(20 * REPAIR_INTERVAL) {
        w.tick(&[]);
    }
    assert_eq!(
        w.house_credits(1),
        credits_after_sell,
        "no further repair charges once the building is sold"
    );
}

// ===========================================================================
// 4. A building destroyed by real combat while mid-repair: no panic, clean
//    removal (no dangling `is_repairing` state), and the sim keeps ticking.
// ===========================================================================

fn lethal_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 10_000, // one-shot kill against a 500-hp building
        rof: 30,
        range: 5 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 999, // collapse distance falloff, see handicap_suite.rs
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1_000_000,
    }
}

#[test]
fn destroyed_mid_repair_by_combat_is_removed_cleanly_no_panic() {
    let mut w = world(10_000);
    let b = w.spawn_building(B_WEAP, 1, CellCoord::new(10, 10)).unwrap();
    w.buildings.get_mut(b).unwrap().health = 100;
    w.tick(&[Command::Repair {
        house: 1,
        building: b,
    }]);
    for _ in 0..(3 * REPAIR_INTERVAL) {
        w.tick(&[]);
    }
    assert!(w.buildings.get(b).unwrap().is_repairing);
    assert!(
        w.buildings.get(b).unwrap().health > 100,
        "should have healed a bit already"
    );

    // An enemy one-shots it.
    let attacker = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(attacker, 0, Some(lethal_weapon()), false);
    w.tick(&[Command::Attack {
        unit: attacker,
        target: ra_sim::Target::Building(b),
        house: 2,
    }]);

    let mut destroyed = false;
    for t in 0..200 {
        w.tick(&[]);
        if w.buildings.get(b).is_none() {
            destroyed = true;
            eprintln!("building destroyed at tick {t}");
            break;
        }
    }
    assert!(
        destroyed,
        "the building should have been destroyed within 200 ticks"
    );
    assert!(
        !w.house(1).unwrap().owns_building(B_WEAP),
        "the house's building-ownership bookkeeping must reflect the destruction"
    );

    // No dangling repair state: the sim must keep ticking cleanly (no panic),
    // and no further repair charge is possible for a handle that no longer
    // resolves.
    let credits_after_destruction = w.house_credits(1);
    for _ in 0..(10 * REPAIR_INTERVAL) {
        w.tick(&[]);
    }
    assert_eq!(
        w.house_credits(1),
        credits_after_destruction,
        "no repair charge can land against a destroyed building"
    );
}
