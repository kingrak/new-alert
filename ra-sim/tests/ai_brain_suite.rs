//! Audit coverage (ra-tester, post-M7.10): deep unit-level coverage for the
//! M7.10 AI brain (`ra-sim/src/ai.rs`) that `ai_suite.rs`'s behavioural/
//! showcase tests don't pin — hand-computed scoring, the rubber-band cap's
//! ratchet property, the team decimation threshold's exact boundary,
//! fire-sale gating in both directions, the raise-money priority order, and
//! kill-tally attribution (including splash, and same-seed determinism across
//! *mixed* difficulties). Drives everything through the public `World`/
//! `AiPlayer`/`Command` API, per the ra-tester charter.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Command, Difficulty, EconRules, Handle, MoveStats,
    Passability, Target, UnitProto, WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Shared fixture (mirrors `ai_suite.rs`'s catalog/skirmish pattern, plus two
// non-essential "defense" building types for the raise-money priority test).
// ===========================================================================

const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
#[allow(dead_code)]
const B_PROC: u32 = 2;
const B_WEAP: u32 = 3;
const B_DEF1: u32 = 4; // non-essential (no power/refinery/wf/barracks) — lower id
const B_DEF2: u32 = 5; // non-essential, higher id — must be sold first

const U_MCV: u32 = 0;
const U_HARV: u32 = 1;
const U_TANK: u32 = 2;
#[allow(dead_code)]
const U_ARTY: u32 = 3;

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
                  sprite_id: u32,
                  harv: bool,
                  deploys: Option<u32>,
                  weapon: Option<WeaponProfile>,
                  cost: i32,
                  prereq: Vec<u32>| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name: name.to_string(),
        sprite_id,
        max_health: 400,
        stats: stats(),
        armor: 0,
        weapon,
        secondary: None,
        has_turret: weapon.is_some(),
        is_harvester: harv,
        deploys_to: deploys,
        cost,
        prereq,
        sight: 4,
    };
    Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 0, 100, vec![], true, false, false),
            bproto("POWR", 2, 2, 100, 30, vec![B_FACT], false, false, false),
            bproto("PROC", 3, 3, -30, 50, vec![B_POWR], false, true, false),
            bproto("WEAP", 3, 3, -20, 60, vec![B_POWR], false, false, true),
            bproto("DEF1", 1, 1, 0, 40, vec![B_POWR], false, false, false),
            bproto("DEF2", 1, 1, 0, 45, vec![B_POWR], false, false, false),
        ],
        units: vec![
            uproto("MCV", 0, false, Some(B_FACT), None, 100, vec![]),
            uproto("HARV", 1, true, None, None, 140, vec![]),
            uproto("TANK", 2, false, None, Some(weapon(25)), 80, vec![B_WEAP]),
            uproto("ARTY", 3, false, None, Some(weapon(40)), 90, vec![B_WEAP]),
        ],
        econ: EconRules::default(),
    }
}

fn home1() -> CellCoord {
    CellCoord::new(15, 15)
}
fn home2() -> CellCoord {
    CellCoord::new(110, 110)
}

const CREDITS: i32 = 6000;

fn skirmish_mixed(seed: u32, diff_a: Difficulty, diff_b: Difficulty) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, CREDITS);
    w.spawn_unit(U_MCV, 1, home1(), Facing(0), 400, stats());
    w.spawn_unit(U_MCV, 2, home2(), Facing(0), 400, stats());
    w.set_ai(vec![AiPlayer::new(1, diff_a), AiPlayer::new(2, diff_b)]);
    w
}

fn skirmish(seed: u32, difficulty: Difficulty) -> World {
    skirmish_mixed(seed, difficulty, difficulty)
}

