//! Win/lose resolution suite (M6 coverage item 5): house-elimination
//! victory/defeat, the "does a command still mechanically apply after
//! game-over" question, and the same-tick mutual-elimination edge case in
//! `update_game_over`'s check order. Public-API only (`World::tick`,
//! `Command`, `set_player_house`, `set_ai`, `game_over()`, `house_alive()`,
//! `World::buildings`/`units` arenas) — synthetic worlds, no real assets
//! needed (DESIGN.md §4.2, §4.9 M6).
//!
//! Read `ra-sim/src/world.rs`'s `update_game_over` (system 8 of `apply`)
//! before touching this file: it checks `!house_alive(player)` FIRST →
//! `Defeat`, and only then (and only if that didn't fire) checks "every AI
//! house eliminated" → `Victory`. That ordering is exactly what
//! `simultaneous_elimination_resolves_defeat_never_victory` below pins.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Command, Difficulty, EconRules, GameOver, Handle, MoveStats,
    Passability, Target, WarheadProfile, WeaponProfile, World,
};

/// The only building type in the test catalog: a cheap 1x1 "HUT". Win/lose
/// resolution (`house_alive`/`update_game_over`) never reads catalog fields at
/// all, so this only needs to exist so `spawn_building` can resolve a proto.
const B_HUT: u32 = 0;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "HUT".to_string(),
            foot_w: 1,
            foot_h: 1,
            max_health: 100,
            armor: 0,
            power: 0,
            cost: 10,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }],
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

/// Give `house` one live building and one live unit at `cell`/`cell + (5,5)`,
/// returning their handles so a test can eliminate them individually.
fn give_house_assets(w: &mut World, house: u8, cell: CellCoord) -> (Handle, Handle) {
    let b = w.spawn_building(B_HUT, house, cell).unwrap();
    let u = w.spawn_unit(
        0,
        house,
        CellCoord::new(cell.x + 5, cell.y + 5),
        Facing(0),
        100,
        stats(),
    );
    (b, u)
}

// ---------------------------------------------------------------------
// 1/2. Basic victory / defeat.
// ---------------------------------------------------------------------

