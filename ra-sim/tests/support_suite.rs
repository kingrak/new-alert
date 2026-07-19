//! M7.7 Chunk C support-systems adversarial coverage (ra-tester charter): SILO
//! two-pool storage arithmetic, FIX repair-depot cadence/cost/contention, E6
//! engineer capture boundary conditions, and MEDI heal targeting. Chunk C
//! landed these features with almost no dedicated test coverage of its own
//! (a couple of colocated smoke tests in `world.rs`'s `mod tests` —
//! `medic_heals_a_wounded_friendly_infantryman` and
//! `engineer_captures_a_weak_enemy_building_and_is_consumed` — cover only the
//! "it basically works" case). This file drives the *boundaries*: exact
//! arithmetic, exact cadence ticks, tie-breaks, and the no-op/edge paths a
//! smoke test does not reach.
//!
//! Every pinned number here is derived from reading the actual source (cited
//! per-test), not assumed from the task brief — several brief hypotheses
//! turned out subtly wrong or incomplete once checked against
//! `ra-sim/src/world.rs` and `ra-sim/src/house.rs`; see the `FINDING:`-tagged
//! comments below for the ones worth flagging.
//!
//! Uses its own minimal fixture catalog (per `building_combat_economy_edges.rs`'s
//! established convention: don't reach into `world.rs`'s private test module).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, Handle, MoveStats, Passability, Target, UnitProto,
    WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Fixture
// ===========================================================================

// Building type ids. No FACT/POWR filler here (unlike
// `building_combat_economy_edges.rs`) — nothing in this file exercises
// prereqs or power totals, so an unused entry would just be dead weight.
const B_PROC: u32 = 0; // refinery, 3x3, storage=1000
const B_SILO: u32 = 1; // silo, 1x1, storage=500
const B_FIX: u32 = 2; // service depot, 2x2, no power/storage
const B_TARGET: u32 = 3; // generic capturable building, 2x2, max_health=400, cost=300
const B_WALL: u32 = 4; // wall-flagged building, 2x2 (chunk B: walls are 1x1 in
                       // practice, but this file only cares about `is_wall`
                       // being ignored by the capture path, so the footprint
                       // size is irrelevant — 2x2 keeps the same adjacency
                       // math as B_TARGET for a clean side-by-side comparison)

// Unit-proto id — only consulted by `run_repair`'s `catalog.unit(type_id).cost`
// lookup; every other test spawns units directly and attaches combat stats via
// `set_unit_combat`, bypassing the catalog entirely (same pattern as
// `building_combat_economy_edges.rs`'s `spawn_attacker`).
const U_TANK: u32 = 0; // cost=400, referenced by FIX repair-cost tests

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

#[allow(clippy::too_many_arguments)]
fn bproto(
    name: &str,
    w: u8,
    h: u8,
    power: i32,
    cost: i32,
    max_health: u16,
    storage: i32,
    is_wall: bool,
) -> BuildingProto {
    BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health,
        armor: 0,
        power,
        cost,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall,
        storage,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![
            bproto("PROC", 3, 3, 0, 50, 500, 1000, false), // B_PROC
            bproto("SILO", 1, 1, 0, 150, 500, 500, false), // B_SILO
            bproto("FIX", 2, 2, 0, 200, 500, 0, false),    // B_FIX
            bproto("TARGET", 2, 2, 0, 300, 400, 0, false), // B_TARGET
            bproto("WALL", 2, 2, 0, 300, 400, 0, true),    // B_WALL
        ],
        units: vec![UnitProto {
            is_infantry: false,
            locomotor: 1,
            name: "TANK".to_string(),
            sprite_id: 0,
            max_health: 400,
            stats: stats(),
            armor: 0,
            weapon: None,
            secondary: None,
            has_turret: false,
            is_harvester: false,
            deploys_to: None,
            cost: 400, // U_TANK — drives the FIX repair-cost formula
            prereq: vec![],
            sight: 2,
        }],
        econ: EconRules::default(),
    }
}

fn world(credits: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    w.set_catalog(catalog());
    w.init_houses(4, credits);
    w
}

/// A heal weapon in the same shape as `world.rs`'s own colocated
/// `heal_weapon()` fixture (negative damage, Organic-only Verses, point-blank
/// range) — duplicated independently per this file's established convention.
fn heal_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: -50,
        rof: 80,
        range: 468, // 1.83 cells
        proj_speed: 255,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 0,
            verses: pct5([100, 0, 0, 0, 0]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// Spawn an infantry unit with the given weapon (or `None`), armor 0, facing
/// `facing` (chosen by the caller to already equal the aim direction so the
/// turret-alignment gate never delays the first shot).
fn spawn_infantry(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    facing: Facing,
    health: u16,
    weapon: Option<WeaponProfile>,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, facing, health, stats());
    w.set_unit_combat(h, 0, weapon, false);
    if let Some(u) = w.units.get_mut(h) {
        u.make_infantry(0);
    }
    h
}