// ===========================================================================
// 1. Expert_AI enemy scoring: a hand-built, three-candidate scenario. Every
//    candidate is a single 1×1 building (so `army`/`building`/`infantry`
//    terms cancel identically for all three, per the derivation below),
//    isolating the distance / kill-tally / last-attacker terms.
//
//    score = ((128×2) − dist)×2 + buildings_killed×5 + units_killed
//            + (eu−my_units) + (eb−my_buildings) + (ei−my_infantry)/4
//            + (100 if last_attacker)
//
//    AI (house 0) base at (64,64). Candidates, each a lone 1×1 building
//    (eu=0, eb=1, ei=0 — identical to the AI's own eb=1, eu=0, ei=0, so all
//    three size terms are exactly 0 for every candidate):
//      house 1 @ (74,64):  dist=10, no kills, not last attacker.
//        score = (256-10)*2            = 492
//      house 2 @ (114,64): dist=50, buildings_killed=10, units_killed=5.
//        score = (256-50)*2 + 50 + 5   = 412 + 55 = 467
//      house 3 @ (94,64):  dist=30, last attacker.
//        score = (256-30)*2 + 100      = 452 + 100 = 552   <- winner
//
//    House 3 (medium distance + last-attacker bonus) must beat both the
//    closer house 1 (pure distance) and the heavily-farmed house 2 (distance
//    + kills), proving the pick is a genuine multi-term score, not "nearest".
// ===========================================================================

fn one_by_one(name: &str) -> BuildingProto {
    BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 1,
        foot_h: 1,
        max_health: 100,
        armor: 0,
        power: 0,
        cost: 50,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 1,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

#[test]
fn expert_ai_enemy_scoring_picks_the_hand_computed_winner() {
    let mut w = World::new(Passability::all_passable(), 1);
    w.set_catalog(Catalog {
        buildings: vec![one_by_one("BASE")],
        units: vec![],
        econ: EconRules::default(),
    });
    w.init_houses(4, 0);

    w.spawn_building(0, 0, CellCoord::new(64, 64)).unwrap(); // AI's own base
    w.spawn_building(0, 1, CellCoord::new(74, 64)).unwrap(); // dist 10
    w.spawn_building(0, 2, CellCoord::new(114, 64)).unwrap(); // dist 50
    w.spawn_building(0, 3, CellCoord::new(94, 64)).unwrap(); // dist 30

    // `buildings_killed_by[attacker]` lives on the VICTIM house — house 2 was
    // hit by house 0 (the AI) 10 times (attacker index 0).
    w.houses[2].buildings_killed_by = vec![10, 0, 0, 0];
    w.houses[2].units_killed_by = vec![5, 0, 0, 0];
    // The last-attacker bonus rewards whichever candidate most recently
    // attacked the AI **itself** (`world.house(self.house).last_attacker`),
    // not a house the AI attacked — so this lives on house 0 (the AI).
    w.houses[0].last_attacker = Some(3);

    w.set_ai(vec![AiPlayer::new(0, Difficulty::Normal)]);

    // Run one Expert_AI pass (EXPERT_PERIOD = 150 ticks; `step` runs it on the
    // very first tick too, since `expert_timer` starts at 0).
    w.tick(&[]);

    let enemy = w.ai()[0].enemy();
    assert_eq!(
        enemy,
        Some(3),
        "house 3 (medium distance + last-attacker bonus, score 552) must beat \
         house 1 (closest, score 492) and house 2 (farmed kills, score 467)"
    );
}

// ===========================================================================
// 2. Rubber-band caps: a ratchet — never shrink even as the tracked enemy's
//    army shrinks back down after having grown.
// ===========================================================================

#[test]
fn rubber_band_caps_grow_with_the_enemy_and_never_shrink() {
    let mut w = skirmish(0x7A6B_0001, Difficulty::Normal);

    // First Expert_AI pass: house 2 (the only other house) starts with just
    // its MCV (not an army unit — MCV isn't in the catalog's armed-vehicle
    // sense, but `army_size` counts ALL non-harvester, non-infantry units, so
    // the MCV itself counts as 1). Cap floors at `max(avg+10, 10)`.
    w.tick(&[]);
    let (cap_units_0, _cap_buildings_0) = w.ai()[0].caps();
    assert!(
        cap_units_0 >= 10,
        "cap must floor at >= 10 even for a tiny enemy"
    );

    // Grow house 2's army substantially, then run another Expert_AI pass
    // (advance past the 150-tick cadence).
    for i in 0..30 {
        w.spawn_unit(
            U_TANK,
            2,
            CellCoord::new(100 + i % 10, 100 + i / 10),
            Facing(0),
            400,
            stats(),
        );
    }
    for _ in 0..151 {
        w.tick(&[]);
    }
    let (cap_units_1, _) = w.ai()[0].caps();
    assert!(
        cap_units_1 > cap_units_0,
        "the cap must rise once the tracked enemy's army grew: {cap_units_0} -> {cap_units_1}"
    );

    // Now shrink house 2's army back down to almost nothing.
    let doomed: Vec<Handle> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 2 && u.type_id == U_TANK)
        .map(|(h, _)| h)
        .collect();
    for h in doomed {
        w.units.remove(h);
    }
    for _ in 0..151 {
        w.tick(&[]);
    }
    let (cap_units_2, _) = w.ai()[0].caps();
    assert_eq!(
        cap_units_2, cap_units_1,
        "the cap must NOT shrink when the enemy's army shrinks back down \
         (a ratchet, not a live average): {cap_units_1} -> {cap_units_2}"
    );
}

