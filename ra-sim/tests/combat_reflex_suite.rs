//! Combat-reflex coverage (M7.14 audit P2): the IQ-gated **artillery/grenade
//! dodge** and **area-guard-on-produce**. These are the "fighting tricks" the
//! M7.14 audit deferred and this milestone lands. Both differentiate a
//! **computer** house (IQ = MaxIQ) from a **human** house (IQ 0) — the audit
//! flagged that differentiation as currently unobservable, so each is pinned in
//! BOTH directions (computer does it, human does not).
//!
//! Synthetic, asset-free: a hand-built catalog with `econ.incoming_speed` set (the
//! real `Rule.Incoming` gate) and a slow test weapon, driven through the public
//! `World::tick` command pipeline.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildItem, BuildingProto, Catalog, Command, MoveStats, Passability, Target, UnitProto,
    WarheadProfile, WeaponProfile, World,
};

const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
const B_WEAP: u32 = 2;

const U_TANK: u32 = 0;

const CREDITS: i32 = 8000;

/// The `Rule.Incoming` gate our test catalog uses (scaled units, matching
/// `proj_speed`). Our slow weapon sits below it; a "fast" weapon above it.
const INCOMING_THRESHOLD: i32 = 50;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 20,
    }
}

/// A weapon with the given projectile speed (leptons/tick, same scale as
/// `Rule.Incoming`). Long range + small damage so the target is hit but survives
/// many shots (we observe the *dodge*, not a kill).
fn weapon(proj_speed: i32) -> WeaponProfile {
    WeaponProfile {
        damage: 5,
        rof: 20,
        range: 10 * 256,
        proj_speed,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 1,
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// FACT / POWR / WEAP + an armed TANK. `econ.incoming_speed = INCOMING_THRESHOLD`.
fn catalog() -> Catalog {
    let b =
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
                sight: 5,
                sprite_id: 0,
                weapon: None,
                has_turret: false,
                charges: false,
                is_wall: false,
                storage: 0,
            }
        };
    let tank = UnitProto {
        is_infantry: false,
        locomotor: 1,
        name: "TANK".to_string(),
        sprite_id: 2,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon: Some(weapon(20)),
        secondary: None,
        has_turret: false,
        is_harvester: false,
        deploys_to: None,
        cost: 200,
        prereq: vec![B_WEAP],
        sight: 4,
        passengers: 0,
    };
    let econ = ra_sim::EconRules {
        incoming_speed: INCOMING_THRESHOLD,
        ..Default::default()
    };
    Catalog {
        buildings: vec![
            b("FACT", 3, 3, 0, 100, vec![], true, false),
            b("POWR", 2, 2, 100, 30, vec![B_FACT], false, false),
            b("WEAP", 3, 3, -20, 60, vec![B_FACT], false, true),
        ],
        units: vec![tank],
        econ,
    }
}

fn spawn_armed(w: &mut World, house: u8, cell: CellCoord, wpn: WeaponProfile) -> ra_sim::Handle {
    let h = w.spawn_unit(2, house, cell, Facing(0), 400, stats());
    w.set_unit_max_health(h, 400);
    w.set_unit_combat(h, 0, Some(wpn), false);
    w.set_unit_sight(h, 4);
    h
}

fn spawn_unarmed(w: &mut World, house: u8, cell: CellCoord) -> ra_sim::Handle {
    let h = w.spawn_unit(2, house, cell, Facing(0), 400, stats());
    w.set_unit_max_health(h, 400);
    w.set_unit_combat(h, 0, None, false);
    w.set_unit_sight(h, 4);
    h
}

// ===========================================================================
// P2b — artillery/grenade dodge (IQ-gated combat threat-scatter)
// ===========================================================================

/// Drive one dodge scenario: an attacker (house 0) with `attacker_speed`-speed
/// weapon fires on an unarmed target (house 1) whose house IQ is `target_iq`.
/// Returns whether the target's cell changed within the window (i.e. it dodged).
fn ran_from_shell(attacker_speed: i32, target_iq: i32) -> bool {
    let mut w = World::new(Passability::all_passable(), 0xD0D6_E1EE);
    w.set_catalog(catalog());
    w.init_houses(2, CREDITS);
    let attacker = spawn_armed(&mut w, 0, CellCoord::new(30, 40), weapon(attacker_speed));
    let target = spawn_unarmed(&mut w, 1, CellCoord::new(35, 40));
    w.set_house_iq(1, target_iq);
    let start = w.units.get(target).unwrap().cell();

    // Order the attacker to fire on the target, then let combat run.
    w.tick(&[Command::Attack {
        unit: attacker,
        target: Target::Unit(target),
        house: 0,
    }]);
    for _ in 0..60 {
        w.tick(&[]);
        let now = w.units.get(target).map(|u| u.cell());
        if let Some(c) = now {
            if c != start {
                return true; // dodged
            }
        } else {
            break; // target somehow died — shouldn't with 5 dmg vs 400 hp
        }
    }
    false
}

