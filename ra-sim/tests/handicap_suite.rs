//! Audit coverage (ra-tester, post-M7.9/M7.10): a **targeted property test per
//! difficulty-handicap bias site** (`House::handicap`, M7.9 P2a), each proving
//! two things on the same fixture:
//!   1. a handicapped house's stat is scaled by *exactly* the hand-computed
//!      raw-16.16 multiplier (`fx_mul` semantics: `round(val × bias / 65536)`);
//!   2. a **neutral** house (the default — a human on Normal, or any house
//!      nobody ever biased) is *exactly* unaffected (byte-identical to the
//!      pre-M7.9 behaviour).
//!
//! Bias magnitudes are the real `redalert.mix` rules.ini `[Easy]`/`[Difficult]`
//! sections (confirmed by extracting the actual asset — ground truth, not the
//! brief), parsed the same way `ra_data::combat::parse_fixed_raw` parses them:
//!   - `.8`  -> `52428`  (`8 × 65536 / 10`, truncated)
//!   - `1.2` -> `78643`  (`65536 + 2 × 65536 / 10`, truncated)
//!   - `1.0` -> `65536`  (neutral, `FX_ONE`)
//!
//! `[Easy]`: FirePower=1.2, Armor=1.2, ROF=.8, Groundspeed=1.2, Cost=.8,
//! BuildTime=.8 — the section our label→section inversion (QUIRKS Q15) assigns
//! to a **Hard** AI (the buffed, "strong" opponent). Cost/BuildTime end-to-end
//! coverage (including the AI-vs-human comparison) lives in
//! `build_time_fidelity.rs`; this file covers the remaining four combat/
//! movement sites plus a light formula check of the other two for completeness.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Catalog, Command, Handicap, MoveStats, Passability, Target, WarheadProfile, WeaponProfile,
    World,
};

/// `.8` in raw 16.16 (`round(0.8 × 65536)` truncated the way `parse_fixed_raw`
/// truncates — `8 × 65536 / 10 = 52428`), the real rules.ini `[Easy] ROF`/
/// `Cost`/`BuildTime` and `[Difficult] FirePower`/`Armor`/`Groundspeed` value.
const BIAS_08: i32 = 52428;
/// `1.2` in raw 16.16 (`65536 + 2 × 65536 / 10 = 78643`), the real rules.ini
/// `[Easy] FirePower`/`Armor`/`Groundspeed` and `[Difficult] ROF` value.
const BIAS_12: i32 = 78643;
/// `1.0` — the whole-number neutral bias (`FX_ONE`).
const NEUTRAL: i32 = 65536;

/// Round `val * bias_raw` to nearest, mirroring `house::fx_mul`'s (crate-
/// private) rounding exactly, so expectations below are hand-computed with the
/// *same* rounding rule the production code uses — not re-derived by accident.
fn fx_mul_expected(val: i32, bias_raw: i32) -> i32 {
    ((val as i64 * bias_raw as i64 + (1i64 << 15)) >> 16) as i32
}

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

/// A weapon whose damage is trivial to hand-verify: no warhead falloff (a huge
/// `spread` collapses the distance term to 0 regardless of actual cell gap —
/// `combat.rs::modify_damage`'s `distance /= spread * 5` floors to 0 for any
/// realistic gap when `spread` is this large) and an identity `Verses` (all
/// `65536` = 100%, exact round-trip through the warhead-vs-armor modifier).
fn point_blank_weapon(damage: i32, rof: u16) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof,
        range: 50 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 999,
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1_000_000,
    }
}

/// A bare combat-only world (no catalog/production — per `World::new`'s own
/// doc, movement/combat worlds never touch it), two houses, both starting
/// neutral (`Handicap::default()`).
fn combat_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0xA11A_5EED);
    w.set_catalog(Catalog::new());
    w.init_houses(3, 0);
    w
}

// ===========================================================================
// 1. Firepower bias (damage *dealt*): `fire()`, techno.cpp:3303.
// ===========================================================================

/// Fire one shot from house `shooter_house` (armed with `point_blank_weapon
/// (100, ...)`) at an effectively-immortal, unarmed, neutral-armor target of a
/// *different* house; return the exact HP the first shot removed.
fn one_shot_damage(shooter_house: u8, shooter_handicap: Handicap) -> i32 {
    let mut w = combat_world();
    w.houses[shooter_house as usize].handicap = shooter_handicap;
    let shooter = w.spawn_unit(
        0,
        shooter_house,
        CellCoord::new(10, 10),
        Facing(0),
        400,
        stats(),
    );
    w.set_unit_combat(shooter, 0, Some(point_blank_weapon(100, 9999)), false);
    // Target house is whichever of {1,2} isn't the shooter — always neutral.
    let target_house = if shooter_house == 1 { 2 } else { 1 };
    let target = w.spawn_unit(
        0,
        target_house,
        CellCoord::new(11, 10),
        Facing(0),
        60_000,
        stats(),
    );

    w.tick(&[Command::Attack {
        unit: shooter,
        target: Target::Unit(target),
        house: shooter_house,
    }]);
    let before = 60_000u16;
    for _ in 0..200 {
        w.tick(&[]);
        let now = w.units.get(target).unwrap().health;
        if now < before {
            return (before - now) as i32;
        }
    }
    panic!("shooter never fired within 200 ticks");
}

