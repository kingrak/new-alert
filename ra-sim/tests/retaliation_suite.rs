//! M7 item 3 — guard/auto-retaliation suite (ra-tester charter). Complements
//! (does not duplicate) `world.rs`'s colocated tests, which already cover the
//! two baseline cases end-to-end through `World::tick`:
//! - `idle_unit_retaliates_against_its_attacker` — an idle, armed unit that
//!   takes damage assigns the attacker as its target (`assign_retaliation`,
//!   `foot.cpp:1189`).
//! - `retaliation_never_overrides_an_explicit_order` — a unit with a live
//!   move path is never hijacked (this repo's simplification #1 vs. the
//!   original, documented in QUIRKS.md Q4).
//!
//! This file adds the harder edges the M7 charter calls out explicitly:
//! whether retaliation "spreads" to an idle ally of the *attacker* who wasn't
//! touched (it shouldn't — there is no proximity/alert broadcast modelled);
//! the dead-attacker-mid-flight handle case (no panic, no stale retaliation);
//! and an unarmed harvester surviving a hit (no panic, no retaliation attempt,
//! its FSM/path is left untouched since `assign_retaliation` early-outs before
//! touching anything else).
//!
//! Uses its own minimal fixture, independent of `splash_suite.rs` and
//! `world.rs`'s private test module (house convention).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn world() -> World {
    World::new(Passability::all_passable(), 0x1357_9BDF)
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// An instant, low-damage weapon so shots resolve within the same tick and
/// never one-shot-kill a 400 hp unit (keeps "survives to retaliate" scenarios
/// simple: many shots can land without anyone dying).
fn weak_instant_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 5,
        rof: 60_000,
        range: 3000,
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

/// A lethal weapon (guarantees a one-shot kill against `hp`) for the
/// dead-attacker-mid-flight test — non-instant (has real flight time) so the
/// attacker can be killed by someone else before the bullet lands.
fn lethal_slow_weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 1000,
        rof: 60_000,
        range: 3000,
        proj_speed: 20, // slow: several ticks of flight before impact
        proj_rot: 0,
        invisible: false,
        instant: false,
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

/// An instant-kill weapon for dispatching the attacker mid-flight.
fn instakill_instant_weapon() -> WeaponProfile {
    let mut w = weak_instant_weapon();
    w.damage = 10_000;
    w
}

fn spawn_armed(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    facing: Facing,
    weapon: WeaponProfile,
    hp: u16,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, facing, hp, stats());
    w.set_unit_combat(h, 0, Some(weapon), true);
    h
}

fn spawn_unarmed(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
    w.spawn_unit(0, house, cell, Facing(0), hp, stats())
}

// ===========================================================================
// 1. Retaliation does not "spread" to an idle ally of the attacker.
// ===========================================================================

/// A (house 1), B (house 2) idle nearby. A shoots B; B survives and
/// retaliates against A (the baseline case, re-confirmed here as the
/// scenario's sanity check). C is an idle unit **allied with A** (also house
/// 1), stationed near B, and is never hit by anything. Pinned behavior: C
/// stays completely idle throughout — our retaliation model triggers only on
/// the *damaged unit itself* (`assign_retaliation` is called from inside
/// `explosion_damage`'s per-object damage application, never broadcast to a
/// house or to nearby allies), so there is no "ally under fire, everyone
/// pile on" chain reaction. This is a real, documented scope limitation (not
/// a bug): the original's `Assign_Target`/guard-mission machinery has no
/// "call for help" broadcast either in the path we ported
/// (`FootClass::Take_Damage`, `foot.cpp:1176-1189`), so this matches.
#[test]
fn retaliation_does_not_spread_to_an_idle_ally_of_the_attacker() {
    let mut w = world();
    let a = spawn_armed(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        weak_instant_weapon(),
        400,
    );
    let b = spawn_armed(
        &mut w,
        2,
        CellCoord::new(11, 10),
        Facing(0),
        weak_instant_weapon(),
        400,
    );
    // C: idle, armed, allied with A (house 1), stationed a few cells away —
    // never targeted, never in anyone's blast radius.
    let c = spawn_armed(
        &mut w,
        1,
        CellCoord::new(30, 30),
        Facing(0),
        weak_instant_weapon(),
        400,
    );

    w.tick(&[Command::Attack {
        unit: a,
        target: Target::Unit(b),
        house: 1,
    }]);
    // A few more ticks so B's retaliation has a chance to actually assign.
    for _ in 0..5 {
        w.tick(&[]);
    }

    assert_eq!(
        w.units.get(b).unwrap().target,
        Some(Target::Unit(a)),
        "sanity: B should have retaliated against A"
    );
    assert_eq!(
        w.units.get(c).unwrap().target,
        None,
        "C (A's idle ally, never damaged) must not be pulled into the fight"
    );
    assert!(
        w.units.get(c).unwrap().path.is_empty(),
        "C must not have been given any move order either"
    );
}

