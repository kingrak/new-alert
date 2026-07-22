//! M7.20 anti-gridlock pins — each one revert-sensitive (disable the
//! mechanism and the pin fails), covering the cycle's four load-bearing
//! changes:
//!
//! §1 **Corridor-clear placement** (`AiPlayer::placement_cell` +
//!    `reserved_corridor_cells`, ai.rs): the AI never places a building on a
//!    refinery's dock approach or on the dock→ore corridor — even when that
//!    is the ONLY legal spot left.
//! §2 **Harvester unstick** (`HarvestState::retarget` + the movement layer's
//!    `PATH_RETRY` abandonment): a harvester pinned away from its chosen ore
//!    cell re-paths and then targets a *different* ore cell, instead of
//!    re-picking the unreachable one forever.
//! §3 **Runaway guard** (`Control.MaxInfantry` rubber band + the
//!    `RUBBER_BAND_CEILING` clamp, HOUSE.CPP:729-731/4962-4963/6281): the
//!    infantry lane is capped like the vehicle lane, and no rubber-band cap
//!    can ratchet past the original's `Rule.UnitMax/6` constructor value.
//! §4 **Zombie-team timeout** (`ATTACK_TIMEOUT`, ai.rs): a team stalled in
//!    the Attacking phase dissolves as a FAILED attack (freeing the slot and
//!    feeding the escalation counter) instead of monopolising the team slot
//!    forever — the scm01ea/scg05ea stalemate signature.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Difficulty, EconRules, MoveStats, OreField, Passability,
    UnitProto, WarheadProfile, WeaponProfile, World,
};

const B_FACT: u32 = 0;
const B_PROC: u32 = 1;
const B_PILL: u32 = 2; // 1x1 filler building the AI wants to place
const B_TENT: u32 = 3; // barracks (for the infantry-lane cap pin)
const B_WEAP: u32 = 4; // enemy production target (zombie-team pin)

const U_TANK: u32 = 1;
const U_HARV: u32 = 2;
const U_INF: u32 = 3;

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
    let bproto = |name: &str, w: u8, h: u8| BuildingProto {
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor: 0,
        power: 0,
        cost: 60,
        prereq: vec![],
        is_refinery: name == "PROC",
        is_construction_yard: name == "FACT",
        is_war_factory: name == "WEAP",
        is_barracks: name == "TENT",
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let uproto = |name: &str,
                  is_infantry: bool,
                  is_harvester: bool,
                  wpn: Option<WeaponProfile>,
                  deploys_to: Option<u32>| UnitProto {
        is_infantry,
        locomotor: 1,
        name: name.into(),
        sprite_id: 0,
        max_health: 200,
        stats: stats(),
        armor: 0,
        weapon: wpn,
        secondary: None,
        has_turret: false,
        is_harvester,
        deploys_to,
        cost: 50,
        prereq: vec![],
        sight: 4,
        passengers: 0,
        ammo: 0,
    };
    Catalog {
        buildings: vec![
            bproto("FACT", 2, 2),
            bproto("PROC", 2, 2),
            bproto("PILL", 1, 1),
            bproto("TENT", 2, 2),
            bproto("WEAP", 2, 2),
        ],
        units: vec![
            uproto("MCV", false, false, None, Some(B_FACT)),
            uproto("TANK", false, false, Some(weapon(25)), None),
            uproto("HARV", false, true, None, None),
            uproto("E1", true, false, Some(weapon(10)), None),
        ],
        econ: EconRules::default(),
    }
}

// ===========================================================================
// §1 — corridor-clear placement.
// ===========================================================================