// ===========================================================================
// 3. Composed-team decimation threshold: exact boundary
//    (`retreat_floor = max(initial_size/2, 2)`). At `alive == retreat_floor`
//    the team must still be intact; at `alive == retreat_floor - 1` it must
//    dissolve.
// ===========================================================================

#[test]
fn team_dissolves_exactly_below_half_strength_not_at_half() {
    // Hard: bigger team_vehicles target, so `initial_size` is more likely > 2
    // (needed for the "at floor, not below" half of this test to be
    // meaningful — a 2-member team's floor is already 2, so the "still
    // intact at the floor" case only distinguishes itself when the team is
    // bigger than the minimum).
    let mut w = skirmish(0x7EA3_0002, Difficulty::Hard);

    // Wait for house 1's AI to form a team AND reach the Attacking phase (not
    // just Staging): only once attacking does every member carry a live
    // `Command::Attack` target, which is what the kill-selection proxy below
    // (`target.is_some()`) relies on to pick out team members from any other
    // idle house-1 unit.
    let mut initial_size = None;
    for _ in 0..4000 {
        w.tick(&[]);
        if let Some((n, init, staging, _)) = w
            .ai()
            .iter()
            .find(|a| a.house() == 1)
            .unwrap()
            .team_summary()
        {
            if !staging {
                initial_size = Some((n, init));
                break;
            }
        }
    }
    let (_, init) =
        initial_size.expect("house 1 should have formed and attacked with a team within budget");
    let retreat_floor = (init / 2).max(2);

    // Kill members one at a time down to exactly `retreat_floor` alive.
    loop {
        let alive = w
            .units
            .iter()
            .filter(|(_, u)| u.house == 1 && u.target.is_some())
            .count();
        if alive <= retreat_floor {
            break;
        }
        let victim = w
            .units
            .iter()
            .find(|(_, u)| u.house == 1 && u.target.is_some())
            .map(|(h, _)| h)
            .unwrap();
        w.units.remove(victim);
        w.tick(&[]);
    }
    // At exactly the floor, the team must still exist (not yet dissolved).
    let still_alive_count = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.target.is_some())
        .count();
    assert_eq!(
        still_alive_count, retreat_floor,
        "the kill loop must land exactly on the retreat floor, not overshoot it \
         (a jump past it would mean the target-based member proxy diverged from \
         the team's real membership — a sign this fixture needs revisiting, not \
         a pass-by-vacuity)"
    );
    {
        assert!(
            w.ai()
                .iter()
                .find(|a| a.house() == 1)
                .unwrap()
                .team_summary()
                .is_some(),
            "at exactly the retreat floor ({retreat_floor}), the team must still be intact \
             (dissolve condition is `alive < retreat_floor`, strictly less)"
        );

        // One more kill crosses strictly below the floor -> must dissolve.
        let victim = w
            .units
            .iter()
            .find(|(_, u)| u.house == 1 && u.target.is_some())
            .map(|(h, _)| h)
            .unwrap();
        w.units.remove(victim);
        let mut dissolved = false;
        for _ in 0..50 {
            w.tick(&[]);
            if w.ai()
                .iter()
                .find(|a| a.house() == 1)
                .unwrap()
                .team_summary()
                .is_none()
            {
                dissolved = true;
                break;
            }
        }
        assert!(
            dissolved,
            "one member below the retreat floor ({}) must dissolve the team",
            retreat_floor - 1
        );
    }
}