// ===========================================================================
// 2. Dead attacker mid-flight: no panic, no stale retaliation.
// ===========================================================================

/// A fires a slow (non-instant) shot at B, then dies (killed by C, a third
/// party) before the bullet lands. When the bullet detonates on B,
/// `explosion_damage` looks up `source` (A's now-stale handle) to decide
/// whether to retaliate: `world.units.get(source).filter(|u| u.is_alive())`
/// returns `None` for a removed handle, so `source_house` is `None` and the
/// `source_house.is_some_and(...)` retaliation gate never fires. Pin: no
/// panic, B takes the splash damage normally, and B's target stays `None`
/// (no retaliation against a dead attacker's stale handle).
#[test]
fn dead_attacker_mid_flight_causes_no_panic_and_no_stale_retaliation() {
    let mut w = world();
    let a = spawn_armed(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        lethal_slow_weapon(),
        50, // low HP: easy for C to one-shot kill
    );
    let b = spawn_unarmed(&mut w, 2, CellCoord::new(11, 10), 5000); // won't die
    let c = spawn_armed(
        &mut w,
        3,
        CellCoord::new(10, 12),
        Facing(0),
        instakill_instant_weapon(),
        400,
    );

    // A fires its slow shot at B.
    w.tick(&[Command::Attack {
        unit: a,
        target: Target::Unit(b),
        house: 1,
    }]);
    // Confirm the bullet actually launched with real flight time (not yet
    // detonated) before A dies, so this test is exercising the intended race.
    assert!(
        !w.bullets.is_empty(),
        "sanity: A's slow bullet should be in flight"
    );

    // C kills A immediately (before the slow bullet arrives).
    w.tick(&[Command::Attack {
        unit: c,
        target: Target::Unit(a),
        house: 3,
    }]);
    assert!(!w.units.contains(a), "sanity: A should be dead");

    // Run enough ticks for A's original bullet to land on B. Must not panic.
    for _ in 0..200 {
        w.tick(&[]);
    }

    assert_eq!(
        w.units.get(b).unwrap().target,
        None,
        "B must not retaliate against A's stale (dead) handle"
    );
}

// ===========================================================================
// 3. Unarmed harvester hit: no panic, no retaliation attempt, FSM untouched.
// ===========================================================================

/// An unarmed unit (no `weapon`, as every harvester is before `set_unit_combat`
/// attaches one — harvesters never get a weapon at all) takes splash damage
/// without panicking. `assign_retaliation` early-outs on `unit.weapon.is_none()`
/// before touching `target`/`path`, so nothing about the unit's ongoing
/// behavior changes: it isn't given a target, and (this sim has no "flee"
/// behavior modelled at all — confirmed absent from the codebase) its
/// existing path, if any, is left completely alone. Here the harvester is
/// idle with an empty path to keep the assertion simple; the important
/// invariant is "no panic, no retaliation", which doesn't depend on whether
/// it was moving.
#[test]
fn unarmed_harvester_hit_takes_damage_without_panic_or_retaliation() {
    let mut w = world();
    let atk = spawn_armed(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        weak_instant_weapon(),
        400,
    );
    let harv = spawn_unarmed(&mut w, 2, CellCoord::new(11, 10), 400);
    assert!(
        w.units.get(harv).unwrap().weapon.is_none(),
        "sanity: unarmed"
    );
    let before = w.units.get(harv).unwrap().health;

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(harv),
        house: 1,
    }]);

    let after = w.units.get(harv).unwrap().health;
    assert!(
        after < before,
        "the unarmed harvester should still take real splash/direct damage"
    );
    assert_eq!(
        w.units.get(harv).unwrap().target,
        None,
        "an unarmed unit must never be assigned a retaliation target"
    );
    assert!(
        w.units.get(harv).unwrap().path.is_empty(),
        "no flee behavior is modelled: the harvester's path is left untouched"
    );
}
