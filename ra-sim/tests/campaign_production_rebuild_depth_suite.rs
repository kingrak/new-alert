//! M7.5-C depth audit (ra-tester): scripted-production + `[Base]` rebuild depth
//! beyond `campaign_activation_suite`'s three P2 tests (drains credits / no
//! `IsStarted` no build / rebuilds one node). Covers: credit-exhaustion
//! stall+resume, list-order rebuild priority across multiple destroyed nodes,
//! the construction-yard requirement (destroy CY -> no rebuild), and the
//! documented proximity-rule bypass (`building.cpp:2196`).

use ra_sim::coords::CellCoord;
use ra_sim::{
    BuildingProto, Campaign, Catalog, EconRules, EnemyActivation, Handicap, MoveStats, Passability,
    UnitProto, WarheadProfile, WeaponProfile, World,
};

const B_YARD: u32 = 0;
const B_WEAP: u32 = 1;
const B_POWR: u32 = 2;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn weapon(damage: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof: 20,
        range: 50 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 999,
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

fn bldg(name: &str, yard: bool, weap: bool, cost: i32) -> BuildingProto {
    BuildingProto {
        name: name.into(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 0,
        cost,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: yard,
        is_war_factory: weap,
        is_barracks: false,
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![
            bldg("FACT", true, false, 2000),
            bldg("WEAP", false, true, 2000),
            bldg("POWR", false, false, 500),
        ],
        units: vec![UnitProto {
            name: "TANK".into(),
            sprite_id: 0,
            max_health: 400,
            stats: stats(),
            armor: 0,
            weapon: Some(weapon(100)),
            secondary: None,
            has_turret: false,
            is_harvester: false,
            is_infantry: false,
            locomotor: 1,
            deploys_to: None,
            cost: 300,
            prereq: vec![],
            sight: 5,
            passengers: 0,
        }],
        econ: EconRules {
            ticks_per_minute: 60,
            difficulty: [Handicap::default(); 3],
            ..EconRules::default()
        },
    }
}

fn empty_campaign() -> Campaign {
    Campaign {
        triggers: Vec::new(),
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 16],
        cell_triggers: Vec::new(),
        state: Vec::new(),
        started: false,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 20],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    }
}

fn base_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0x5EED_F00D);
    w.set_catalog(catalog());
    w.init_houses(20, 0);
    w.set_player_house(1);
    w.set_campaign(empty_campaign());
    w
}

fn has_building(w: &World, house: u8, type_id: u32, cell: CellCoord) -> bool {
    w.buildings
        .iter()
        .any(|(_, b)| b.house == house && b.type_id == type_id && b.cell == cell && b.is_alive())
}

// ===========================================================================
// 1. Credit-exhaustion stall + resume.
// ===========================================================================

/// Production stops dead at 0 credits (no negative-credit builds, no free
/// money — `FactoryClass::AI`, factory.cpp:203) and resumes the moment credits
/// are granted again.
#[test]
fn production_stalls_at_zero_credits_and_resumes_when_credits_are_granted() {
    let mut w = base_world();
    // Just enough credits to *start* one unit (300) but not finish/repeat
    // indefinitely: give exactly 300 so the queue starts, then drains to 0.
    w.set_house_credits(2, 300);
    w.spawn_building(B_WEAP, 2, CellCoord::new(30, 30)).unwrap();
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    // Run long enough that, with only 300 credits and a 300-cost unit, credits
    // hit (and stay at) 0 -- production must not go negative or keep spawning
    // units for free.
    for _ in 0..1500 {
        w.tick(&[]);
    }
    let credits_after_stall = w.house_credits(2);
    assert_eq!(credits_after_stall, 0, "credits must not go negative");
    let units_at_stall = w.units.iter().filter(|(_, u)| u.house == 2).count();
    // Run further with zero credits: nothing more must be produced.
    for _ in 0..600 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.iter().filter(|(_, u)| u.house == 2).count(),
        units_at_stall,
        "no further production while credits remain at 0"
    );
    assert_eq!(w.house_credits(2), 0, "credits stay at 0, never negative");

    // Grant more credits: production resumes.
    w.set_house_credits(2, 1000);
    for _ in 0..1500 {
        w.tick(&[]);
    }
    assert!(
        w.units.iter().filter(|(_, u)| u.house == 2).count() > units_at_stall,
        "production must resume once credits are granted again"
    );
}

// ===========================================================================
// 2. Rebuild requires a live construction yard.
// ===========================================================================

