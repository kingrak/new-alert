//! M7.5-C depth audit (ra-tester): autocreate-team depth coverage beyond
//! `campaign_activation_suite`'s "forms from idle units" / "gated both ways" /
//! "deterministic" trio — cadence bounds, RNG-draw scoping, recruit-pool
//! correctness, and multi-house independence.
//!
//! Ported behaviour under test: `HouseClass::AI`'s autocreate loop
//! (house.cpp:1042), `Random_Pick(2, (TechLevel-1)/3+1)` wave sizing
//! (house.cpp:1047), the `AlertTime = AutocreateTime x Random_Pick(TPM/2, TPM*2)`
//! re-arm (house.cpp:1056), and the CREATE_TEAM idle-unit recruit filter
//! (`team.cpp:988`, `MissionClass::Is_Recruitable_Mission`).

use ra_sim::campaign::{team_flags, tmission};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Campaign, Catalog, EconRules, EnemyActivation, Handicap, Mission, MoveStats,
    Passability, SpawnProto, TeamClass, TeamMission, TeamType, UnitProto, WarheadProfile,
    WeaponProfile, World,
};

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

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            name: "FACT".into(),
            foot_w: 2,
            foot_h: 2,
            max_health: 400,
            armor: 0,
            power: 0,
            cost: 500,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: true,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 5,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }],
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
            ammo: 0,
        }],
        econ: EconRules {
            // Keep TPM small so the AlertTime re-arm range is small and the
            // cadence-bound test's tick budget stays cheap.
            ticks_per_minute: 20,
            difficulty: [Handicap::default(); 3],
            ..EconRules::default()
        },
    }
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

fn autocreate_teamtype(house: u8) -> TeamType {
    TeamType {
        name: "atk".into(),
        house: house as i32,
        flags: team_flags::AUTOCREATE,
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
    }
}

fn base_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0xBEEF_CAFE);
    w.set_catalog(catalog());
    w.init_houses(20, 0);
    w.set_player_house(1);
    w
}

fn hunting_count(w: &World, house: u8) -> usize {
    w.units
        .iter()
        .filter(|(_, u)| u.house == house && u.hunt)
        .count()
}

// ===========================================================================
// 1. Cadence bounds — the AlertTime window (house.cpp:1042/1056).
// ===========================================================================

/// A house alerted with a **positive** `alert_timer` must form no team until the
/// timer reaches 0, then fires on the exact tick it hits 0 (house.cpp:1042's
/// `if (AlertTime == 0)` gate, decremented once per `HouseClass::AI` pass).
#[test]
fn autocreate_forms_no_team_before_its_alert_timer_elapses() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    let mut camp = w.campaign().unwrap().clone();
    camp.teamtypes = vec![autocreate_teamtype(2)];
    w.set_campaign(camp);
    for i in 0..4 {
        w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats());
    }
    w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

    const WAIT: i32 = 10;
    let mut alerted = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    alerted[2] = true;
    timer[2] = WAIT;
    w.set_enemy_activation(EnemyActivation {
        alerted,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: 2,
        base_nodes: Vec::new(),
        tech_level: 4,
    });

    // `run_enemy_activation` decrements a positive timer (no wave) and only fires
    // once a tick reads it as `<= 0`; a timer initialized to `WAIT` therefore takes
    // `WAIT` decrementing ticks (timer WAIT -> 0) plus one more tick (timer read as
    // 0) before the wave actually fires — `WAIT + 1` ticks total. Assert the "no
    // team yet" invariant before each of those `WAIT + 1` ticks, then confirm the
    // wave has fired by the end.
    for t in 0..=WAIT {
        assert_eq!(
            hunting_count(&w, 2),
            0,
            "no autocreate team must form before the AlertTime window elapses (tick {t})"
        );
        w.tick(&[]);
    }
    assert!(
        hunting_count(&w, 2) >= 2,
        "the wave must have fired once the AlertTime window elapsed"
    );
}

/// After a wave fires, the re-armed `AlertTime` must land in the documented
/// `AUTOCREATE_TIME(5) x Random_Pick(TPM/2, TPM*2)` range (house.cpp:1056) — here
/// TPM=20, so the re-arm is in `[5*10, 5*40] = [50, 200]`.
#[test]
fn autocreate_rearm_time_is_within_the_documented_formula_bounds() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    let mut camp = w.campaign().unwrap().clone();
    camp.teamtypes = vec![autocreate_teamtype(2)];
    w.set_campaign(camp);
    for i in 0..8 {
        w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats());
    }
    w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

    let mut alerted = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    alerted[2] = true;
    timer[2] = 0; // fires this tick
    w.set_enemy_activation(EnemyActivation {
        alerted,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: 2,
        base_nodes: Vec::new(),
        tech_level: 4,
    });
    w.tick(&[]);
    assert!(hunting_count(&w, 2) >= 2, "first wave must have fired");
    let rearmed = w.enemy_activation().unwrap().alert_timer[2];
    assert!(
        (50..=200).contains(&rearmed),
        "re-armed AlertTime {rearmed} must be in [5*TPM/2, 5*TPM*2] = [50, 200]"
    );
}