#[test]
fn victory_when_every_ai_house_is_eliminated() {
    let mut w = world(0xA11C_E001);
    let (_pb, _pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (ab, au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    // Both houses alive: still ongoing.
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Ongoing);
    assert!(w.house_alive(1));
    assert!(w.house_alive(2));

    // Destroy every building AND unit of the enemy house.
    w.buildings.remove(ab);
    w.units.remove(au);
    assert!(!w.house_alive(2), "house 2 should be fully eliminated");

    w.tick(&[]);
    assert_eq!(
        w.game_over(),
        GameOver::Victory,
        "every AI house eliminated with the player alive must resolve Victory"
    );
}

#[test]
fn defeat_when_the_player_house_is_eliminated() {
    let mut w = world(0xD0FE_A702);
    let (pb, pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (_ab, _au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Ongoing);

    // Destroy every building AND unit of the player's house instead.
    w.buildings.remove(pb);
    w.units.remove(pu);
    assert!(!w.house_alive(1), "house 1 should be fully eliminated");

    w.tick(&[]);
    assert_eq!(
        w.game_over(),
        GameOver::Defeat,
        "the player house being eliminated must resolve Defeat, even with a live enemy"
    );
}

/// No skirmish setup (`set_player_house` never called) → `game_over()` stays
/// `Ongoing` forever, even once every house on the map is empty. Sanity check
/// that the resolution is opt-in, not automatic from empty-house state alone.
#[test]
fn no_player_house_designated_never_resolves() {
    let mut w = world(0x0BAD_F00D);
    // No houses own anything at all, and no player/AI are configured.
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert_eq!(w.game_over(), GameOver::Ongoing);
}

// ---------------------------------------------------------------------
// 3. Commands after game over.
// ---------------------------------------------------------------------

/// **Finding, not a bug (report only, per the ra-tester charter — production
/// code is ra-coder's domain).** `apply_command` (`ra-sim/src/world.rs`) has
/// no `game_over` gate anywhere in its match arms; `apply`'s system order
/// only *checks* `game_over` at the very end (system 8). So a `Command`
/// issued for the still-alive house *after* the terminal state is reached
/// keeps mechanically applying — a unit ordered to move after Victory/Defeat
/// really does move. Whatever gating exists (see `ra-client/src/appcore.rs`'s
/// `accepting_orders()`) is purely client-side UI policy, not a sim-level
/// invariant; a raw command log / network peer that ignores the client could
/// still mutate a "finished" game indefinitely. Pinned here explicitly so a
/// future change to `apply_command` shows up as an intentional diff, not a
/// silent behavior change.
#[test]
fn commands_still_mechanically_apply_after_game_over() {
    let mut w = world(0xC0DE_AF7E);
    let (_pb, pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (ab, au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
    w.tick(&[]);

    w.buildings.remove(ab);
    w.units.remove(au);
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Victory, "setup: should be Victory");

    // Issue a Move for the still-alive player unit, post-Victory.
    let before = w.units.get(pu).unwrap().cell();
    let dest = CellCoord::new(before.x + 10, before.y);
    w.tick(&[Command::Move {
        unit: pu,
        dest,
        house: 1,
    }]);
    // Let it walk a while.
    for _ in 0..200 {
        w.tick(&[]);
    }
    let after = w.units.get(pu).unwrap().cell();
    assert_ne!(
        after, before,
        "FINDING: a Move command issued after game_over() != Ongoing still moved the unit — \
         apply_command has no game-over gate at the sim level"
    );
    // The terminal state itself is unaffected by post-game-over activity.
    assert_eq!(w.game_over(), GameOver::Victory);

    // A command referencing a handle from an already-eliminated house is
    // still just a normal ownership-check no-op (nothing special about
    // game-over here — ordinary stale-handle rejection).
    w.tick(&[Command::Move {
        unit: au, // stale: removed above
        dest: CellCoord::new(0, 0),
        house: 2,
    }]);
    assert_eq!(w.game_over(), GameOver::Victory);
}

/// Once a terminal state is reached it is sticky: `update_game_over` returns
/// immediately when `game_over != Ongoing`, so even if the winning side is
/// wiped out *after* the fact (a stray attack, a sell-everything script,
/// whatever), the outcome does not flip to Defeat.
#[test]
fn terminal_state_is_sticky_and_does_not_flip() {
    let mut w = world(0x571C_4444);
    let (pb, pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (ab, au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
    w.tick(&[]);

    w.buildings.remove(ab);
    w.units.remove(au);
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Victory, "setup: should be Victory");

    // Now wipe out the player's own house too, post-Victory.
    w.buildings.remove(pb);
    w.units.remove(pu);
    assert!(!w.house_alive(1));
    w.tick(&[]);

    assert_eq!(
        w.game_over(),
        GameOver::Victory,
        "a terminal state must stick even if the winning house is later wiped out too"
    );
}

// ---------------------------------------------------------------------
// 4. Simultaneous elimination.
// ---------------------------------------------------------------------

/// **The headline finding of this suite.** `update_game_over` checks
/// `!house_alive(player)` first and returns `Defeat` immediately, before ever
/// looking at whether every AI house is also dead. So a same-tick mutual
/// elimination — the player's last building/unit and the AI's last
/// building/unit both gone by the time system 8 runs — always resolves
/// `Defeat` from the player's point of view. It can never be `Victory`, and
/// there is no `Draw` variant, so a true simultaneous wipeout is
/// indistinguishable from a pure player loss.
///
/// Whether that's "reasonable" is a product call, not a correctness bug: the
/// original `house.cpp` MP defeat check is per-house and has the same kind of
/// ordering dependency (whichever house's `AI` logic runs first sees the
/// other already dead), and RA1 has no historical "simultaneous draw" UX
/// either — so this matches the reference behavior's spirit closely enough.
/// But it does mean a razor's-edge mutual kill always reads as a loss to the
/// player, never a draw, which a modern design might want to special-case.
/// Flagged for ra-coder/product to decide; not fixed here.
#[test]
fn simultaneous_elimination_resolves_defeat_never_victory() {
    let mut w = world(0x51A1_7000);
    let (pb, pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (ab, au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Ongoing, "setup: both sides alive");

    // Simulate "two bullets detonating on the same tick": both houses' last
    // assets vanish before the tick that resolves game-over runs its system-8
    // check. (Direct removal, not a scripted bullet exchange, keeps the
    // "same tick" property exact and independent of combat/travel-time
    // timing, which is the point being pinned here — not combat itself.)
    w.buildings.remove(pb);
    w.units.remove(pu);
    w.buildings.remove(ab);
    w.units.remove(au);
    assert!(!w.house_alive(1) && !w.house_alive(2), "both eliminated");

    w.tick(&[]);

    assert_eq!(
        w.game_over(),
        GameOver::Defeat,
        "CONFIRMED: a same-tick mutual elimination resolves Defeat, never Victory (no Draw \
         variant exists) — update_game_over checks the player's own elimination before ever \
         checking the AI houses"
    );
}

// ---------------------------------------------------------------------
// M7.22 Fix 4: "I killed all units, but the game is not over."
//
// `house_alive`'s units arm used to be `units.any(|u| u.house == house)` with
// NO liveness check, so a dead-but-not-yet-reaped unit (health 0) kept the
// house alive and blocked game-over. The reference defeat scan
// (`HouseClass::AI`, HOUSE.CPP:1225-1226) requires `!UScan && !ActiveIScan &&
// !ActiveVScan && ...` — the `Active*Scan` masks only include LIVE objects.
// ---------------------------------------------------------------------

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// A cheap, fast-firing weapon so a scripted kill resolves in a few ticks.
fn quick_gun() -> WeaponProfile {
    WeaponProfile {
        damage: 200,
        rof: 5,
        range: 1216,
        proj_speed: 102,
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

/// Directly pins the fix + its revert-drill: a corpse (health 0) still sitting
/// in the arena must NOT keep its house alive. Restoring the old
/// `units.any(|u| u.house == house)` (no `is_alive()`) fails this.
#[test]
fn dead_but_unreaped_unit_does_not_keep_house_alive() {
    let mut w = world(0xDEAD_0001);
    let u = w.spawn_unit(0, 2, CellCoord::new(40, 40), Facing(0), 100, stats());
    assert!(w.house_alive(2), "a live unit keeps the house alive");

    // Turn it into a corpse in place (health 0) without removing it — the exact
    // dead-but-unreaped state the bug hinged on.
    w.units.get_mut(u).unwrap().health = 0;
    assert!(
        !w.house_alive(2),
        "a health-0 corpse must not count as a live unit (Fix 4)"
    );
}

/// End-to-end through real combat + reap: house 1 guns down house 2's only
/// unit (no buildings on either side means the win check is purely the units
/// scan), and Victory must fire within a bounded tick budget after the kill.
#[test]
fn game_over_fires_within_bounded_ticks_after_last_unit_killed() {
    let mut w = world(0xDEAD_0002);
    let atk = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
    w.set_unit_combat(atk, 3, Some(quick_gun()), true);
    let victim = w.spawn_unit(0, 2, CellCoord::new(22, 20), Facing(0), 100, stats());
    w.set_unit_combat(victim, 0, None, false);
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(victim),
        house: 1,
    }]);

    // Bound: alignment + a couple of bullet flights + reap + the system-8 check.
    let mut resolved_at = None;
    for t in 0..80 {
        w.tick(&[]);
        if w.game_over() != GameOver::Ongoing {
            resolved_at = Some(t);
            break;
        }
    }
    assert_eq!(
        w.game_over(),
        GameOver::Victory,
        "killing the enemy's last unit must resolve Victory"
    );
    assert!(
        !w.units.iter().any(|(_, u)| u.house == 2),
        "the victim must actually be reaped, not lingering"
    );
    assert!(
        resolved_at.is_some_and(|t| t < 80),
        "game-over must fire within a bounded window after the last kill"
    );
}

/// Do NOT overshoot: a house that has lost every unit but still owns a live
/// building is NOT defeated (the original keeps the game going until the base
/// is gone too — `!ActiveBScan` is part of the AND). Killing all units while a
/// structure stands must leave the game Ongoing.
#[test]
fn buildings_only_house_is_still_alive() {
    let mut w = world(0xDEAD_0003);
    let (_pb, _pu) = give_house_assets(&mut w, 1, CellCoord::new(20, 20));
    let (ab, au) = give_house_assets(&mut w, 2, CellCoord::new(60, 60));
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    // Kill only the enemy's unit; its building still stands.
    w.units.get_mut(au).unwrap().health = 0;
    assert!(
        w.house_alive(2),
        "a house with a live building but no units is still alive"
    );

    w.tick(&[]);
    assert_eq!(
        w.game_over(),
        GameOver::Ongoing,
        "killing all units but leaving a building must NOT end the game"
    );

    // Finish the base off: now it resolves.
    w.buildings.remove(ab);
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Victory);
}
