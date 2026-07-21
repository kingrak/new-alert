//! M7 item 4 — production-abandon suite (ra-tester charter): losing the last
//! factory able to host an in-flight lane (construction yard for the building
//! lane, war factory for the unit lane) abandons that lane and refunds the
//! credits already spent, via **both** removal paths — `Command::Sell` and
//! combat destruction (`remove_building`'s `abandon_production_lane`, port of
//! `FactoryClass::Abandon`, `factory.cpp:479`).
//!
//! `building_combat_economy_edges.rs`'s
//! `sell_while_producing_unrelated_continues_and_factory_sale_abandons_with_refund`
//! already covers **Sell** on the **war-factory/unit** lane in detail (re-pinned
//! for M7, see the audit) — not duplicated here. This file adds the three
//! combinations that test still leaves open, plus the two extra invariants the
//! M7 charter calls for:
//! - Sell the last **construction yard** while the **building** lane is active.
//! - Combat-destroy the last **construction yard** while the **building** lane
//!   is active.
//! - Combat-destroy the last **war factory** while the **unit** lane is active.
//! - After an abandon, placing a *replacement* factory makes production
//!   startable again (the lane isn't just cleared, the house's ability to
//!   build is genuinely restored).
//! - Selling/destroying a **non-last** factory (a second one of the same kind
//!   still standing) must NOT abandon anything — `house_has_construction_yard`
//!   / `house_has_war_factory` only fire the abandon when the type has truly
//!   dropped to zero live instances.
//!
//! Own minimal fixture catalog (house convention: independent of other test
//! files' fixtures and of `world.rs`'s private test module).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildItem, BuildingProto, Catalog, Command, EconRules, Handle, MoveStats, Passability, Target,
    UnitProto, WarheadProfile, WeaponProfile, World,
};

// Building type ids.
const B_FACT: u32 = 0; // construction yard, 3x3, cost 100
const B_POWR: u32 = 1; // power plant, 2x2, +100 power, prereq FACT, cost 30
const B_WEAP: u32 = 2; // war factory, 3x3, -20 power, prereq POWR, cost 60
const B_LAB: u32 = 3; // 1x1 building-lane filler, prereq POWR, cost 80

// Unit-proto id.
const U_TANK: u32 = 0; // war-factory product, prereq WEAP, cost 120
const U_TANK_SPRITE: u32 = 99;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn catalog() -> Catalog {
    let bproto =
        |name: &str, w: u8, h: u8, power: i32, cost: i32, prereq: Vec<u32>, cy: bool, wf: bool| {
            BuildingProto {
                is_barracks: false,
                name: name.to_string(),
                foot_w: w,
                foot_h: h,
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
            }
        };
    Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 0, 100, vec![], true, false),
            bproto("POWR", 2, 2, 100, 30, vec![B_FACT], false, false),
            bproto("WEAP", 3, 3, -20, 60, vec![B_POWR], false, true),
            bproto("LAB", 1, 1, 0, 80, vec![B_POWR], false, false),
        ],
        units: vec![UnitProto {
            is_infantry: false,
            locomotor: 1,
            name: "TANK".to_string(),
            sprite_id: U_TANK_SPRITE,
            max_health: 400,
            stats: stats(),
            armor: 0,
            weapon: None,
            secondary: None,
            has_turret: false,
            is_harvester: false,
            deploys_to: None,
            cost: 120,
            prereq: vec![B_WEAP],
            sight: 2,
            passengers: 0,
            ammo: 0,
        }],
        econ: EconRules::default(),
    }
}

fn world(credits: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    w.set_catalog(catalog());
    w.init_houses(2, credits);
    w
}

/// Sell refund for a building of the given `cost` under the default 50%
/// `RefundPercent` (`EconRules::default`, integer-truncating).
fn sell_refund(cost: i32) -> i32 {
    cost * 50 / 100
}

/// A wildly overkill instant weapon (100% vs. every armor class, way past any
/// fixture's max health) so a single shot always one-shot-kills its building
/// target, deterministically, with no need to model a realistic damage
/// exchange -- this suite is about the abandon wiring on `remove_building`,
/// not combat math (`splash_suite.rs`/`damage_matrix.rs` own that).
fn lethal_instant_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 100_000,
        rof: 60_000,
        range: 3000,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
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
        max_damage: 100_000,
    }
}