/// Geometry: an all-impassable map except the FACT footprint, the PROC
/// footprint, and the refinery's dock + dock→ore corridor cells. The ONLY
/// cell where the 1x1 PILL is legal by footprint/proximity rules is the dock
/// cell itself — so with the corridor guard the AI must refuse to place at
/// all, and disabling the guard (reverting `reserved_corridor_cells`) makes
/// `placement_cell` return the dock and the pin fail.
#[test]
fn placement_never_blocks_the_refinery_dock_corridor_even_when_it_is_the_only_spot() {
    let (w, h) = (128i32, 128i32);
    let mut cells = vec![false; (w * h) as usize];
    let mut open = |x: i32, y: i32| cells[(y * w + x) as usize] = true;
    // FACT 2x2 at (8,10), PROC 2x2 at (10,10) (adjacent).
    for (x, y) in [(8, 10), (9, 10), (8, 11), (9, 11)] {
        open(x, y);
    }
    for (x, y) in [(10, 10), (11, 10), (10, 11), (11, 11)] {
        open(x, y);
    }
    // Dock (south of the PROC centre (11,11)) + corridor to the ore at (11,14).
    for (x, y) in [(11, 12), (11, 13), (11, 14)] {
        open(x, y);
    }
    let grid = Passability::new(w, h, cells);
    let mut world = World::new(grid, 0xC0DE_0001);
    world.set_catalog(catalog());
    world.init_houses(3, 6000);
    world
        .spawn_building(B_FACT, 1, CellCoord::new(8, 10))
        .unwrap();
    world
        .spawn_building(B_PROC, 1, CellCoord::new(10, 10))
        .unwrap();
    // Ore at the corridor's far end.
    let mut overlay = vec![0u8; (w * h) as usize];
    overlay[(14 * w + 11) as usize] = 5; // gold overlay id
    world.set_ore(OreField::from_overlay(w, h, &overlay));

    let ai = AiPlayer::new(1, Difficulty::Normal);
    // Sanity: the dock cell IS legal by the raw placement rules — only the
    // corridor guard can be the thing rejecting it.
    assert!(
        world.can_place_building(1, B_PILL, CellCoord::new(11, 12)),
        "fixture sanity: the dock cell must be footprint/proximity-legal"
    );
    assert_eq!(
        ai.debug_placement_cell(&world, B_PILL),
        None,
        "the AI must refuse to wall its refinery's dock/ore corridor even when \
         that is the only legal placement left (corridor-clear guard, M7.20 P1a)"
    );

    // Second half: open one lawful non-corridor cell beside the refinery and
    // the AI must place there — the guard rejects corridor cells, not
    // placement in general.
    let mut cells2 = vec![false; (w * h) as usize];
    let mut open2 = |x: i32, y: i32| cells2[(y * w + x) as usize] = true;
    for (x, y) in [(8, 10), (9, 10), (8, 11), (9, 11)] {
        open2(x, y);
    }
    for (x, y) in [(10, 10), (11, 10), (10, 11), (11, 11)] {
        open2(x, y);
    }
    for (x, y) in [(11, 12), (11, 13), (11, 14)] {
        open2(x, y);
    }
    open2(12, 12); // the lawful alternative (diagonally adjacent to PROC)
    let grid2 = Passability::new(w, h, cells2);
    let mut world2 = World::new(grid2, 0xC0DE_0002);
    world2.set_catalog(catalog());
    world2.init_houses(3, 6000);
    world2
        .spawn_building(B_FACT, 1, CellCoord::new(8, 10))
        .unwrap();
    world2
        .spawn_building(B_PROC, 1, CellCoord::new(10, 10))
        .unwrap();
    let mut overlay2 = vec![0u8; (w * h) as usize];
    overlay2[(14 * w + 11) as usize] = 5;
    world2.set_ore(OreField::from_overlay(w, h, &overlay2));
    assert_eq!(
        ai.debug_placement_cell(&world2, B_PILL),
        Some(CellCoord::new(12, 12)),
        "with a lawful non-corridor cell available the AI must place there"
    );
}

// ===========================================================================
// §2 — harvester unstick: re-path, then target a different ore cell.
// ===========================================================================