/// Spawn a plain vehicle (never infantry), armor 0, no weapon by default.
fn spawn_vehicle(w: &mut World, house: u8, cell: CellCoord, health: u16) -> Handle {
    let h = w.spawn_unit(U_TANK, house, cell, Facing(0), health, stats());
    w.set_unit_combat(h, 0, None, false);
    h
}

// ===========================================================================
// 1. SILO two-pool arithmetic.
// ===========================================================================

/// `House::deduct` (`house.rs:141-148`, `HouseClass::Spend_Money`): spending
/// draws from stored `tiberium` **first**, falling into `credits` only once
/// `tiberium` is exhausted. Confirmed as the real order (not assumed) by
/// reading the source directly. Both the production-installment path
/// (`world.rs:2327`) and the FIX repair-cost path (`world.rs:1893`) route
/// through this same method, so this isn't a theoretical unit-level rule —
/// it's the one spend path the whole sim shares.
#[test]
fn spending_draws_from_tiberium_pool_before_credits() {
    // (a) amount < tiberium: comes entirely out of tiberium, credits untouched.
    {
        let mut w = world(0);
        w.houses[1].credits = 500;
        w.houses[1].tiberium = 300;
        w.houses[1].deduct(200);
        assert_eq!(w.houses[1].tiberium, 100, "200 of 300 tiberium spent");
        assert_eq!(w.houses[1].credits, 500, "credits must be untouched");
    }
    // (b) amount == tiberium exactly: drains tiberium to zero, credits untouched.
    {
        let mut w = world(0);
        w.houses[1].credits = 500;
        w.houses[1].tiberium = 300;
        w.houses[1].deduct(300);
        assert_eq!(w.houses[1].tiberium, 0);
        assert_eq!(w.houses[1].credits, 500);
    }
    // (c) amount > tiberium: tiberium drains to zero, the remainder comes out
    // of credits.
    {
        let mut w = world(0);
        w.houses[1].credits = 500;
        w.houses[1].tiberium = 300;
        w.houses[1].deduct(350);
        assert_eq!(w.houses[1].tiberium, 0, "tiberium pool fully drained");
        assert_eq!(
            w.houses[1].credits, 450,
            "the 50 remaining after tiberium should come out of credits"
        );
    }
    // (d) `available()` (what `World::house_credits` reports) is always the sum
    // of both pools, so the two-pool split is invisible to a plain balance read.
    {
        let mut w = world(0);
        w.houses[1].credits = 500;
        w.houses[1].tiberium = 300;
        assert_eq!(w.house_credits(1), 800);
    }
}

/// `House::add_harvest` (`house.rs:150-161`, `HouseClass::Harvested`): income
/// beyond the house's storage capacity is **wasted**, via a plain `.min(capacity)`
/// clamp — exact at the boundary (storing exactly to capacity never wastes
/// anything; one unit over wastes exactly that one unit, not a whole step or a
/// rounded amount).
#[test]
fn silo_cap_boundary_is_exact_no_off_by_one() {
    // capacity = PROC(1000) + SILO(500) = 1500.
    let cap = {
        let mut w = world(0);
        w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_SILO, 1, CellCoord::new(20, 10)).unwrap();
        w.house_capacity(1)
    };
    assert_eq!(cap, 1500);

    // Storing exactly to the cap in one shot: succeeds fully, no waste.
    {
        let mut w = world(0);
        w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_SILO, 1, CellCoord::new(20, 10)).unwrap();
        w.houses[1].add_harvest(1500, cap);
        assert_eq!(w.houses[1].tiberium, 1500);
    }

    // One unit over the cap in a single deposit: wastes exactly 1, not more.
    {
        let mut w = world(0);
        w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_SILO, 1, CellCoord::new(20, 10)).unwrap();
        w.houses[1].add_harvest(1501, cap);
        assert_eq!(
            w.houses[1].tiberium, 1500,
            "capped at exactly capacity, not 1501"
        );
    }

    // Cumulative overflow across two deposits: 1499 then +2 (=1501 raw) still
    // clamps to exactly 1500 — the waste is computed against the running
    // total, not per-call.
    {
        let mut w = world(0);
        w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_SILO, 1, CellCoord::new(20, 10)).unwrap();
        w.houses[1].add_harvest(1499, cap);
        assert_eq!(
            w.houses[1].tiberium, 1499,
            "sanity: under cap, no clamp yet"
        );
        w.houses[1].add_harvest(2, cap);
        assert_eq!(
            w.houses[1].tiberium, 1500,
            "1499+2=1501 raw, clamped to exactly 1500 (1 wasted)"
        );
    }

    // A house with NO storage-declaring building (`capacity == 0`) is
    // deliberately uncapped: `add_harvest` routes straight to `credits`
    // instead of the tiberium pool (house.rs:151-154). Confirms the
    // "capacity <= 0 bypasses the pool entirely" branch, not just "capacity
    // == 0 means waste everything".
    {
        let mut w = world(0);
        assert_eq!(w.house_capacity(1), 0);
        w.houses[1].add_harvest(999_999, 0);
        assert_eq!(w.houses[1].tiberium, 0, "no storage building: pool unused");
        assert_eq!(
            w.houses[1].credits, 999_999,
            "income goes straight to credits, uncapped"
        );
    }
}

