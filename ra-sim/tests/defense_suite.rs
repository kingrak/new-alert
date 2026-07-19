//! M7.7 Chunk B follow-up (ra-tester charter): dedicated, adversarial coverage
//! for defense buildings (PBOX/HBOX/GUN/FTUR/TSLA, `run_building_combat`,
//! `ra-sim/src/world.rs` ~1533) and walls-as-1x1-buildings (SBAG/CYCL/BRIK,
//! `is_wall`, QUIRKS Q9). Chunk B (commit 23251ac) landed this system with
//! only one colocated smoke test (`defense_building_auto_acquires_and_fires_
//! on_an_enemy_in_range`, `world.rs` ~3338) — this file is the real coverage:
//! TSLA charge timing + the power-gate reset-vs-pause question, the GUN
//! alignment gate + rotation-rate derivation, nearest-enemy auto-acquire +
//! deterministic tie-break, retargeting after a kill, death mid-charge, and
//! the wall mechanics (chain placement, attackability, all-locomotor
//! blocking, and the wall-only-house elimination question).
//!
//! Own minimal fixture catalog throughout (independent of `world.rs`'s
//! private test module and of every other test file's catalog, per repo
//! convention). Every weapon fixture below uses a deliberately oversized
//! warhead `spread` (1000) so the distance-falloff divisor in `modify_damage`
//! (`distance / (spread * 5)`) truncates to 0 for any in-test firing range,
//! which skips the `damage /= distance` step entirely (`ra-sim/src/combat.rs`
//! `modify_damage`) — every hit lands for its full, undivided base damage.
//! This is a deliberate test-fixture simplification (not a real weapon's
//! `Spread=`) so expected damage is exact arithmetic, not falloff-sensitive.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    modify_damage, AiPlayer, BuildingProto, Catalog, Command, Difficulty, EconRules, GameOver,
    Handle, Locomotor, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Fixture
// ===========================================================================

// Building type ids.
const B_PBOX: u32 = 0; // fixed emplacement, no charge, no turret (PBOX/HBOX/FTUR shape)
const B_GUN: u32 = 1; // rotating turret (ROT=12), no charge
const B_TSLA: u32 = 2; // charges (Charges=yes), no turret
const B_WALL: u32 = 3; // is_wall=true, no weapon (SBAG/CYCL/BRIK shape)
const B_HUT: u32 = 4; // plain non-combat, non-wall filler structure (a base "asset")

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// A weapon whose warhead does full, undivided damage against **none** armor
/// (index 0) at any in-test range (see module docs re: the `spread = 1000`
/// falloff-defeat trick). `instant = true` so the bullet detonates the same
/// tick it is fired (`Bullet::advance`, `ra-sim/src/bullet.rs:74-78`) — no
/// projectile-flight ticks to account for in the tick-count assertions below.
fn full_damage_weapon(damage: i32, rof: u16, range: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof,
        range,
        proj_speed: 255,
        proj_rot: 0, // non-homing: aligned_to_fire's plain `diff < 8` gate applies
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 1000,
            verses: pct5([100, 50, 50, 50, 50]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

#[allow(clippy::too_many_arguments)]
fn defense_proto(
    name: &str,
    weapon: Option<WeaponProfile>,
    has_turret: bool,
    charges: bool,
    is_wall: bool,
    armor: u8,
    max_health: u16,
) -> BuildingProto {
    BuildingProto {
        name: name.to_string(),
        foot_w: 1,
        foot_h: 1,
        max_health,
        armor,
        power: -15,
        cost: 400,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        is_barracks: false,
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon,
        has_turret,
        charges,
        is_wall,
        storage: 0,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![
            defense_proto(
                "PBOX",
                Some(full_damage_weapon(50, 30, 2048)),
                false,
                false,
                false,
                1,
                400,
            ),
            defense_proto(
                "GUN",
                Some(full_damage_weapon(60, 999, 2048)),
                true,
                false,
                false,
                3,
                300,
            ),
            defense_proto(
                "TSLA",
                Some(full_damage_weapon(100, 30, 2048)),
                false,
                true,
                false,
                3,
                200,
            ),
            defense_proto("SBAG", None, false, false, true, 0, 40),
            defense_proto("HUT", None, false, false, false, 0, 100),
        ],
        units: vec![],
        econ: EconRules::default(),
    }
}

fn world(seed: u32) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, 1000);
    w
}