/// No construction yard at all -> the destroyed `[Base]` node is never
/// rebuilt, no matter how many credits/ticks are available. Adding a CY then
/// lets the very same node rebuild (proving the gate is the CY, not something
/// else silently blocking it).
#[test]
fn rebuild_requires_a_live_construction_yard() {
    let mut w = base_world();
    w.set_house_credits(2, 20_000);
    let node_cell = CellCoord::new(50, 50);
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        base_nodes: vec![(B_WEAP, node_cell)],
        tech_level: 4,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    // No construction yard anywhere for house 2: the node must never rebuild,
    // however long we run.
    for _ in 0..2000 {
        w.tick(&[]);
    }
    assert!(
        !has_building(&w, 2, B_WEAP, node_cell),
        "no rebuild without a live construction yard"
    );

    // Now give house 2 a construction yard: the same destroyed node rebuilds.
    w.spawn_building(B_YARD, 2, CellCoord::new(10, 10)).unwrap();
    for _ in 0..2000 {
        w.tick(&[]);
    }
    assert!(
        has_building(&w, 2, B_WEAP, node_cell),
        "adding a construction yard unblocks the rebuild"
    );
}

/// Destroying the construction yard *mid-rebuild-cycle* (after one node has
/// already rebuilt) halts further rebuilds of the remaining destroyed nodes.
#[test]
fn destroying_the_construction_yard_mid_cycle_halts_further_rebuilds() {
    let mut w = base_world();
    w.set_house_credits(2, 30_000);
    let cy = w.spawn_building(B_YARD, 2, CellCoord::new(10, 10)).unwrap();
    let node_a = CellCoord::new(50, 50);
    let node_b = CellCoord::new(60, 60);
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        base_nodes: vec![(B_WEAP, node_a), (B_POWR, node_b)],
        tech_level: 4,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    // Run until the first node rebuilds.
    for _ in 0..1500 {
        w.tick(&[]);
        if has_building(&w, 2, B_WEAP, node_a) {
            break;
        }
    }
    assert!(
        has_building(&w, 2, B_WEAP, node_a),
        "first node must rebuild"
    );
    assert!(
        !has_building(&w, 2, B_POWR, node_b),
        "second node not yet rebuilt"
    );

    // Destroy the construction yard now.
    w.buildings.remove(cy);
    for _ in 0..3000 {
        w.tick(&[]);
    }
    assert!(
        !has_building(&w, 2, B_POWR, node_b),
        "second node must never rebuild once the construction yard is gone"
    );
}

// ===========================================================================
// 3. List-order rebuild priority + proximity-rule bypass.
// ===========================================================================

/// Two destroyed nodes: the earlier list entry (`Next_Buildable`, base.cpp:377)
/// must complete first, and each rebuilds at its exact scripted cell — far from
/// the construction yard and from each other, which the normal player/AI
/// placement proximity rule would reject (documented bypass,
/// `building.cpp:2196`).
#[test]
fn nodes_rebuild_in_list_order_at_their_scripted_cells_bypassing_proximity() {
    let mut w = base_world();
    w.set_house_credits(2, 30_000);
    w.spawn_building(B_YARD, 2, CellCoord::new(5, 5)).unwrap();
    // Node cells are far from the CY and from each other -- the ordinary
    // build-adjacency rule would reject placement this far out.
    let node_first = CellCoord::new(90, 90); // POWR (cheap -> finishes first if built first)
    let node_second = CellCoord::new(10, 95);
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        base_nodes: vec![(B_POWR, node_first), (B_WEAP, node_second)],
        tech_level: 4,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    // The moment exactly one node is complete, it must be the FIRST list entry,
    // never the second (proves list-order priority, not e.g. cheapest-first).
    let mut first_done_before_second = false;
    for _ in 0..3000 {
        w.tick(&[]);
        let a = has_building(&w, 2, B_POWR, node_first);
        let b = has_building(&w, 2, B_WEAP, node_second);
        if a && !b {
            first_done_before_second = true;
        }
        if b && !a {
            panic!("the second [Base] node completed before the first -- list order violated");
        }
        if a && b {
            break;
        }
    }
    assert!(
        first_done_before_second,
        "the first list entry must complete strictly before the second"
    );
    assert!(
        has_building(&w, 2, B_POWR, node_first),
        "first node rebuilt at its scripted cell"
    );
    assert!(
        has_building(&w, 2, B_WEAP, node_second),
        "second node rebuilt at its scripted cell, far from the CY -- proximity bypassed"
    );
}