/// FINDING (design gap, not a crash/panic bug): selling a SILO while the
/// tiberium pool is above the house's *new* (lower) capacity does **not**
/// reconcile the stored tiberium against the new cap, and does **not**
/// convert any of it to credits — `remove_building` (`world.rs:1125-1165`)
/// only reverses power/building-count bookkeeping, it never touches
/// `house.tiberium`. The sell refund (`apply_sell`, `world.rs:1104-1115`) is
/// purely `cost * refund_percent / 100`, unrelated to stored resources. So
/// the over-cap tiberium just sits there, stale, until the **next**
/// `add_harvest` call (the next harvester delivery) silently clamps it down
/// to the new capacity — at which point the excess evaporates with **zero**
/// credit compensation, and nothing in the API surfaces that it happened.
/// This test pins both halves of that behavior precisely.
#[test]
fn selling_a_full_silo_does_not_reconcile_or_convert_stored_tiberium() {
    let mut w = world(0);
    w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
    let silo = w.spawn_building(B_SILO, 1, CellCoord::new(20, 10)).unwrap();
    let cap_before = w.house_capacity(1);
    assert_eq!(cap_before, 1500);
    w.houses[1].add_harvest(1500, cap_before);
    assert_eq!(w.houses[1].tiberium, 1500, "silo filled to its cap");

    let credits_before = w.house_credits(1);
    w.tick(&[Command::Sell {
        house: 1,
        building: silo,
    }]);
    assert!(!w.buildings.contains(silo), "sell should remove the SILO");

    let cap_after = w.house_capacity(1);
    assert_eq!(cap_after, 1000, "capacity should drop by the SILO's 500");

    // The refund is exactly 150 (SILO cost) * 50% (default refund) = 75 credits
    // — unrelated to the 1500 stored tiberium.
    assert_eq!(
        w.house_credits(1),
        credits_before + 75,
        "sell refund is cost-based only, not tiberium-based"
    );

    // FINDING part 1: the stored tiberium is untouched by the sell itself,
    // even though it now exceeds the new capacity.
    assert_eq!(
        w.houses[1].tiberium, 1500,
        "FINDING: stale over-cap tiberium is not clamped or refunded at sell time"
    );

    // FINDING part 2: the very next harvest booking (even a zero-income one)
    // silently clamps the stale pool down to the new cap, and the difference
    // (500) simply vanishes — no credit conversion, no waste counter exposed.
    w.houses[1].add_harvest(0, cap_after);
    assert_eq!(
        w.houses[1].tiberium, 1000,
        "FINDING: the next harvest tick quietly deletes the excess 500 with no compensation"
    );
}

// ===========================================================================
// 2. FIX service depot: cadence, cost, contention, no-op.
// ===========================================================================

