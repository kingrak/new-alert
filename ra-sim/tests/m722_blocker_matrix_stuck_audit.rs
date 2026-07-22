//! M7.22 audit follow-up on `m722_blocker_matrix.rs`.
//!
//! `corridor_friendly_passes_impassable_does_not` asserts only that the mover
//! did NOT reach `dest` within the tick budget. Its own doc comment promises
//! more: "must go idle (dest cleared) rather than spin forever (M7.20
//! `PATH_RETRY` abandonment)". "Did not arrive in 400 ticks" and "genuinely
//! abandoned the order (idle, `dest == None`)" are different properties — the
//! former is also true of a mover that is still slowly re-planning and would
//! arrive at tick 401, or one stuck in a live retry loop. This file asserts
//! the stronger, documented property directly: `Unit::dest` (`unit.rs:241`)
//! is cleared for every impassable corridor direction/blocker pair, matching
//! the M7.20 `PATH_RETRY` abandonment this suite's docs invoke.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{BuildingProto, Catalog, Command, EconRules, Mission, MoveStats, Passability, World};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 40,
    }
}

const B_WALLHUT: u32 = 0;

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "HUT".to_string(),
            foot_w: 1,
            foot_h: 1,
            max_health: 400,
            armor: 0,
            power: 0,
            cost: 10,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }],
        units: vec![],
        econ: EconRules::default(),
    }
}

#[derive(Clone, Copy)]
enum Blocker {
    Enemy,
    Building,
}

impl Blocker {
    fn label(self) -> &'static str {
        match self {
            Blocker::Enemy => "enemy-unit",
            Blocker::Building => "building",
        }
    }
}

/// Verbatim geometry from `m722_blocker_matrix.rs::setup`, corridor-only,
/// E/W directions only (matches what
/// `corridor_friendly_passes_impassable_does_not` exercises for the
/// impassable cases).
fn setup(dir: (i32, i32), blocker: Blocker) -> (World, ra_sim::Handle, CellCoord) {
    let w = 40;
    let h = 40;
    let mut cells = vec![false; (w * h) as usize];
    for x in 0..w {
        cells[(20 * w + x) as usize] = true;
    }
    let mut world = World::new(Passability::new(w, h, cells), 0x0BADCAFE);
    world.set_catalog(catalog());

    let m = CellCoord::new(15, 20);
    let bcell = CellCoord::new(m.x + dir.0, m.y + dir.1);
    let dest = CellCoord::new(m.x + dir.0 * 2, m.y + dir.1 * 2);

    let mover = world.spawn_unit(0, 1, m, Facing(0), 400, stats());
    match blocker {
        Blocker::Building => {
            world.spawn_building(B_WALLHUT, 1, bcell);
        }
        Blocker::Enemy => {
            let b = world.spawn_unit(0, 2, bcell, Facing(0), 400, stats());
            world.set_unit_mission(b, Mission::Sleep);
        }
    }
    (world, mover, dest)
}

/// A 1-wide corridor, fully walled by an unarmed mover's impassable blocker
/// (enemy unit / friendly building), in every E/W direction: the mover must
/// genuinely abandon the order — `Unit::dest` cleared — not merely fail to
/// have arrived yet within the budget.
#[test]
fn corridor_impassable_cases_genuinely_abandon_not_just_fail_to_arrive_in_time() {
    const BUDGET: u32 = 400;
    for (dx, dy, dname) in [(1i32, 0i32, "E"), (-1, 0, "W")] {
        for blocker in [Blocker::Enemy, Blocker::Building] {
            let (mut world, mover, dest) = setup((dx, dy), blocker);
            world.tick(&[Command::Move {
                unit: mover,
                dest,
                house: 1,
            }]);
            for _ in 0..BUDGET {
                world.tick(&[]);
            }
            let u = world
                .units
                .get(mover)
                .expect("mover must still exist (never destroyed in this scenario)");
            assert_ne!(
                u.cell(),
                dest,
                "corridor {dname}/{}: sanity — must not have arrived",
                blocker.label()
            );
            assert_eq!(
                u.dest,
                None,
                "corridor {dname}/{}: mover must have genuinely abandoned the order \
                 (Unit::dest cleared) after {BUDGET} ticks, not be left silently \
                 mid-retry with a live destination — the doc-promised \
                 'go idle (dest cleared)' behaviour",
                blocker.label()
            );
        }
    }
}
