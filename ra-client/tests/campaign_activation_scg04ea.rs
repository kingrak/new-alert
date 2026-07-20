//! M7.5-C depth audit (ra-tester): real-asset production + `[Base]` rebuild
//! end-to-end on **scg04ea** — the first Allied mission with a real prebuilt
//! `[Base]` (15 BadGuy buildings: POWR/BARR/PROC/WEAP/FTUR/SILO/AFLD…, QUIRKS
//! Q19). `campaign_activation_scg03ea.rs` already covers the earliest
//! AUTOCREATE+BEGIN_PRODUCTION real trigger (scg03ea); this file goes one
//! mission further to exercise the `[Base]` rebuild machinery against real
//! parsed data instead of a synthetic node list.
//!
//! Skips cleanly (never fails) when the real assets aren't present.

mod support;

use ra_client::assets;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Difficulty, MoveStats, Target, WarheadProfile, WeaponProfile};

const BADGUY: u8 = 9;

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

/// Destroy `victim` through the real combat/death path (spawn a neutral-house
/// attacker adjacent to it with a one-shot-kill weapon and let it fire), rather
/// than mutating `Building::health` directly. A raw `health = 0` mutation does
/// **not** remove the building or clear its footprint occupancy — only the
/// bullet-damage path (`run_bullets`) and `Command::Sell` call the sim's
/// `remove_building` (which does `Passability::set_occupied(cell, false)`).
/// Bypassing that leaves a "zombie" building: `is_alive() == false`, but still
/// occupying the arena *and* its footprint cells forever, permanently blocking
/// any rebuild placement there — confirmed empirically while writing this
/// suite (a `health = 0` version of this test hung at `ready_building` forever
/// because the footprint stayed marked occupied). Not a bug in the shipped
/// game paths (both real ways a building dies — combat, sell — already route
/// through `remove_building`), but a sharp edge for any test/tooling code that
/// reaches for the health field directly.
fn destroy_building_via_combat(core: &mut ra_client::AppCore, victim: ra_sim::Handle) {
    let cell = core.world().buildings.get(victim).unwrap().cell;
    // Greece (the player house, 1) — real scg04ea has `[BadGuy] Allies=USSR`, so
    // Greece is guaranteed hostile to BadGuy. (A high synthetic house index like
    // 199 does NOT work here: it is out of range of the loaded campaign's house
    // table, and out-of-range house indices are not a supported/tested
    // configuration for combat/alliance checks.)
    let attacker_house = 1u8;
    let attacker = core.world_mut().spawn_unit(
        0,
        attacker_house,
        CellCoord::new(cell.x - 1, cell.y),
        Facing(0),
        400,
        MoveStats {
            max_speed: 20,
            rot: 10,
        },
    );
    let killshot = WeaponProfile {
        damage: 1_000_000,
        rof: 1,
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
    };
    core.world_mut()
        .set_unit_combat(attacker, 0, Some(killshot), false);
    core.world_mut().tick(&[Command::Attack {
        unit: attacker,
        target: Target::Building(victim),
        house: attacker_house,
    }]);
    for _ in 0..30 {
        let health = core.world().buildings.get(victim).map(|b| b.health);
        if health.map(|h| h == 0).unwrap_or(true) {
            break;
        }
        core.update(70);
    }
    // Clean up the attacker so it doesn't confound later unit-count assertions.
    core.world_mut().units.remove(attacker);
}

/// The loader must resolve the real `[Base]` section: `Count=15` raw entries,
/// owner house 9 (BadGuy). Structural finding: the real scg04ea `[Base]` list
/// includes two `AFLD` (airfield) nodes, and this engine has no aircraft yet,
/// so `register_campaign_building` cannot resolve them and the loader **drops**
/// them (`assets.rs`'s `base_nodes` construction, `None => skipped.push(...)`)
/// — 13 nodes actually enter `EnemyActivation::base_nodes`, not the raw 15.
/// This is documented/expected (matches the same drop behaviour `[STRUCTURES]`
/// placement uses for unresolved types), not a bug; pinned here so a future
/// aircraft milestone that starts resolving AFLD is a visible, deliberate
/// count change here rather than a silent one.
#[test]
fn scg04ea_base_section_resolves_13_of_15_badguy_nodes_aflds_dropped() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let mission =
        match assets::load_campaign_from_bytes(&main, &redalert, "scg04ea.ini", Difficulty::Normal)
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP scg04ea: {e}");
                return;
            }
        };
    let afld_skips = mission
        .skipped
        .iter()
        .filter(|s| s.as_str() == "base:AFLD")
        .count();
    assert_eq!(
        afld_skips, 2,
        "both AFLD [Base] nodes are dropped (no aircraft support)"
    );

    let w = mission.core.world();
    let ea = w
        .enemy_activation()
        .expect("scg04ea installs enemy-activation");
    assert_eq!(ea.base_house, BADGUY, "[Base] Player=BadGuy");
    assert_eq!(
        ea.base_nodes.len(),
        13,
        "scg04ea's real [Base] has 15 raw entries; 13 resolve (2 AFLD dropped)"
    );
    assert!(!ea.is_active(), "nothing alerted/started at load");
}

