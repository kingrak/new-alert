//! M7.17-B audit (ra-tester): the AI genuinely fields **air** now.
//!
//! M7.17-A deliberately cut AI-air (helis excluded from the vehicle lane, AA
//! excluded from ground defense). M7.17-B re-enabled it: a helipad is built per
//! `HelipadRatio` once the economy runs and a war factory is owned, and helis —
//! gated on the helipad by their `Prerequisite` — re-enter the weighted-random
//! production pool. This suite proves the re-enable is real (not silently dead):
//! a genuine AI-vs-AI skirmish builds a helipad and produces at least one
//! aircraft within a bounded tick budget.
//!
//! The coder reported first heli ~t10000, peak 6. We pin a deliberately weaker
//! bound — helipad built AND >= 1 aircraft alive by t15000 — so the test asserts
//! "air is fielded" without being brittle to exact production timing.
//!
//! Harness mirrors `ai_suite.rs`: a small local catalog, two AIs planted in
//! opposite corners, driven through the public `World` API. Adds a helipad
//! building and a HELI aircraft proto (Air locomotor, HPAD prerequisite, a full
//! magazine) so the AI's air path has something to build.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Difficulty, EconRules, MoveStats, Passability, UnitProto,
    WarheadProfile, WeaponProfile, World, LOCO_AIR_INDEX,
};

// Building ids (declaration order in `catalog()`; id 2 = PROC/refinery).
const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
const B_WEAP: u32 = 3;
const B_HPAD: u32 = 4;

// Unit-proto ids (id 2 = TANK, id 3 = HELI).
const U_MCV: u32 = 0;
const U_HARV: u32 = 1;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn air_stats() -> MoveStats {
    MoveStats {
        max_speed: 60,
        rot: 20,
    }
}

fn weapon(damage: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof: 30,
        range: 5 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
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
        max_damage: 1000,
    }
}

#[allow(clippy::too_many_arguments)]
fn catalog() -> Catalog {
    let bproto = |name: &str,
                  w: u8,
                  h: u8,
                  power: i32,
                  cost: i32,
                  prereq: Vec<u32>,
                  cy: bool,
                  refin: bool,
                  wf: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor: 0,
        power,
        cost,
        prereq,
        is_refinery: refin,
        is_construction_yard: cy,
        is_war_factory: wf,
        free_harvester_unit: if refin { Some(U_HARV) } else { None },
        sight: 5,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let uproto = |name: &str,
                  loco: u8,
                  harv: bool,
                  deploys: Option<u32>,
                  weapon: Option<WeaponProfile>,
                  cost: i32,
                  prereq: Vec<u32>,
                  ammo: u16| UnitProto {
        is_infantry: false,
        locomotor: loco,
        name: name.to_string(),
        sprite_id: 0,
        max_health: 400,
        stats: if loco == LOCO_AIR_INDEX {
            air_stats()
        } else {
            stats()
        },
        armor: 0,
        weapon,
        secondary: None,
        has_turret: weapon.is_some(),
        is_harvester: harv,
        deploys_to: deploys,
        cost,
        prereq,
        sight: 4,
        passengers: 0,
        ammo,
    };
    Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 0, 100, vec![], true, false, false),
            bproto("POWR", 2, 2, 100, 30, vec![B_FACT], false, false, false),
            bproto("PROC", 3, 3, -30, 50, vec![B_POWR], false, true, false),
            bproto("WEAP", 3, 3, -20, 60, vec![B_POWR], false, false, true),
            // Helipad: identified by name; needs a war factory present (economy gate).
            bproto("HPAD", 2, 2, -10, 60, vec![B_WEAP], false, false, false),
        ],
        units: vec![
            uproto("MCV", 1, false, Some(B_FACT), None, 100, vec![], 0),
            uproto("HARV", 2, true, None, None, 140, vec![], 0),
            uproto(
                "TANK",
                1,
                false,
                None,
                Some(weapon(25)),
                80,
                vec![B_WEAP],
                0,
            ),
            // HELI: aircraft (Air locomotor), gated on a helipad, 3-round magazine.
            uproto(
                "HELI",
                LOCO_AIR_INDEX,
                false,
                None,
                Some(weapon(30)),
                90,
                vec![B_HPAD],
                3,
            ),
        ],
        econ: EconRules::default(),
    }
}