// ===========================================================================
// 4. Fire-sale gating: undeployed does NOT fire-sale; deployed + lost
//    production DOES.
// ===========================================================================

#[test]
fn undeployed_house_with_a_building_never_fire_sales() {
    // House 1 has an AI but NEVER got an MCV (so it can never deploy — the
    // `deployed` flag can only ever be false), and owns a single non-factory
    // building (mirrors the scenario/test-house edge case QUIRKS Q16 cites:
    // "a scenario/test house holding a lone non-factory building"). Plenty of
    // credits so the *other* selling reflex (raise-money, which fires
    // independently of `deployed` whenever a broke house can't make money)
    // can't confound this — this test isolates the fire-sale gate only.
    let mut w = World::new(Passability::all_passable(), 5);
    w.set_catalog(catalog());
    w.init_houses(2, CREDITS);
    w.spawn_building(B_DEF1, 1, CellCoord::new(20, 20)).unwrap();
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);

    for _ in 0..2000 {
        w.tick(&[]);
    }
    assert!(
        w.house(1).unwrap().owns_building(B_DEF1),
        "an undeployed house must never fire-sale its lone building"
    );
}

#[test]
fn deployed_house_that_loses_all_production_fire_sales_everything() {
    let mut w = skirmish(0x7EA5_0003, Difficulty::Normal);
    // Let house 1 deploy (into FACT) and build up a bit.
    for _ in 0..600 {
        w.tick(&[]);
    }
    assert!(
        w.house(1).unwrap().owns_building(B_FACT),
        "house 1 should have deployed its MCV into a FACT by now"
    );

    // Strip production: sell the construction yard AND any war factory (so it
    // has no way to recover), leaving only a non-factory building (its POWR,
    // if built) — the genuine lost-cause endgame. **Sold via `Command::Sell`,
    // not raw arena removal**: a raw `world.buildings.remove(h)` skips the
    // house's cached `building_counts` decrement (`House::adjust_building_
    // count`, only touched by `spawn_building`/the real removal path), which
    // would leave `owns_role(WarFactory)` stale-true and mask the very
    // gating this test exists to check.
    let fact: Vec<Handle> = w
        .buildings
        .iter()
        .filter(|(_, b)| b.house == 1 && b.is_construction_yard)
        .map(|(h, _)| h)
        .collect();
    for h in fact {
        w.tick(&[Command::Sell {
            house: 1,
            building: h,
        }]);
    }
    let weap: Vec<Handle> = w
        .buildings
        .iter()
        .filter(|(_, b)| b.house == 1 && b.is_war_factory)
        .map(|(h, _)| h)
        .collect();
    for h in weap {
        w.tick(&[Command::Sell {
            house: 1,
            building: h,
        }]);
    }
    let mcvs: Vec<Handle> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.type_id == U_MCV)
        .map(|(h, _)| h)
        .collect();
    for h in mcvs {
        w.units.remove(h);
    }
    // Give it a non-essential building so there's something to sell (and to
    // confirm fire-sale, not just "nothing left to sell").
    w.spawn_building(B_DEF1, 1, CellCoord::new(16, 16)).unwrap();
    assert!(w.house(1).unwrap().owns_building(B_DEF1));

    for _ in 0..300 {
        w.tick(&[]);
    }
    assert!(
        !w.buildings.iter().any(|(_, b)| b.house == 1),
        "a deployed house that lost all production (no CY/WEAP/barracks) and \
         has no MCV to recover must fire-sale every remaining building"
    );
}

// ===========================================================================
// 5. Raise-money: sells the least-essential building, and picks the
//    *highest-id* non-essential one first (the documented priority order).
// ===========================================================================

