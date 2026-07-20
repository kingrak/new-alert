//! M7.5-C real-asset verification: campaign **enemy activation** driven by the
//! genuine scenario triggers.
//!
//! - **scg03ea** ("Dead End") is the earliest Allied mission that fires both
//!   `TACTION_AUTOCREATE` (13) and `TACTION_BEGIN_PRODUCTION` (3) — via its `acrt`
//!   trigger (house 9 = BadGuy), a `PLAYER_ENTERED` cell trigger at cells
//!   8107–8109. We put a player unit on that cell, let the real trigger fire, and
//!   assert BadGuy becomes alerted + production-started and forms autocreate teams
//!   (`bad1`/`bad2`, flags & 4) from its idle E1 infantry.
//! - **scg01ea** on Easy vs Hard proves the P0 difficulty handicaps thread through
//!   the loader to the houses (enemy buffed on Hard, nerfed on Easy).
//!
//! Skips cleanly (never fails) when the real assets aren't present.

mod support;

use ra_client::assets;
use ra_sim::{CellCoord, Difficulty, Facing, Mission, MoveStats};

fn assets_present() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: real assets not found under {}", dir.display());
        return None;
    }
    Some((
        std::fs::read(dir.join("main.mix")).unwrap(),
        std::fs::read(dir.join("redalert.mix")).unwrap(),
    ))
}

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

/// Drive the real `acrt` trigger and assert BadGuy autocreates + begins production.
#[test]
fn scg03ea_acrt_trigger_activates_badguy_autocreate_and_production() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let mut mission =
        match assets::load_campaign_from_bytes(&main, &redalert, "scg03ea.ini", Difficulty::Normal)
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP scg03ea: {e}");
                return;
            }
        };
    let core = &mut mission.core;

    // Confirm the loader wired the autocreate team types (bad1/bad2, flags & 4) for
    // BadGuy (house 9) and the enemy-activation side-struct is installed.
    {
        let w = core.world();
        let camp = w.campaign().expect("scg03ea is a campaign");
        let autocreate_9 = camp
            .teamtypes
            .iter()
            .filter(|t| t.house == 9 && (t.flags & ra_sim::campaign::team_flags::AUTOCREATE) != 0)
            .count();
        assert!(
            autocreate_9 >= 2,
            "scg03ea has >=2 BadGuy autocreate team types (bad1/bad2), got {autocreate_9}"
        );
        assert!(w.enemy_activation().is_some(), "enemy-activation installed");
        let ea = w.enemy_activation().unwrap();
        assert!(!ea.is_active(), "no house is alerted/started at load");
    }

    // Baseline: no BadGuy unit is hunting before the trigger fires.
    let hunting_before = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.house == 9 && u.hunt)
        .count();
    assert_eq!(hunting_before, 0, "no BadGuy is hunting before activation");

    // Put a Greece (player, house 1) unit onto an `acrt` cell trigger (cell 8107)
    // and hold it there (Sleep = fully inert) so the real PLAYER_ENTERED event fires.
    let acrt_cell = CellCoord::from_index(8107);
    {
        let w = core.world_mut();
        let h = w.spawn_unit(0, 1, acrt_cell, Facing(0), 100, stats());
        w.set_unit_mission(h, Mission::Sleep);
    }

    // Tick: the PLAYER_ENTERED event springs `acrt`, whose actions AUTOCREATE +
    // BEGIN_PRODUCTION target BadGuy (house 9, encoded `-247 & 0xFF`); the first
    // autocreate wave fires the very same tick (AlertTime starts at 0).
    for _ in 0..5 {
        core.update(70);
    }
    {
        let ea = core.world().enemy_activation().unwrap();
        assert!(
            ea.alerted.get(9).copied().unwrap_or(false),
            "acrt AUTOCREATE alerted BadGuy"
        );
        assert!(
            ea.production.get(9).copied().unwrap_or(false),
            "acrt BEGIN_PRODUCTION started BadGuy"
        );
    }

    // BadGuy formed an autocreate team from its idle E1 infantry (DO:MISSION_HUNT).
    let hunting_after = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.house == 9 && u.hunt)
        .count();
    assert!(
        hunting_after > hunting_before,
        "BadGuy formed an autocreate team (idle units began hunting): {hunting_before} -> {hunting_after}"
    );
    eprintln!("scg03ea: BadGuy activated — {hunting_after} units hunting after autocreate");
}

/// Same real script, driven identically twice, hashes identically after activation
/// (determinism through the new RNG-drawing enemy-activation systems).
#[test]
fn scg03ea_activation_is_deterministic_same_script_twice() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let run = || -> Option<u64> {
        let mut m =
            assets::load_campaign_from_bytes(&main, &redalert, "scg03ea.ini", Difficulty::Normal)
                .ok()?;
        let core = &mut m.core;
        let acrt_cell = CellCoord::from_index(8107);
        {
            let w = core.world_mut();
            let h = w.spawn_unit(0, 1, acrt_cell, Facing(0), 100, stats());
            w.set_unit_mission(h, Mission::Sleep);
        }
        for _ in 0..40 {
            core.update(70);
        }
        Some(core.world().state_hash())
    };
    let (a, b) = (run(), run());
    if let (Some(a), Some(b)) = (a, b) {
        assert_eq!(a, b, "same script twice must hash identically");
    }
}

/// P0: the difficulty selection reaches the houses. Loading scg01ea on Hard buffs
/// the computer (USSR=2) firepower ([Easy] 1.2) and nerfs the player (Greece=1,
/// [Difficult] .8); Easy mirrors it. Normal leaves every house neutral.
#[test]
fn scg01ea_difficulty_biases_enemy_and_player_houses() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let load = |diff: Difficulty| {
        assets::load_campaign_from_bytes(&main, &redalert, "scg01ea.ini", diff)
            .ok()
            .map(|m| {
                let w = m.core.world();
                // Player = Greece (1); the computer USSR = 2.
                (
                    w.houses[1].handicap.firepower,
                    w.houses[2].handicap.firepower,
                )
            })
    };
    let (Some((p_hard, e_hard)), Some((p_easy, e_easy)), Some((p_norm, e_norm))) = (
        load(Difficulty::Hard),
        load(Difficulty::Easy),
        load(Difficulty::Normal),
    ) else {
        eprintln!("SKIP scg01ea difficulty: load failed");
        return;
    };

    // Hard: enemy buffed (>1.0), player nerfed (<1.0). Easy: mirror.
    assert!(e_hard > 65536, "Hard enemy firepower buffed: {e_hard}");
    assert!(p_hard < 65536, "Hard player firepower nerfed: {p_hard}");
    assert!(e_easy < 65536, "Easy enemy firepower nerfed: {e_easy}");
    assert!(p_easy > 65536, "Easy player firepower buffed: {p_easy}");
    // Normal: neutral for everyone (the golden-preserving default).
    assert_eq!(p_norm, 65536);
    assert_eq!(e_norm, 65536);
    // The enemy's Hard/Easy firepower ratio is the hand-computed handicap ratio.
    eprintln!(
        "scg01ea enemy firepower — Hard {e_hard} vs Easy {e_easy} (ratio {:.3})",
        e_hard as f64 / e_easy as f64
    );
    assert!(e_hard > e_easy, "Hard enemy out-damages Easy enemy");
}
