//! M7.6 infantry sub-cell suite (QUIRKS Q7): integration-level coverage of
//! the five-spots-per-cell mechanism through the public `World`/`Command`
//! API. Complements `ra-sim/src/occupancy.rs`'s own colocated `UnitGrid`
//! unit tests (which exercise the bitmask math in isolation) with tests that
//! drive real infantry through real moves.
//!
//! Covers item 3 of the M7.6 test plan: spot-assignment determinism, the
//! canonical 5-spot pack + 6th-infantry overflow, mixed vehicle/infantry
//! cell rules (pinned as-implemented, with a divergence flagged against both
//! the original `Can_Enter_Cell` and against QUIRKS.md's own Q5.3 wording —
//! see the dedicated tests below), and spot reuse after death.

use ra_sim::coords::{CellCoord, Facing, SPOT_OFFSET, SUBCELL_COUNT};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// Spawn an infantry unit (vehicle-spawned then converted, exactly the
/// sequence `spawn_produced_unit` uses: spawn, then `make_infantry`).
fn spawn_infantry(world: &mut World, type_id: u32, house: u8, cell: CellCoord) -> Handle {
    let h = world.spawn_unit(type_id, house, cell, Facing(0), 50, stats(20, 8));
    world.units.get_mut(h).unwrap().make_infantry(0);
    h
}

fn infantry_at(world: &World, cell: CellCoord) -> Vec<Handle> {
    world
        .units
        .iter()
        .filter(|(_, u)| u.is_infantry() && u.cell() == cell)
        .map(|(h, _)| h)
        .collect()
}

// ===========================================================================
// 1. `SPOT_OFFSET` matches the transcribed `StoppingCoordAbs[5]`
//    (`const.cpp:282`) values verbatim.
// ===========================================================================

#[test]
fn spot_offset_table_matches_stopping_coord_abs() {
    // `StoppingCoordAbs[5] = {0x00800080, 0x00400040, 0x004000C0, 0x00C00040,
    // 0x00C000C0}` (`const.cpp:282`): a `COORDINATE` packs Y in the high 16
    // bits, X in the low 16 (`Coord_X`/`Coord_Y`), so index i's (x, y) offset
    // in leptons is (low16, high16).
    let expected: [(i32, i32); SUBCELL_COUNT] = [
        (0x0080, 0x0080), // 0 center
        (0x0040, 0x0040), // 1 upper-left
        (0x00C0, 0x0040), // 2 upper-right
        (0x0040, 0x00C0), // 3 lower-left
        (0x00C0, 0x00C0), // 4 lower-right
    ];
    assert_eq!(SPOT_OFFSET, expected);
}

// ===========================================================================
// 2. Spot assignment determinism: the same script run twice assigns the
//    same spots.
// ===========================================================================

fn pack_script(seed: u32) -> (World, Vec<Handle>) {
    let mut world = World::new(Passability::all_passable(), seed);
    let target = CellCoord::new(30, 30);
    // Five distinct approach cells, spread so their paths interleave rather
    // than all arriving in slot order down one line.
    let starts = [
        CellCoord::new(30, 25),
        CellCoord::new(35, 30),
        CellCoord::new(30, 35),
        CellCoord::new(25, 30),
        CellCoord::new(27, 27),
    ];
    let handles: Vec<Handle> = starts
        .iter()
        .enumerate()
        .map(|(i, &c)| spawn_infantry(&mut world, i as u32, 1, c))
        .collect();
    let cmds: Vec<Command> = handles
        .iter()
        .map(|&unit| Command::Move {
            unit,
            dest: target,
            house: 1,
        })
        .collect();
    world.tick(&cmds);
    for _ in 0..80 {
        world.tick(&[]);
    }
    (world, handles)
}