/// `REPAIR_INTERVAL = 15` (`world.rs:1817`), gated on `tick_count % 15 == 0`
/// using the tick's value **before** increment (`world.rs:1831`, `apply`'s
/// final `tick_count += 1`). Since `tick_count` starts at 0, the very first
/// `tick()` call already lands on a repair boundary — repairs happen at
/// entry-ticks 0, 15, 30, ..., i.e. on tick() calls #1, #16, #31, ...
///
/// Cost formula (`world.rs:1881-1893`): `step = min(UREPAIR_STEP=10, missing)`,
/// `step_cost = unit_cost * 20 * step / 100 / max_health` (all truncating
/// integer division, in that exact order). With `unit_cost=400,
/// max_health=400`: a full 10hp step costs `400*20*10/100/400 = 2` credits; a
/// partial 5hp final step costs `400*20*5/100/400 = 1` credit — the cost is
/// genuinely proportional to HP restored, not a flat per-tick charge.
#[test]
fn fix_repair_cadence_and_cost_are_exact() {
    let mut w = world(1000);
    w.spawn_building(B_FIX, 1, CellCoord::new(30, 30)).unwrap();
    // Adjacent to the FIX footprint (2x2 at (30,30), ring = x[29,32] y[29,32]).
    let tank = spawn_vehicle(&mut w, 1, CellCoord::new(29, 30), 400);
    if let Some(u) = w.units.get_mut(tank) {
        u.health = 375; // missing 25: steps of 10, 10, 5
    }

    // Call #1 (entry tick_count == 0): first repair step fires immediately.
    w.tick(&[]);
    assert_eq!(w.units.get(tank).unwrap().health, 385, "step 1: +10 hp");
    assert_eq!(
        w.house_credits(1),
        998,
        "step 1: -2 credits (400*20*10/100/400)"
    );

    // Calls #2..#15 (entry tick_count 1..14): no cadence boundary, no change.
    for _ in 0..14 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(tank).unwrap().health,
        385,
        "no repair should happen between cadence boundaries"
    );
    assert_eq!(w.house_credits(1), 998);

    // Call #16 (entry tick_count == 15): second repair step.
    w.tick(&[]);
    assert_eq!(w.units.get(tank).unwrap().health, 395, "step 2: +10 hp");
    assert_eq!(w.house_credits(1), 996, "step 2: -2 credits");

    // Calls #17..#30: quiet again.
    for _ in 0..14 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(tank).unwrap().health, 395);
    assert_eq!(w.house_credits(1), 996);

    // Call #31 (entry tick_count == 30): final, partial 5hp step — a smaller
    // charge, proving the cost is HP-proportional, not flat-per-step.
    w.tick(&[]);
    assert_eq!(
        w.units.get(tank).unwrap().health,
        400,
        "step 3: +5 hp, now full"
    );
    assert_eq!(
        w.house_credits(1),
        995,
        "step 3: -1 credit (400*20*5/100/400, a smaller partial-step charge)"
    );

    // Full health now: further cadence boundaries must be a true no-op (also
    // covers the standalone "full-health unit sent to FIX" case below more
    // thoroughly — many more cadence windows, not just one).
    for _ in 0..60 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(tank).unwrap().health, 400);
    assert_eq!(
        w.house_credits(1),
        995,
        "no charge should ever occur once the unit is at full health"
    );
}

/// A unit already at full health parked on a FIX depot must never be charged
/// or touched — `run_repair`'s `missing == 0` -> `step <= 0` -> `continue`
/// (`world.rs:1881-1884`) is a true no-op, not a zero-effect-but-still-billed
/// charge. Isolated from the cadence test above so a regression here can't be
/// masked by that test's later assertions.
#[test]
fn fix_repair_is_a_true_no_op_on_a_full_health_unit() {
    let mut w = world(1000);
    w.spawn_building(B_FIX, 1, CellCoord::new(30, 30)).unwrap();
    let tank = spawn_vehicle(&mut w, 1, CellCoord::new(29, 30), 400); // full health
    assert_eq!(w.units.get(tank).unwrap().health, 400);

    for _ in 0..50 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(tank).unwrap().health, 400, "still full");
    assert_eq!(
        w.house_credits(1),
        1000,
        "no credits should ever be charged"
    );
}

/// Two damaged vehicles in the same FIX's adjacency ring at once: `run_repair`
/// picks the single **nearest** (Manhattan distance to the footprint
/// top-left) damaged unit each cadence tick (`world.rs:1855-1872`) — not
/// simultaneous multi-unit repair, and not a rejection of the second unit.
/// Since "nearest" is re-scanned fresh every cadence tick (no sticky queue),
/// the farther unit is effectively serviced only once the nearer one reaches
/// full health and drops out of the "damaged" filter.
#[test]
fn fix_repairs_the_nearest_of_two_damaged_vehicles_first_not_simultaneously() {
    let mut w = world(1000);
    w.spawn_building(B_FIX, 1, CellCoord::new(60, 60)).unwrap(); // 2x2, ring x[59,62] y[59,62]
    let near = spawn_vehicle(&mut w, 1, CellCoord::new(59, 60), 400); // dist 1
    let far = spawn_vehicle(&mut w, 1, CellCoord::new(62, 61), 400); // dist 3
    for h in [near, far] {
        if let Some(u) = w.units.get_mut(h) {
            u.health = 390; // missing 10: exactly one step to full each
        }
    }

    // Call #1: only the nearer vehicle is touched.
    w.tick(&[]);
    assert_eq!(
        w.units.get(near).unwrap().health,
        400,
        "nearest heals first"
    );
    assert_eq!(
        w.units.get(far).unwrap().health,
        390,
        "the farther vehicle must NOT be repaired simultaneously"
    );
    assert_eq!(w.house_credits(1), 998);

    // Calls #2..#15: quiet (near is full, far isn't due yet).
    for _ in 0..14 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(far).unwrap().health, 390);

    // Call #16 (next cadence boundary): now the near unit is excluded (full
    // health), so the far one becomes the nearest *damaged* candidate.
    w.tick(&[]);
    assert_eq!(
        w.units.get(near).unwrap().health,
        400,
        "unchanged, still full"
    );
    assert_eq!(
        w.units.get(far).unwrap().health,
        400,
        "far vehicle now serviced"
    );
    assert_eq!(w.house_credits(1), 996, "second vehicle's step charged too");
}

