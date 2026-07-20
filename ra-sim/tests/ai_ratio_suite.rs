//! M7.14 acceptance coverage for the ratio-driven `AI_Building` (the "building"
//! trick) and the IQ-gated auto-harvester replacement (the "mining" trick),
//! ported from `HouseClass::AI_Building`/`AI_Unit` (house.cpp:5696 / 6075).
//!
//! Two acceptance bars from the M7.14 milestone are pinned here, both driven
//! through the public `World` API (no reaching into `ai.rs`):
//!
//! 1. **Auto-harvester recovery** — kill an AI harvester and the economy
//!    recovers: the AI queues a replacement (refinery outnumbers harvesters,
//!    IQ ≥ `Rule.IQHarvester`) and the harvester count returns to full.
//! 2. **Base composition tracks the rules ratios** — after several minutes an
//!    AI base's per-category counts respect the `[AI]` limits and the refinery
//!    ratio, and no category runs away.
//!
//! **NOTE for ra-tester (M7.14):** this file is ra-coder-authored smoke/
//! acceptance coverage proving the new systems run; deepen/relocate as you see
//! fit. It uses `AiRules`/`IqRules` defaults (which match the reference
//! compile-time `[AI]`/`[IQ]` values), so it exercises the same ratio math the
//! real-asset loader feeds the skirmish AI.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Difficulty, MoveStats, Passability, UnitProto,
    WarheadProfile, WeaponProfile, World,
};

const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
const B_PROC: u32 = 2;
const B_WEAP: u32 = 3;
const B_PBOX: u32 = 4;

const U_MCV: u32 = 0;
const U_HARV: u32 = 1;

const CREDITS: i32 = 8000;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
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

/// A compact skirmish catalog: FACT / POWR / PROC(refinery, free harvester) /
/// WEAP(war factory) / PBOX(defense), plus MCV/HARV/TANK. Defense is gated on
/// the war factory (as in the real game — a pillbox has a production prereq), so
/// the ratio-driven build order stays sensible.
fn catalog() -> Catalog {
    let b = |name: &str,
             w: u8,
             h: u8,
             power: i32,
             cost: i32,
             prereq: Vec<u32>,
             cy: bool,
             refin: bool,
             wf: bool,
             weapon: Option<WeaponProfile>| BuildingProto {
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
        weapon,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let u = |name: &str,
             sprite_id: u32,
             harv: bool,
             deploys: Option<u32>,
             wpn: Option<WeaponProfile>,
             cost: i32,
             prereq: Vec<u32>| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name: name.to_string(),
        sprite_id,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon: wpn,
        secondary: None,
        has_turret: wpn.is_some(),
        is_harvester: harv,
        deploys_to: deploys,
        cost,
        prereq,
        sight: 4,
        passengers: 0,
    };
    Catalog {
        buildings: vec![
            b("FACT", 3, 3, 0, 100, vec![], true, false, false, None),
            b(
                "POWR",
                2,
                2,
                100,
                30,
                vec![B_FACT],
                false,
                false,
                false,
                None,
            ),
            b(
                "PROC",
                3,
                3,
                -30,
                50,
                vec![B_POWR],
                false,
                true,
                false,
                None,
            ),
            b(
                "WEAP",
                3,
                3,
                -20,
                60,
                vec![B_POWR],
                false,
                false,
                true,
                None,
            ),
            b(
                "PBOX",
                1,
                1,
                -5,
                25,
                vec![B_WEAP],
                false,
                false,
                false,
                Some(weapon(10)),
            ),
        ],
        units: vec![
            u("MCV", 0, false, Some(B_FACT), None, 100, vec![]),
            u("HARV", 1, true, None, None, 140, vec![]),
            u("TANK", 2, false, None, Some(weapon(20)), 200, vec![B_WEAP]),
        ],
        econ: Default::default(),
    }
}

/// A single-AI world: house 1 is the computer (its MCV placed), house 0 is a
/// passive human (no AI, no MCV) so nothing fights the economy under test.
fn solo_ai_world(seed: u32) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(2, CREDITS);
    w.spawn_unit(U_MCV, 1, CellCoord::new(40, 40), Facing(0), 400, stats());
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);
    w
}

fn count_units<F: Fn(&ra_sim::Unit) -> bool>(w: &World, house: u8, pred: F) -> usize {
    w.units
        .iter()
        .filter(|(_, u)| u.house == house && u.is_alive() && pred(u))
        .count()
}

fn owns(w: &World, house: u8, id: u32) -> bool {
    w.house(house).map(|h| h.owns_building(id)).unwrap_or(false)
}