#[test]
fn spot_assignment_is_deterministic_same_script_twice() {
    let (world_a, handles_a) = pack_script(0x5B07_0001);
    let (world_b, handles_b) = pack_script(0x5B07_0001);
    let spots_a: Vec<u8> = handles_a
        .iter()
        .map(|&h| world_a.units.get(h).unwrap().sub_cell)
        .collect();
    let spots_b: Vec<u8> = handles_b
        .iter()
        .map(|&h| world_b.units.get(h).unwrap().sub_cell)
        .collect();
    assert_eq!(
        spots_a, spots_b,
        "same script, same seed must assign the same sub-cell spots"
    );
}

// ===========================================================================
// 3. Five infantry pack one cell at the 5 canonical offsets; a 6th disperses
//    to an adjacent cell.
// ===========================================================================

#[test]
fn five_infantry_pack_one_cell_at_the_canonical_offsets() {
    let (world, handles) = pack_script(0x5B07_0002);
    let target = CellCoord::new(30, 30);
    let occupants = infantry_at(&world, target);
    assert_eq!(
        occupants.len(),
        5,
        "all 5 infantry should have packed the target cell"
    );
    let mut spots: Vec<u8> = handles
        .iter()
        .map(|&h| world.units.get(h).unwrap())
        .map(|u| {
            assert_eq!(
                u.cell(),
                target,
                "every infantryman should reach the target cell"
            );
            assert!(
                !u.is_moving(),
                "every infantryman should have finished settling into its spot"
            );
            u.sub_cell
        })
        .collect();
    spots.sort_unstable();
    assert_eq!(
        spots,
        vec![0, 1, 2, 3, 4],
        "the 5 infantry should occupy all 5 distinct sub-cell spots, not double up"
    );
    // Each unit's world coordinate is exactly its cell's `spot_center` — the
    // canonical offset table, exercised end-to-end (not just as a static
    // table, per test 1 above).
    for &h in &handles {
        let u = world.units.get(h).unwrap();
        assert_eq!(u.coord, target.spot_center(u.sub_cell));
    }
}

#[test]
fn sixth_infantry_disperses_to_an_adjacent_cell() {
    let mut world = World::new(Passability::all_passable(), 0x5B07_0003);
    let target = CellCoord::new(30, 30);
    let starts = [
        CellCoord::new(30, 25),
        CellCoord::new(35, 30),
        CellCoord::new(30, 35),
        CellCoord::new(25, 30),
        CellCoord::new(27, 27),
        CellCoord::new(33, 33), // the 6th
    ];
    let handles: Vec<Handle> = starts
        .iter()
        .enumerate()
        .map(|(i, &c)| spawn_infantry(&mut world, i as u32, 1, c))
        .collect();
    let cmds: Vec<Command> = handles
        .iter()
        .map(|&unit| Command::Move {
            unit,
            dest: target,
            house: 1,
        })
        .collect();
    world.tick(&cmds);
    for _ in 0..100 {
        world.tick(&[]);
    }
    for &h in &handles {
        assert!(
            !world.units.get(h).unwrap().is_moving(),
            "every infantryman should have finished moving"
        );
    }
    let at_target = infantry_at(&world, target);
    assert_eq!(
        at_target.len(),
        5,
        "the target cell holds at most 5 infantry"
    );
    let sixth = *handles.last().unwrap();
    let sixth_cell = world.units.get(sixth).unwrap().cell();
    assert_ne!(
        sixth_cell, target,
        "the 6th infantryman must not have packed into the already-full target cell"
    );
    let dist = (sixth_cell.x - target.x)
        .abs()
        .max((sixth_cell.y - target.y).abs());
    assert!(
        dist <= 2,
        "the 6th infantryman should disperse to a *nearby* cell (got {dist} cells away)"
    );
    // No cell anywhere ever exceeds 5 infantry (restate the invariant for
    // every cell any unit ended up on, not just the target).
    for &h in &handles {
        let c = world.units.get(h).unwrap().cell();
        assert!(
            infantry_at(&world, c).len() <= 5,
            "cell {c:?} exceeds 5 infantry"
        );
    }
}

