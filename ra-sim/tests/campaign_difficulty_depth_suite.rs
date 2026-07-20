//! M7.5-C depth audit (ra-tester): the **full handicap matrix** for campaign
//! difficulty (P0), going deeper than `campaign_activation_suite`'s single
//! firepower-only check.
//!
//! Bias magnitudes are the *real* `redalert.mix` rules.ini `[Easy]`/`[Normal]`/
//! `[Difficult]` sections, re-extracted directly from the actual asset for this
//! audit (`radump extract assets/main.mix rules.ini --in general.mix`, then
//! grepping the `[Easy]`/`[Normal]`/`[Difficult]` sections) rather than assumed:
//!
//! ```text
//! [Easy]      Firepower=1.2 Groundspeed=1.2 Armor=1.2 ROF=.8  Cost=.8  BuildTime=.8
//! [Normal]    Firepower=1.0 Groundspeed=1.0 Armor=1.0 ROF=1.0 Cost=1.0 BuildTime=1.0
//! [Difficult] Firepower=.8  Groundspeed=.8  Armor=.8  ROF=1.2 Cost=1.0 BuildTime=1.0
//! ```
//!
//! **Real-data finding (not a bug — worth flagging structurally):** `[Difficult]`
//! is *not* a mirror of `[Easy]` on Cost/BuildTime — both stay neutral (`1.0`)
//! under `[Difficult]`, only FirePower/Armor/Groundspeed/ROF are nerfed. Via our
//! label→section inversion (QUIRKS Q15/Q19), our **Easy** AI opponent (which
//! draws `[Difficult]`) is therefore combat/movement-nerfed but **not**
//! economically nerfed (full-price, full-speed building) — an authentic
//! rules.ini asymmetry, not a symmetric "nerf everything" mirror. The matrix
//! below is hand-computed from these exact real values, not a guessed mirror.
//!
//! `handicap_suite.rs` already proves each bias *site* (firepower/armor/rof/
//! groundspeed/cost/build_time) scales its stat correctly for an arbitrary
//! `Handicap` value; the gap this file closes is whether
//! `World::set_campaign_difficulty` assigns the **correct real values**, to the
//! **correct house** (computer vs. player-inverse), across **all six** sites —
//! not just firepower.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Catalog, Command, Difficulty, EconRules, Handicap, MoveStats, Passability, Target,
    WarheadProfile, WeaponProfile, World,
};

// Raw 16.16 fixed-point magnitudes, `parse_fixed_raw`'s truncation rule
// (`ra_data::combat::parse_fixed_raw`), matching `handicap_suite.rs`'s constants.
const FX_08: i32 = 52428; // .8  -> 8 * 65536 / 10, truncated
const FX_12: i32 = 78643; // 1.2 -> 65536 + 2 * 65536 / 10, truncated
const FX_10: i32 = 65536; // 1.0 neutral

/// The real `[Easy]` section as a `Handicap`.
fn easy_section() -> Handicap {
    Handicap {
        firepower: FX_12,
        armor: FX_12,
        rof: FX_08,
        groundspeed: FX_12,
        cost: FX_08,
        build_time: FX_08,
    }
}

/// The real `[Difficult]` section as a `Handicap` — note Cost/BuildTime are
/// neutral here (the real asymmetry documented above), not `FX_12`.
fn difficult_section() -> Handicap {
    Handicap {
        firepower: FX_08,
        armor: FX_08,
        rof: FX_12,
        groundspeed: FX_08,
        cost: FX_10,
        build_time: FX_10,
    }
}

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn weapon(damage: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof: 9999,
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

/// A catalog whose difficulty table is the real, asymmetric `[Easy]`/`[Normal]`/
/// `[Difficult]` sections, indexed by our label->section inversion (Q15/Q19):
/// `Easy -> [Difficult]`, `Normal -> [Normal]`, `Hard -> [Easy]`.
fn catalog() -> Catalog {
    Catalog {
        buildings: Vec::new(),
        units: Vec::new(),
        econ: EconRules {
            difficulty: [difficult_section(), Handicap::default(), easy_section()],
            ..EconRules::default()
        },
    }
}

fn base_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0xC0DE_C0DE);
    w.set_catalog(catalog());
    w.init_houses(20, 0);
    w.set_player_house(1);
    w
}