// ===========================================================================
// 3. Engineer (E6) capture boundary conditions.
// ===========================================================================

/// `ENGINEER_CAPTURE_NUM/DEN = 1/4` and `ENGINEER_DAMAGE_NUM/DEN = 1/3`
/// (`world.rs:1692-1700`) — independently re-verified against
/// `references/vanilla-conquer/redalert/rules.cpp:280-281` and `globals.cpp:172-173`
/// (`EngineerDamage = (fixed)1/3`, `EngineerCaptureLevel = ConditionRed =
/// fixed(1,4)`), matching the source's own citation exactly.
///
/// The capture test uses `bhealth * 4 <= bmax * 1` (`world.rs:1766`, a `<=`,
/// not `<`): at *exactly* the 1/4 boundary the building is still captured.
/// One HP above the boundary switches to the damage-only branch. With
/// `max_health = 400`, the exact boundary is `health == 100`.
#[test]
fn engineer_capture_boundary_is_at_health_le_quarter_max_not_strictly_below() {
    // At exactly 1/4 (health == 100): captured.
    {
        let mut w = world(0);
        let bldg = w
            .spawn_building(B_TARGET, 2, CellCoord::new(20, 20))
            .unwrap();
        w.buildings.get_mut(bldg).unwrap().health = 100; // == max_health/4 exactly
        let eng = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 25, None);

        w.tick(&[Command::Attack {
            unit: eng,
            target: Target::Building(bldg),
            house: 1,
        }]);

        assert_eq!(
            w.buildings.get(bldg).map(|b| b.house),
            Some(1),
            "exactly-at-threshold health should capture, not just damage"
        );
        assert!(
            !w.units.contains(eng),
            "engineer must be consumed on a successful capture"
        );
    }

    // One HP above the boundary (health == 101): NOT captured — damaged
    // instead by exactly `min(max_health/3, health-1)` =
    // `min(133, 100) = 100`, landing at health 1 (never killed outright: the
    // `.min(bhealth - 1)` clamp exists specifically to keep the building alive
    // for a follow-up capture attempt).
    {
        let mut w = world(0);
        let bldg = w
            .spawn_building(B_TARGET, 2, CellCoord::new(20, 20))
            .unwrap();
        w.buildings.get_mut(bldg).unwrap().health = 101;
        let eng = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 25, None);

        w.tick(&[Command::Attack {
            unit: eng,
            target: Target::Building(bldg),
            house: 1,
        }]);

        assert_eq!(
            w.buildings.get(bldg).map(|b| b.house),
            Some(2),
            "one HP above the capture threshold: still enemy-owned, not captured"
        );
        assert_eq!(
            w.buildings.get(bldg).unwrap().health,
            1,
            "damaged by min(400/3, 101-1) = min(133, 100) = 100, landing at 1 hp"
        );
        assert!(
            !w.units.contains(eng),
            "engineer must be consumed on a damage-only interaction too"
        );
    }
}

/// The `EngineerDamage = 1/3` fraction itself, isolated from the
/// `.min(bhealth - 1)` clamp above by using a building healthy enough that the
/// clamp never engages: `max_health = 900` (cleanly divisible by 3),
/// `health = 900` (full). `900/3 = 300` exactly — no truncation noise, and
/// `900 - 300 = 600` is comfortably above `900 - 1`, so this is the raw
/// fraction, not the "avoid killing it" fallback.
#[test]
fn engineer_damage_only_removes_exactly_one_third_of_max_strength() {
    let mut w = world(0);
    let bldg = w
        .spawn_building(B_TARGET, 2, CellCoord::new(20, 20))
        .unwrap();
    if let Some(b) = w.buildings.get_mut(bldg) {
        b.max_health = 900;
        b.health = 900;
    }
    let eng = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 25, None);

    w.tick(&[Command::Attack {
        unit: eng,
        target: Target::Building(bldg),
        house: 1,
    }]);

    assert_eq!(
        w.buildings.get(bldg).map(|b| b.house),
        Some(2),
        "900 is far above the 225 capture threshold: not captured"
    );
    assert_eq!(
        w.buildings.get(bldg).unwrap().health,
        600,
        "900 - (900/3 = 300) = 600, the unclamped 1/3-max-strength fraction"
    );
    assert!(!w.units.contains(eng));
}