#[test]
fn computer_unit_dodges_an_incoming_slow_projectile() {
    // Target house at MaxIQ (>= IQScatter=3) → scatters from the slow shell.
    assert!(
        ran_from_shell(20, 5),
        "a computer unit (IQ 5 >= IQScatter) must scatter from an incoming slow \
         projectile (Speed 20 < Incoming 50) — the artillery/grenade dodge"
    );
}

#[test]
fn human_unit_does_not_dodge_the_identical_shell() {
    // Identical setup, target house IQ 0 (< IQScatter) → stands its ground.
    assert!(
        !ran_from_shell(20, 0),
        "a human unit (IQ 0 < IQScatter) must NOT scatter — it stands its ground, \
         the human/computer differentiation the scatter gate exists for"
    );
}

#[test]
fn no_dodge_when_the_projectile_is_faster_than_the_incoming_threshold() {
    // Even a computer unit ignores a *fast* projectile (Speed 80 > Incoming 50) —
    // only genuinely slow/ballistic shells trigger the reflex (`MaxSpeed <
    // Rule.Incoming`), so tank cannon fire is not dodged.
    assert!(
        !ran_from_shell(80, 5),
        "a fast projectile (Speed 80 > Incoming 50) must not trigger the dodge, \
         even for a computer unit"
    );
}

#[test]
fn dodge_is_deterministic_same_seed_twice() {
    // The scatter draws sync RNG; two identical runs must land the target on the
    // same cell (determinism contract §4.2).
    let run = || {
        let mut w = World::new(Passability::all_passable(), 0x5EED_5EED);
        w.set_catalog(catalog());
        w.init_houses(2, CREDITS);
        let attacker = spawn_armed(&mut w, 0, CellCoord::new(30, 40), weapon(20));
        let target = spawn_unarmed(&mut w, 1, CellCoord::new(35, 40));
        w.set_house_iq(1, 5);
        w.tick(&[Command::Attack {
            unit: attacker,
            target: Target::Unit(target),
            house: 0,
        }]);
        for _ in 0..60 {
            w.tick(&[]);
        }
        w.units.get(target).map(|u| u.cell())
    };
    assert_eq!(run(), run(), "same-seed dodge must be reproducible");
}

// ===========================================================================
// P2a — area-guard on produce (computer units guard a zone; human units don't)
// ===========================================================================

/// Give house `house` a FACT/POWR/WEAP base, set its IQ, produce one TANK, and
/// return the produced unit's [`Mission`].
fn produced_tank_mission(house: u8, iq: i32) -> ra_sim::Mission {
    let mut w = World::new(Passability::all_passable(), 0xA5EA_6A5D);
    w.set_catalog(catalog());
    w.init_houses(2, CREDITS);
    w.spawn_building(B_FACT, house, CellCoord::new(30, 30));
    w.spawn_building(B_POWR, house, CellCoord::new(34, 30));
    w.spawn_building(B_WEAP, house, CellCoord::new(30, 34));
    w.set_house_iq(house, iq);

    w.tick(&[Command::StartProduction {
        house,
        item: BuildItem::Unit(U_TANK),
    }]);
    for _ in 0..900 {
        w.tick(&[]);
        if let Some((_, u)) = w
            .units
            .iter()
            .find(|(_, u)| u.house == house && u.is_alive())
        {
            return u.mission;
        }
    }
    panic!("house {house} never produced a tank");
}

#[test]
fn computer_produced_unit_gets_area_guard() {
    // A computer house (IQ 5 >= IQGuardArea=4) exits produced armed units in Guard
    // Area mode (guards a zone around the factory), not plain Guard.
    assert_eq!(
        produced_tank_mission(1, 5),
        ra_sim::Mission::AreaGuard,
        "a computer house's produced armed unit must start in Area Guard \
         (IQ 5 >= IQGuardArea)"
    );
}

#[test]
fn human_produced_unit_stays_plain_guard() {
    // A human house (IQ 0 < IQGuardArea) exits produced units in plain Guard.
    assert_eq!(
        produced_tank_mission(0, 0),
        ra_sim::Mission::Guard,
        "a human house's produced unit must stay plain Guard (IQ 0 < IQGuardArea)"
    );
}