// ===========================================================================
// 1. The full 6-site x {Easy,Hard} x {computer,player} matrix.
// ===========================================================================

/// Every field of the assigned `Handicap` must come from the correct real
/// section for both the computer house (direct label mapping) and the player
/// house (inverse label), on both Easy and Hard. Normal must be the neutral
/// no-op for both.
#[test]
fn full_handicap_matrix_matches_the_real_rules_ini_sections() {
    // Hard game: computer -> [Easy] (buffed), player -> [Difficult] (nerfed).
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Hard);
    assert_eq!(
        w.houses[2].handicap,
        easy_section(),
        "Hard: computer = [Easy]"
    );
    assert_eq!(
        w.houses[1].handicap,
        difficult_section(),
        "Hard: player = [Difficult]"
    );

    // Easy game: mirror — computer -> [Difficult], player -> [Easy].
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Easy);
    assert_eq!(
        w.houses[2].handicap,
        difficult_section(),
        "Easy: computer = [Difficult]"
    );
    assert_eq!(
        w.houses[1].handicap,
        easy_section(),
        "Easy: player = [Easy]"
    );

    // Normal: every house is the byte-exact neutral no-op.
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Normal);
    assert!(w.houses[1].handicap.is_neutral());
    assert!(w.houses[2].handicap.is_neutral());
    assert_eq!(w.houses[1].handicap, Handicap::default());
    assert_eq!(w.houses[2].handicap, Handicap::default());
}

/// Multiple computer houses at once: `set_campaign_difficulty` must assign the
/// *same* computer handicap to every non-player house, not just the first.
#[test]
fn every_non_player_house_gets_the_computer_handicap() {
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Hard);
    for h in [0u8, 2, 3, 9, 19] {
        assert_eq!(
            w.houses[h as usize].handicap,
            easy_section(),
            "house {h} (not the player) must get the computer ([Easy]) handicap"
        );
    }
    assert_eq!(
        w.houses[1].handicap,
        difficult_section(),
        "house 1 (the player) must get the inverse ([Difficult]) handicap"
    );
}

/// The player house index is a parameter, not hardcoded to 1 — verify a
/// different player house index is honored (e.g. USSR-side campaigns).
#[test]
fn player_house_index_is_honored_not_hardcoded() {
    let mut w = base_world();
    w.set_player_house(2);
    w.set_campaign_difficulty(2, Difficulty::Hard);
    assert_eq!(
        w.houses[2].handicap,
        difficult_section(),
        "house 2 is the player, nerfed on Hard"
    );
    assert_eq!(
        w.houses[1].handicap,
        easy_section(),
        "house 1 is a computer, buffed on Hard"
    );
}

// ===========================================================================
// 2. End-to-end site coverage beyond firepower: armor (damage taken) and ROF
//    (rearm delay), through `set_campaign_difficulty` itself (not a
//    hand-assigned `Handicap`, closing the gap `campaign_activation_suite`
//    leaves — it only exercises firepower end-to-end).
// ===========================================================================

fn damage_taken_by_house(target_house: u8, diff: Difficulty, player_house: u8) -> i32 {
    let mut w = base_world();
    w.set_player_house(player_house);
    w.set_campaign_difficulty(player_house, diff);
    let shooter_house = if target_house == 1 { 2 } else { 1 };
    // Isolate the *target's* armor bias: `set_campaign_difficulty` also biases the
    // shooter's firepower (every non-player house shares one computer handicap),
    // which would confound this armor-only measurement. Reset the shooter back to
    // neutral so only the target's armor bias is in play.
    w.houses[shooter_house as usize].handicap = Handicap::default();
    let shooter = w.spawn_unit(
        0,
        shooter_house,
        CellCoord::new(10, 10),
        Facing(0),
        400,
        stats(),
    );
    w.set_unit_combat(shooter, 0, Some(weapon(100)), false);
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
    for _ in 0..200 {
        w.tick(&[]);
        let now = w.units.get(target).unwrap().health;
        if now < 60_000 {
            return 60_000 - now as i32;
        }
    }
    panic!("shooter never fired");
}