/// Spawn a lethal attacker (house 9, hostile to everyone) one cell directly
/// north of `target`'s footprint, already facing south (toward the
/// building's centre column, so `aligned_to_fire` passes immediately — same
/// "already aligned, already in range" trick `building_combat_economy_edges.rs`
/// uses), and fire one guaranteed-lethal shot at it via a single `tick`.
/// Works for any footprint size: the spawn cell is one row above the
/// footprint's top edge, so it's never inside it.
fn kill_building(w: &mut World, target: Handle) {
    let b = w.buildings.get(target).unwrap();
    let center = b.center_cell();
    let spawn_cell = CellCoord::new(center.x, b.cell.y - 1);
    let atk = w.spawn_unit(0, 9, spawn_cell, Facing(128), 400, stats());
    w.set_unit_combat(atk, 0, Some(lethal_instant_weapon()), true);
    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Building(target),
        house: 9,
    }]);
}

// ===========================================================================
// 1. Sell the last construction yard mid-building-production.
// ===========================================================================

#[test]
fn sell_last_construction_yard_mid_building_production_abandons_and_refunds() {
    let mut w = world(1000);
    let fact = w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();

    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    assert!(
        w.house(1).unwrap().building_prod.is_some(),
        "sanity: lane started"
    );
    for _ in 0..5 {
        w.tick(&[]);
    }
    let spent_before = w.house(1).unwrap().building_prod.unwrap().spent;
    assert!(spent_before > 0, "sanity: some credits already spent");
    let credits_before = w.house_credits(1);

    w.tick(&[Command::Sell {
        house: 1,
        building: fact,
    }]);

    assert!(!w.buildings.contains(fact));
    assert!(
        w.house(1).unwrap().building_prod.is_none(),
        "selling the last construction yard mid-build must abandon the building lane"
    );
    assert_eq!(
        w.house_credits(1),
        credits_before + spent_before + sell_refund(100),
        "abandon refunds the spent progress; selling FACT (cost 100) refunds 50%"
    );

    // Not soft-locked forever: replace the yard and confirm production is
    // startable again.
    w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    let credits_before_restart = w.house_credits(1);
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    assert!(
        w.house(1).unwrap().building_prod.is_some(),
        "production must be restartable once a construction yard exists again"
    );
    for _ in 0..200 {
        w.tick(&[]);
    }
    assert_eq!(
        w.house(1).unwrap().ready_building,
        Some(B_LAB),
        "the restarted lane should complete normally"
    );
    assert_eq!(
        w.house_credits(1),
        credits_before_restart - 80,
        "the restarted build should have paid its full cost (80), nothing more"
    );
}

// ===========================================================================
// 2. Combat-destroy the last construction yard mid-building-production.
// ===========================================================================

#[test]
fn combat_destroy_last_construction_yard_mid_building_production_abandons_and_refunds() {
    let mut w = world(1000);
    let fact = w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();

    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    for _ in 0..5 {
        w.tick(&[]);
    }
    let spent_before = w.house(1).unwrap().building_prod.unwrap().spent;
    assert!(spent_before > 0, "sanity: some credits already spent");
    let credits_before = w.house_credits(1);

    // Combat-destroy the CY via a real lethal shot (buildings are only ever
    // removed through the bullet-detonation death sweep, `remove_building`
    // isn't reachable any other way from outside `world.rs`).
    kill_building(&mut w, fact);

    assert!(!w.buildings.contains(fact), "sanity: FACT should be gone");
    assert!(
        w.house(1).unwrap().building_prod.is_none(),
        "combat-destroying the last construction yard mid-build must abandon the lane"
    );
    assert_eq!(
        w.house_credits(1),
        credits_before + spent_before,
        "abandon refunds exactly the spent progress -- combat destruction pays no sell refund"
    );
}

// ===========================================================================
// 3. Combat-destroy the last war factory mid-unit-production.
// ===========================================================================

