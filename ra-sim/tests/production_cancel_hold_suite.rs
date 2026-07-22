//! M7.21 — production cancel + hold suite (sim side).
//!
//! The player-reported must-have: production must be cancellable — mid-build
//! (refund exactly the installments spent so far, `FactoryClass::Abandon`,
//! FACTORY.CPP:481: `Refund_Money(money - Balance)`) and after completion
//! (a ready-but-unplaced structure refunds its full cost). Plus the M7.21 P1
//! hold semantics ported from the original sidebar: right-click on an
//! actively-building cameo SUSPENDs (`FactoryClass::Suspend`, FACTORY.CPP:410
//! — `IsSuspended = true`, `Set_Rate(0)`: no stage step, no installment), and
//! a left-click on the suspended cameo resumes via a PRODUCE re-issue
//! (`FactoryClass::Start`, FACTORY.CPP:439).
//!
//! Own minimal fixture catalog (house convention: independent of other test
//! files' fixtures and of `world.rs`'s private test module).

use ra_sim::coords::CellCoord;
use ra_sim::{BuildItem, BuildingProto, Catalog, Command, EconRules, Passability, ProdKind, World};

// Building type ids.
const B_FACT: u32 = 0; // construction yard, 3x3, cost 100
const B_POWR: u32 = 1; // power plant, 2x2, cost 30, prereq FACT
const B_LAB: u32 = 2; // 1x1 filler, cost 80, prereq FACT (restart-after-cancel)

fn catalog() -> Catalog {
    let bproto = |name: &str, w: u8, h: u8, cost: i32, prereq: Vec<u32>, cy: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 400,
        armor: 0,
        power: 0,
        cost,
        prereq,
        is_refinery: false,
        is_construction_yard: cy,
        is_war_factory: false,
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
            bproto("FACT", 3, 3, 100, vec![], true),
            bproto("POWR", 2, 2, 30, vec![B_FACT], false),
            bproto("LAB", 1, 1, 80, vec![B_FACT], false),
        ],
        units: vec![],
        econ: EconRules::default(),
    }
}

/// A world with a construction yard already standing for house 1.
fn world(credits: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0x0521_C0DE);
    w.set_catalog(catalog());
    w.init_houses(2, credits);
    w.spawn_building(B_FACT, 1, CellCoord::new(20, 20)).unwrap();
    w
}

fn start_powr(w: &mut World) {
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_POWR),
    }]);
}

// ===========================================================================
// P3a — mid-build cancel refunds exactly the spent-so-far amount, checked
// against the installment drain schedule every tick before the cancel.
// ===========================================================================

#[test]
fn mid_build_cancel_refunds_exactly_the_drain_schedule() {
    const INITIAL: i32 = 1000;
    let mut w = world(INITIAL);
    start_powr(&mut w);

    // Drive 5 ticks, checking the drain schedule after each: the credits
    // drained so far must equal the lane's `spent`, and `spent` must equal
    // the original's installment formula `cost * progress / total_ticks`
    // (FACTORY.CPP:194-227 — total spend sums to cost exactly).
    for _ in 0..5 {
        w.tick(&[]);
        let p = w.house(1).unwrap().building_prod.expect("lane in progress");
        assert!(!p.done, "test setup: POWR must not finish this early");
        let scheduled = (p.cost as i64 * p.progress as i64 / p.total_ticks as i64) as i32;
        assert_eq!(
            p.spent, scheduled,
            "installments must follow the drain schedule cost*progress/total"
        );
        assert_eq!(
            w.house_credits(1),
            INITIAL - p.spent,
            "credits drained must equal the lane's recorded spend"
        );
    }
    let spent = w.house(1).unwrap().building_prod.unwrap().spent;
    assert!(
        spent > 0,
        "test setup: some installments must have been paid"
    );

    w.tick(&[Command::CancelProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);

    assert!(w.house(1).unwrap().building_prod.is_none(), "lane cleared");
    assert_eq!(
        w.house_credits(1),
        INITIAL,
        "cancel must refund exactly the spent-so-far amount (round trip to the \
         pre-build treasury, `Refund_Money(money - Balance)`)"
    );
}

// ===========================================================================
// P1 — hold freezes progress and spending; a re-issued StartProduction for
// the same item resumes from the frozen stage and completes for exactly
// the full cost.
// ===========================================================================

#[test]
fn hold_freezes_progress_and_spending_and_resume_completes_for_exact_cost() {
    const INITIAL: i32 = 1000;
    let mut w = world(INITIAL);
    start_powr(&mut w);
    for _ in 0..4 {
        w.tick(&[]);
    }
    let before = w.house(1).unwrap().building_prod.unwrap();
    assert!(!before.paused && !before.done);

    // Hold (cameo right-click while `Is_Building()`, SIDEBAR.CPP:2186-2189).
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    let held = w.house(1).unwrap().building_prod.unwrap();
    assert!(held.paused, "lane must report on-hold");

    // 50 ticks on hold: zero progress, zero installments (`Set_Rate(0)`).
    let credits_on_hold = w.house_credits(1);
    for _ in 0..50 {
        w.tick(&[]);
    }
    let frozen = w.house(1).unwrap().building_prod.unwrap();
    assert_eq!(
        frozen.progress, held.progress,
        "no stage step while on hold"
    );
    assert_eq!(frozen.spent, held.spent, "no installment while on hold");
    assert_eq!(
        w.house_credits(1),
        credits_on_hold,
        "treasury untouched while on hold"
    );

    // Resume: re-issuing PRODUCE for the same item (SIDEBAR.CPP:2222-2234 →
    // `FactoryClass::Start`) unpauses the existing lane rather than being
    // rejected as busy, and the build completes.
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_POWR),
    }]);
    let resumed = w.house(1).unwrap().building_prod.unwrap();
    assert!(!resumed.paused, "resume must clear the hold");
    // Commands apply before the production system runs, so the resume tick
    // itself advances one stage: exactly frozen+1, not a restart from 0.
    assert_eq!(
        resumed.progress,
        frozen.progress + 1,
        "resume continues from the frozen stage, not from scratch"
    );

    let mut finished = false;
    for _ in 0..500 {
        w.tick(&[]);
        if w.house(1).unwrap().ready_building == Some(B_POWR) {
            finished = true;
            break;
        }
    }
    assert!(finished, "resumed build should complete");
    assert_eq!(
        w.house_credits(1),
        INITIAL - 30,
        "total spend across hold/resume must still equal the item's exact cost"
    );
}