/// Geometry: two east-west corridors (rows 2 and 4) joined only at the west
/// end (column 0). Ore A (10,2) is nearer but its corridor is permanently
/// plugged by a parked ENEMY vehicle at (6,2) (enemies are never scattered);
/// ore B (10,4) is reachable via the southern loop. Plain pathing ignores
/// units, so the scan always prefers A — only the M7.20 chain (blocked →
/// `PATH_RETRY` abandonment → `retarget` bump → scan rotation) gets the
/// harvester to B. Reverting any link leaves it jammed at (5,2) forever and
/// it never mines a bail.
#[test]
fn pinned_harvester_repaths_and_targets_a_different_ore_cell() {
    let (w, h) = (20i32, 8i32);
    let mut cells = vec![false; (w * h) as usize];
    let mut open = |x: i32, y: i32| cells[(y * w + x) as usize] = true;
    for x in 0..=10 {
        open(x, 2);
        open(x, 4);
    }
    open(0, 3); // the western join
                // Refinery footprint cells (2x2 at (0,5)) — south of the loop, adjacent
                // to row 4 but covering none of the corridor/join cells.
    for (x, y) in [(0, 5), (1, 5), (0, 6), (1, 6)] {
        open(x, y);
    }
    let grid = Passability::new(w, h, cells);
    let mut world = World::new(grid, 0xC0DE_0003);
    world.set_catalog(catalog());
    world.init_houses(3, 6000);
    // A refinery so the harvest FSM runs (house_has_refinery gate).
    world
        .spawn_building(B_PROC, 1, CellCoord::new(0, 5))
        .unwrap();

    let mut overlay = vec![0u8; (w * h) as usize];
    overlay[(2 * w + 10) as usize] = 5; // ore A (nearer, plugged corridor)
    overlay[(4 * w + 10) as usize] = 5; // ore B (reachable loop)
    world.set_ore(OreField::from_overlay(w, h, &overlay));

    let harv = world.spawn_unit(U_HARV, 1, CellCoord::new(2, 2), Facing(0), 200, stats());
    world.set_unit_harvester(harv, true);
    // The permanent enemy plug.
    world.spawn_unit(U_TANK, 2, CellCoord::new(6, 2), Facing(0), 400, stats());

    let mut mined = false;
    for _ in 0..2000 {
        world.tick(&[]);
        let u = world.units.get(harv).unwrap();
        if u.harvest.cargo > 0 {
            mined = true;
            break;
        }
    }
    let u = world.units.get(harv).unwrap();
    assert!(
        mined,
        "the pinned harvester must unstick (abandon the plugged route, rotate \
         its ore scan) and mine the reachable patch; it is at {:?} with cargo {} \
         retarget {}",
        u.cell(),
        u.harvest.cargo,
        u.harvest.retarget
    );
    assert_eq!(
        u.cell(),
        CellCoord::new(10, 4),
        "the bails must have come from ore B (the different, reachable cell)"
    );
}

// ===========================================================================
// §3 — runaway guard: infantry rubber band + the cap ceiling.
// ===========================================================================

/// An enemy fielding 200 infantry must NOT ratchet our infantry cap to 210 —
/// every rubber-band raise clamps at the original's constructor value
/// `Rule.InfantryMax/6 = 500/6 = 83` (HOUSE.CPP:729-731). Reverting the
/// clamp makes the cap 210 and this pin fail.
#[test]
fn rubber_band_caps_clamp_at_the_original_constructor_ceiling() {
    let mut world = World::new(Passability::all_passable(), 0xC0DE_0004);
    world.set_catalog(catalog());
    world.init_houses(3, 6000);
    world
        .spawn_building(B_FACT, 1, CellCoord::new(15, 15))
        .unwrap();
    world
        .spawn_building(B_FACT, 2, CellCoord::new(100, 100))
        .unwrap();
    for i in 0..200 {
        let c = CellCoord::new(60 + (i % 20), 60 + (i / 20));
        let h = world.spawn_unit(U_INF, 2, c, Facing(0), 50, stats());
        world.units.get_mut(h).unwrap().make_infantry((i % 5) as u8);
    }
    world.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);
    world.tick(&[]); // first tick runs the Expert_AI pass (expert_timer 0)
    let ai = &world.ai()[0];
    assert_eq!(
        ai.infantry_cap(),
        500 / 6,
        "the infantry rubber band must clamp at Rule.InfantryMax/6 (83), not \
         chase the 200-strong enemy to 210"
    );
    let (units_cap, buildings_cap) = ai.caps();
    assert!(
        units_cap <= 500 / 6 && buildings_cap <= 500 / 6,
        "every rubber-band cap must respect the ceiling (got units={units_cap} \
         buildings={buildings_cap})"
    );
}

