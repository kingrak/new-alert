//! M7.5-C verification: campaign **enemy activation** — difficulty handicaps
//! (P0), autocreate teams (P1), and scripted production + `[Base]` rebuild (P2).
//! Asset-free, synthetic scenarios, expectations derived from the reference
//! (`house.cpp:278/1042/5700`, `base.cpp:377`, `taction.cpp:621/645`) and pinned
//! with citations.
//!
//! The damage-ratio checks reuse the real rules.ini bias magnitudes proved in
//! `handicap_suite.rs`: `.8` -> `52428`, `1.2` -> `78643`, `1.0` -> `65536`.

use ra_sim::campaign::{team_flags, tmission};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Campaign, Catalog, Command, Difficulty, EconRules, EnemyActivation, Handicap,
    MoveStats, Passability, SpawnProto, Target, TeamClass, TeamMission, TeamType, UnitProto,
    WarheadProfile, WeaponProfile, World,
};

const BIAS_08: i32 = 52428; // .8  ([Difficult] FirePower — the nerf)
const BIAS_12: i32 = 78643; // 1.2 ([Easy] FirePower — the buff)

// Catalog building ids (construction yard = 0, war factory = 1).
const B_YARD: u32 = 0;
const B_WEAP: u32 = 1;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

/// A point-blank identity weapon (no falloff, `Verses` all 100%) — damage is the
/// raw `damage` field, trivial to hand-verify (see `handicap_suite`).
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

/// A catalog with a difficulty table wired to the real `[Difficult]`/`[Normal]`/
/// `[Easy]` FirePower biases (indexed by our label→section inversion, Q15): our
/// `Easy` = the nerf, our `Hard` = the buff. Plus one armed tank, a construction
/// yard, and a war factory for the production tests.
fn catalog() -> Catalog {
    let nerf = Handicap {
        firepower: BIAS_08,
        ..Handicap::default()
    };
    let buff = Handicap {
        firepower: BIAS_12,
        ..Handicap::default()
    };
    let econ = EconRules {
        // 60 ticks/minute keeps the AlertTime arithmetic small for the cadence test.
        ticks_per_minute: 60,
        difficulty: [nerf, Handicap::default(), buff],
        ..EconRules::default()
    };
    let tank = UnitProto {
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
    };
    let bldg = |name: &str, yard: bool, weap: bool| BuildingProto {
        name: name.into(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 500,
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
    };
    Catalog {
        buildings: vec![bldg("FACT", true, false), bldg("WEAP", false, true)],
        units: vec![tank],
        econ,
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
    let mut w = World::new(Passability::all_passable(), 0xC0DE_1234);
    w.set_catalog(catalog());
    w.init_houses(20, 0);
    w.set_player_house(1);
    w
}

fn tank_proto() -> SpawnProto {
    SpawnProto {
        type_id: 0,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon: Some(weapon(100)),
        secondary: None,
        has_turret: false,
        sight: 5,
        is_infantry: false,
        is_harvester: false,
        is_civ_evac: false,
        passengers: 0,
    }
}

// ===========================================================================
// P0 — campaign difficulty handicaps
// ===========================================================================

/// `HouseClass::Assign_Handicap` campaign semantics (house.cpp:742 +
/// scenario.cpp:2332, init.cpp:681): the **computer** houses get the chosen
/// difficulty; the **player** gets the *inverse* (buffed on Easy, nerfed on Hard).
/// Normal is neutral for everyone.
#[test]
fn campaign_difficulty_maps_computer_and_player_houses() {
    // Hard game: computers buffed ([Easy] FirePower 1.2), player nerfed ([Difficult]).
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Hard);
    assert_eq!(
        w.houses[1].handicap.firepower, BIAS_08,
        "player nerfed on Hard"
    );
    assert_eq!(
        w.houses[2].handicap.firepower, BIAS_12,
        "computer buffed on Hard"
    );

    // Easy game: mirror — computers nerfed, player buffed.
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Easy);
    assert_eq!(
        w.houses[1].handicap.firepower, BIAS_12,
        "player buffed on Easy"
    );
    assert_eq!(
        w.houses[2].handicap.firepower, BIAS_08,
        "computer nerfed on Easy"
    );

    // Normal: every house neutral (the byte-exact no-op default).
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Normal);
    assert!(w.houses[1].handicap.is_neutral() && w.houses[2].handicap.is_neutral());
}