/// An engineer ordered onto its **own house's** building is a no-op: no
/// capture, no damage, and — the notable pin here — the engineer is **not**
/// consumed (`world.rs:1733-1739`: the friendly-house branch drops the target
/// and `continue`s *before* reaching the unconditional `world.units.remove`
/// at the end of the function, which only the capture/damage branches reach).
/// This directly contradicts the "always consumed" reading a surface glance
/// at "the engineer is consumed on use either way" (the function's own doc
/// comment) would suggest — "either way" only covers capture vs. damage, not
/// the friendly no-op, which is a third, earlier-return path.
#[test]
fn engineer_walking_into_a_friendly_building_is_a_no_op_and_survives() {
    let mut w = world(0);
    let bldg = w
        .spawn_building(B_TARGET, 1, CellCoord::new(20, 20))
        .unwrap();
    let health_before = w.buildings.get(bldg).unwrap().health;
    let eng = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 25, None);

    w.tick(&[Command::Attack {
        unit: eng,
        target: Target::Building(bldg),
        house: 1,
    }]);
    assert_eq!(
        w.buildings.get(bldg).map(|b| b.health),
        Some(health_before),
        "friendly building must be untouched"
    );
    assert_eq!(w.buildings.get(bldg).map(|b| b.house), Some(1));
    assert!(
        w.units.contains(eng),
        "PIN: the engineer survives a friendly-building no-op (not consumed)"
    );

    // Stays that way indefinitely — no delayed consumption, no delayed effect.
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(w.units.contains(eng), "still alive many ticks later");
    assert_eq!(w.buildings.get(bldg).unwrap().health, health_before);
    assert!(
        !w.units.get(eng).unwrap().has_target(),
        "the dropped order should not silently linger as a target"
    );
}

/// FINDING (design gap, not a crash): a wall (`is_wall = true`) is captured
/// exactly like any ordinary building — `run_engineers`/`capture_building`
/// never inspect `is_wall` at all. Whether that's intended (walls are
/// legitimately "just 1x1 buildings" per the M7.7 Chunk B QUIRKS Q9 model) or
/// an oversight (capturing a wall segment has no real-RA analogue — engineers
/// can't capture walls in the original), this pins the actual, current
/// behavior rather than assuming either way.
#[test]
fn engineer_captures_a_wall_exactly_like_any_other_building() {
    let mut w = world(0);
    let wall = w.spawn_building(B_WALL, 2, CellCoord::new(20, 20)).unwrap();
    w.buildings.get_mut(wall).unwrap().health = 100; // == max_health/4 exactly, capturable
    let eng = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 25, None);

    w.tick(&[Command::Attack {
        unit: eng,
        target: Target::Building(wall),
        house: 1,
    }]);

    assert_eq!(
        w.buildings.get(wall).map(|b| b.house),
        Some(1),
        "FINDING: a wall flips ownership on capture, no special-casing for is_wall"
    );
    assert!(
        !w.units.contains(eng),
        "consumed on capture, same as any building"
    );
}

// ===========================================================================
// 4. MEDI heal targeting.
// ===========================================================================

/// Healing never overshoots `max_health`, even when the raw negative-damage
/// magnitude (-50) would overshoot it by a wide margin (95+50=145 vs a cap of
/// 100) — the clamp lives at the explosion-damage application site
/// (`world.rs:2130-2133`: `(u.health as i32 - dmg).min(u.max_health as i32)`),
/// not in `modify_damage` itself. Once healed to exactly max, the medic must
/// stop re-triggering (its auto-acquire `keep`/re-scan filters
/// `health < max_health`, `world.rs:1659,1672`), so health must stay pinned
/// at max indefinitely afterward, not fluctuate.
#[test]
fn medic_heal_clamps_exactly_at_max_health_and_stays_there() {
    let mut w = world(0);
    let medic = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(128),
        80,
        Some(heal_weapon()),
    );
    // Spawn at health=100 (so max_health snaps to 100 too, per `Unit::new`),
    // then wound it lightly: near-full, only 5 hp missing. The heal pulse's
    // raw magnitude (50) would overshoot max_health by 45 if unclamped.
    let patient = spawn_infantry(&mut w, 1, CellCoord::new(10, 11), Facing(0), 100, None);
    if let Some(u) = w.units.get_mut(patient) {
        u.health = 95; // near-full (max 100), 5 missing
    }
    let _ = medic;

    for _ in 0..60 {
        w.tick(&[]);
    }
    let hp = w.units.get(patient).unwrap().health;
    assert_eq!(hp, w.units.get(patient).unwrap().max_health);
    assert!(hp <= 100, "must never exceed max_health (100)");

    // Stays pinned at max — no drift from repeated heal pulses.
    for _ in 0..60 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(patient).unwrap().health, hp);
}