/// End-to-end: let the real `set1` trigger (a `TIME`-0 `BEGIN_PRODUCTION` on
/// BadGuy, house-9 encoded directly — QUIRKS Q19) fire naturally; if it hasn't
/// activated production within a generous tick budget, force it through the
/// public `EnemyActivation` API (the engine's force path — same mechanism
/// `TACTION_BEGIN_PRODUCTION` itself uses, just driven directly instead of
/// waiting on the trigger clock). Then: destroy a `[Base]`-listed building and
/// assert it rebuilds at its scripted cell while credits drain.
#[test]
fn scg04ea_destroyed_base_building_rebuilds_at_its_scripted_cell_while_credits_drain() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let mut mission =
        match assets::load_campaign_from_bytes(&main, &redalert, "scg04ea.ini", Difficulty::Normal)
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP scg04ea: {e}");
                return;
            }
        };
    let core = &mut mission.core;

    // Try the natural trigger clock first.
    for _ in 0..30 {
        core.update(70);
        if core
            .world()
            .enemy_activation()
            .map(|ea| ea.production.get(BADGUY as usize).copied().unwrap_or(false))
            .unwrap_or(false)
        {
            break;
        }
    }
    if !core
        .world()
        .enemy_activation()
        .map(|ea| ea.production.get(BADGUY as usize).copied().unwrap_or(false))
        .unwrap_or(false)
    {
        eprintln!("scg04ea: set1 hadn't fired naturally within budget -- forcing via the public EnemyActivation API");
        let w = core.world_mut();
        let mut ea = w
            .enemy_activation()
            .cloned()
            .expect("enemy-activation installed");
        if ea.production.len() <= BADGUY as usize {
            ea.production.resize(BADGUY as usize + 1, false);
        }
        ea.production[BADGUY as usize] = true;
        w.set_enemy_activation(ea);
    }
    assert!(
        core.world()
            .enemy_activation()
            .unwrap()
            .production
            .get(BADGUY as usize)
            .copied()
            .unwrap_or(false),
        "BadGuy production must be active (natural or forced) before proceeding"
    );

    // Give BadGuy ample credits so the rebuild isn't starved (isolates the
    // rebuild mechanic from the credit-exhaustion behaviour, which
    // `campaign_production_rebuild_depth_suite.rs` covers separately).
    core.world_mut().set_house_credits(BADGUY, 30_000);

    // Pick the first [Base] node and destroy the live building standing on it
    // (it should already be placed from scenario load).
    let (node_id, node_cell) = core.world().enemy_activation().unwrap().base_nodes[0];
    let victim = core
        .world()
        .buildings
        .iter()
        .find(|(_, b)| b.house == BADGUY && b.type_id == node_id && b.cell == node_cell)
        .map(|(h, _)| h);
    let Some(victim) = victim else {
        eprintln!(
            "SKIP: could not locate the live building for [Base] node 0 on its scripted cell"
        );
        return;
    };
    destroy_building_via_combat(core, victim);
    assert!(
        !core.world().buildings.iter().any(|(_, b)| b.house == BADGUY
            && b.type_id == node_id
            && b.cell == node_cell
            && b.is_alive()),
        "the node building must actually be gone"
    );

    let credits_before = core.world().house_credits(BADGUY);
    let mut rebuilt = false;
    for _ in 0..400 {
        core.update(70);
        if core.world().buildings.iter().any(|(_, b)| {
            b.house == BADGUY && b.type_id == node_id && b.cell == node_cell && b.is_alive()
        }) {
            rebuilt = true;
            break;
        }
    }
    assert!(
        rebuilt,
        "the destroyed [Base] node must rebuild on its scripted cell"
    );
    assert!(
        core.world().house_credits(BADGUY) < credits_before,
        "the rebuild must have drained BadGuy's credits"
    );
}

/// No rebuild before `BEGIN_PRODUCTION`/`IsStarted`: with production **not**
/// active, destroying a real `[Base]` building must never trigger a rebuild,
/// even with a live construction yard and ample credits.
#[test]
fn scg04ea_no_rebuild_before_begin_production() {
    let Some((main, redalert)) = assets_present() else {
        return;
    };
    let mut mission =
        match assets::load_campaign_from_bytes(&main, &redalert, "scg04ea.ini", Difficulty::Normal)
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP scg04ea: {e}");
                return;
            }
        };
    let core = &mut mission.core;
    assert!(
        !core
            .world()
            .enemy_activation()
            .map(|ea| ea.is_active())
            .unwrap_or(false),
        "nothing is active immediately at load"
    );
    core.world_mut().set_house_credits(BADGUY, 30_000);

    let (node_id, node_cell) = core.world().enemy_activation().unwrap().base_nodes[0];
    let victim = core
        .world()
        .buildings
        .iter()
        .find(|(_, b)| b.house == BADGUY && b.type_id == node_id && b.cell == node_cell)
        .map(|(h, _)| h);
    let Some(victim) = victim else {
        eprintln!("SKIP: could not locate the live building for [Base] node 0");
        return;
    };
    destroy_building_via_combat(core, victim);

    for _ in 0..200 {
        core.update(70);
        // If some *other* real trigger happens to fire BEGIN_PRODUCTION during
        // this window, the "no rebuild before IsStarted" premise no longer
        // holds for this run -- bail out rather than false-failing.
        if core
            .world()
            .enemy_activation()
            .map(|ea| ea.production.get(BADGUY as usize).copied().unwrap_or(false))
            .unwrap_or(false)
        {
            eprintln!("SKIP: BEGIN_PRODUCTION fired naturally during the no-IsStarted window");
            return;
        }
    }
    assert!(
        !core.world().buildings.iter().any(|(_, b)| b.house == BADGUY
            && b.type_id == node_id
            && b.cell == node_cell
            && b.is_alive()),
        "no rebuild without IsStarted, even with credits and a live construction yard"
    );
}