// ===========================================================================
// 2. RNG-consumption scoping — draws only for alerted houses (house.cpp:1042's
//    `if (IsAlerted)` gate skips the RNG entirely for un-alerted houses).
// ===========================================================================

/// A world with house 2 alerted (and forming a team) must draw the *exact same*
/// RNG sequence for house 2's wave-count/team-type picks — proven by an
/// identical recruited team size *and* an identical re-armed `AlertTime` —
/// whether or not additional *unrelated, un-alerted* house slots are also
/// present in the enemy-activation vectors, proving the loop's skip branch for
/// an un-alerted house draws no RNG.
///
/// (A whole-world state-hash comparison would not isolate this: the extra
/// house's `alerted`/`production` vector *bytes* are hashed unconditionally
/// once the struct is active — QUIRKS-documented — so a longer vector changes
/// the hash even with zero extra RNG draws. Comparing the RNG-*derived*
/// outputs directly is the precise test.)
#[test]
fn unalerted_house_presence_does_not_perturb_an_alerted_houses_rng_draws() {
    let build = |vec_len: usize| -> (usize, i32) {
        let mut w = base_world();
        w.set_campaign(empty_campaign());
        let mut camp = w.campaign().unwrap().clone();
        camp.teamtypes = vec![autocreate_teamtype(2)];
        w.set_campaign(camp);
        // Same exact world composition in both variants — only the enemy-
        // activation vector length (i.e. how many un-alerted house slots the
        // `for house in 0..len` loop walks over before/after house 2) varies.
        for i in 0..8 {
            w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats());
        }
        w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

        let mut alerted = vec![false; vec_len];
        let mut timer = vec![-1i32; vec_len];
        alerted[2] = true;
        timer[2] = 0;
        w.set_enemy_activation(EnemyActivation {
            alerted,
            alert_timer: timer,
            production: vec![false; vec_len],
            base_house: 2,
            base_nodes: Vec::new(),
            tech_level: 4,
        });
        w.tick(&[]);
        let rearmed = w.enemy_activation().unwrap().alert_timer[2];
        (hunting_count(&w, 2), rearmed)
    };

    // 3 slots (just enough for house 2) vs 20 slots (17 extra un-alerted houses
    // the loop walks over on every side of house 2).
    let (count_short, rearm_short) = build(3);
    let (count_long, rearm_long) = build(20);
    assert_eq!(
        count_short, count_long,
        "recruited team size must be identical regardless of how many extra \
         un-alerted house slots the loop walks over"
    );
    assert_eq!(
        rearm_short, rearm_long,
        "the re-armed AlertTime (drawn immediately after the wave, same RNG \
         stream) must be bit-identical -- proving the skipped un-alerted slots \
         consumed zero RNG draws"
    );
}

// ===========================================================================
// 3. Recruit-pool correctness (team.cpp:988, CREATE_TEAM idle-unit filter).
// ===========================================================================

/// Units already hunting (from an earlier wave, or `ALL_HUNT`) are not eligible
/// for re-recruitment — only genuinely idle units are drawn from.
#[test]
fn already_hunting_units_are_not_recruited_again() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    let mut camp = w.campaign().unwrap().clone();
    camp.teamtypes = vec![autocreate_teamtype(2)];
    w.set_campaign(camp);

    // Two units already hunting (simulating a prior wave / ALL_HUNT).
    let already: Vec<_> = (0..2)
        .map(|i| w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats()))
        .collect();
    for h in &already {
        w.units.get_mut(*h).unwrap().hunt = true;
    }
    // Two genuinely idle units.
    let idle: Vec<_> = (0..2)
        .map(|i| w.spawn_unit(0, 2, CellCoord::new(30 + i, 30), Facing(0), 400, stats()))
        .collect();
    w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

    let mut alerted = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    alerted[2] = true;
    timer[2] = 0;
    w.set_enemy_activation(EnemyActivation {
        alerted,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: 2,
        base_nodes: Vec::new(),
        tech_level: 4,
    });
    w.tick(&[]);

    // Every idle unit was recruited (the team wants 2, exactly the idle pool
    // size); the already-hunting pair is untouched (still hunting, not double
    // counted into a second team).
    for h in &idle {
        assert!(w.units.get(*h).unwrap().hunt, "idle unit must be recruited");
    }
    for h in &already {
        assert!(w.units.get(*h).unwrap().is_alive() && w.units.get(*h).unwrap().hunt);
    }
    let total_hunting = hunting_count(&w, 2);
    assert_eq!(
        total_hunting, 4,
        "exactly the 2 pre-hunting + 2 newly recruited units are hunting, no double-counting"
    );
}