#[test]
fn raise_money_sells_the_least_essential_highest_id_building_first() {
    let mut w = World::new(Passability::all_passable(), 9);
    w.set_catalog(catalog());
    w.init_houses(2, 50); // below RAISE_MONEY_FLOOR (100), and no refinery+harvester
    w.spawn_building(B_FACT, 1, CellCoord::new(20, 20)).unwrap(); // essential (keeps fire-sale off)
    w.spawn_building(B_DEF1, 1, CellCoord::new(24, 20)).unwrap(); // non-essential, lower id
    let def2 = w.spawn_building(B_DEF2, 1, CellCoord::new(26, 20)).unwrap(); // non-essential, higher id — must go first
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);

    for _ in 0..200 {
        w.tick(&[]);
        if w.buildings.get(def2).is_none() {
            break;
        }
    }
    assert!(
        w.buildings.get(def2).is_none(),
        "the higher-id non-essential building (DEF2) must be sold first"
    );
    assert!(
        w.house(1).unwrap().owns_building(B_DEF1),
        "the lower-id non-essential building (DEF1) must be kept while broke \
         (one sale raises money above the immediate floor)"
    );
    assert!(
        w.house(1).unwrap().owns_building(B_FACT),
        "the essential construction yard must never be sold by raise-money"
    );
}

// ===========================================================================
// 6. Kill-tally attribution: direct AND splash kills both credit the
//    attacker's house — not just a direct hit.
// ===========================================================================

#[test]
fn kill_tally_credits_the_attacker_for_both_direct_and_splash_kills() {
    let mut w = World::new(Passability::all_passable(), 0xA11A_1111);
    w.set_catalog(Catalog::new());
    w.init_houses(3, 0);

    // A splash weapon: real spread (not the handicap_suite.rs point-blank
    // trick) so a force-fire at a cell also kills a *bystander* nearby who
    // was never the explicit target.
    let splash = WeaponProfile {
        damage: 10_000,
        rof: 9999,
        range: 20 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 3,
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1_000_000,
    };

    let attacker = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(attacker, 0, Some(splash), false);
    // The explicit target, at the force-fire cell.
    let direct = w.spawn_unit(0, 2, CellCoord::new(20, 20), Facing(0), 1, stats());
    // A bystander one cell away — killed purely by splash, never targeted.
    let bystander = w.spawn_unit(0, 2, CellCoord::new(21, 20), Facing(0), 1, stats());

    w.tick(&[Command::Attack {
        unit: attacker,
        target: Target::Cell(CellCoord::new(20, 20)),
        house: 1,
    }]);
    for _ in 0..50 {
        w.tick(&[]);
        if w.units.get(direct).is_none() && w.units.get(bystander).is_none() {
            break;
        }
    }
    assert!(
        w.units.get(direct).is_none(),
        "the direct target should have died"
    );
    assert!(
        w.units.get(bystander).is_none(),
        "the splash bystander should also have died"
    );
    assert_eq!(
        w.house(2).unwrap().units_killed_by(1),
        2,
        "house 1 must be credited for BOTH kills — the direct hit and the \
         splash-only bystander, not just the explicit target"
    );
}

// ===========================================================================
// 7. Determinism across *mixed* difficulties (Hard house 1 vs Normal house
//    2) — same seed twice must give an identical hash chain, extending the
//    existing same-difficulty-both-sides coverage in `ai_suite.rs`.
// ===========================================================================

#[test]
fn determinism_holds_for_a_hard_vs_normal_pairing() {
    const TICKS: u32 = 2000;
    let run = || {
        let mut w = skirmish_mixed(0xD1FF_0001, Difficulty::Hard, Difficulty::Normal);
        let mut hashes = Vec::with_capacity(TICKS as usize);
        for _ in 0..TICKS {
            hashes.push(w.tick(&[]));
        }
        hashes
    };
    assert_eq!(
        run(),
        run(),
        "Hard-house-1-vs-Normal-house-2: hash chain diverged between two runs \
         of the identical seed/setup"
    );
}