#[test]
fn firepower_handicap_scales_damage_dealt_for_the_handicapped_house_only() {
    let neutral = one_shot_damage(1, Handicap::default());
    assert_eq!(
        neutral, 100,
        "a neutral house's firepower must be exact (no bias)"
    );

    let buffed = one_shot_damage(
        1,
        Handicap {
            firepower: BIAS_12,
            ..Handicap::default()
        },
    );
    assert_eq!(
        buffed,
        fx_mul_expected(100, BIAS_12),
        "[Easy]-buffed firepower (1.2): round(100 × 1.2) = 120"
    );
    assert_eq!(buffed, 120);

    let nerfed = one_shot_damage(
        1,
        Handicap {
            firepower: BIAS_08,
            ..Handicap::default()
        },
    );
    assert_eq!(
        nerfed,
        fx_mul_expected(100, BIAS_08),
        "[Difficult]-nerfed firepower (.8): round(100 × .8) = 80"
    );
    assert_eq!(nerfed, 80);
}

// ===========================================================================
// 2. Armor bias (damage *taken*): `explosion_damage` -> `house_armor_scaled`,
//    techno.cpp:4099. The *target's* handicap, not the shooter's.
// ===========================================================================

fn one_shot_damage_taken(target_handicap: Handicap) -> i32 {
    let mut w = combat_world();
    let shooter = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(shooter, 0, Some(point_blank_weapon(100, 9999)), false);
    w.houses[2].handicap = target_handicap;
    let target = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 60_000, stats());

    w.tick(&[Command::Attack {
        unit: shooter,
        target: Target::Unit(target),
        house: 1,
    }]);
    let before = 60_000u16;
    for _ in 0..200 {
        w.tick(&[]);
        let now = w.units.get(target).unwrap().health;
        if now < before {
            return (before - now) as i32;
        }
    }
    panic!("shooter never fired within 200 ticks");
}

#[test]
fn armor_handicap_scales_damage_taken_for_the_handicapped_house_only() {
    let neutral = one_shot_damage_taken(Handicap::default());
    assert_eq!(
        neutral, 100,
        "a neutral house's armor must be exact (no bias)"
    );

    // [Easy]-buffed armor (1.2): the object *takes more* damage (this is the
    // player's easy-mode section; QUIRKS Q15 — for an AI it maps to Hard).
    let weak_armor = one_shot_damage_taken(Handicap {
        armor: BIAS_12,
        ..Handicap::default()
    });
    assert_eq!(weak_armor, fx_mul_expected(100, BIAS_12));
    assert_eq!(weak_armor, 120);

    // [Difficult]-nerfed armor (.8): takes less damage.
    let strong_armor = one_shot_damage_taken(Handicap {
        armor: BIAS_08,
        ..Handicap::default()
    });
    assert_eq!(strong_armor, fx_mul_expected(100, BIAS_08));
    assert_eq!(strong_armor, 80);
}

// ===========================================================================
// 3. ROF bias (rearm delay): `run_combat`/`run_building_combat` ->
//    `house_rof_scaled`, techno.cpp:3066. Measured as the tick gap between two
//    consecutive shots (the `arm` cooldown is private; this is the observable
//    behaviour a player/AI actually experiences).
// ===========================================================================

/// Ticks between the first and second shot a house's unit lands on an
/// (effectively immortal) target, for a weapon whose base ROF is `base_rof`.
fn shot_gap_ticks(shooter_handicap: Handicap, base_rof: u16) -> u32 {
    let mut w = combat_world();
    w.houses[1].handicap = shooter_handicap;
    let shooter = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(shooter, 0, Some(point_blank_weapon(1, base_rof)), false);
    let target = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 60_000, stats());

    w.tick(&[Command::Attack {
        unit: shooter,
        target: Target::Unit(target),
        house: 1,
    }]);
    let mut last_health = 60_000u16;
    let mut shot_ticks: Vec<u32> = Vec::new();
    for t in 1..2000u32 {
        w.tick(&[]);
        let now = w.units.get(target).unwrap().health;
        if now < last_health {
            shot_ticks.push(t);
            last_health = now;
            if shot_ticks.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(shot_ticks.len(), 2, "expected exactly two observed shots");
    shot_ticks[1] - shot_ticks[0]
}

#[test]
fn rof_handicap_scales_rearm_delay_for_the_handicapped_house_only() {
    const BASE_ROF: u16 = 60;
    let neutral_gap = shot_gap_ticks(Handicap::default(), BASE_ROF);
    assert_eq!(
        neutral_gap, BASE_ROF as u32,
        "a neutral house's rearm delay must be exact (no bias)"
    );

    // [Easy]-buffed ROF (.8, faster) -> shorter gap: round(60 × .8) = 48, floored
    // at >= 1 by `house_rof_scaled` (not binding here).
    let fast_gap = shot_gap_ticks(
        Handicap {
            rof: BIAS_08,
            ..Handicap::default()
        },
        BASE_ROF,
    );
    assert_eq!(fast_gap, fx_mul_expected(BASE_ROF as i32, BIAS_08) as u32);
    assert_eq!(fast_gap, 48);

    // [Difficult]-nerfed ROF (1.2, slower) -> longer gap: round(60 × 1.2) = 72.
    let slow_gap = shot_gap_ticks(
        Handicap {
            rof: BIAS_12,
            ..Handicap::default()
        },
        BASE_ROF,
    );
    assert_eq!(slow_gap, fx_mul_expected(BASE_ROF as i32, BIAS_12) as u32);
    assert_eq!(slow_gap, 72);
}

// ===========================================================================
// 4. Groundspeed bias (move speed + turn rate): `move_units`, drive.cpp:
//    648/1354. Measured as cells actually covered over a fixed tick budget.
// ===========================================================================

fn cells_covered(mover_handicap: Handicap, ticks: u32) -> i32 {
    let mut w = combat_world();
    w.houses[1].handicap = mover_handicap;
    let unit = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 400, stats());
    w.tick(&[Command::Move {
        unit,
        dest: CellCoord::new(5, 100),
        house: 1,
    }]);
    for _ in 0..ticks {
        w.tick(&[]);
    }
    let end = w.units.get(unit).unwrap().cell();
    end.y - 5
}