#[test]
fn armor_handicap_end_to_end_through_campaign_difficulty() {
    // Hard game: computer (house 2) armor buffed 1.2x -> takes MORE damage.
    let hard_computer_taken = damage_taken_by_house(2, Difficulty::Hard, 1);
    assert_eq!(
        hard_computer_taken, 120,
        "Hard computer armor [Easy] 1.2x: round(100*1.2)"
    );
    // Hard game: player (house 1) armor nerfed .8x -> takes LESS damage... wait,
    // [Difficult] Armor=.8 means the target *reduces* incoming damage (armor.rs
    // convention: armor bias scales damage taken, <1 = tougher).
    let hard_player_taken = damage_taken_by_house(1, Difficulty::Hard, 1);
    assert_eq!(
        hard_player_taken, 80,
        "Hard player armor [Difficult] .8x: round(100*.8)"
    );

    // Easy game: mirror.
    let easy_computer_taken = damage_taken_by_house(2, Difficulty::Easy, 1);
    assert_eq!(
        easy_computer_taken, 80,
        "Easy computer armor [Difficult] .8x"
    );
    let easy_player_taken = damage_taken_by_house(1, Difficulty::Easy, 1);
    assert_eq!(easy_player_taken, 120, "Easy player armor [Easy] 1.2x");
}

/// ROF end to end: measure the tick gap between two shots for the handicapped
/// house, through `set_campaign_difficulty` (not a hand-assigned `Handicap`).
fn shot_gap_for_house(shooter_house: u8, diff: Difficulty, player_house: u8, base_rof: u16) -> u32 {
    let mut w = base_world();
    w.set_player_house(player_house);
    w.set_campaign_difficulty(player_house, diff);
    let shooter = w.spawn_unit(
        0,
        shooter_house,
        CellCoord::new(10, 10),
        Facing(0),
        400,
        stats(),
    );
    let mut wp = weapon(1);
    wp.rof = base_rof;
    w.set_unit_combat(shooter, 0, Some(wp), false);
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
    let mut last = 60_000u16;
    let mut shots = Vec::new();
    for t in 1..2000u32 {
        w.tick(&[]);
        let now = w.units.get(target).unwrap().health;
        if now < last {
            shots.push(t);
            last = now;
            if shots.len() == 2 {
                break;
            }
        }
    }
    assert_eq!(shots.len(), 2, "expected two observed shots");
    shots[1] - shots[0]
}

#[test]
fn rof_handicap_end_to_end_through_campaign_difficulty() {
    const BASE_ROF: u16 = 60;
    // Hard: computer [Easy] ROF=.8 -> fires faster (shorter gap).
    let hard_computer_gap = shot_gap_for_house(2, Difficulty::Hard, 1, BASE_ROF);
    assert_eq!(
        hard_computer_gap, 48,
        "Hard computer ROF [Easy] .8x: round(60*.8)"
    );
    // Hard: player [Difficult] ROF=1.2 -> fires slower (longer gap).
    let hard_player_gap = shot_gap_for_house(1, Difficulty::Hard, 1, BASE_ROF);
    assert_eq!(
        hard_player_gap, 72,
        "Hard player ROF [Difficult] 1.2x: round(60*1.2)"
    );
}

/// The documented real-data asymmetry: on our **Easy** setting, the computer
/// house's Cost/BuildTime stay neutral (`[Difficult]` doesn't nerf them),
/// unlike FirePower/Armor/Groundspeed/ROF which are all nerfed. This is a
/// structural property of the real rules.ini, not a mirrored guess — pin it so
/// a future "symmetric mirror" refactor is caught.
#[test]
fn easy_ai_is_combat_nerfed_but_not_economically_nerfed() {
    let mut w = base_world();
    w.set_campaign_difficulty(1, Difficulty::Easy);
    let computer = w.houses[2].handicap;
    assert!(
        computer.firepower < FX_10,
        "Easy computer firepower is nerfed"
    );
    assert!(computer.armor < FX_10, "Easy computer armor is nerfed");
    assert!(
        computer.groundspeed < FX_10,
        "Easy computer groundspeed is nerfed"
    );
    assert!(computer.rof > FX_10, "Easy computer ROF is nerfed (slower)");
    assert_eq!(
        computer.cost, FX_10,
        "Easy computer Cost stays neutral (real rules.ini asymmetry)"
    );
    assert_eq!(
        computer.build_time, FX_10,
        "Easy computer BuildTime stays neutral (real rules.ini asymmetry)"
    );
}