/// Defense-scenario catalog: the economy plus a real **ground** pillbox (PBOX)
/// and an **anti-air** gun (AGUN), with AGUN declared LAST so a naive
/// "strongest buildable armed building, reverse catalog order" pick would grab
/// the AA first. This is the exact shape that produced the M7.17-A stall
/// (AA-as-ground-defense). Used by the stall-guard test.
fn defense_catalog() -> Catalog {
    let mut cat = catalog();
    let defense = |name: &str, weapon: WeaponProfile| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 1,
        foot_h: 1,
        max_health: 400,
        armor: 0,
        power: -5,
        cost: 40,
        prereq: vec![B_POWR],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon: Some(weapon),
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    // A PURE-GROUND catalog: drop the helipad AND the heli aircraft proto (so no
    // air ever exists → `enemy_air_threat` stays false), then add a real ground
    // pillbox and an AA gun (AGUN LAST → first in reverse catalog order, so an
    // un-guarded "strongest defense" pick would grab it).
    cat.buildings = vec![
        cat.buildings[B_FACT as usize].clone(),
        cat.buildings[B_POWR as usize].clone(),
        cat.buildings[2].clone(), // PROC
        cat.buildings[B_WEAP as usize].clone(),
        defense("PBOX", weapon(30)),
        defense("AGUN", weapon(30)),
    ];
    // Units: MCV/HARV/TANK only (index 3 = HELI dropped).
    cat.units.truncate(3);
    cat
}

// Building ids in `defense_catalog()` (no HPAD; PBOX then AGUN appended).
const D_PBOX: u32 = 4;
const D_AGUN: u32 = 5;

const CREDITS: i32 = 8000;

fn skirmish(seed: u32, difficulty: Difficulty) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, CREDITS);
    w.spawn_unit(U_MCV, 1, CellCoord::new(15, 15), Facing(0), 400, stats());
    w.spawn_unit(U_MCV, 2, CellCoord::new(110, 110), Facing(0), 400, stats());
    w.set_ai(vec![
        AiPlayer::new(1, difficulty),
        AiPlayer::new(2, difficulty),
    ]);
    w
}

/// Count live aircraft owned by `house`.
fn aircraft_count(w: &World, house: u8) -> usize {
    w.units
        .iter()
        .filter(|(_, u)| u.house == house && u.is_alive() && u.is_aircraft())
        .count()
}

#[test]
fn ai_builds_a_helipad_and_fields_at_least_one_aircraft() {
    const MAX_TICKS: u32 = 15_000;
    let mut w = skirmish(0xA1_A100, Difficulty::Hard);

    let mut helipad_tick: Option<u32> = None;
    let mut first_air_tick: Option<u32> = None;
    let mut peak_air = 0usize;

    for t in 0..MAX_TICKS {
        w.tick(&[]);
        // Helipad built by either AI house.
        if helipad_tick.is_none() {
            let any_pad = w.buildings.iter().any(|(_, b)| {
                b.is_alive() && (b.house == 1 || b.house == 2) && b.type_id == B_HPAD
            });
            if any_pad {
                helipad_tick = Some(t);
            }
        }
        let air = aircraft_count(&w, 1) + aircraft_count(&w, 2);
        peak_air = peak_air.max(air);
        if first_air_tick.is_none() && air >= 1 {
            first_air_tick = Some(t);
        }
        // Early out once both milestones are met.
        if helipad_tick.is_some() && first_air_tick.is_some() {
            break;
        }
    }

    let pad = helipad_tick.expect(
        "an AI house must build a helipad within 15000 ticks (AI-air re-enabled in M7.17-B)",
    );
    let air =
        first_air_tick.expect("an AI house must produce at least one aircraft within 15000 ticks");
    assert!(
        pad <= air,
        "the helipad (tick {pad}) must precede the aircraft it produces (tick {air})"
    );
    eprintln!(
        "AI-AIR: first helipad @t{pad}, first aircraft @t{air}, peak concurrent air {peak_air}"
    );
}

