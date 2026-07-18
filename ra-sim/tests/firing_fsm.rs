//! Firing FSM integration tests (M4, DESIGN.md §4.4's fixed system order):
//! the approach/aim/fire state machine `run_combat` drives every tick,
//! exercised at the `World::tick` level (as opposed to `combat.rs`'s
//! pure-function unit tests or `world.rs`'s own colocated combat tests,
//! which this file complements rather than duplicates — see each test's doc
//! comment for what's new here).
//!
//! Covers: out-of-range approach-then-fire, the turret alignment gate
//! blocking fire, exact ROF shot cadence, target death mid-bullet-flight
//! (no dangling-handle panic, TarCom cleared, a slot-reused successor unit
//! is not accidentally damaged), and `Move` overriding `Attack`.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 25,
        rot: 10,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// 2TNK's 90mm cannon: AP, Damage 30, ROF 50, Range 4.75 cells (1216
/// leptons), Speed 40 -> 102 leptons/tick, non-instant (straight flight).
fn ninety_mm() -> WeaponProfile {
    WeaponProfile {
        damage: 30,
        rof: 50,
        range: 1216,
        proj_speed: 102,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 3,
            verses: pct5([30, 75, 75, 100, 50]),
        },
        warhead_ap: true,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

fn world() -> World {
    World::new(Passability::all_passable(), 0xF00D_1234)
}

fn spawn_tank(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, stats());
    w.set_unit_combat(h, 3 /* heavy=steel */, Some(ninety_mm()), true);
    h
}

// ---------------------------------------------------------------------
// 1. Out-of-range -> approach -> in range -> fire.
// ---------------------------------------------------------------------

/// A unit ordered to attack a target well outside its weapon's range must
/// first *approach* (path toward the target, no bullets, no damage), and
/// only start firing once it has closed to within `weapon.range`. This is
/// new coverage: `world.rs`'s own `tank_kills_adjacent_enemy_...` test
/// starts the units already adjacent (in range from tick 0), so it never
/// exercises the approach branch of `run_combat` at all.
#[test]
fn out_of_range_unit_approaches_then_fires() {
    let mut w = world();
    // 90mm range is 1216 leptons (4.75 cells); start the attacker 12 cells
    // away so it is unambiguously out of range and must travel.
    let atk = spawn_tank(&mut w, 1, CellCoord::new(5, 5), 400);
    let tgt = w.spawn_unit(0, 2, CellCoord::new(17, 5), Facing(0), 600, stats());
    w.set_unit_combat(tgt, 3, None, false);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    }]);

    // Immediately after the order: still far out of range, so it must be
    // pathing (not idle) and must not have fired (no bullets, full health).
    assert!(
        w.units.get(atk).unwrap().is_moving(),
        "out-of-range attacker should be approaching, not idle"
    );
    assert!(w.bullets.is_empty(), "should not fire while out of range");
    assert_eq!(w.units.get(tgt).unwrap().health, 600);

    // Run until either it starts shooting (bullet count > 0 at some tick, or
    // target takes damage) or a generous timeout.
    let mut started_firing = false;
    for _ in 0..400 {
        w.tick(&[]);
        if !w.bullets.is_empty() || w.units.get(tgt).unwrap().health < 600 {
            started_firing = true;
            break;
        }
    }
    assert!(
        started_firing,
        "attacker never started firing after closing distance"
    );
    // Once in range and firing, the unit should have stopped pathing (the
    // in-range branch clears path/dest — `run_combat`'s "hold position"
    // behavior).
    assert!(
        !w.units.get(atk).unwrap().is_moving(),
        "attacker should hold position once in range, not keep approaching"
    );
}

// ---------------------------------------------------------------------
// 2. Turret alignment gate: no fire until |diff| < 8.
// ---------------------------------------------------------------------

