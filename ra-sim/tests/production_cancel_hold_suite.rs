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

// ===========================================================================
// Wedge hunting (audit item 8): adversarial sequences that try to strand
// credits, freeze a lane forever, or let progress sneak through a hold.
// ===========================================================================

/// Hold a lane, then sell the *only* building able to host it
/// (`abandon_production_lane`, `remove_building`/Detach_All): the lane must
/// abandon with the exact spent-before-hold refund — pause must not change
/// the refund math — and a resume attempt afterwards (StartProduction
/// re-issue for an item whose lane is now empty and whose factory building
/// is gone) must neither panic nor silently start an impossible build nor
/// double-refund.
#[test]
fn hold_then_sell_the_only_factory_abandons_exactly_and_resume_is_safe() {
    const INITIAL: i32 = 1000;
    let mut w = world(INITIAL);
    start_powr(&mut w);
    for _ in 0..4 {
        w.tick(&[]);
    }
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    let held = w.house(1).unwrap().building_prod.unwrap();
    assert!(held.paused);
    let spent_before_sell = held.spent;
    assert!(spent_before_sell > 0, "test setup: some spend must exist");

    let fact_handle = w
        .buildings
        .iter()
        .find(|(_, b)| b.type_id == B_FACT)
        .map(|(h, _)| h)
        .expect("construction yard must exist");
    let credits_before_sell = w.house_credits(1);
    w.tick(&[Command::Sell {
        house: 1,
        building: fact_handle,
    }]);

    assert!(
        w.house(1).unwrap().building_prod.is_none(),
        "selling the only factory must abandon the held lane, not leave it wedged"
    );
    // Sell refunds 50% of the *building's* cost (Rule.RefundPercent) on top of
    // abandoning the held lane's spent-so-far — both must land, neither more
    // nor less. FACT costs 100, so sell refunds 50.
    let expected = credits_before_sell + 50 /* sell refund */ + spent_before_sell /* lane abandon refund */;
    assert_eq!(
        w.house_credits(1),
        expected,
        "hold must not change the abandon-on-factory-loss refund math"
    );
    assert!(w.house_credits(1) >= 0);

    // Resume attempt on a lane with no factory left: must not panic, must not
    // wedge, must not fabricate credits. Either it stays empty (no yard to
    // build from) or it starts a fresh lane at full cost — either is
    // acceptable, but double-spending or double-refunding is not.
    let credits_before_resume = w.house_credits(1);
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_POWR),
    }]);
    let after = w.house(1).unwrap().building_prod;
    match after {
        None => assert_eq!(
            w.house_credits(1),
            credits_before_resume,
            "a rejected resume (no construction yard) must not move credits"
        ),
        Some(p) => {
            assert_eq!(
                p.spent, 0,
                "a genuinely fresh build must start at zero spend"
            );
            assert!(!p.paused, "a fresh build must not start pre-paused");
        }
    }
    assert!(w.house_credits(1) >= 0, "credits must never go negative");
}

/// Refund exactness, adversarially: cancel at the very first tick of
/// progress (minimal but non-zero spend) and one tick before completion
/// (maximal spend short of finishing) — both must round-trip to the
/// pre-build treasury exactly, matching the drain-schedule test's cadence
/// but at the two extremes instead of a fixed midpoint.
#[test]
fn cancel_refunds_exactly_at_the_first_and_penultimate_tick() {
    const INITIAL: i32 = 1000;

    // First tick: minimal non-zero spend.
    {
        let mut w = world(INITIAL);
        start_powr(&mut w);
        w.tick(&[]); // exactly one production step
        let p = w.house(1).unwrap().building_prod.expect("lane in progress");
        assert!(!p.done);
        assert!(p.spent > 0, "test setup: first tick must pay something");
        w.tick(&[Command::CancelProduction {
            house: 1,
            kind: ProdKind::Building,
        }]);
        assert_eq!(
            w.house_credits(1),
            INITIAL,
            "cancelling after exactly one installment must still round-trip exactly"
        );
    }

    // Penultimate tick: drive to total_ticks - 1 (one step short of done),
    // cancel there — the largest possible spend that still isn't a finished
    // build.
    {
        let mut w = world(INITIAL);
        // `start_powr` itself ticks once (its `Command::StartProduction` is
        // applied and the production system immediately runs), so progress is
        // already 1 by the time it returns — account for that below.
        start_powr(&mut w);
        let total_ticks = w.house(1).unwrap().building_prod.unwrap().total_ticks;
        assert!(total_ticks > 2, "test setup: needs at least 3 build ticks");
        for _ in 0..(total_ticks - 2) {
            w.tick(&[]);
        }
        let p = w
            .house(1)
            .unwrap()
            .building_prod
            .expect("lane still in progress");
        assert!(!p.done, "test setup: must not have finished yet");
        assert_eq!(p.progress, total_ticks - 1);
        w.tick(&[Command::CancelProduction {
            house: 1,
            kind: ProdKind::Building,
        }]);
        assert!(w.house(1).unwrap().building_prod.is_none());
        assert_eq!(
            w.house_credits(1),
            INITIAL,
            "cancelling one tick before completion must still round-trip exactly"
        );
    }
}