/// Spawn an unarmed enemy unit of `house`, centred at `cell` — a passive,
/// stationary target that never fights back or moves (no weapon, no orders).
fn spawn_enemy(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, stats());
    w.set_unit_combat(h, 0, None, false); // armor none, unarmed
    h
}

/// Give `house` ample power output. `init_houses` starts every house at
/// `power_output = power_drain = 0` (`House::new`), and every defense proto
/// in this file's catalog drains 15 (`defense_proto`'s `power: -15`) — so a
/// freshly-placed TSLA is *born* `low_power()` (`power_drain(15) > 0 &&
/// power_output(0) < power_drain`, `ra-sim/src/house.rs:194-196`) unless a
/// test explicitly powers the house first. The charge-timing tests below need
/// power to be a controlled variable, not an accidental default.
fn power_up(w: &mut World, house: u8) {
    w.houses[house as usize].power_output = 1000;
}

// ===========================================================================
// 1. TSLA charge timing.
// ===========================================================================

/// `TESLA_CHARGE_TICKS = 15` (`ra-sim/src/world.rs:1512`, citing `Charging_AI`,
/// `building.cpp:45`'s 9-stage animation, approximated at ~1s of ticks). Trace
/// through `run_building_combat` (`world.rs:1607-1625`) starting from a fresh
/// building (`charge = 0`, `arm = 0`): tick *k* (1-indexed) reads `c = charge`
/// (== k-1), and continues charging (sets `charge = k`, no fire) whenever
/// `c + 1 < 15`, i.e. for k = 1..=14. At k = 15, `c = 14`, `c + 1 == 15` is
/// **not** `< 15`, so it resets `charge = 0` and falls through to `fire()`.
/// So the bolt lands on **exactly the 15th tick** with a target continuously
/// in range — pinned here, not guessed.
#[test]
fn tesla_full_charge_cycle_fires_on_the_15th_tick_exactly() {
    let mut w = world(0xDEF0_0001);
    let tsla = w.spawn_building(B_TSLA, 1, CellCoord::new(20, 20)).unwrap();
    power_up(&mut w, 1);
    let enemy = spawn_enemy(&mut w, 2, CellCoord::new(21, 20), 500);
    let hp0 = w.units.get(enemy).unwrap().health;

    for i in 1..=14 {
        w.tick(&[]);
        assert_eq!(
            w.units.get(enemy).unwrap().health,
            hp0,
            "tick {i}/15: the tesla coil must still be charging, not firing yet"
        );
        assert_eq!(
            w.buildings.get(tsla).unwrap().charge,
            i as u16,
            "tick {i}: charge counter should equal the tick index while charging"
        );
    }
    // 15th tick: charge completes and the bolt fires (instant hitscan, so
    // damage lands the same tick — see `full_damage_weapon`'s doc comment).
    w.tick(&[]);
    let hp1 = w.units.get(enemy).unwrap().health;
    assert!(
        hp1 < hp0,
        "the 15th tick must fire the tesla bolt (hp {hp0} -> {hp1})"
    );
    assert_eq!(
        hp1,
        hp0 - 100,
        "full, undivided 100 dmg (Super-style bolt) vs none-armor at the fixture's spread=1000 trick"
    );
    assert_eq!(
        w.buildings.get(tsla).unwrap().charge,
        0,
        "charge resets to 0 after the bolt fires, ready to start a new cycle"
    );
}

