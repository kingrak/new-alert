//! M7.22 Fix 3 pins — per-weapon fire sounds through the client sound path.
//!
//! The bug: the fire cue fired only on a *new projectile* (a bullet appearing in
//! the arena), so hitscan weapons (rifles, MGs, tesla, pillbox — `instant`,
//! leave no persistent bullet) were silent, and every weapon shared one sound
//! (`CANNON1`). The reference plays each weapon's own `Report=` on every shot at
//! the fire chokepoint (`TechnoClass::Fire_At` → `Sound_Effect(weapon->Sound)`,
//! TECHNO.CPP:3290), projectile and hitscan alike.
//!
//! These are asset-free: a synthetic armed unit + an installed report table.

mod support;

use ra_client::appcore::{AppCore, SoundEvent};
use ra_client::compositor::IndexedImage;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World, ARMOR_COUNT,
};

const TICK_MS: u32 = 67;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 0, // stationary so it just stands and shoots
        rot: 20,      // a normal turn rate (already facing the target — no delay)
    }
}

/// A hitscan rifle: `instant` (invisible + light speed) so it leaves no bullet —
/// the exact weapon class the old bullet-diff cue never noticed. Fast ROF so the
/// shot lands within a couple of ticks.
fn hitscan_rifle() -> WeaponProfile {
    WeaponProfile {
        damage: 15,
        rof: 4,
        range: 1024, // 4 cells
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
    }
}

/// Build a core: a house-1 hitscan unit (type id 0) two cells from a house-2
/// target, with `unit_fire_reports[0]` set to `report`. Drives `update()` and
/// returns every sound cue drained across the run.
fn run_firefight(report: Option<&'static str>) -> Vec<SoundEvent> {
    let mut world = World::new(Passability::all_passable(), 0x51F3_0001);
    let atk = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    world.set_unit_combat(atk, 0, Some(hitscan_rifle()), true);
    let tgt = world.spawn_unit(0, 2, CellCoord::new(12, 10), Facing(0), 800, stats());
    world.set_unit_combat(tgt, 0, None, false);

    let raster = IndexedImage {
        width: 16,
        height: 16,
        pixels: vec![0u8; 16 * 16],
    };
    let mut core = AppCore::with_sim(raster, [[0u8; 3]; 256], world, Vec::new(), Vec::new());
    // Report table: unit type 0 -> the given report; buildings none.
    core.set_fire_reports(vec![report], Vec::new());

    core.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 200,
    });
    // Order the attack through the real command path.
    core.inject_command(Command::Attack {
        unit: atk,
        target: Target::Unit(tgt),
        house: 1,
    });

    let mut sounds = Vec::new();
    for _ in 0..30 {
        core.update(TICK_MS);
        sounds.extend(core.drain_sounds());
    }
    sounds
}

/// A hitscan infantry shot emits its weapon's own report — NOT the generic
/// `Fire` (CANNON1) cue, and NOT nothing.
#[test]
fn hitscan_shot_emits_its_weapon_report() {
    let sounds = run_firefight(Some("GUN11.AUD"));
    assert!(
        sounds.contains(&SoundEvent::WeaponFire("GUN11.AUD")),
        "a hitscan rifle must cue its own report (GUN11); got {sounds:?}"
    );
    assert!(
        !sounds.contains(&SoundEvent::Fire),
        "a weapon with a mapped report must NOT fall back to the generic CANNON1 cue"
    );
}

/// Revert-drill on the hitscan cue path: with NO report mapped, a hitscan shot
/// still cues (the generic `Fire` fallback) — proving the arm-diff detection,
/// not the projectile diff, is what drives the cue. The pre-fix bullet-diff cue
/// would emit *nothing at all* here (a hitscan weapon spawns no lingering
/// bullet), so this asserts the detection path itself.
#[test]
fn hitscan_shot_cues_even_without_a_mapped_report() {
    let sounds = run_firefight(None);
    assert!(
        sounds.contains(&SoundEvent::Fire),
        "a hitscan shot with no mapped report must still cue (generic Fire); got {sounds:?}"
    );
    assert!(
        !sounds.contains(&SoundEvent::WeaponFire("GUN11.AUD")),
        "no report was mapped, so no per-weapon cue should appear"
    );
}