/// MEDI never **auto-acquires** a vehicle as a heal target — `is_infantry()`
/// gates `maybe_acquire_heal_target`'s fresh-acquisition scan (`world.rs:1676`).
///
/// FINDING (design gap): that filter only runs on *fresh* acquisition. The
/// function's "keep the current target" fast path (`world.rs:1662-1669`)
/// checks only `is_alive() && house == house && health < max_health` — no
/// `is_infantry()` re-check. So once an explicit `Command::Attack` has set the
/// medic's target to a friendly, damaged vehicle for one tick, every
/// subsequent tick's "keep" check happily re-validates that same target
/// forever (a vehicle is exactly as "keepable" as a wounded infantryman), and
/// `modify_damage`'s heal branch (`combat.rs:165-169`) itself only checks
/// `armor == 0` and point-blank distance — it has no infantry-vs-vehicle
/// concept either. Net effect: a vehicle can never be *acquired* as a heal
/// target, but once explicitly assigned, it heals exactly like infantry would.
#[test]
fn medic_never_auto_targets_a_vehicle_but_an_explicit_order_can_still_heal_one() {
    let mut w = world(0);
    let medic = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(128),
        80,
        Some(heal_weapon()),
    );
    let vehicle = spawn_vehicle(&mut w, 1, CellCoord::new(10, 11), 100);
    if let Some(u) = w.units.get_mut(vehicle) {
        u.health = 10;
    }

    for _ in 0..80 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(vehicle).unwrap().health,
        10,
        "auto-acquire must never select a vehicle as a heal target"
    );
    assert!(
        !w.units.get(medic).unwrap().has_target(),
        "the medic should be sitting idle, having found no valid auto target"
    );

    // FINDING: once explicitly assigned, the "keep" fast path lets a
    // friendly vehicle target survive indefinitely (it is never re-subjected
    // to the is_infantry() acquisition filter), so it genuinely heals.
    w.tick(&[Command::Attack {
        unit: medic,
        target: Target::Unit(vehicle),
        house: 1,
    }]);
    let hp_after = w.units.get(vehicle).unwrap().health;
    assert!(
        hp_after > 10,
        "FINDING: an explicitly-ordered heal on a friendly vehicle (armor 0) IS effective — \
         the is_infantry() filter only gates fresh acquisition, not the 'keep the current \
         target' fast path (hp: 10 -> {hp_after})"
    );
}

/// MEDI never **auto-acquires** an enemy unit — the house filter in
/// `maybe_acquire_heal_target`'s acquisition scan (`world.rs:1676`,
/// `u.house != house`).
///
/// Unlike the vehicle case above, an explicit `Command::Attack` ordering the
/// medic onto an **enemy** unit does NOT work, and the reason is subtle: the
/// "keep the current target" fast path (`world.rs:1662-1669`) *does* check
/// `tu.house == house`, so a just-assigned enemy target fails that check on
/// the very same tick, falls through to a fresh acquisition scan (which also
/// excludes non-friendly candidates), finds nothing, and **clobbers the
/// explicit order back to `None`** before `run_combat` ever gets a chance to
/// fire — all within the tick the command was issued. So the "no enemy
/// heals" rule holds, but only as an accidental side effect of the
/// friendly-only re-validation, not as a deliberate guard on the explicit
/// order path (contrast with the vehicle case, where the equivalent
/// re-validation is looser and doesn't catch it).
#[test]
fn medic_explicit_order_to_heal_an_enemy_is_silently_clobbered_back_to_a_no_op() {
    let mut w = world(0);
    let medic = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(128),
        80,
        Some(heal_weapon()),
    );
    let enemy = spawn_infantry(&mut w, 2, CellCoord::new(10, 11), Facing(0), 50, None);
    if let Some(u) = w.units.get_mut(enemy) {
        u.health = 10;
    }

    for _ in 0..80 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        10,
        "auto-acquire must never select an enemy unit as a heal target"
    );

    // The explicit order is accepted by `Command::Attack` (it has no
    // target-house check of its own) — but is immediately clobbered back to
    // `None` by `maybe_acquire_heal_target`'s same-tick re-validation, so it
    // never reaches `run_combat`'s firing logic at all.
    w.tick(&[Command::Attack {
        unit: medic,
        target: Target::Unit(enemy),
        house: 1,
    }]);
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        10,
        "an explicitly-ordered heal-the-enemy command must NOT restore enemy HP"
    );
    assert!(
        !w.units.get(medic).unwrap().has_target(),
        "PIN: the explicit enemy target is silently cleared the same tick it was \
         issued (maybe_acquire_heal_target's 'keep' check requires a friendly house, \
         falls through to a fresh scan, finds nothing, and overwrites the order to None)"
    );

    // Persists indefinitely: no delayed effect, no retry.
    for _ in 0..20 {
        w.tick(&[]);
    }
    assert_eq!(w.units.get(enemy).unwrap().health, 10);
}

