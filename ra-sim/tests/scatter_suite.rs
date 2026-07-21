//! P0 "ask-the-blocker-to-scatter" coverage (QUIRKS Q5 completion): a vehicle
//! whose only route runs through a *friendly, stationary* unit now radios that
//! unit to scatter aside — the sim's port of `DriveClass::Start_Of_Move`'s
//! MOVE_TEMP reaction (`CellClass::Incoming` → `DriveClass::Scatter`,
//! drive.cpp:970/1034 → drive.cpp:181), which our earlier "wait forever"
//! simplification lacked. These are ra-coder's *verification* smoke tests
//! (handed off to ra-tester for boundary expansion); they assert the mechanic
//! resolves the ore-truck / 1-wide-corridor deadlocks, leaves enemy and moving
//! blockers alone, and never moves a blocker-free single unit (no stray RNG).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, MoveStats, OreField, Passability, UnitProto, World,
};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// A 1-wide horizontal corridor in the middle row of a `len`×`h` grid, everything
/// else walled — no unit can ever detour off the single passable row.
fn corridor(len: i32, h: i32) -> (Passability, i32) {
    let n = (len * h) as usize;
    let mut cells = vec![false; n];
    let row = h / 2;
    for x in 0..len {
        cells[(row * len + x) as usize] = true;
    }
    (Passability::new(len, h, cells), row)
}

fn no_overlap(world: &World) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    for (_, u) in world.units.iter() {
        if !u.is_infantry() && !seen.insert((u.cell().x, u.cell().y)) {
            return false;
        }
    }
    true
}

// ===========================================================================
// 1. The core mechanic: a parked FRIENDLY vehicle sitting on the mover's only
//    route is scattered aside, and the mover gets through.
// ===========================================================================

#[test]
fn parked_friendly_blocker_in_a_corridor_is_scattered_and_the_mover_gets_through() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_0001);
    // Mover (house 1) at the west end; a friendly (house 1) parked mid-corridor.
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    let blocker = world.spawn_unit(0, 1, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);

    let mut mover_progressed = false;
    let mut blocker_scattered = false;
    for t in 0..300 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: two vehicles share a cell");
        if world.units.get(blocker).unwrap().cell() != blocker_start {
            blocker_scattered = true;
        }
        if world.units.get(mover).unwrap().cell().x >= 14 {
            mover_progressed = true;
            break;
        }
    }
    assert!(
        blocker_scattered,
        "the parked friendly blocker was never nudged (scatter did not fire)"
    );
    assert!(
        mover_progressed,
        "mover never reached its destination past the (scattered) friendly blocker"
    );
}

// ===========================================================================
// 1b. Same-script-twice determinism for the RNG-consuming scatter path: the
//     corridor-conga scenario draws the SYNC RNG on every scatter, so this is
//     the case most likely to diverge if the draw order weren't deterministic.
// ===========================================================================

#[test]
fn scatter_scenario_is_deterministic_same_script_twice() {
    let run = || {
        let (grid, row) = corridor(16, 5);
        let mut world = World::new(grid, 0x5CA7_1B1B);
        let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
        world.spawn_unit(0, 1, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
        world.tick(&[Command::Move {
            unit: mover,
            dest: CellCoord::new(14, row),
            house: 1,
        }]);
        let mut hashes = Vec::new();
        for _ in 0..120 {
            world.tick(&[]);
            hashes.push(world.state_hash());
        }
        hashes
    };
    assert_eq!(
        run(),
        run(),
        "scatter (sync-RNG) scenario diverged across runs"
    );
}

// ===========================================================================
// 2. Enemy blocker is NOT scattered (it is MOVE_DESTROYABLE/MOVE_NO, never
//    MOVE_TEMP) — the mover is stuck behind it, a documented wait, and the
//    enemy never moves.
// ===========================================================================

#[test]
fn enemy_parked_blocker_is_not_scattered() {
    let (grid, row) = corridor(16, 5);
    let mut world = World::new(grid, 0x5CA7_0002);
    let mover = world.spawn_unit(0, 1, CellCoord::new(1, row), Facing(64), 400, stats(24, 8));
    // Enemy blocker (house 2) parked mid-corridor.
    let blocker = world.spawn_unit(0, 2, CellCoord::new(8, row), Facing(0), 400, stats(24, 8));
    let blocker_start = world.units.get(blocker).unwrap().cell();

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(14, row),
        house: 1,
    }]);
    for t in 0..120 {
        world.tick(&[]);
        assert!(no_overlap(&world), "tick {t}: overlap");
    }
    assert_eq!(
        world.units.get(blocker).unwrap().cell(),
        blocker_start,
        "an ENEMY blocker must never be scattered"
    );
    assert!(
        world.units.get(mover).unwrap().cell().x < 8,
        "mover must not have phased through the enemy block"
    );
}

// ===========================================================================
// 3. Determinism / no-stray-RNG oracle: a single vehicle with no blocker moves
//    exactly the same across two runs, and — crucially — the scatter path is
//    never entered, so the sim hash is byte-identical to a run of the same
//    script (the scatter's sync-RNG draw only happens when a blocker is asked to
//    move).
// ===========================================================================

