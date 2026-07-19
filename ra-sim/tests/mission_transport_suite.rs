//! M7.5-B smoke tests (ra-coder): prove the per-unit mission layer (Q18 P0) and
//! the APC transport system (P1) run end-to-end through `World::tick`. These are
//! deliberately minimal — ra-tester owns the exhaustive matrix (leash distances,
//! Area-Guard return-home, Sleep/Sticky non-retaliation, base-alert propagation,
//! die-with-transport, teamtype LOAD/UNLOAD sequencing).

use ra_sim::campaign::Campaign;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Mission, MoveStats, Passability, WarheadProfile, WeaponProfile, World};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 20,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// A short-range instant weapon so an in-range shot resolves immediately.
fn gun() -> WeaponProfile {
    WeaponProfile {
        damage: 20,
        rof: 20,
        range: 3 * 256, // three cells
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 3,
            verses: pct5([100, 100, 100, 100, 100]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// A minimal (empty) campaign so proactive guard behaviour is enabled (Q18: the
/// guard scan + base-alert are campaign-scoped).
fn empty_campaign() -> Campaign {
    Campaign {
        triggers: Vec::new(),
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 64],
        cell_triggers: Vec::new(),
        state: Vec::new(),
        started: true,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 16],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    }
}

fn campaign_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0x0BAD_F00D);
    w.set_campaign(empty_campaign());
    w
}

#[test]
fn guard_unit_auto_acquires_and_engages_an_enemy_in_range() {
    let mut w = campaign_world();
    // A Guard-mission defender (house 2) and an enemy (house 1) two cells away —
    // inside the defender's 3-cell weapon range. No orders are issued.
    let guard = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    w.set_unit_mission(guard, Mission::Guard);
    let enemy = w.spawn_unit(0, 1, CellCoord::new(12, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    // Keep the enemy passive (skirmish default has no proactive acquire, but this
    // is a campaign world — give it Sleep so only the Guard's behaviour is tested).
    w.set_unit_mission(enemy, Mission::Sleep);

    let start_hp = w.units.get(enemy).unwrap().health;
    let mut acquired = false;
    for _ in 0..40 {
        w.tick(&[]);
        if w.units.get(guard).and_then(|u| u.target).is_some() {
            acquired = true;
        }
        if w.units.get(enemy).map(|u| u.health).unwrap_or(0) < start_hp {
            break;
        }
    }
    assert!(acquired, "a Guard unit must auto-acquire an enemy in range");
    assert!(
        w.units.get(enemy).unwrap().health < start_hp,
        "the Guard unit must actually engage (damage) the acquired enemy"
    );
    // A Sleep unit never retaliates: the guard took no return fire.
    assert!(
        w.units.get(guard).unwrap().health == 400,
        "a Sleep-mission unit must not retaliate"
    );
}

#[test]
fn guard_leashes_when_the_target_leaves_weapon_range() {
    let mut w = campaign_world();
    let guard = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    w.set_unit_mission(guard, Mission::Guard);
    let enemy = w.spawn_unit(0, 1, CellCoord::new(11, 10), Facing(0), 4000, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    w.tick(&[]);
    assert!(
        w.units.get(guard).unwrap().target.is_some(),
        "guard should have acquired the adjacent enemy"
    );
    // Teleport the enemy far outside weapon range; the guard must drop it (leash —
    // plain Guard never chases) rather than pathing after it.
    let far = CellCoord::new(40, 40);
    w.units.get_mut(enemy).unwrap().coord = far.center();
    w.tick(&[]);
    let g = w.units.get(guard).unwrap();
    assert!(g.target.is_none(), "Guard must drop an out-of-range target");
    assert!(
        g.path.is_empty(),
        "Guard must not chase (stays at its post)"
    );
    assert_eq!(g.cell(), CellCoord::new(10, 10), "guard stayed put");
}

#[test]
fn apc_load_move_unload_cycle() {
    // A skirmish world is fine for the transport commands (they are not
    // campaign-gated); no campaign needed.
    let mut w = World::new(Passability::all_passable(), 0x1234_5678);
    // APC (house 1) with capacity 5.
    let apc = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 5);
    // An infantryman adjacent to the APC.
    let soldier = w.spawn_unit(0, 1, CellCoord::new(6, 5), Facing(0), 50, stats());
    w.set_unit_combat(soldier, 0, Some(gun()), false);
    w.units.get_mut(soldier).unwrap().make_infantry(0);

    // Load: the soldier boards the adjacent APC this tick.
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    assert!(
        w.units.get(soldier).is_none(),
        "a boarded passenger leaves the map"
    );
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "the APC now carries one passenger"
    );

    // Drive the APC a few cells away.
    w.tick(&[Command::Move {
        unit: apc,
        dest: CellCoord::new(15, 15),
        house: 1,
    }]);
    for _ in 0..200 {
        w.tick(&[]);
        if w.units.get(apc).map(|u| u.path.is_empty()).unwrap_or(true) {
            break;
        }
    }

    // Unload: the passenger re-materialises adjacent to the APC.
    w.tick(&[Command::Unload {
        transport: apc,
        house: 1,
    }]);
    assert!(
        w.units.get(apc).unwrap().cargo.is_empty(),
        "the APC unloaded its cargo"
    );
    let disgorged = w
        .units
        .iter()
        .find(|(h, u)| *h != apc && u.house == 1 && u.is_infantry())
        .map(|(_, u)| u.cell());
    let apc_cell = w.units.get(apc).unwrap().cell();
    let c = disgorged.expect("an infantryman re-appeared after unload");
    assert!(
        (c.x - apc_cell.x).abs() <= 1 && (c.y - apc_cell.y).abs() <= 1,
        "the unloaded passenger stands adjacent to the transport"
    );
}

#[test]
fn passengers_die_with_the_transport() {
    let mut w = World::new(Passability::all_passable(), 0x9);
    let apc = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 1, stats());
    w.set_unit_capacity(apc, 5);
    let soldier = w.spawn_unit(0, 1, CellCoord::new(6, 5), Facing(0), 50, stats());
    w.units.get_mut(soldier).unwrap().make_infantry(0);
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(w.units.get(apc).unwrap().cargo.len(), 1);
    // Destroy the transport outright; the passenger is gone with it.
    w.units.remove(apc);
    let infantry_left = w.units.iter().filter(|(_, u)| u.is_infantry()).count();
    assert_eq!(
        infantry_left, 0,
        "passengers are destroyed with their transport"
    );
}