/// A unit already in range but facing away from its target must rotate its
/// turret into alignment before firing — `aligned_to_fire`'s `< 8` gate,
/// exercised here through the full tick pipeline (as opposed to
/// `combat.rs`'s direct unit test of the pure `aligned_to_fire` function).
#[test]
fn turret_must_align_before_firing() {
    let mut w = world();
    // Attacker faces due "south" (128) initially; target is due "north" (an
    // adjacent cell above it), so the desired aim direction is ~0/north —
    // about a half-turn away from the starting facing, guaranteeing several
    // ticks of turret rotation before `aligned_to_fire` is satisfied.
    let atk = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(128), 400, stats());
    w.set_unit_combat(atk, 3, Some(ninety_mm()), true);
    let tgt = w.spawn_unit(0, 2, CellCoord::new(10, 9), Facing(0), 600, stats());
    w.set_unit_combat(tgt, 3, None, false);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    }]);
    // Already in range (adjacent), so it must NOT be pathing (the in-range
    // branch clears path immediately regardless of alignment)...
    assert!(!w.units.get(atk).unwrap().is_moving());
    // ...but it must not have fired yet: turret starts a half-turn (~128
    // binary-angle units) away from the desired direction, rotating at
    // `rot+1 = 11` units/tick, so alignment (<8 units off) takes multiple
    // ticks — well more than the single tick just elapsed.
    assert!(
        w.bullets.is_empty(),
        "should not fire before the turret has aligned"
    );
    assert_eq!(w.units.get(tgt).unwrap().health, 600);

    // Advance until it fires; confirm it took more than one tick (i.e. the
    // gate genuinely delayed firing) and that the turret facing at the
    // moment of firing really is within the documented tolerance of the
    // desired direction.
    let mut fired_after = None;
    for t in 1..=30 {
        w.tick(&[]);
        if !w.bullets.is_empty() {
            fired_after = Some(t);
            break;
        }
    }
    let fired_after = fired_after.expect("attacker never fired once turret should have aligned");
    assert!(
        fired_after > 1,
        "attacker fired on the very first eligible tick, meaning the alignment gate was not \
         actually delaying it (fired_after={fired_after})"
    );
    let turret = w.units.get(atk).unwrap().turret_facing;
    // Desired direction from (10,10) to (10,9) is due north (Facing 0).
    let diff = turret.difference(Facing(0)).abs();
    assert!(
        diff < 8,
        "bullet spawned while the turret was still {diff} units off (>= the 8-unit gate)"
    );
}

// ---------------------------------------------------------------------
// 3. ROF cadence: shots spaced exactly ROF ticks apart.
// ---------------------------------------------------------------------

/// Once a unit is aligned and in range, consecutive shots must land exactly
/// `ROF` ticks apart (the rearm countdown), not merely "eventually kill the
/// target in roughly the right number of ticks" (which
/// `tank_kills_adjacent_enemy_with_expected_shot_count` in `world.rs`
/// already checks as a sanity *bound*, not an exact cadence).
#[test]
fn rof_cadence_is_exact() {
    let mut w = world();
    let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
    // Adjacent and already facing the target (Facing(0) is due... the
    // attacker's initial facing is 0/north, and the target is due east; to
    // avoid the alignment gate interfering with this cadence measurement,
    // place the target due north instead so the attacker is aligned from
    // tick 0.
    let tgt = w.spawn_unit(0, 2, CellCoord::new(10, 9), Facing(0), 60_000, stats());
    w.set_unit_combat(tgt, 3, None, false);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    }]);

    // Record every tick (relative to the Attack-issuing tick, tick 0) at
    // which a bullet is freshly spawned (bullets arena transitions empty ->
    // non-empty within that tick's pipeline). 90mm has ROF=50; collect
    // enough shots to check several consecutive gaps.
    let mut shot_ticks = Vec::new();
    if !w.bullets.is_empty() {
        shot_ticks.push(0u32);
    }
    for t in 1..=260u32 {
        let before = w.bullets.len();
        w.tick(&[]);
        // A shot happened this tick if the bullet count increased (it may
        // also decrease/detonate the same tick for very fast projectiles,
        // but 90mm's 102 leptons/tick over an adjacent 256-lepton range
        // takes multiple ticks to arrive, so a same-tick spawn+detonate
        // collision cannot mask a spawn here).
        if w.bullets.len() > before {
            shot_ticks.push(t);
        }
    }
    assert!(
        shot_ticks.len() >= 4,
        "expected several shots in 260 ticks at ROF=50, got {}: {:?}",
        shot_ticks.len(),
        shot_ticks
    );
    for pair in shot_ticks.windows(2) {
        let gap = pair[1] - pair[0];
        assert_eq!(
            gap, 50,
            "consecutive shots should be exactly ROF=50 ticks apart, got a {gap}-tick gap in {:?}",
            shot_ticks
        );
    }
}

// ---------------------------------------------------------------------
// 4. Target dies mid-flight of a bullet: no dangling-handle panic, TarCom
//    clears, and a slot-reused successor unit is not accidentally hit.
// ---------------------------------------------------------------------