/// End-to-end: the *same enemy attack* deals `1.2 / .8` more damage on Hard than
/// on Easy — the hand-computed handicap ratio. Same seed both runs → identical
/// combat RNG → the only difference is the firepower bias.
#[test]
fn enemy_damage_scales_by_the_handicap_ratio_hard_vs_easy() {
    let enemy_damage = |diff: Difficulty| -> i32 {
        let mut w = base_world();
        w.set_campaign_difficulty(1, diff); // player = house 1; enemy = house 2
        w.set_campaign(empty_campaign());
        let enemy = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 400, stats());
        w.set_unit_combat(enemy, 0, Some(weapon(100)), false);
        let target = w.spawn_unit(0, 1, CellCoord::new(11, 10), Facing(0), 60_000, stats());
        w.tick(&[Command::Attack {
            unit: enemy,
            target: Target::Unit(target),
            house: 2,
        }]);
        for _ in 0..200 {
            w.tick(&[]);
            let now = w.units.get(target).unwrap().health;
            if now < 60_000 {
                return 60_000 - now as i32;
            }
        }
        panic!("enemy never fired");
    };
    let hard = enemy_damage(Difficulty::Hard);
    let easy = enemy_damage(Difficulty::Easy);
    // 100 × 1.2 = 120 vs 100 × .8 = 80 (fx_mul rounding). Ratio 1.5.
    assert_eq!(hard, 120, "Hard enemy: 100 × 1.2");
    assert_eq!(easy, 80, "Easy enemy: 100 × .8");
    assert!(hard > easy, "Hard enemy out-damages Easy");
}

// ===========================================================================
// P1 — autocreate teams
// ===========================================================================

/// Build a campaign with one autocreate-flagged team type (`DO:MISSION_HUNT`)
/// owned by `house`, plus `n_idle` idle enemy tanks, and install enemy-activation
/// with `house` alerted. Returns the world.
fn autocreate_world(house: u8, alerted: bool, autocreate_flag: bool, n_idle: usize) -> World {
    let mut w = base_world();
    let tt = TeamType {
        name: "atk".into(),
        house: house as i32,
        flags: if autocreate_flag {
            team_flags::AUTOCREATE
        } else {
            0
        },
        recruit: 7,
        init_num: 0,
        max_allowed: 4,
        origin: -1,
        trigger: -1,
        classes: vec![TeamClass {
            proto: Some(tank_proto()),
            count: 2,
        }],
        missions: vec![TeamMission {
            code: tmission::DO,
            arg: tmission::MISSION_HUNT_ARG,
        }],
    };
    let mut camp = empty_campaign();
    camp.teamtypes = vec![tt];
    w.set_campaign(camp);
    for i in 0..n_idle {
        let u = w.spawn_unit(
            0,
            house,
            CellCoord::new(20 + i as i32, 20),
            Facing(0),
            400,
            stats(),
        );
        w.set_unit_combat(u, 0, Some(weapon(100)), false);
    }
    // A distant player target so a hunting recruit has somewhere to go.
    let p = w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());
    w.set_unit_combat(p, 0, Some(weapon(100)), false);
    let mut alerted_v = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    if alerted {
        alerted_v[house as usize] = true;
        timer[house as usize] = 0; // fire immediately
    }
    w.set_enemy_activation(EnemyActivation {
        alerted: alerted_v,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: house,
        base_nodes: Vec::new(),
        tech_level: 4,
    });
    w
}

/// `IsAlerted` + an autocreate-flagged team type → the house forms a team from its
/// idle units on the AlertTime cadence (house.cpp:1042 + teamtype.cpp:414). The
/// recruited units adopt the `DO:MISSION_HUNT` script and start hunting the player.
#[test]
fn alerted_house_forms_an_autocreate_team_from_idle_units() {
    let mut w = autocreate_world(2, true, true, 4);
    let hunting_before = w.units.iter().filter(|(_, u)| u.hunt).count();
    assert_eq!(hunting_before, 0);
    w.tick(&[]); // AlertTime == 0 -> wave fires this tick
    let hunting = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 2 && u.hunt)
        .count();
    assert!(
        hunting >= 2,
        "at least one team (2 members) recruited to hunt, got {hunting}"
    );
}

/// The two gating conditions (both required, house.cpp:1042 + teamtype.cpp:430):
/// a house that is **not alerted** forms nothing, and an alerted house with only
/// **non-autocreate** team types forms nothing.
#[test]
fn autocreate_is_gated_on_both_alert_and_the_autocreate_flag() {
    // Not alerted: nothing forms even though an autocreate team type exists.
    let mut w = autocreate_world(2, false, true, 4);
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units
            .iter()
            .filter(|(_, u)| u.house == 2 && u.hunt)
            .count(),
        0
    );

    // Alerted but the only team type lacks the autocreate flag: nothing forms.
    let mut w = autocreate_world(2, true, false, 4);
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units
            .iter()
            .filter(|(_, u)| u.house == 2 && u.hunt)
            .count(),
        0
    );
}