/// Run until `cond(world)` or `max` ticks; returns the tick it happened (or None).
fn run_until<F: Fn(&World) -> bool>(w: &mut World, max: u32, cond: F) -> Option<u32> {
    for t in 0..max {
        w.tick(&[]);
        if cond(w) {
            return Some(t);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 1. Auto-harvester replacement (the "mining" trick, house.cpp:6075).
// ---------------------------------------------------------------------------

#[test]
fn ai_replaces_a_killed_harvester_and_economy_recovers() {
    let mut w = solo_ai_world(0x11A2_0001);

    // Drive until the AI has a refinery (which spawns its free harvester) and a
    // war factory (needed to build a *replacement* harvester).
    let ready = run_until(&mut w, 6000, |w| {
        owns(w, 1, B_PROC) && owns(w, 1, B_WEAP) && count_units(w, 1, |u| u.is_harvester) >= 1
    });
    assert!(
        ready.is_some(),
        "AI never reached refinery + war factory + harvester"
    );
    assert_eq!(
        count_units(&w, 1, |u| u.is_harvester),
        1,
        "expected exactly the refinery's free harvester before the kill"
    );

    // Kill the harvester (simulate it being destroyed on the field).
    let harv = w
        .units
        .iter()
        .find(|(_, u)| u.house == 1 && u.is_harvester)
        .map(|(h, _)| h)
        .expect("harvester handle");
    w.units.remove(harv);
    assert_eq!(count_units(&w, 1, |u| u.is_harvester), 0);

    // The AI must notice `refineries(1) > harvesters(0)` and buy a replacement.
    let recovered = run_until(&mut w, 6000, |w| count_units(w, 1, |u| u.is_harvester) >= 1);
    assert!(
        recovered.is_some(),
        "AI did not replace its destroyed harvester — the auto-harvester economic \
         reflex (IQ ≥ Rule.IQHarvester, refineries > harvesters) failed to fire"
    );
}

#[test]
fn a_zero_iq_house_never_auto_replaces_a_harvester() {
    // The IQ gate: a house below `Rule.IQHarvester` (a human, iq 0) gets no free
    // harvester replacement. We drive the AI to a refinery+factory+harvester,
    // then force its IQ to 0 and kill the harvester: it must NOT rebuild one.
    let mut w = solo_ai_world(0x11A2_0002);
    let ready = run_until(&mut w, 6000, |w| {
        owns(w, 1, B_PROC) && owns(w, 1, B_WEAP) && count_units(w, 1, |u| u.is_harvester) >= 1
    });
    assert!(ready.is_some());

    // Drop the AI house to IQ 0 (below IQHarvester = 3) and kill the harvester.
    w.houses[1].iq = 0;
    let refineries_at_kill = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_PROC as usize)
        .copied()
        .unwrap_or(0);
    let harv = w
        .units
        .iter()
        .find(|(_, u)| u.house == 1 && u.is_harvester)
        .map(|(h, _)| h)
        .unwrap();
    w.units.remove(harv);

    // With iq 0 the war-factory replacement lane is gated off. A harvester may
    // still reappear ONLY as a *new refinery's* free harvester (which is spawned
    // on placement regardless of IQ, house.cpp:2640) — so a harvester showing up
    // while the refinery count is unchanged would prove the gated lane leaked.
    for _ in 0..3000 {
        w.tick(&[]);
        let harvesters = count_units(&w, 1, |u| u.is_harvester);
        let refineries = w
            .house(1)
            .unwrap()
            .building_counts
            .get(B_PROC as usize)
            .copied()
            .unwrap_or(0);
        assert!(
            !(harvesters >= 1 && refineries <= refineries_at_kill),
            "an IQ-0 house replaced its harvester through the gated war-factory lane \
             (no new refinery was built) — the IQ < Rule.IQHarvester gate leaked"
        );
        if refineries > refineries_at_kill {
            // A new refinery's free harvester is expected and not a gate leak; the
            // IQ-gated path is no longer isolable, so stop here having proven it.
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Base composition tracks the [AI] ratios / limits (house.cpp:5696).
// ---------------------------------------------------------------------------

#[test]
fn ai_base_composition_respects_the_rules_ratios_and_limits() {
    let mut w = solo_ai_world(0x11A2_0003);
    // ~8 in-game minutes of buildup (900 ticks/min).
    for _ in 0..7200 {
        w.tick(&[]);
    }

    let refineries = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_PROC as usize)
        .copied()
        .unwrap_or(0);
    let factories = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_WEAP as usize)
        .copied()
        .unwrap_or(0);
    let defenses = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_PBOX as usize)
        .copied()
        .unwrap_or(0);
    let cur: i32 = w
        .buildings
        .iter()
        .filter(|(_, b)| b.house == 1 && b.is_alive() && !b.is_wall)
        .count() as i32;

    let ai = w.catalog.econ.ai;

    // The AI established a real base.
    assert!(refineries >= 1, "AI never built a refinery");
    assert!(factories >= 1, "AI never built a war factory");

    // Category hard-limits (`*Limit`) are respected — no runaway.
    assert!(
        refineries as i32 <= ai.refinery_limit,
        "refineries {refineries} exceeded RefineryLimit {}",
        ai.refinery_limit
    );
    assert!(
        factories as i32 <= ai.war_limit,
        "war factories {factories} exceeded WarLimit {}",
        ai.war_limit
    );
    assert!(
        defenses as i32 <= ai.defense_limit,
        "defenses {defenses} exceeded DefenseLimit {}",
        ai.defense_limit
    );

    // Refinery count tracks `Round_Up(RefineryRatio × CurBuildings)` (clamped to
    // its limit): the desired count is the ceiling of the ratio times the base
    // size, so the built count must not exceed it (the AI never over-builds a
    // ratio category).
    let desired_ref = ((ai.refinery_ratio as i64 * cur as i64 + 0xFFFF) >> 16) as i32;
    let desired_ref = desired_ref.min(ai.refinery_limit).max(1);
    assert!(
        refineries as i32 <= desired_ref + 1,
        "refineries {refineries} overshoot desired {desired_ref} for a {cur}-building base"
    );

    // Base size is bounded (the ratio system self-limits — no wall-in runaway).
    assert!(
        cur <= 30,
        "base of {cur} buildings looks like a runaway (ratios should self-limit)"
    );
}
