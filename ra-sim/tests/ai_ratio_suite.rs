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
    AiPlayer, BuildingProto, Catalog, Command, Difficulty, MoveStats, Passability, UnitProto,
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
    solo_ai_world_with(seed, |_| {})
}

/// As [`solo_ai_world`] but lets the caller mutate the catalog (e.g. cap
/// `refinery_limit` or tweak the `[IQ]` thresholds) before the world is built.
fn solo_ai_world_with<F: FnOnce(&mut Catalog)>(seed: u32, mutate: F) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    let mut cat = catalog();
    mutate(&mut cat);
    w.set_catalog(cat);
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

// ---------------------------------------------------------------------------
// 3. Auto-harvester replacement, ISOLATED (revert-sensitive).
//
// ra-tester audit note (M7.14): the recovery test above passes even with the
// war-factory replacement lane disabled, because the ratio system builds a 2nd
// refinery whose *free* placement harvester (house.cpp:2640, IQ-ungated) refills
// the count — so that test does not actually pin the replacement feature. These
// tests cap `RefineryLimit = 1` so no 2nd refinery can be built: the ONLY path
// back to a harvester is the AI_Unit war-factory lane (house.cpp:6075), making
// them fail if that lane is removed.
// ---------------------------------------------------------------------------

/// Verified revert-sensitive: with `iq_ok` forced false in `produce_units`, this
/// FAILS (harvester never returns); it passes only because the war-factory
/// replacement lane fires. Refinery count must stay at 1 throughout (proving the
/// recovery was NOT a new refinery's free harvester).
#[test]
fn killed_harvester_recovery_is_via_the_war_factory_lane_not_a_new_refinery() {
    let mut w = solo_ai_world_with(0x11A2_0011, |c| c.econ.ai.refinery_limit = 1);
    let ready = run_until(&mut w, 6000, |w| {
        owns(w, 1, B_PROC) && owns(w, 1, B_WEAP) && count_units(w, 1, |u| u.is_harvester) >= 1
    });
    assert!(
        ready.is_some(),
        "AI never reached refinery + war factory + harvester"
    );

    let ref_at_kill = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_PROC as usize)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        ref_at_kill, 1,
        "RefineryLimit=1 should hold the AI to one refinery"
    );

    let harv = w
        .units
        .iter()
        .find(|(_, u)| u.house == 1 && u.is_harvester)
        .map(|(h, _)| h)
        .expect("harvester handle");
    w.units.remove(harv);

    let recovered = run_until(&mut w, 6000, |w| count_units(w, 1, |u| u.is_harvester) >= 1);
    assert!(
        recovered.is_some(),
        "AI did not replace its harvester through the war-factory lane (no 2nd refinery \
         was available as a fallback) — the auto-harvester reflex failed to fire"
    );
    // The refinery count never changed: the recovery is the replacement lane, not
    // a new refinery's free harvester.
    let ref_after = w
        .house(1)
        .unwrap()
        .building_counts
        .get(B_PROC as usize)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        ref_after, ref_at_kill,
        "refinery count changed during recovery — the free-placement harvester, not the \
         replacement lane, provided the harvester (test would not be isolating the feature)"
    );
}

/// The IQ gate boundary (`IQ >= Rule.IQHarvester`, house.cpp:6075). Exactly at
/// the threshold the lane fires; one below it does not. Runs the same scenario at
/// `iq == IQHarvester` (must recover) and `iq == IQHarvester - 1` (must not),
/// with `RefineryLimit = 1` so the war-factory lane is the only recovery path.
#[test]
fn harvester_replacement_fires_exactly_at_the_iq_harvester_threshold() {
    let run = |iq: i32| -> bool {
        let mut w = solo_ai_world_with(0x11A2_0012 ^ iq as u32, |c| c.econ.ai.refinery_limit = 1);
        let threshold = w.catalog.econ.iq.harvester;
        assert!(threshold >= 1, "IQHarvester default should be >= 1");
        let ready = run_until(&mut w, 6000, |w| {
            owns(w, 1, B_PROC) && owns(w, 1, B_WEAP) && count_units(w, 1, |u| u.is_harvester) >= 1
        });
        assert!(
            ready.is_some(),
            "AI never reached refinery + war factory + harvester"
        );
        // Pin the house's IQ to the exact test value, then kill the harvester.
        w.houses[1].iq = iq;
        let harv = w
            .units
            .iter()
            .find(|(_, u)| u.house == 1 && u.is_harvester)
            .map(|(h, _)| h)
            .unwrap();
        w.units.remove(harv);
        run_until(&mut w, 6000, |w| count_units(w, 1, |u| u.is_harvester) >= 1).is_some()
    };

    let threshold = catalog().econ.iq.harvester;
    assert!(
        run(threshold),
        "at IQ == IQHarvester ({threshold}) the replacement lane must fire"
    );
    assert!(
        !run(threshold - 1),
        "at IQ == IQHarvester-1 ({}) the replacement lane must be gated off",
        threshold - 1
    );
}