/// Power-gate mid-charge. Source (`world.rs:1607-1614`):
/// ```text
/// if charges {
///     let powered = ... !house.low_power() ...;
///     if !powered { building.charge = 0; continue; }
///     ...
/// }
/// ```
/// Losing power **resets** the charge counter to 0 — it does NOT pause and
/// later resume from where it left off. This test pins that exact behavior:
/// charge to 5, cut power for one tick (charge must read back 0, not 5), then
/// restore power and show the coil needs the **full** 15 ticks again (not the
/// 10 remaining from before the cut) to fire.
#[test]
fn tesla_charge_resets_not_pauses_when_power_is_lost() {
    let mut w = world(0x7E5A_0002);
    let tsla = w.spawn_building(B_TSLA, 1, CellCoord::new(20, 20)).unwrap();
    power_up(&mut w, 1);
    let enemy = spawn_enemy(&mut w, 2, CellCoord::new(21, 20), 500);
    let hp0 = w.units.get(enemy).unwrap().health;

    // Charge to 5.
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(w.buildings.get(tsla).unwrap().charge, 5);

    // Cut house 1's power: `low_power() == power_drain > 0 && power_output <
    // power_drain` (`ra-sim/src/house.rs:194-196`).
    w.houses[1].power_output = 0;
    w.houses[1].power_drain = 1;
    w.tick(&[]);
    assert_eq!(
        w.buildings.get(tsla).unwrap().charge,
        0,
        "BUG-check: source resets charge to 0 on power loss (world.rs:1610-1614) \
         — if this ever reads 5 the implementation switched to pause/resume and \
         this test's pin (and its doc comment) need updating, not weakening"
    );
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        hp0,
        "no bolt should have fired during the power-starved tick"
    );

    // Restore power. The coil must re-run the FULL 15-tick cycle, not just
    // the 10 ticks that would remain if it had paused at 5.
    w.houses[1].power_output = 1000;
    w.houses[1].power_drain = 15;
    for i in 1..=14 {
        w.tick(&[]);
        assert_eq!(
            w.units.get(enemy).unwrap().health,
            hp0,
            "post-restore tick {i}/15: must not have fired yet (proves reset, not resume-from-5)"
        );
    }
    w.tick(&[]); // the 15th post-restore tick
    assert!(
        w.units.get(enemy).unwrap().health < hp0,
        "after the full 15-tick cycle post-restore, the bolt must fire"
    );
}

// ===========================================================================
// 2. GUN alignment gate + rotation rate.
// ===========================================================================

/// `BUILDING_TURRET_ROT: u8 = 12` (`ra-sim/src/world.rs:1516`, citing `GUN`'s
/// `ROT=12`). The GUN is placed at (20,20); the enemy at (23,16) — 3 cells
/// east, 4 cells north — which drives `Facing::toward` to `Facing(24)` (a
/// value derived, not guessed: reproduced by hand from `desired_facing256`,
/// `ra-sim/src/coords.rs:246`, and cross-checked against
/// `Facing::rotate_toward`'s own already-proptested convergence
/// (`rotate_toward_converges_and_is_a_fixed_point`, `coords.rs` tests) via the
/// same-rate replay loop below). Starting from the turret's spawn facing
/// `Facing(0)` (`spawn_building`, `world.rs:441`), `rotate_toward(_, 12)`
/// walks the facing 0 -> 12 (tick 1, diff 24->12, still `>= 8`: NOT aligned)
/// -> 24 (tick 2, diff 12->0: aligned, fires). So this GUN must NOT fire on
/// tick 1 and MUST fire on tick 2 exactly.
#[test]
fn gun_does_not_fire_while_rotating_and_fires_exactly_once_aligned() {
    let mut w = world(0x6015_0003);
    let gun = w.spawn_building(B_GUN, 1, CellCoord::new(20, 20)).unwrap();
    let enemy = spawn_enemy(&mut w, 2, CellCoord::new(23, 16), 500);
    let hp0 = w.units.get(enemy).unwrap().health;

    // Derive the expected facing/tick-count from the same public rotation
    // primitive the production code calls (`Facing::rotate_toward`), rather
    // than hardcoding a tick count: this is the "derive, don't guess" ask.
    let building_center = CellCoord::new(20, 20).center();
    let target_center = CellCoord::new(23, 16).center();
    let desired = Facing::toward(building_center, target_center).unwrap();
    assert_eq!(
        desired,
        Facing(24),
        "sanity: hand-derived facing pinned above"
    );
    const BUILDING_TURRET_ROT: u8 = 12; // world.rs:1516, GUN's ROT=12
    let mut f = Facing(0);
    let mut expected_align_tick = 0u32;
    while f != desired && expected_align_tick < 100 {
        f = f.rotate_toward(desired, BUILDING_TURRET_ROT);
        expected_align_tick += 1;
    }
    assert_eq!(
        expected_align_tick, 2,
        "sanity: hand-derived tick count pinned above"
    );

    // Tick 1: still rotating (diff 12, not < 8) — must NOT fire.
    w.tick(&[]);
    assert_eq!(
        w.buildings.get(gun).unwrap().turret_facing,
        Facing(12),
        "tick 1: turret should have advanced by exactly ROT=12"
    );
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        hp0,
        "tick 1: the GUN must not fire while still rotating toward its target"
    );

    // Tick 2: turret snaps to the exact desired facing and fires.
    w.tick(&[]);
    assert_eq!(
        w.buildings.get(gun).unwrap().turret_facing,
        desired,
        "tick 2: turret should be exactly aligned"
    );
    let hp1 = w.units.get(enemy).unwrap().health;
    assert!(hp1 < hp0, "tick 2: aligned — the GUN must fire now");
    assert_eq!(hp1, hp0 - 60, "full undivided 60 dmg on the single shot");

    // Tick 3+: ROF=999, so no second shot lands within any reasonable window
    // — "fires exactly once" for this test's timeframe.
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        hp1,
        "no second shot should have landed yet (ROF=999 rearm gate)"
    );
}