/// The target is killed by an out-of-band cause (simulating "a second
/// attacker's shot landed first") *while a bullet from this attacker is
/// still travelling toward it*. Must not panic, the in-flight bullet must
/// detonate harmlessly (no damage applied to nothing), and the attacker's
/// TarCom must clear on the next combat tick. Then a brand-new unit is
/// spawned (likely reusing the freed arena slot with a bumped generation)
/// to confirm the stale `Target::Unit` handle a lingering bullet might still
/// carry can never alias the new occupant.
#[test]
fn target_death_mid_flight_clears_tarcom_without_panic() {
    let mut w = world();
    let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
    // Distance = 1216 leptons is exactly 2TNK's range boundary; use 1000
    // leptons (< range) so it's in range but the 102-leptons/tick bullet
    // takes several ticks to arrive, giving room to kill the target first.
    let tgt = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 50, stats());
    // Reposition precisely: put target 1000 leptons north of the attacker.
    {
        let u = w.units.get_mut(tgt).unwrap();
        u.coord = ra_sim::coords::WorldCoord::new(2688, 2688 - 1000);
    }
    w.set_unit_combat(tgt, 3, None, false);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    }]);
    // Step until the attacker fires (turret must align first).
    let mut fired = false;
    for _ in 0..30 {
        w.tick(&[]);
        if !w.bullets.is_empty() {
            fired = true;
            break;
        }
    }
    assert!(fired, "attacker never fired");
    assert!(
        !w.bullets.is_empty(),
        "a bullet should still be in flight (1000 leptons at 102/tick takes several ticks)"
    );

    // Kill the target out-of-band (simulating some other damage source) —
    // legitimate direct access via `World::units`, which is `pub`.
    let tgt_index = tgt.index;
    w.units.remove(tgt);
    assert!(!w.units.contains(tgt));

    // Advance several ticks: the in-flight bullet (aimed at a now-stale
    // handle) must detonate without panicking, and the attacker's TarCom
    // must clear on the very next combat tick (before the bullet even
    // lands, since `run_combat` drops a dead/stale unit target
    // immediately).
    w.tick(&[]);
    assert!(
        !w.units.get(atk).unwrap().has_target(),
        "attacker should have dropped its TarCom the tick after the target died"
    );
    for _ in 0..20 {
        w.tick(&[]); // must not panic even once the stale bullet detonates
    }
    assert!(
        w.bullets.is_empty(),
        "the stale-target bullet should have detonated and been removed"
    );

    // Spawn a fresh unit — the generational arena reuses the freed slot
    // (lowest-freed-index-first), so this new unit very likely gets
    // `tgt.index` again, but at a bumped generation.
    let successor = w.spawn_unit(0, 2, CellCoord::new(10, 9), Facing(0), 999, stats());
    if successor.index == tgt_index {
        assert_ne!(
            successor.gen, tgt.gen,
            "slot reuse must bump the generation, or a stale handle could alias the new unit"
        );
    }
    // The (long-cleared) attacker TarCom must not have somehow reattached
    // to the successor just because it landed in the same slot.
    assert!(!w.units.get(atk).unwrap().has_target());
    // One more tick for good measure: still no panic, successor untouched.
    w.tick(&[]);
    assert_eq!(w.units.get(successor).unwrap().health, 999);
}

// ---------------------------------------------------------------------
// 5. Move overrides Attack.
// ---------------------------------------------------------------------

/// Issuing a `Move` to a unit that is currently attacking must cancel the
/// attack (clear TarCom) and start it moving toward the new destination —
/// `apply_command`'s documented "a move order overrides an attack" behavior,
/// exercised end-to-end (spawn -> Attack -> Move -> assert) rather than only
/// implied by `apply_command`'s doc comment.
#[test]
fn move_command_overrides_attack() {
    let mut w = world();
    let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
    let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
    w.set_unit_combat(tgt, 3, None, false);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    }]);
    assert!(w.units.get(atk).unwrap().has_target());

    w.tick(&[Command::Move {
        unit: atk,
        dest: CellCoord::new(0, 0),
        house: 1,
    }]);
    assert!(
        !w.units.get(atk).unwrap().has_target(),
        "Move should have cleared the in-progress Attack's TarCom"
    );
    assert!(
        w.units.get(atk).unwrap().is_moving(),
        "unit should now be pathing toward the Move destination"
    );

    // Advance a few ticks: the attacker must not fire on its former target
    // even though it may still be technically in range for a moment while
    // pathing away.
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(tgt).unwrap().health,
        600,
        "target should be untouched — the attack was overridden before it could fire"
    );
}
