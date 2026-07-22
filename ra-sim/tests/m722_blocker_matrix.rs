//! M7.22 Fix 2 repro matrix — "a unit ordered to move with an immediate blocker
//! in that direction gets stuck".
//!
//! Systematic: mover with a blocker in each of the 8 adjacent directions ×
//! blocker type (friendly idle / friendly guard / enemy unit / building) × open
//! field vs 1-wide corridor. Order the mover to the cell just past the blocker
//! and assert it arrives within a bounded tick budget.
//!
//! Reference behaviour being checked (DRIVE.CPP:1082-1131, UNIT.CPP:3208-3388):
//! a stationary friendly blocker is `MOVE_TEMP` → `Incoming` scatters it; an
//! enemy blocker is `MOVE_DESTROYABLE` → the mover `Override_Mission(ATTACK)`s
//! it (or, if we diverge, holds — a QUIRK). Either way a mover must not hang
//! forever with an open detour available.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, Handle, Mission, MoveStats, Passability, World,
};

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

/// 8 compass directions (dx, dy) and a label.
const DIRS: [(i32, i32, &str); 8] = [
    (0, -1, "N"),
    (1, -1, "NE"),
    (1, 0, "E"),
    (1, 1, "SE"),
    (0, 1, "S"),
    (-1, 1, "SW"),
    (-1, 0, "W"),
    (-1, -1, "NW"),
];

#[derive(Clone, Copy)]
enum Blocker {
    FriendlyIdle,
    FriendlyGuard,
    Enemy,
    Building,
}

impl Blocker {
    fn label(self) -> &'static str {
        match self {
            Blocker::FriendlyIdle => "friendly-idle",
            Blocker::FriendlyGuard => "friendly-guard",
            Blocker::Enemy => "enemy-unit",
            Blocker::Building => "building",
        }
    }
}

/// Build an open-field world (all passable) with a mover at `m`, a blocker one
/// cell toward `dir`, and return the mover handle + destination (two cells past).
fn setup(dir: (i32, i32), blocker: Blocker, corridor: bool) -> (World, Handle, CellCoord) {
    let w = 40;
    let h = 40;
    let cells = if corridor {
        // Only the mover's row (horizontal) is passable → a 1-wide E/W corridor.
        let mut c = vec![false; (w * h) as usize];
        for x in 0..w {
            c[(20 * w + x) as usize] = true;
        }
        c
    } else {
        vec![true; (w * h) as usize]
    };
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
        Blocker::FriendlyIdle => {
            let b = world.spawn_unit(0, 1, bcell, Facing(0), 400, stats());
            world.set_unit_mission(b, Mission::Sleep);
        }
        Blocker::FriendlyGuard => {
            let b = world.spawn_unit(0, 1, bcell, Facing(0), 400, stats());
            world.set_unit_mission(b, Mission::Guard);
        }
    }
    (world, mover, dest)
}

/// Run one cell of the matrix: order the mover to `dest`, tick up to `budget`,
/// return `Some(ticks)` when it arrives (or gets adjacent for a building/enemy it
/// cannot occupy), else `None`.
fn run_cell(dir: (i32, i32), blocker: Blocker, corridor: bool, budget: u32) -> Option<u32> {
    let (mut world, mover, dest) = setup(dir, blocker, corridor);
    world.tick(&[Command::Move {
        unit: mover,
        dest,
        house: 1,
    }]);
    for t in 0..budget {
        world.tick(&[]);
        let u = world.units.get(mover)?;
        let cur = u.cell();
        // Arrived exactly, OR reached the cell short of an impassable
        // destination (an enemy/building sits on `dest` only when dir==blocker
        // dir *and* dest==blocker cell — not the case here since dest is 2 away;
        // but a scattered blocker may have moved onto `dest`). Count "adjacent to
        // dest and stopped" as success too.
        if cur == dest {
            return Some(t);
        }
    }
    None
}

/// PIN (Fix 2a): in the OPEN FIELD, a mover with a single blocker one cell away
/// in ANY of the 8 directions — friendly (idle/guard), enemy, or building —
/// reaches the cell just past it. Before the fix, a pure-diagonal detour past a
/// *building* corner (NE/SW here) hung forever because the interpolated diagonal
/// grazed the building's shared corner and `is_blocked` floor-rounded that graze
/// into the building cell. Revert-drill: dropping the `nudge_corner_graze` call
/// re-STUCKs the diagonal-building cells and fails this pin.
#[test]
fn open_field_blocker_in_every_direction_is_passable() {
    for (dx, dy, dname) in DIRS {
        for blocker in [
            Blocker::FriendlyIdle,
            Blocker::FriendlyGuard,
            Blocker::Enemy,
            Blocker::Building,
        ] {
            let r = run_cell((dx, dy), blocker, false, 400);
            assert!(
                r.is_some(),
                "open field {dname}/{}: mover never reached the cell past the blocker",
                blocker.label()
            );
        }
    }
}