/// Same seed twice → identical hash after autocreate waves fire (determinism).
#[test]
fn autocreate_is_deterministic_same_seed_twice() {
    let run = || -> u64 {
        let mut w = autocreate_world(2, true, true, 6);
        let mut h = 0;
        for _ in 0..30 {
            h = w.tick(&[]);
        }
        h
    };
    assert_eq!(run(), run());
}

// ===========================================================================
// P2 — scripted production + [Base] rebuild
// ===========================================================================

/// `IsStarted` (BEGIN_PRODUCTION) → the house produces from its live war factory,
/// draining its scenario credits — no free money (factory.cpp:203). A house that
/// never began production produces nothing.
#[test]
fn production_started_house_builds_and_drains_credits() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    w.set_house_credits(2, 5000);
    // A live war factory for house 2.
    let f = w.spawn_building(B_WEAP, 2, CellCoord::new(30, 30)).unwrap();
    assert!(w.buildings.get(f).is_some());
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    let units_before = w.units.iter().filter(|(_, u)| u.house == 2).count();
    let credits_before = w.house_credits(2);
    for _ in 0..600 {
        w.tick(&[]);
    }
    assert!(
        w.house_credits(2) < credits_before,
        "production drained credits"
    );
    let units_after = w.units.iter().filter(|(_, u)| u.house == 2).count();
    assert!(units_after > units_before, "war factory produced a unit");
}

/// Not production-started → no build even with a factory + money (the IsStarted
/// gate, building.cpp:5600).
#[test]
fn house_without_begin_production_builds_nothing() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    w.set_house_credits(2, 5000);
    w.spawn_building(B_WEAP, 2, CellCoord::new(30, 30)).unwrap();
    // enemy_activation present but no production flag → run_enemy_activation is inert.
    w.set_enemy_activation(EnemyActivation {
        production: vec![false; 20],
        ..Default::default()
    });
    let credits_before = w.house_credits(2);
    for _ in 0..300 {
        w.tick(&[]);
    }
    assert_eq!(
        w.house_credits(2),
        credits_before,
        "no production without IsStarted"
    );
    assert_eq!(w.units.iter().filter(|(_, u)| u.house == 2).count(), 0);
}

/// `[Base]` rebuild: a destroyed base node is rebuilt in list order when the base
/// house owns a construction yard + credits (base.cpp:377 Next_Buildable). The
/// node is placed back on its scripted cell.
#[test]
fn base_house_rebuilds_a_destroyed_node_on_its_cell() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    w.set_house_credits(2, 10_000);
    // Live construction yard (needed to build structures).
    w.spawn_building(B_YARD, 2, CellCoord::new(10, 10)).unwrap();
    // The [Base] node: a war factory at cell (40,40) that is currently NOT built.
    let node_cell = CellCoord::new(40, 40);
    let mut ea = EnemyActivation {
        production: vec![false; 20],
        base_house: 2,
        base_nodes: vec![(B_WEAP, node_cell)],
        tech_level: 4,
        ..Default::default()
    };
    ea.production[2] = true;
    w.set_enemy_activation(ea);

    let had_weap = w
        .buildings
        .iter()
        .any(|(_, b)| b.house == 2 && b.type_id == B_WEAP);
    assert!(!had_weap, "node starts destroyed");
    for _ in 0..1200 {
        w.tick(&[]);
    }
    let rebuilt = w
        .buildings
        .iter()
        .any(|(_, b)| b.house == 2 && b.type_id == B_WEAP && b.cell == node_cell && b.is_alive());
    assert!(
        rebuilt,
        "the destroyed [Base] war factory was rebuilt on its cell"
    );
}

// ===========================================================================
// Hash gating
// ===========================================================================

/// An inactive enemy-activation (no house alerted / started) folds **nothing**
/// into the hash — a campaign that never fires either trigger is byte-identical to
/// one with no enemy-activation at all.
#[test]
fn inactive_enemy_activation_does_not_perturb_the_hash() {
    let mut with = base_world();
    with.set_campaign(empty_campaign());
    with.set_enemy_activation(EnemyActivation {
        alerted: vec![false; 20],
        alert_timer: vec![-1; 20],
        production: vec![false; 20],
        base_house: 9,
        base_nodes: vec![(B_WEAP, CellCoord::new(40, 40))],
        tech_level: 15,
    });

    let mut without = base_world();
    without.set_campaign(empty_campaign());

    assert_eq!(
        with.tick(&[]),
        without.tick(&[]),
        "an inactive enemy-activation must not change the world hash"
    );
}