// ===========================================================================
// 4. Mixed vehicle+infantry cell rules — pinned AS IMPLEMENTED, with a
//    divergence flagged (see doc comments) against both the original
//    `Can_Enter_Cell` (`unit.cpp:3400`) and QUIRKS.md's own Q5.3 wording.
// ===========================================================================

/// **Pinned current behavior — flagged divergence, not a fix.**
///
/// QUIRKS.md's Q5.3 ("No crushing") reads: "a vehicle simply cannot enter a
/// cell whose infantry spots leave it (for the movement gate)
/// impassable-equivalent." Reading the actual gate in `world.rs::move_units`
/// (`is_blocked`'s `occ_block` computation), a **vehicle** mover's occupancy
/// check is `grid.vehicle_blocked_for(land, handle)`, which only consults
/// `UnitGrid`'s `veh` map — it never looks at `spots` at all. So as
/// currently implemented, a vehicle is **not** blocked by infantry occupying
/// a cell, fully packed or not; it can drive straight onto/through them. The
/// original's `Can_Enter_Cell` (`unit.cpp:3400`, cited in QUIRKS Q5) *does*
/// treat vehicle-vs-infantry interaction specially (crush eligibility /
/// blockage depending on `IsCrushable` in the same function). This test
/// pins **today's actual behavior** (vehicle passes through freely) so any
/// future change to close this gap shows up here as an intentional,
/// reviewed diff — and flags the QUIRKS.md wording as currently inaccurate
/// for ra-coder to reconcile (either implement the guard the doc describes,
/// or correct the doc to say vehicle/infantry co-occupancy is unblocked).
#[test]
fn vehicle_currently_passes_through_an_infantry_full_cell_unblocked() {
    let mut world = World::new(Passability::all_passable(), 0xB0A6_0001);
    let target = CellCoord::new(20, 20);
    // Pack the cell with 5 infantry first.
    let infantry: Vec<Handle> = (0..5)
        .map(|i| spawn_infantry(&mut world, i, 1, CellCoord::new(20, 15 + i as i32)))
        .collect();
    let cmds: Vec<Command> = infantry
        .iter()
        .map(|&unit| Command::Move {
            unit,
            dest: target,
            house: 1,
        })
        .collect();
    world.tick(&cmds);
    for _ in 0..60 {
        world.tick(&[]);
    }
    assert_eq!(
        infantry_at(&world, target).len(),
        5,
        "sanity: cell packed full"
    );

    // Now drive a vehicle straight through the same cell.
    let vehicle = world.spawn_unit(9, 2, CellCoord::new(20, 10), Facing(128), 400, stats(24, 8));
    world.tick(&[Command::Move {
        unit: vehicle,
        dest: CellCoord::new(20, 30),
        house: 2,
    }]);
    for _ in 0..250 {
        world.tick(&[]);
    }
    let vu = world.units.get(vehicle).unwrap();
    assert!(
        !vu.is_moving(),
        "the vehicle should have completed its move"
    );
    assert_eq!(
        vu.cell(),
        CellCoord::new(20, 30),
        "as currently implemented, the vehicle reaches its destination straight through the \
         infantry-packed cell, unblocked"
    );
}

/// Mirror of the above in the other direction: an infantry unit's occupancy
/// gate (`!grid.has_free_spot(land)`) only checks the `spots` bitmask, never
/// `veh` — so infantry are similarly **not** blocked from stepping onto a
/// cell a vehicle already occupies, as currently implemented.
#[test]
fn infantry_currently_enters_a_vehicle_occupied_cell_unblocked() {
    let mut world = World::new(Passability::all_passable(), 0xB0A6_0002);
    let target = CellCoord::new(20, 20);
    let vehicle = world.spawn_unit(9, 2, target, Facing(0), 400, stats(24, 8));
    let _ = vehicle; // parked, never ordered to move

    let inf = spawn_infantry(&mut world, 0, 1, CellCoord::new(20, 10));
    world.tick(&[Command::Move {
        unit: inf,
        dest: target,
        house: 1,
    }]);
    for _ in 0..150 {
        world.tick(&[]);
    }
    let iu = world.units.get(inf).unwrap();
    assert!(
        !iu.is_moving(),
        "the infantryman should have completed its move"
    );
    assert_eq!(
        iu.cell(),
        target,
        "as currently implemented, infantry can step onto a vehicle-occupied cell unblocked"
    );
}