// ===========================================================================
// 3. Auto-acquire nearest + deterministic tie-break.
// ===========================================================================

/// Three enemies at strictly increasing distance from a PBOX: it must lock
/// onto the nearest one, not e.g. spawn order or arbitrary iteration order.
#[test]
fn defense_auto_acquires_the_strictly_nearest_of_several_enemies() {
    let mut w = world(0xA091_0004);
    let pbox = w.spawn_building(B_PBOX, 1, CellCoord::new(20, 20)).unwrap();
    // Spawned deliberately out of distance order (farthest first) so a
    // "first in slot order wins" bug would be caught, not accidentally
    // satisfied by insertion order lining up with distance order.
    let _far = spawn_enemy(&mut w, 2, CellCoord::new(26, 20), 500); // 6 cells
    let _mid = spawn_enemy(&mut w, 2, CellCoord::new(24, 20), 500); // 4 cells
    let near = spawn_enemy(&mut w, 2, CellCoord::new(21, 20), 500); // 1 cell — nearest

    w.tick(&[]);
    assert_eq!(
        w.buildings.get(pbox).unwrap().target,
        Some(Target::Unit(near)),
        "the defense must acquire the strictly-nearest enemy, not spawn/slot order"
    );
}

/// Two enemies at **exactly** equal `leptons_distance` from the defense
/// (mirrored placements east/west of it, so `|dx|` is identical and `dy = 0`
/// for both — `leptons_distance`, `ra-sim/src/coords.rs:127`, depends only on
/// `|dx|`/`|dy|`, so this ties exactly, not approximately).
/// `acquire_nearest_enemy` (`world.rs:1941-1958`) uses a **strict**
/// `d < bd` comparison when updating `best`, so a later-found equal-distance
/// candidate never replaces the current `best` — the winner is whichever
/// candidate the `world.units.iter()` slot-order scan (`Arena::iter`,
/// ascending index — `arena.rs:115-127`) reaches **first**, i.e. the
/// **lower handle index / earlier-spawned** unit. Pinned exactly, not just
/// "some deterministic winner".
#[test]
fn defense_tie_break_is_deterministic_and_favors_the_lower_handle() {
    let mut w = world(0x71E0_0005);
    let pbox = w.spawn_building(B_PBOX, 1, CellCoord::new(20, 20)).unwrap();
    // Spawned first -> lower handle index -> must win the tie.
    let west = spawn_enemy(&mut w, 2, CellCoord::new(17, 20), 500); // 3 cells west
    let east = spawn_enemy(&mut w, 2, CellCoord::new(23, 20), 500); // 3 cells east
    assert!(
        west.index < east.index,
        "sanity: west must have the lower handle index (spawned first)"
    );

    w.tick(&[]);
    assert_eq!(
        w.buildings.get(pbox).unwrap().target,
        Some(Target::Unit(west)),
        "an exact-distance tie must resolve to the lower (earlier-spawned) handle, deterministically"
    );

    // Re-run with a fresh world and the identical setup: same winner every
    // time (determinism, not "happened to win once").
    for seed in [0x1111_0005u32, 0x2222_0005, 0x3333_0005] {
        let mut w2 = world(seed);
        let pbox2 = w2
            .spawn_building(B_PBOX, 1, CellCoord::new(20, 20))
            .unwrap();
        let west2 = spawn_enemy(&mut w2, 2, CellCoord::new(17, 20), 500);
        let _east2 = spawn_enemy(&mut w2, 2, CellCoord::new(23, 20), 500);
        w2.tick(&[]);
        assert_eq!(
            w2.buildings.get(pbox2).unwrap().target,
            Some(Target::Unit(west2)),
            "seed {seed:#x}: tie-break must be deterministic across different RNG seeds too \
             (targeting draws no RNG at all)"
        );
    }
}