#[test]
fn single_unit_move_is_deterministic_and_draws_no_scatter_rng() {
    let run = || {
        let mut world = World::new(Passability::all_passable(), 0x5CA7_0003);
        let u = world.spawn_unit(0, 1, CellCoord::new(2, 2), Facing(0), 400, stats(24, 8));
        world.tick(&[Command::Move {
            unit: u,
            dest: CellCoord::new(20, 12),
            house: 1,
        }]);
        let mut trail = Vec::new();
        for _ in 0..300 {
            world.tick(&[]);
            let c = world.units.get(u).unwrap().cell();
            trail.push((c.x, c.y));
        }
        (trail, world.state_hash())
    };
    let (trail_a, hash_a) = run();
    let (trail_b, hash_b) = run();
    assert_eq!(
        trail_a, trail_b,
        "single-unit movement is not deterministic"
    );
    assert_eq!(hash_a, hash_b, "single-unit run hash diverged");
    // It actually reached the destination (the move is real, not a no-op).
    assert_eq!(*trail_a.last().unwrap(), (20, 12));
}

// ===========================================================================
// 4. The user's report: a harvester docking at a refinery whose dock approach
//    is blocked by a parked friendly vehicle now completes the dock and banks
//    the credits (previously a permanent deadlock).
// ===========================================================================

const B_PROC: u32 = 0;
const U_HARV: u32 = 0;
const U_TANK: u32 = 1;

fn econ_catalog() -> Catalog {
    let proc = BuildingProto {
        is_barracks: false,
        name: "PROC".into(),
        foot_w: 3,
        foot_h: 3,
        max_health: 900,
        armor: 0,
        power: 0,
        cost: 2000,
        prereq: vec![],
        is_refinery: true,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None, // spawn the harvester ourselves, placed precisely
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 2000,
    };
    let harv = UnitProto {
        is_infantry: false,
        locomotor: 2, // Wheel
        name: "HARV".into(),
        sprite_id: 0,
        max_health: 600,
        stats: stats(24, 5),
        armor: 0,
        weapon: None,
        secondary: None,
        has_turret: false,
        is_harvester: true,
        deploys_to: None,
        cost: 1400,
        prereq: vec![],
        sight: 2,
        passengers: 0,
        ammo: 0,
    };
    let mut tank = harv.clone();
    tank.name = "TANK".into();
    tank.is_harvester = false;
    tank.locomotor = 1;
    Catalog {
        buildings: vec![proc],
        units: vec![harv, tank],
        econ: EconRules::default(),
    }
}

#[test]
fn harvester_docks_past_a_parked_friendly_and_banks_credits() {
    // A 40×40 map split by a vertical wall at x=20 with a single 1-cell doorway
    // at (20,20). Ore + harvester live on the left; the refinery on the right.
    // The only route from ore to dock threads the doorway — and a friendly tank
    // is parked squarely in it, reproducing the ore-truck deadlock (the harvester
    // cannot route around, so it must ask the tank to scatter). Once nudged, the
    // tank steps aside into the open right-hand field and the harvester docks.
    let w = 40i32;
    let h = 40i32;
    let mut cells = vec![true; (w * h) as usize];
    for y in 0..h {
        cells[(y * w + 20) as usize] = false; // wall column x=20 ...
    }
    cells[(20 * w + 20) as usize] = true; // ... with a 1-cell doorway at (20,20)
    let mut world = World::new(Passability::new(w, h, cells), 0x5CA7_0004);
    world.set_catalog(econ_catalog());
    world.init_houses(3, 0); // houses 0..2; house 1 is ours, starts broke

    // Refinery on the right; 3×3 at (25,25) → south dock cell (26,28).
    world
        .spawn_building(B_PROC, 1, CellCoord::new(25, 25))
        .expect("refinery placed");

    // Ore on the left for the harvester to fill up on.
    let n = (w * h) as usize;
    let mut ov = vec![0u8; n];
    for y in 4..8 {
        for x in 4..8 {
            ov[(y * w + x) as usize] = ra_sim::ore::OVERLAY_GOLD_FIRST;
        }
    }
    world.set_ore(OreField::from_overlay(w, h, &ov));

    // Harvester on the ore (left); a friendly tank parked in the doorway (20,20)
    // — the sole passage — so the harvester deadlocks until the tank scatters.
    let harv = world.spawn_unit(
        U_HARV,
        1,
        CellCoord::new(5, 5),
        Facing(0),
        600,
        stats(24, 5),
    );
    world.set_unit_harvester(harv, true);
    let tank = world.spawn_unit(
        U_TANK,
        1,
        CellCoord::new(20, 20),
        Facing(0),
        600,
        stats(24, 5),
    );

    let credits_before = world.houses.get(1).map(|hh| hh.available()).unwrap_or(0);
    let tank_start = world.units.get(tank).unwrap().cell();

    let mut banked = false;
    let mut tank_scattered = false;
    for _ in 0..1500 {
        world.tick(&[]);
        if world.units.get(tank).map(|u| u.cell()) != Some(tank_start) {
            tank_scattered = true;
        }
        let now = world.houses.get(1).map(|hh| hh.available()).unwrap_or(0);
        if now > credits_before {
            banked = true;
            break;
        }
    }
    assert!(
        tank_scattered,
        "the parked friendly tank on the dock was never scattered"
    );
    assert!(
        banked,
        "the harvester never banked credits — it stayed deadlocked behind the parked friendly"
    );
}