// ===========================================================================
// 5. Infantry death frees its spot for reuse.
// ===========================================================================

/// The real M60mg/SA profile (`ra-sim/tests/damage_matrix.rs`'s `sa_case()`:
/// `Damage=15`, `Spread=3`, `Verses=100%` vs. `none` armor) — the same "SA
/// splash-kill" weapon the M7.6 review brief calls out, chosen deliberately
/// *instead of* an overkill weapon: `explosion_damage` (`world.rs`) applies
/// **every** hit as a full-radius area blast (QUIRKS Q4 — "full friendly
/// fire", 384-lepton radius) even for a single-target attack order, so an
/// overkill weapon would blast-kill all 5 packed infantry at once (they sit
/// well within 384 leptons of each other) and this test would not isolate
/// "one death frees one spot". At the sub-cell spot separations here (64..181
/// leptons), this weapon's falloff leaves the 4 survivors (50 HP) with a
/// scratch (1-3 damage) while one-shotting the victim once its health is
/// dropped to 1 below.
fn sa_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 15,
        rof: 20,
        range: 3 * 256,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 3,
            verses: [65536, 32768, 39321, 16384, 16384], // [none,wood,light,heavy,concrete]
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

#[test]
fn infantry_death_frees_its_spot_for_reuse() {
    let (mut world, handles) = pack_script(0x5B07_0004);
    let target = CellCoord::new(30, 30);
    assert_eq!(
        infantry_at(&world, target).len(),
        5,
        "sanity: cell packed full"
    );
    let victim = handles[0];
    let victim_spot = world.units.get(victim).unwrap().sub_cell;
    // Drop the victim to 1 HP so the SA weapon's direct-hit damage (15, full
    // `Verses=100%` vs. `none` armor) kills only it — the survivors' 50 HP
    // easily rides out the splash fringe (see `sa_weapon`'s doc comment).
    world.units.get_mut(victim).unwrap().health = 1;

    // An attacker aligned and in range (same trick
    // `factory_abandon_suite.rs::kill_building` uses).
    let atk = world.spawn_unit(
        99,
        9,
        CellCoord::new(30, 29),
        Facing(128),
        400,
        stats(20, 8),
    );
    world.set_unit_combat(atk, 0, Some(sa_weapon()), true);
    world.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(victim),
        house: 9,
    }]);
    assert!(
        !world.units.contains(victim),
        "the infantryman should be dead"
    );
    assert_eq!(
        infantry_at(&world, target).len(),
        4,
        "the cell should now hold only the 4 survivors"
    );
    for &h in &handles[1..] {
        let hp = world.units.get(h).unwrap().health;
        assert!(
            hp > 0,
            "a survivor at {victim_spot}-adjacent spot died too — splash isolation failed"
        );
    }

    // A 6th infantryman ordered to the same cell should now take the freed
    // spot (not disperse to a neighbour, since a spot is free again).
    let sixth = spawn_infantry(&mut world, 50, 1, CellCoord::new(30, 24));
    world.tick(&[Command::Move {
        unit: sixth,
        dest: target,
        house: 1,
    }]);
    for _ in 0..80 {
        world.tick(&[]);
    }
    let su = world.units.get(sixth).unwrap();
    assert!(
        !su.is_moving(),
        "the 6th infantryman should have finished moving"
    );
    assert_eq!(
        su.cell(),
        target,
        "the 6th infantryman should reuse the freed spot in the target cell"
    );
    assert_eq!(
        su.sub_cell, victim_spot,
        "closest_free_spot should reassign the exact spot the victim vacated (deterministic, only one free spot to pick)"
    );
    assert_eq!(
        infantry_at(&world, target).len(),
        5,
        "the cell is full again"
    );
}