// ===========================================================================
// 4. Defense vs multiple attackers.
// ===========================================================================

/// Three enemies approach (well, sit in range of) a single PBOX. After it
/// kills its current target it must retarget to the next-nearest live enemy
/// — never stay locked on a dead handle, never panic, never idle while a live
/// enemy remains in range.
#[test]
fn defense_retargets_after_killing_its_current_target_with_attackers_remaining() {
    let mut w = world(0xBEEF_0006);
    let pbox = w.spawn_building(B_PBOX, 1, CellCoord::new(20, 20)).unwrap();
    // Low HP so a single 50-dmg shot (full damage weapon) kills each one.
    let e1 = spawn_enemy(&mut w, 2, CellCoord::new(21, 20), 10); // nearest
    let e2 = spawn_enemy(&mut w, 2, CellCoord::new(22, 20), 10); // next
    let e3 = spawn_enemy(&mut w, 2, CellCoord::new(23, 20), 10); // last
    let enemies = [e1, e2, e3];

    // PBOX ROF=30: budget generously — 3 kills * (rearm + a few ticks slack).
    for tick_i in 0..150 {
        w.tick(&[]);
        let any_alive_in_range = enemies.iter().any(|&h| w.units.contains(h));
        if any_alive_in_range {
            assert!(
                w.buildings.get(pbox).unwrap().target.is_some(),
                "tick {tick_i}: a live enemy remains in range but the defense has no target \
                 (idled instead of retargeting)"
            );
        }
        if !any_alive_in_range {
            break;
        }
    }
    for (i, &h) in enemies.iter().enumerate() {
        assert!(
            !w.units.contains(h),
            "enemy {i} should have been killed eventually"
        );
    }
    // One more tick: the target field is snapshotted at the *start* of the
    // tick that kills the last enemy (`run_building_combat` reads/validates
    // `cur` before `run_bullets` removes the newly-dead unit later that same
    // tick, per `apply`'s system order, `world.rs`'s "combat, ... bullets"
    // comment), so it still reads the just-killed handle at the moment the
    // loop above broke. The *next* tick's `validate_building_target` call
    // sees it's dead and clears/re-acquires — that's the state under test.
    w.tick(&[]);
    // Building itself never panicked getting here (the loop completing is
    // the panic-freedom assertion); also confirm it settled with no dangling
    // target once every enemy is gone.
    assert_eq!(
        w.buildings.get(pbox).unwrap().target,
        None,
        "with no live enemies left, the target must be cleared, not a stale handle"
    );
}

// ===========================================================================
// 5. Defense death mid-charge.
// ===========================================================================