/// The infantry production lane must refuse once `CurInfantry >=
/// Control.MaxInfantry` (`AI_Infantry`, HOUSE.CPP:6281). House 1 owns a
/// barracks, credits, and 20 infantry against a 1-infantry enemy (cap =
/// 1+10 = 11 < 20): no infantry production may ever start. Reverting the
/// `under_icap` gate lets the lane run and the count grow.
#[test]
fn infantry_lane_refuses_over_the_rubber_band_cap() {
    let mut world = World::new(Passability::all_passable(), 0xC0DE_0005);
    world.set_catalog(catalog());
    world.init_houses(3, 60000);
    world
        .spawn_building(B_FACT, 1, CellCoord::new(15, 15))
        .unwrap();
    world
        .spawn_building(B_TENT, 1, CellCoord::new(19, 15))
        .unwrap();
    world
        .spawn_building(B_FACT, 2, CellCoord::new(100, 100))
        .unwrap();
    for i in 0..20 {
        let c = CellCoord::new(30 + (i % 10), 30 + (i / 10));
        let h = world.spawn_unit(U_INF, 1, c, Facing(0), 50, stats());
        world.units.get_mut(h).unwrap().make_infantry((i % 5) as u8);
    }
    let e = world.spawn_unit(U_INF, 2, CellCoord::new(100, 104), Facing(0), 50, stats());
    world.units.get_mut(e).unwrap().make_infantry(0);
    world.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);

    let count_inf = |world: &World| {
        world
            .units
            .iter()
            .filter(|(_, u)| u.house == 1 && u.is_infantry() && u.is_alive())
            .count()
    };
    let before = count_inf(&world);
    for _ in 0..900 {
        world.tick(&[]);
    }
    assert_eq!(
        count_inf(&world),
        before,
        "house 1 (20 infantry, cap 11) must not train more infantry — the \
         MaxInfantry gate (HOUSE.CPP:6281) has regressed"
    );
}

// ===========================================================================
// §4 — zombie-team timeout: a stalled Attacking team dissolves as FAILED.
// ===========================================================================

/// The enemy's only building sits on an island ringed by impassable terrain:
/// a team forms, transitions to Attacking, and can never reach it. Without
/// `ATTACK_TIMEOUT` the team monopolises the slot forever with
/// `failed_attacks` frozen (the scm01ea 28,000-tick zombie); with it, the
/// team dissolves as a failed attack within the timeout and the escalation
/// counter moves.
#[test]
fn stalled_attacking_team_dissolves_as_failed_within_the_timeout() {
    let (w, h) = (128i32, 128i32);
    let mut cells = vec![true; (w * h) as usize];
    // Impassable ring band around the island at (110,110).
    for y in 100..=120 {
        for x in 100..=120 {
            let band = !(104..=116).contains(&x) || !(104..=116).contains(&y);
            let in_outer = (100..=120).contains(&x) && (100..=120).contains(&y);
            if in_outer && band {
                cells[(y * w + x) as usize] = false;
            }
        }
    }
    let grid = Passability::new(w, h, cells);
    let mut world = World::new(grid, 0xC0DE_0006);
    world.set_catalog(catalog());
    world.init_houses(3, 6000);
    world
        .spawn_building(B_FACT, 1, CellCoord::new(15, 15))
        .unwrap();
    world
        .spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();
    for i in 0..4 {
        let hdl = world.spawn_unit(
            U_TANK,
            1,
            CellCoord::new(20 + i, 20),
            Facing(0),
            400,
            stats(),
        );
        world.set_unit_combat(hdl, 0, Some(weapon(25)), true);
    }
    world.set_ai(vec![AiPlayer::new(1, Difficulty::Normal)]);

    // Wait for a team to reach Attacking.
    let mut attacking_at = None;
    for t in 0..4000u32 {
        world.tick(&[]);
        if let Some((_, _, staging, _)) = world.ai()[0].team_summary() {
            if !staging {
                attacking_at = Some(t);
                break;
            }
        }
    }
    let start = attacking_at.expect("a team should form and reach Attacking");

    // Within ATTACK_TIMEOUT (3000) + slack the stalled team must dissolve as
    // a FAILED attack: slot freed AND the escalation counter bumped.
    let mut dissolved_and_escalated = None;
    for t in start..start + 3500 {
        world.tick(&[]);
        let ai = &world.ai()[0];
        if ai.failed_attacks() >= 1 && ai.team_summary().is_none() {
            dissolved_and_escalated = Some(t - start);
            break;
        }
    }
    assert!(
        dissolved_and_escalated.is_some(),
        "a team stalled in Attacking (island target) must dissolve as a FAILED \
         attack within ATTACK_TIMEOUT — otherwise it monopolises the team slot \
         forever and the AI can never escalate (the scm01ea zombie-team stalemate)"
    );
}