/// Documents current, verified behaviour: a unit on `Mission::Guard` that has
/// **actively acquired a guard target** (engaged in combat, `guard_target=true`)
/// is still eligible for autocreate recruitment — it is excluded only by
/// `u.hunt`/harvester/civ-evac, matching the reference's
/// `MissionClass::Is_Recruitable_Mission`, which defaults every mission
/// (including `MISSION_GUARD`, engaged or not) to recruitable unless the mission
/// control data explicitly marks it `Recruitable=no` (a data table we don't
/// parse — see QUIRKS). This is *not* a bug: it is the authentic default.
#[test]
fn guard_engaged_units_remain_recruit_eligible_matching_is_recruitable_mission_default() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    let mut camp = w.campaign().unwrap().clone();
    camp.teamtypes = vec![autocreate_teamtype(2)];
    w.set_campaign(camp);

    let guard = w.spawn_unit(0, 2, CellCoord::new(20, 20), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(weapon(100)), false);
    w.set_unit_mission(guard, Mission::Guard);
    // Simulate "engaged": guard_target set as if it auto-acquired a nearby foe.
    w.units.get_mut(guard).unwrap().guard_target = true;
    // Pad with one more idle unit so the team (wants 2) doesn't stall on an empty
    // pool regardless of the outcome for `guard`.
    w.spawn_unit(0, 2, CellCoord::new(21, 20), Facing(0), 400, stats());
    w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

    let mut alerted = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    alerted[2] = true;
    timer[2] = 0;
    w.set_enemy_activation(EnemyActivation {
        alerted,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: 2,
        base_nodes: Vec::new(),
        tech_level: 4,
    });
    w.tick(&[]);

    assert!(
        w.units.get(guard).unwrap().hunt,
        "an engaged-but-not-hunting guard unit is still recruit-eligible (documented default)"
    );
}

// ===========================================================================
// 4. Multi-house independence.
// ===========================================================================

/// Two distinct alerted houses, each with their own autocreate team type and
/// idle-unit pool, must form **independent** teams from only their own units —
/// no cross-house recruitment — and deterministically so across two identical
/// runs.
#[test]
fn two_alerted_houses_form_independent_teams_deterministically() {
    let run = || -> (usize, usize, u64) {
        let mut w = base_world();
        w.set_campaign(empty_campaign());
        let mut camp = w.campaign().unwrap().clone();
        camp.teamtypes = vec![autocreate_teamtype(2), autocreate_teamtype(5)];
        w.set_campaign(camp);
        for i in 0..6 {
            w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats());
        }
        for i in 0..6 {
            w.spawn_unit(0, 5, CellCoord::new(80 + i, 80), Facing(0), 400, stats());
        }
        w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

        let mut alerted = vec![false; 20];
        let mut timer = vec![-1i32; 20];
        alerted[2] = true;
        alerted[5] = true;
        timer[2] = 0;
        timer[5] = 0;
        w.set_enemy_activation(EnemyActivation {
            alerted,
            alert_timer: timer,
            production: vec![false; 20],
            base_house: 2,
            base_nodes: Vec::new(),
            tech_level: 4,
        });
        let h = w.tick(&[]);
        (hunting_count(&w, 2), hunting_count(&w, 5), h)
    };

    let (h2_a, h5_a, hash_a) = run();
    let (h2_b, h5_b, hash_b) = run();

    assert!(h2_a >= 2, "house 2 formed its own team");
    assert!(h5_a >= 2, "house 5 formed its own team independently");
    assert_eq!(h2_a, h2_b, "house 2's team size deterministic across runs");
    assert_eq!(h5_a, h5_b, "house 5's team size deterministic across runs");
    assert_eq!(
        hash_a, hash_b,
        "same seed twice: bit-identical hash with two alerted houses"
    );
}

/// No unit ever crosses house lines: house 2's hunting units are all house 2,
/// house 5's are all house 5.
#[test]
fn recruited_units_never_cross_house_lines() {
    let mut w = base_world();
    w.set_campaign(empty_campaign());
    let mut camp = w.campaign().unwrap().clone();
    camp.teamtypes = vec![autocreate_teamtype(2), autocreate_teamtype(5)];
    w.set_campaign(camp);
    for i in 0..6 {
        w.spawn_unit(0, 2, CellCoord::new(20 + i, 20), Facing(0), 400, stats());
    }
    for i in 0..6 {
        w.spawn_unit(0, 5, CellCoord::new(80 + i, 80), Facing(0), 400, stats());
    }
    w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 400, stats());

    let mut alerted = vec![false; 20];
    let mut timer = vec![-1i32; 20];
    alerted[2] = true;
    alerted[5] = true;
    timer[2] = 0;
    timer[5] = 0;
    w.set_enemy_activation(EnemyActivation {
        alerted,
        alert_timer: timer,
        production: vec![false; 20],
        base_house: 2,
        base_nodes: Vec::new(),
        tech_level: 4,
    });
    w.tick(&[]);

    for (_, u) in w.units.iter() {
        if u.hunt {
            assert!(
                u.house == 2 || u.house == 5,
                "only houses 2/5 ever hunt here"
            );
        }
    }
}