/// Kill a TSLA while it is mid-charge-cycle (charge > 0, < 15) via a real
/// scripted attack (not direct health manipulation — that would bypass
/// `remove_building`'s occupancy/power/count cleanup, `world.rs:1125-1147`,
/// which is exactly the invariant this test needs to exercise). Asserts: no
/// panic (the tick loop completing is the proof), the dead building is fully
/// gone from the arena, its footprint occupancy is freed, and the world
/// keeps ticking cleanly afterward with the TSLA's own (still-alive) target
/// unaffected.
#[test]
fn tesla_death_mid_charge_cleans_up_and_does_not_panic() {
    let mut w = world(0xDEAD_0007);
    let tsla_cell = CellCoord::new(20, 20);
    let tsla = w.spawn_building(B_TSLA, 1, tsla_cell).unwrap();
    power_up(&mut w, 1);
    // TSLA's own target: far enough south that the killer (placed north, see
    // below) never threatens it; unarmed so it can't retaliate either.
    let tsla_target = spawn_enemy(&mut w, 2, CellCoord::new(20, 25), 500);

    // Let it charge partway (5 ticks -> charge == 5, per test 1's derivation).
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.buildings.get(tsla).unwrap().charge,
        5,
        "setup: mid-charge"
    );
    assert!(
        w.units.contains(tsla_target),
        "setup: TSLA's target still alive"
    );

    // The killer: house 2 (arbitrary — Command::Attack has no house-mismatch
    // requirement, `world.rs:860-864`), placed directly south of the TSLA so
    // `Facing::toward` resolves to exactly `Facing(0)` (north = 0, derived in
    // test 2's doc comment) — matching the killer's spawn facing, so it is
    // already aligned on tick 1 with no rotation delay to account for.
    // The TSLA's own armor is `heavy` (index 3, `defense_proto("TSLA", ...,
    // 3, ...)`), so `full_damage_weapon`'s verses (`pct5([100, 50, 50, 50,
    // 50])`) only lands at 50% here — cross-checked via `modify_damage`
    // directly (same pattern as the wall-attackability test) rather than
    // hand-multiplying, so a one-shot kill of the 200-hp TSLA needs a big
    // enough base damage.
    let killer_weapon = full_damage_weapon(500, 50, 600);
    let tsla_armor = w.buildings.get(tsla).unwrap().armor;
    let expected_dmg = modify_damage(
        killer_weapon.damage,
        &killer_weapon.warhead,
        tsla_armor,
        0,
        killer_weapon.min_damage,
        killer_weapon.max_damage,
    );
    assert!(
        expected_dmg >= 200,
        "test setup: the killer's shot ({expected_dmg} dmg) must one-shot the 200-hp TSLA"
    );
    let killer = w.spawn_unit(0, 2, CellCoord::new(20, 21), Facing(0), 100, stats());
    w.set_unit_combat(killer, 0, Some(killer_weapon), false);

    w.tick(&[Command::Attack {
        unit: killer,
        target: Target::Building(tsla),
        house: 2,
    }]);
    // The order + the shot can both land within this same tick (unit combat
    // runs before building combat, both before bullets — `world.rs`'s
    // `apply` system order) since the killer is already in range and aligned;
    // give a couple of extra ticks of slack in case not.
    for _ in 0..3 {
        if !w.buildings.contains(tsla) {
            break;
        }
        w.tick(&[]);
    }
    assert!(
        !w.buildings.contains(tsla),
        "the TSLA (200 hp, one {expected_dmg}-dmg full-damage shot) should be dead"
    );
    assert!(
        w.passability().is_passable(tsla_cell),
        "the dead TSLA's footprint occupancy must be freed"
    );
    assert_eq!(
        w.houses[1].power_drain, 0,
        "the dead TSLA's power drain must be reversed off house 1's books"
    );

    // World keeps ticking cleanly afterward — no panic, and the TSLA's own
    // (unrelated) target is unaffected by its death.
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(
        w.units.contains(tsla_target),
        "the TSLA's former target (never itself attacked) should be untouched"
    );
}

// ===========================================================================
// 6. Walls.
// ===========================================================================

/// Several wall segments placed adjacent to each other exist as independent
/// 1x1 buildings, each with its own handle and its own occupancy stamp.
#[test]
fn wall_chain_placement_yields_independent_1x1_buildings() {
    let mut w = world(0x5A11_0008);
    let cells: Vec<CellCoord> = (0..5).map(|i| CellCoord::new(10 + i, 10)).collect();
    let handles: Vec<Handle> = cells
        .iter()
        .map(|&c| w.spawn_building(B_WALL, 1, c).unwrap())
        .collect();

    // All distinct handles, all alive, all is_wall, all occupying exactly
    // their own cell (not merged into one multi-cell structure).
    let unique: std::collections::HashSet<_> = handles.iter().map(|h| (h.index, h.gen)).collect();
    assert_eq!(
        unique.len(),
        handles.len(),
        "each wall segment is a distinct entity"
    );
    for (&h, &c) in handles.iter().zip(&cells) {
        let b = w.buildings.get(h).unwrap();
        assert!(b.is_alive());
        assert!(b.is_wall);
        assert_eq!(b.cell, c);
        assert_eq!((b.foot_w, b.foot_h), (1, 1));
        assert!(
            !w.passability().is_passable(c),
            "wall cell must be occupied"
        );
    }
    // The cell just past the chain's end is untouched.
    assert!(w.passability().is_passable(CellCoord::new(15, 10)));
}