#[test]
fn hold_then_cancel_refunds_the_spent_so_far() {
    const INITIAL: i32 = 500;
    let mut w = world(INITIAL);
    start_powr(&mut w);
    for _ in 0..6 {
        w.tick(&[]);
    }
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    assert!(w.house(1).unwrap().building_prod.unwrap().paused);

    // The second right-click of the original's two-stage cameo cancel:
    // suspended → ABANDON with refund (SIDEBAR.CPP:2183-2185).
    w.tick(&[Command::CancelProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    assert!(w.house(1).unwrap().building_prod.is_none());
    assert_eq!(
        w.house_credits(1),
        INITIAL,
        "cancelling a held lane refunds everything spent before the hold"
    );
}

// ===========================================================================
// P0 — a completed-but-unplaced structure cancels for a full refund, and the
// freed lane can start something else (the stuck-naval-yard shape, sim side;
// the UI-path twin lives in ra-client/tests/ui_cancel_hold_suite.rs).
// ===========================================================================

#[test]
fn completed_unplaced_building_cancels_for_full_refund_and_lane_restarts() {
    const INITIAL: i32 = 1000;
    let mut w = world(INITIAL);
    start_powr(&mut w);
    let mut ready = false;
    for _ in 0..500 {
        w.tick(&[]);
        if w.house(1).unwrap().ready_building == Some(B_POWR) {
            ready = true;
            break;
        }
    }
    assert!(ready, "test setup: POWR should complete");
    assert_eq!(w.house_credits(1), INITIAL - 30);

    w.tick(&[Command::CancelProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    let hs = w.house(1).unwrap();
    assert_eq!(hs.ready_building, None, "ready slot cleared");
    assert!(hs.building_prod.is_none(), "lane free");
    assert_eq!(
        w.house_credits(1),
        INITIAL,
        "a completed-but-unplaced building refunds its full cost"
    );

    // The freed lane genuinely restarts on a different item.
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    assert_eq!(
        w.house(1).unwrap().building_prod.map(|p| p.item),
        Some(BuildItem::Building(B_LAB)),
        "the lane must accept a new item after the cancel"
    );
}

// ===========================================================================
// Edge cases: hold is suspend-only and never wedges anything.
// ===========================================================================

#[test]
fn hold_on_empty_lane_is_ignored_and_paused_lane_rejects_other_items() {
    const INITIAL: i32 = 800;
    let mut w = world(INITIAL);

    // Hold with nothing in production: no-op, no credit movement.
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    assert!(w.house(1).unwrap().building_prod.is_none());
    assert_eq!(w.house_credits(1), INITIAL);

    // A paused lane still counts as busy for *different* items — only the
    // matching item resumes (the original's per-type factory fetch).
    start_powr(&mut w);
    for _ in 0..3 {
        w.tick(&[]);
    }
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    let p = w.house(1).unwrap().building_prod.unwrap();
    assert_eq!(
        p.item,
        BuildItem::Building(B_POWR),
        "a different item must not hijack a held lane"
    );
    assert!(p.paused, "the held POWR build stays held");
}