/// Race: a hold arrives on the *exact* tick the build would otherwise
/// complete. Commands apply before the production system runs each tick
/// (`apply()`: System 1 commands, then System 2 production), so the hold
/// must win — the item must NOT sneak through to completion just because the
/// suspend order was contemporaneous with its last step.
#[test]
fn holding_on_the_tick_that_would_complete_freezes_it_short_of_done() {
    const INITIAL: i32 = 1000;
    let mut w = world(INITIAL);
    // As above: `start_powr` already consumes the first production tick.
    start_powr(&mut w);
    let total_ticks = w.house(1).unwrap().building_prod.unwrap().total_ticks;
    assert!(total_ticks > 2, "test setup: needs at least 3 build ticks");
    for _ in 0..(total_ticks - 2) {
        w.tick(&[]);
    }
    let before = w
        .house(1)
        .unwrap()
        .building_prod
        .expect("lane still in progress");
    assert_eq!(
        before.progress,
        total_ticks - 1,
        "test setup: one tick from done"
    );
    assert!(!before.done);

    // This tick would otherwise complete the build; issue HOLD in the same
    // tick instead.
    w.tick(&[Command::HoldProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);

    let after = w
        .house(1)
        .unwrap()
        .building_prod
        .expect("lane must still exist, held rather than completed");
    assert!(after.paused, "hold must take effect");
    assert!(
        !after.done,
        "hold on the completing tick must block completion"
    );
    assert_eq!(
        after.progress,
        total_ticks - 1,
        "progress must not advance on a tick that only holds"
    );
    assert!(
        w.house(1).unwrap().ready_building.is_none(),
        "the item must not have slipped into ready_building despite the hold"
    );

    // Confirm it isn't actually wedged: resuming still completes for the
    // same total cost, no more, no less.
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_POWR),
    }]);
    let mut finished = false;
    for _ in 0..10 {
        w.tick(&[]);
        if w.house(1).unwrap().ready_building == Some(B_POWR) {
            finished = true;
            break;
        }
    }
    assert!(finished, "resumed build must still complete");
    assert_eq!(w.house_credits(1), INITIAL - 30);
}

/// Rapid right-click-spam analogue: alternating Hold/Cancel/StartProduction
/// every single tick for a long stretch. No panics (implicit — a panic fails
/// the test), credits never negative, and the lane always ends in a sane
/// terminal state (empty, or in-progress with `spent` bounded by the item's
/// cost).
#[test]
fn rapid_alternating_hold_cancel_start_never_wedges_or_goes_negative() {
    const INITIAL: i32 = 2000;
    let mut w = world(INITIAL);
    for i in 0..300u32 {
        let cmds = match i % 4 {
            0 => vec![Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_POWR),
            }],
            1 => vec![Command::HoldProduction {
                house: 1,
                kind: ProdKind::Building,
            }],
            2 => vec![Command::CancelProduction {
                house: 1,
                kind: ProdKind::Building,
            }],
            _ => vec![], // let a tick pass with no input, like real play
        };
        w.tick(&cmds);
        assert!(
            w.house_credits(1) >= 0,
            "credits went negative at op {i}: {}",
            w.house_credits(1)
        );
        if let Some(p) = w.house(1).unwrap().building_prod {
            assert!(
                p.spent <= p.cost,
                "op {i}: overspent a lane ({} > {})",
                p.spent,
                p.cost
            );
            assert!(p.progress <= p.total_ticks, "op {i}: overshot total_ticks");
        }
    }
    // Whatever state the spam left it in, a final cancel must always be able
    // to free the lane cleanly (the no-wedge guarantee).
    w.tick(&[Command::CancelProduction {
        house: 1,
        kind: ProdKind::Building,
    }]);
    assert!(w.house(1).unwrap().building_prod.is_none());
    assert!(w.house_credits(1) >= 0);
}