/// A damaged MEDI is a perfectly ordinary heal target for another MEDI — the
/// candidate filter only checks `is_infantry`/friendly/alive/damaged
/// (`world.rs:1668-1679`); it has no special case excluding other medics.
#[test]
fn a_damaged_medic_can_be_healed_by_another_medic() {
    let mut w = world(0);
    let healer = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(128),
        80,
        Some(heal_weapon()),
    );
    let patient_medic = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 11),
        Facing(0),
        80,
        Some(heal_weapon()),
    );
    if let Some(u) = w.units.get_mut(patient_medic) {
        u.health = 10;
    }
    let _ = healer;

    for _ in 0..60 {
        w.tick(&[]);
    }
    assert!(
        w.units.get(patient_medic).unwrap().health > 10,
        "a wounded MEDI should be healable by a peer MEDI, same as any infantry"
    );
}

/// A MEDI can never heal **itself** — neither via auto-acquire (the scan
/// explicitly excludes `h == handle`, `world.rs:1669`) nor via an explicit
/// self-targeting `Command::Attack` (rejected outright at the command layer:
/// `Target::Unit(t) if t == unit => return`, `world.rs:861`). Both paths are
/// pinned here so a regression in either one is caught independently.
#[test]
fn medic_cannot_self_heal_via_auto_acquire_or_explicit_command() {
    let mut w = world(0);
    let medic = spawn_infantry(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(0),
        80,
        Some(heal_weapon()),
    );
    if let Some(u) = w.units.get_mut(medic) {
        u.health = 10;
    }

    // No other friendly infantry exists anywhere: if self-heal were possible
    // via auto-acquire, this is the only way health could ever rise.
    for _ in 0..80 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(medic).unwrap().health,
        10,
        "auto-acquire must never let a medic target itself"
    );
    assert!(!w.units.get(medic).unwrap().has_target());

    // Explicit self-targeting order: rejected at the command layer, a no-op.
    w.tick(&[Command::Attack {
        unit: medic,
        target: Target::Unit(medic),
        house: 1,
    }]);
    assert_eq!(
        w.units.get(medic).unwrap().health,
        10,
        "an explicit self-targeting Attack command must be rejected outright"
    );
    assert!(!w.units.get(medic).unwrap().has_target());
}

/// Acquisition determinism: two equally-wounded friendly infantry sit at
/// identical distance from the medic (mirrored east/west). `run_combat`'s
/// candidate scan breaks ties with a **strict** `<` comparison
/// (`world.rs:1676`: `d < bd`), so the first candidate encountered in ascending
/// **slot order** wins every tie, and slot order is insertion order here
/// (fresh arena, no removals — `arena.rs:57-74`). Spawning `patient_a` before
/// `patient_b` therefore deterministically means `patient_a` is healed first
/// — not "whichever happens to be nearer in floating-point terms" or
/// "unspecified". Verified by rebuilding the identical scenario twice from
/// scratch and requiring byte-identical outcomes both times.
#[test]
fn medic_tie_break_between_equidistant_targets_is_deterministic_first_inserted_wins() {
    fn run() -> (u16, u16) {
        let mut w = world(0);
        let medic = spawn_infantry(
            &mut w,
            1,
            CellCoord::new(20, 20),
            Facing(0),
            80,
            Some(heal_weapon()),
        );
        let patient_a = spawn_infantry(&mut w, 1, CellCoord::new(21, 20), Facing(0), 50, None); // east
        let patient_b = spawn_infantry(&mut w, 1, CellCoord::new(19, 20), Facing(0), 50, None); // west
        for h in [patient_a, patient_b] {
            if let Some(u) = w.units.get_mut(h) {
                u.health = 10;
            }
        }
        let _ = medic;
        // Enough ticks for the medic to rotate (worst case ~192 binary-angle
        // units at rot=10/tick ~= 20 ticks) and fire once. The heal weapon's
        // `rof=80` cooldown guarantees only ONE shot can land within this
        // budget, so whichever patient it hits is unambiguous — the other is
        // guaranteed untouched, not just "probably still behind".
        for _ in 0..40 {
            w.tick(&[]);
        }
        (
            w.units.get(patient_a).unwrap().health,
            w.units.get(patient_b).unwrap().health,
        )
    }

    let (a1, b1) = run();
    assert!(
        a1 > 10,
        "PIN: patient_a (spawned first, lower slot index) should win the tie and be healed"
    );
    assert_eq!(
        b1, 10,
        "patient_b (spawned second) should be untouched so far"
    );

    // Same setup, rebuilt from scratch: identical outcome both times.
    let (a2, b2) = run();
    assert_eq!(
        (a1, b1),
        (a2, b2),
        "tie-break must be reproducible run-to-run"
    );
}