// ---------------------------------------------------------------------------
// 4. Scatter IQ gate (crux): the movement-deadlock scatter site is the forced
//    `nokidding == true` variant (drive.cpp:1090/1214), so a ZERO-IQ house's
//    unit is still scattered out of a committed mover's landing cell. This pins
//    that the forced dock-nudge is IQ-independent (the Q5 human-harvester case).
// ---------------------------------------------------------------------------

/// A parked, path-empty friendly blocker owned by an explicitly IQ-0 house is
/// still forced out of a mover's sole landing cell. (The broader M7.13 scatter
/// suites cover the geometry; this one nails the IQ-independence explicitly by
/// setting `house.iq = 0` and asserting the blocker still moves.)
#[test]
fn a_zero_iq_blocker_is_still_force_scattered_from_a_committed_movers_cell() {
    // 1-wide horizontal corridor: row 2 of a 16×5 grid passable, all else walled.
    let (len, h) = (16i32, 5i32);
    let row = h / 2;
    let mut cells = vec![false; (len * h) as usize];
    for x in 0..len {
        cells[(row * len + x) as usize] = true;
    }
    let mut w = World::new(Passability::new(len, h, cells), 0x5CA7_7E20);
    w.set_catalog(catalog());
    w.init_houses(2, 0);
    // Both units belong to house 1; force house 1 to IQ 0 (a "human").
    w.houses[1].iq = 0;
    // Blocker parked mid-corridor (no order -> stationary), mover heading east.
    let blocker = w.spawn_unit(U_MCV, 1, CellCoord::new(8, row), Facing(0), 400, stats());
    let mover = w.spawn_unit(U_MCV, 1, CellCoord::new(1, row), Facing(64), 400, stats());
    w.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);

    let start = w.units.get(blocker).map(|u| u.cell()).unwrap();
    let mut blocker_moved = false;
    for _ in 0..400 {
        w.tick(&[]);
        if w.units
            .get(blocker)
            .map(|u| u.cell() != start)
            .unwrap_or(false)
        {
            blocker_moved = true;
            break;
        }
    }
    assert!(
        blocker_moved,
        "an IQ-0 house's parked blocker was NOT force-scattered — the deadlock scatter \
         site must be the nokidding=true forced variant (IQ-independent), cell.cpp:2025"
    );
}

// ---------------------------------------------------------------------------
// 5. AiProfile A/B knob (M7.15) — a fast, default-running smoke that the
//    Expert-vs-Legacy profile switch is genuinely LIVE (both branches
//    reachable), so the ~4-min real-asset A/B
//    (`ui_ai_vs_ai::real_expert_vs_legacy_ai_ab_record`) can stay `#[ignore]`d.
// ---------------------------------------------------------------------------

/// The crisp, deterministic divergence between the two A/B profiles: the
/// auto-harvester replacement gate. Below `IQHarvester`, the shipping **Expert**
/// policy gates replacement OFF (M7.14 fidelity), while **Legacy** (the verbatim
/// pre-M7.14 baseline) replaces a lost harvester *unconditionally*. On the same
/// seed/catalog/sub-threshold IQ, Legacy re-mines and Expert does not — proving
/// both `AiProfile` branches are exercised by default, without the 4-min real
/// A/B. (Complements the Expert-only `harvester_replacement_fires_exactly_at_the
/// _iq_harvester_threshold` above with its Legacy counterpart.)
#[test]
fn ab_profile_knob_is_live_legacy_replaces_a_sub_iq_harvester_but_expert_does_not() {
    let drive = |profile: ra_sim::AiProfile| -> bool {
        let mut w = World::new(Passability::all_passable(), 0x11A2_0AB0);
        let mut cat = catalog();
        cat.econ.ai.refinery_limit = 1;
        w.set_catalog(cat);
        w.init_houses(2, CREDITS);
        w.spawn_unit(U_MCV, 1, CellCoord::new(40, 40), Facing(0), 400, stats());
        w.set_ai(vec![
            AiPlayer::new(1, Difficulty::Normal).with_profile(profile)
        ]);
        let threshold = w.catalog.econ.iq.harvester;
        let ready = run_until(&mut w, 6000, |w| {
            owns(w, 1, B_PROC) && owns(w, 1, B_WEAP) && count_units(w, 1, |u| u.is_harvester) >= 1
        });
        assert!(
            ready.is_some(),
            "AI ({profile:?}) never reached refinery + war factory + harvester"
        );
        // Pin IQ one below the harvester threshold — the gate Expert fails and
        // Legacy ignores — then kill the harvester and see who re-mines.
        w.houses[1].iq = threshold - 1;
        let harv = w
            .units
            .iter()
            .find(|(_, u)| u.house == 1 && u.is_harvester)
            .map(|(h, _)| h)
            .unwrap();
        w.units.remove(harv);
        run_until(&mut w, 6000, |w| count_units(w, 1, |u| u.is_harvester) >= 1).is_some()
    };
    assert!(
        drive(ra_sim::AiProfile::Legacy),
        "Legacy profile must replace a lost harvester UNCONDITIONALLY (sub-IQ) — \
         the pre-M7.14 baseline behaviour"
    );
    assert!(
        !drive(ra_sim::AiProfile::Expert),
        "Expert profile must NOT replace the harvester below IQHarvester — the \
         M7.14 gate; the two profiles must diverge here (knob is live)"
    );
}