/// Walls are attackable through the normal `Target::Building` -> `modify_damage`
/// path, using **`Armor=none`** (index 0) per QUIRKS Q9 ("Armor=none,
/// Strength=1" — this fixture uses a higher `Strength` than the real 1 so the
/// exact-damage assertion below is meaningful instead of an instant one-shot,
/// but keeps `Armor=none`, the load-bearing part). A weapon whose `Verses`
/// has a nonzero none-armor modifier does real, computable damage and the
/// wall dies at 0 health exactly like any other building.
#[test]
fn walls_are_attackable_via_verses_none_armor_and_die_at_zero_health() {
    let mut w = world(0x1A11_0009);
    let wall = w.spawn_building(B_WALL, 2, CellCoord::new(20, 20)).unwrap();
    let wall_armor = w.buildings.get(wall).unwrap().armor;
    assert_eq!(
        wall_armor, 0,
        "wall armor class must be `none` (index 0), per QUIRKS Q9"
    );
    let hp0 = w.buildings.get(wall).unwrap().health; // 40, this fixture's Strength

    let weapon = full_damage_weapon(40, 10, 600);
    // Cross-check the expected per-shot damage independently via the exported
    // `modify_damage`, the same way `damage_matrix.rs` pins its numbers.
    let expected_dmg = modify_damage(
        weapon.damage,
        &weapon.warhead,
        wall_armor,
        0, // point-blank distance, matching this fixture's spread=1000 trick
        weapon.min_damage,
        weapon.max_damage,
    );
    assert_eq!(
        expected_dmg, 40,
        "full, undivided damage vs none armor at 100% verses"
    );

    let attacker = w.spawn_unit(0, 1, CellCoord::new(20, 21), Facing(0), 100, stats());
    w.set_unit_combat(attacker, 0, Some(weapon), false);
    w.tick(&[Command::Attack {
        unit: attacker,
        target: Target::Building(wall),
        house: 1,
    }]);
    for _ in 0..3 {
        if !w.buildings.contains(wall) {
            break;
        }
        w.tick(&[]);
    }
    assert!(
        !w.buildings.contains(wall),
        "a {hp0}-hp wall hit by a {}-dmg shot should have died (0 health -> removed, \
         same as any other building)",
        expected_dmg
    );
}

/// A wall cell blocks **every** ground locomotor kind the sim models —
/// `Foot`, `Track`, `Wheel` (`ra-sim/src/coords.rs:296-304`) — not just the
/// `Track` default `is_passable` uses internally. `Passability::set_occupied`
/// (`path.rs:171-177`) stamps a single shared `blocked` layer consulted by
/// `is_passable_loco` for all three locomotors uniformly, so this should hold
/// for all of them identically; asserted explicitly per-locomotor rather than
/// assumed.
#[test]
fn wall_cell_blocks_every_locomotor_kind() {
    let mut w = world(0xB10C_000A);
    let cell = CellCoord::new(20, 20);
    for loco in [Locomotor::Foot, Locomotor::Track, Locomotor::Wheel] {
        assert!(
            w.passability().is_passable_loco(cell, loco),
            "sanity: cell must start passable for {loco:?} before the wall is placed"
        );
    }
    w.spawn_building(B_WALL, 1, cell).unwrap();
    for loco in [Locomotor::Foot, Locomotor::Track, Locomotor::Wheel] {
        assert!(
            !w.passability().is_passable_loco(cell, loco),
            "wall must block {loco:?} — occupancy is not locomotor-specific"
        );
    }
}

/// A house whose *only* remaining assets are wall segments is treated as
/// **eliminated**, exactly like an empty house — confirmed intentional design
/// (`World::house_alive`, `world.rs:279-288`: "a house whose only remaining
/// 'buildings' are walls is defeated, matching the original where walls are
/// overlays, not buildings" — QUIRKS Q9). This is *not* a design question to
/// flag: the behavior is already deliberately implemented and documented: the
/// `is_wall` exclusion is right there in `house_alive`'s filter
/// (`!b.is_wall`). Pinned here as an integration-level regression guard
/// through the full win/lose system, not just the `house_alive` predicate.
#[test]
fn wall_only_house_is_eliminated_and_resolves_victory_for_the_opponent() {
    let mut w = world(0xA11A_000B);
    // House 1 (player): a normal, non-wall structure + a unit — a real base.
    let _hut = w.spawn_building(B_HUT, 1, CellCoord::new(10, 10)).unwrap();
    let _pu = w.spawn_unit(0, 1, CellCoord::new(11, 11), Facing(0), 100, stats());
    // House 2 (AI): ONLY a wall segment. No other buildings, no units.
    let _wall = w.spawn_building(B_WALL, 2, CellCoord::new(30, 30)).unwrap();

    assert!(
        !w.house_alive(2),
        "a wall-only house must read as already eliminated"
    );
    assert!(w.house_alive(1), "the real base must read as alive");

    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
    w.tick(&[]);
    assert_eq!(
        w.game_over(),
        GameOver::Victory,
        "the wall-only AI house must resolve as eliminated -> player Victory"
    );
}