#[test]
fn combat_destroy_last_war_factory_mid_unit_production_abandons_refunds_and_allows_restart() {
    let mut w = world(1000);
    w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();
    let weap = w.spawn_building(B_WEAP, 1, CellCoord::new(30, 10)).unwrap();

    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Unit(U_TANK),
    }]);
    assert!(
        w.house(1).unwrap().unit_prod.is_some(),
        "sanity: lane started"
    );
    for _ in 0..5 {
        w.tick(&[]);
    }
    let spent_before = w.house(1).unwrap().unit_prod.unwrap().spent;
    assert!(spent_before > 0, "sanity: some credits already spent");
    let credits_before = w.house_credits(1);

    kill_building(&mut w, weap);

    assert!(!w.buildings.contains(weap), "sanity: WEAP should be gone");
    assert!(
        w.house(1).unwrap().unit_prod.is_none(),
        "combat-destroying the last war factory mid-build must abandon the unit lane"
    );
    assert_eq!(
        w.house_credits(1),
        credits_before + spent_before,
        "abandon refunds exactly the spent progress"
    );

    // Run far past when the unit would normally have finished: no panic, no
    // spawn (the exit factory is gone and the lane is cleared, not stuck).
    for _ in 0..5000 {
        w.tick(&[]);
    }
    assert!(
        !w.units
            .iter()
            .any(|(_, u)| u.house == 1 && u.type_id == U_TANK_SPRITE),
        "no unit should ever spawn once its war factory was destroyed mid-production"
    );

    // Replace the factory and confirm production is startable again.
    w.spawn_building(B_WEAP, 1, CellCoord::new(30, 10)).unwrap();
    let credits_before_restart = w.house_credits(1);
    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Unit(U_TANK),
    }]);
    assert!(
        w.house(1).unwrap().unit_prod.is_some(),
        "unit production must be restartable once a war factory exists again"
    );
    for _ in 0..500 {
        w.tick(&[]);
    }
    assert!(
        w.units
            .iter()
            .any(|(_, u)| u.house == 1 && u.type_id == U_TANK_SPRITE),
        "the restarted lane should complete and spawn the unit normally"
    );
    assert_eq!(
        w.house_credits(1),
        credits_before_restart - 120,
        "the restarted build should have paid its full cost (120), nothing more"
    );
}

// ===========================================================================
// 4. A non-last factory being sold/destroyed must NOT abandon anything.
// ===========================================================================

#[test]
fn selling_one_of_two_construction_yards_does_not_abandon_the_building_lane() {
    let mut w = world(1000);
    let fact1 = w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_FACT, 1, CellCoord::new(40, 10)).unwrap(); // second CY
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();

    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Building(B_LAB),
    }]);
    for _ in 0..5 {
        w.tick(&[]);
    }
    let spent_before = w.house(1).unwrap().building_prod.unwrap().spent;
    assert!(spent_before > 0);

    // A "control" clone that gets a no-op tick in the same tick slot, so the
    // lane's *normal* per-tick installment progression (which happens
    // regardless of any command) is known independently of the sell. Selling
    // an unrelated building must add exactly zero extra effect on top of
    // that: the production installment for this tick, not "frozen in place".
    let mut control = w.clone();
    control.tick(&[]);
    let expected_spent = control.house(1).unwrap().building_prod.unwrap().spent;

    w.tick(&[Command::Sell {
        house: 1,
        building: fact1,
    }]);
    assert!(!w.buildings.contains(fact1));

    assert_eq!(
        w.house(1).unwrap().building_prod.unwrap().spent,
        expected_spent,
        "the lane must be undisturbed: selling a second, non-last construction yard \
         must add nothing beyond the tick's ordinary installment progression"
    );
    // Let it finish normally to further confirm it was never abandoned.
    for _ in 0..200 {
        w.tick(&[]);
    }
    assert_eq!(w.house(1).unwrap().ready_building, Some(B_LAB));
}

#[test]
fn destroying_one_of_two_war_factories_does_not_abandon_the_unit_lane() {
    let mut w = world(1000);
    w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
    w.spawn_building(B_POWR, 1, CellCoord::new(20, 10)).unwrap();
    let weap1 = w.spawn_building(B_WEAP, 1, CellCoord::new(30, 10)).unwrap();
    w.spawn_building(B_WEAP, 1, CellCoord::new(50, 10)).unwrap(); // second WEAP

    w.tick(&[Command::StartProduction {
        house: 1,
        item: BuildItem::Unit(U_TANK),
    }]);
    for _ in 0..5 {
        w.tick(&[]);
    }
    let spent_before = w.house(1).unwrap().unit_prod.unwrap().spent;
    assert!(spent_before > 0);

    // See the sibling CY test's comment: a "control" clone isolates the
    // lane's ordinary per-tick installment progress (which happens
    // regardless of any command) from whatever `kill_building`'s attack tick
    // does.
    let mut control = w.clone();
    control.tick(&[]);
    let expected_spent = control.house(1).unwrap().unit_prod.unwrap().spent;

    kill_building(&mut w, weap1);
    assert!(!w.buildings.contains(weap1));

    assert_eq!(
        w.house(1).unwrap().unit_prod.unwrap().spent,
        expected_spent,
        "the lane must be undisturbed: destroying a second, non-last war factory \
         must add nothing beyond the tick's ordinary installment progression"
    );
    for _ in 0..500 {
        w.tick(&[]);
    }
    assert!(w
        .units
        .iter()
        .any(|(_, u)| u.house == 1 && u.type_id == U_TANK_SPRITE));
}