/// **Stall-guard** (task 2a, revert-sensitivity pin). In a pure-ground symmetric
/// skirmish where an AA gun (AGUN) is available and declared strongest-in-reverse
/// order, the Expert AI must build a REAL ground defense (PBOX) and must NOT
/// build the useless AGUN — because (i) `is_air_only_defense` excludes AA from
/// the ground-defense pick, and (ii) `enemy_air_threat` is false (no house owns
/// aircraft here), so the separate AA category never fires.
///
/// This encodes the M7.17-B guard directly: reverting EITHER half — letting AA
/// be picked as ground defense again, or dropping the threat gate — makes the AI
/// build AGUN instead of / as well as PBOX and fails this test. That is the
/// load-bearing property the guard exists to protect (M7.17-A stalled here).
#[test]
fn ai_ground_defense_never_wastes_production_on_anti_air_without_an_air_threat() {
    const MAX_TICKS: u32 = 5_000;
    let mut w = World::new(Passability::all_passable(), 0xDEF_0001);
    w.set_catalog(defense_catalog());
    w.init_houses(3, CREDITS);
    w.spawn_unit(U_MCV, 1, CellCoord::new(15, 15), Facing(0), 400, stats());
    w.spawn_unit(U_MCV, 2, CellCoord::new(110, 110), Facing(0), 400, stats());
    w.set_ai(vec![
        AiPlayer::new(1, Difficulty::Hard),
        AiPlayer::new(2, Difficulty::Hard),
    ]);

    let owned = |w: &World, house: u8, id: u32| -> usize {
        w.buildings
            .iter()
            .filter(|(_, b)| b.is_alive() && b.house == house && b.type_id == id)
            .count()
    };

    let mut pbox_built = false;
    for _ in 0..MAX_TICKS {
        w.tick(&[]);
        // The invariant that must hold at EVERY tick: no AGUN is ever built by an
        // AI house (no air threat exists → the AA category must stay dormant).
        for h in [1u8, 2u8] {
            assert_eq!(
                owned(&w, h, D_AGUN),
                0,
                "house {h} built an anti-air gun with no enemy air present — the \
                 threat gate / air-only-defense exclusion has regressed (the \
                 M7.17-A stall)"
            );
        }
        if owned(&w, 1, D_PBOX) > 0 || owned(&w, 2, D_PBOX) > 0 {
            pbox_built = true;
        }
    }
    assert!(
        pbox_built,
        "an AI house must build a real GROUND defense (PBOX) within budget — the \
         defense category must resolve to the pillbox, not stall on the AA gun"
    );
    eprintln!(
        "STALL GUARD: AI built ground defense (PBOX), zero AA guns despite AGUN \
         being available & strongest-in-reverse — guard holds"
    );
}

/// Determinism guard: the same-seed AI-vs-AI air skirmish hashes identically
/// twice (the aircraft/helipad path draws sim RNG in a fixed order).
#[test]
fn ai_air_skirmish_is_deterministic_same_seed_twice() {
    let run = || -> Vec<u64> {
        let mut w = skirmish(0xA1_A1DD, Difficulty::Hard);
        let mut hs = Vec::with_capacity(2500);
        for _ in 0..2500 {
            hs.push(w.tick(&[]));
        }
        hs
    };
    assert_eq!(
        run(),
        run(),
        "same-seed AI air skirmish must hash identically"
    );
}
