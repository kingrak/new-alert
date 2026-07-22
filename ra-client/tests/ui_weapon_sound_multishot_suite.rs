//! M7.22 Fix 3 audit follow-up: `ui_weapon_sound_suite.rs` only asserts a
//! hitscan cue fires at least once across a 30-tick run (`.contains(...)`).
//! That does not distinguish "fires per shot" (the claim in this suite's own
//! doc comment: "plays each weapon's own `Report=` sound on every shot ...
//! `TECHNO.CPP:3290`") from "fires once on first acquisition and then goes
//! quiet" (the very bug class M7.22 Fix 3 was written to close, just
//! recurring one level up). This file counts cues for a rapid-ROF weapon
//! over enough ticks to land several shots, and pins the count against a
//! hand-computed expectation from the ROF cadence.

use ra_client::appcore::{AppCore, SoundEvent};
use ra_client::compositor::IndexedImage;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World, ARMOR_COUNT,
};

const TICK_MS: u32 = 67;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 0,
        rot: 20,
    }
}

/// A hitscan weapon with a short ROF (rearms in 3 ticks) and huge target
/// health so the target survives long enough to be shot at repeatedly.
fn rapid_hitscan(rof: u16) -> WeaponProfile {
    WeaponProfile {
        damage: 1, // minimal, so the huge-health target never dies mid-run
        rof,
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
        max_damage: 1,
    }
}

/// Same construction as `ui_weapon_sound_suite.rs::run_firefight`, parametrized
/// by ROF and tick budget so this file stays self-contained.
fn run_firefight(rof: u16, ticks: u32) -> Vec<SoundEvent> {
    let mut world = World::new(Passability::all_passable(), 0x51F3_0002);
    let atk = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    world.set_unit_combat(atk, 0, Some(rapid_hitscan(rof)), true);
    let tgt = world.spawn_unit(0, 2, CellCoord::new(12, 10), Facing(0), 30_000, stats());
    world.set_unit_combat(tgt, 0, None, false);

    let raster = IndexedImage {
        width: 16,
        height: 16,
        pixels: vec![0u8; 16 * 16],
    };
    let mut core = AppCore::with_sim(raster, [[0u8; 3]; 256], world, Vec::new(), Vec::new());
    core.set_fire_reports(vec![Some("GUN11.AUD")], Vec::new());
    core.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 200,
    });
    core.inject_command(Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    });

    let mut sounds = Vec::new();
    for _ in 0..ticks {
        core.update(TICK_MS);
        sounds.extend(core.drain_sounds());
    }
    sounds
}

/// A rapid-ROF hitscan weapon cues once PER SHOT, not once total: over 40
/// ticks at ROF=3 (rearm every 3 ticks, already in range/aligned from tick 0),
/// several distinct `WeaponFire` cues must appear, not just one.
#[test]
fn rapid_rof_hitscan_weapon_cues_multiple_times_not_once() {
    let sounds = run_firefight(3, 40);
    let shots = sounds
        .iter()
        .filter(|e| **e == SoundEvent::WeaponFire("GUN11.AUD"))
        .count();
    assert!(
        shots >= 5,
        "a ROF=3 hitscan weapon over 40 ticks must cue on (most) every shot, \
         not once on acquisition; got {shots} cues total: {sounds:?}"
    );
}

/// The cue count is bounded above too: the weapon cannot cue MORE often than
/// physically possible under its ROF (no double-counting the same shot, no
/// spurious cues from something other than an actual `arm` jump). With ROF=3
/// over 40 ticks, at most `40 / 3 + 1` shots can land.
#[test]
fn cue_count_never_exceeds_the_rof_cadence_upper_bound() {
    let rof = 3u32;
    let ticks = 40u32;
    let sounds = run_firefight(rof as u16, ticks);
    let shots = sounds
        .iter()
        .filter(|e| **e == SoundEvent::WeaponFire("GUN11.AUD"))
        .count();
    let upper_bound = (ticks / rof + 2) as usize; // + slack for approach/alignment ticks
    assert!(
        shots <= upper_bound,
        "got {shots} cues, more than the ROF={rof} cadence permits over {ticks} ticks \
         (upper bound {upper_bound}) — the arm-diff detector may be double-firing"
    );
}

/// Sanity floor: at the slowest end (ROF very large relative to the run), the
/// cue count must still be small and MATCH what a hand count of `arm==0` fire
/// opportunities predicts to within +/-1 — not just "greater than zero".
#[test]
fn cue_count_matches_a_hand_computed_shot_count_within_one() {
    // ROF=10 over 25 ticks, already in range and aligned from tick 0: shots
    // land at ticks 0, 10, 20 (arm counts down 10->0 then refires) — 3 shots,
    // modulo the +/-1 slack for exactly which tick `update()` lands a shot on
    // (input delay / first-tick alignment is an implementation detail this
    // test does not want to overfit to).
    let sounds = run_firefight(10, 25);
    let shots = sounds
        .iter()
        .filter(|e| **e == SoundEvent::WeaponFire("GUN11.AUD"))
        .count();
    assert!(
        (2..=4).contains(&shots),
        "ROF=10 over 25 ticks should land ~3 shots (hand count), got {shots}: {sounds:?}"
    );
}