#[test]
fn groundspeed_handicap_scales_move_speed_for_the_handicapped_house_only() {
    const TICKS: u32 = 300;
    let neutral = cells_covered(Handicap::default(), TICKS);
    let fast = cells_covered(
        Handicap {
            groundspeed: BIAS_12,
            ..Handicap::default()
        },
        TICKS,
    );
    let slow = cells_covered(
        Handicap {
            groundspeed: BIAS_08,
            ..Handicap::default()
        },
        TICKS,
    );
    assert!(
        fast > neutral,
        "a [Easy]-buffed (1.2×) groundspeed house must cover more ground: fast={fast} neutral={neutral}"
    );
    assert!(
        slow < neutral,
        "a [Difficult]-nerfed (.8×) groundspeed house must cover less ground: slow={slow} neutral={neutral}"
    );
    // Within a tick or two of the exact ratio (path/cell-boundary rounding
    // keeps this from being bit-exact over a multi-cell trip).
    let want_fast = (neutral as f64 * 1.2).round() as i32;
    let want_slow = (neutral as f64 * 0.8).round() as i32;
    assert!(
        (fast - want_fast).abs() <= 2,
        "fast={fast} should be near neutral×1.2={want_fast}"
    );
    assert!(
        (slow - want_slow).abs() <= 2,
        "slow={slow} should be near neutral×0.8={want_slow}"
    );
}

// ===========================================================================
// 5. Cost / BuildTime bias — formula-level check for completeness (the
//    end-to-end AI-vs-human production measurement lives in
//    `build_time_fidelity.rs::build_time_bias_speeds_up_the_ai_and_leaves_the_
//    human_at_baseline`, which this deliberately does not duplicate).
// ===========================================================================

#[test]
fn cost_and_build_time_bias_formulas_match_hand_computation() {
    // `house::fx_mul` is crate-private; this exercises the identical rounding
    // rule via the public `fx_mul_expected` mirror above, and cross-checks it
    // against the values `build_time_fidelity.rs` measures end-to-end.
    assert_eq!(fx_mul_expected(800, BIAS_08), 640, "Cost .8 on $800");
    assert_eq!(
        fx_mul_expected(540, BIAS_08),
        432,
        "BuildTime .8 on 540 ticks"
    );
    assert_eq!(fx_mul_expected(800, NEUTRAL), 800, "Cost neutral is exact");
    assert_eq!(
        fx_mul_expected(540, NEUTRAL),
        540,
        "BuildTime neutral is exact"
    );
}

// ===========================================================================
// 6. `Handicap::is_neutral` — the hashing/no-op gate every site relies on to
//    keep pre-M7.9 goldens untouched.
// ===========================================================================

#[test]
fn is_neutral_is_true_only_for_the_all_one_default() {
    assert!(Handicap::default().is_neutral());
    for (field, name) in [
        (
            Handicap {
                firepower: BIAS_12,
                ..Handicap::default()
            },
            "firepower",
        ),
        (
            Handicap {
                armor: BIAS_12,
                ..Handicap::default()
            },
            "armor",
        ),
        (
            Handicap {
                rof: BIAS_12,
                ..Handicap::default()
            },
            "rof",
        ),
        (
            Handicap {
                groundspeed: BIAS_12,
                ..Handicap::default()
            },
            "groundspeed",
        ),
        (
            Handicap {
                cost: BIAS_12,
                ..Handicap::default()
            },
            "cost",
        ),
        (
            Handicap {
                build_time: BIAS_12,
                ..Handicap::default()
            },
            "build_time",
        ),
    ] {
        assert!(
            !field.is_neutral(),
            "{name} alone biased must not be neutral"
        );
    }
}