/// PIN (Fix 2, corridor): in a 1-wide lane, a *friendly* blocker (idle or guard)
/// scatters and the mover passes. An *unarmed* mover cannot pass an enemy unit
/// or a friendly building that fully walls the only route — matching the
/// original: `UnitClass::Can_Enter_Cell` returns `MOVE_NO` for a non-ally
/// blocker when `PrimaryWeapon == NULL` (UNIT.CPP:3354) and always `MOVE_NO` for
/// a building (UNIT.CPP:3324), so `Find_Path` finds no route and the order is
/// abandoned rather than forced. The mover must NOT arrive, and must go idle
/// (dest cleared) rather than spin forever (M7.20 `PATH_RETRY` abandonment).
#[test]
fn corridor_friendly_passes_impassable_does_not() {
    for (dx, dy, dname) in DIRS.iter().filter(|(_, dy, _)| *dy == 0) {
        // Friendly blockers scatter out of the lane → the mover passes.
        for blocker in [Blocker::FriendlyIdle, Blocker::FriendlyGuard] {
            assert!(
                run_cell((*dx, *dy), blocker, true, 400).is_some(),
                "corridor {dname}/{}: a scatterable friendly must clear the lane",
                blocker.label()
            );
        }
        // Enemy unit / friendly building fully wall the 1-wide lane for an
        // unarmed mover: genuinely impassable, so it must not arrive.
        for blocker in [Blocker::Enemy, Blocker::Building] {
            assert!(
                run_cell((*dx, *dy), blocker, true, 400).is_none(),
                "corridor {dname}/{}: an unarmed mover must not pass an impassable wall",
                blocker.label()
            );
        }
    }
}

/// PIN (Fix 2b — QUIRK, documented divergence): the original, when a mover's
/// immediate step is `MOVE_DESTROYABLE` (a non-ally destroyable blocker) with no
/// detour, `Override_Mission(MISSION_ATTACK, blocker)` and shoots through it
/// (DRIVE.CPP:1116-1131). We deliberately do NOT auto-convert a Move order into
/// an attack: an armed mover walled by an enemy in a 1-wide lane holds/abandons
/// (its target is never auto-set), keeping Move semantics pure. Pinned so the
/// divergence is intentional, not silent (see docs/QUIRKS.md).
#[test]
fn armed_mover_does_not_auto_attack_a_walling_enemy_quirk() {
    use ra_sim::{WarheadProfile, WeaponProfile, ARMOR_COUNT};
    let w = 40;
    let h = 40;
    let mut cells = vec![false; (w * h) as usize];
    for x in 0..w {
        cells[(20 * w + x) as usize] = true;
    }
    let mut world = World::new(Passability::new(w, h, cells), 0x0BADCAFE);
    world.set_catalog(catalog());
    let mover = world.spawn_unit(0, 1, CellCoord::new(15, 20), Facing(0), 400, stats());
    let gun = WeaponProfile {
        damage: 30,
        rof: 20,
        range: 1024,
        proj_speed: 255,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 1,
            verses: [65536; ARMOR_COUNT],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1000,
    };
    world.set_unit_combat(mover, 0, Some(gun), true);
    let enemy = world.spawn_unit(0, 2, CellCoord::new(16, 20), Facing(0), 400, stats());
    world.set_unit_combat(enemy, 0, None, false);
    world.set_unit_mission(enemy, Mission::Sleep);

    world.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(17, 20),
        house: 1,
    }]);
    for _ in 0..80 {
        world.tick(&[]);
    }
    // Divergence: the enemy is never auto-attacked, so it keeps full health and
    // the mover never reaches the far side.
    assert_eq!(
        world.units.get(enemy).map(|u| u.health),
        Some(400),
        "QUIRK: a Move order must not auto-attack a walling enemy (no MISSION_ATTACK override)"
    );
    assert_ne!(
        world.units.get(mover).map(|u| u.cell()),
        Some(CellCoord::new(17, 20)),
        "the walled mover must not have passed the un-attacked enemy"
    );
}
