//! The determinism/replay suite (DESIGN.md §4.2) — the centerpiece of the M3
//! test coverage. Every claim the determinism contract makes is exercised
//! here at the sim level (`World::tick`); the client-level mirror (driving
//! identical `InputEvent` scripts through `AppCore` on virtual time) lives in
//! `ra-client/tests/ui_determinism.rs`.
//!
//! **Coverage note on RNG.** The contract calls for "RNG consumption in the
//! scripted path". As of M3, `World`'s per-tick systems (`apply_command`,
//! `move_units`) never call `RandomLcg::next`/`range` — movement is pure
//! coordinate arithmetic, so the seed folded into every hash never actually
//! changes tick-to-tick yet. This suite still proves what's true today (the
//! seed *is* part of the hashed state, so a seed change is caught —
//! `hash_sensitivity_rng_seed` below) and exercises `RandomLcg` directly with
//! a `Random_Pick`-style ranged-draw sequence (`command_log_replay`'s
//! destination generator) to prove *that* machinery replays identically, but
//! it cannot exercise "the sim consumes RNG mid-tick" because nothing in
//! `ra-sim` does that yet. Flagged in the M3 test report as a coverage gap
//! for whichever future system (random pathing jitter, combat rolls, …)
//! first draws from `World`'s owned RNG.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, RandomLcg, World};

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// Three units of two houses on an open synthetic map: enough to exercise
/// ownership, independent paths, and re-issued orders without real assets.
struct Fixture {
    world: World,
    a: Handle, // house 1, fast
    b: Handle, // house 1, slow turner
    c: Handle, // house 2
}

fn build_fixture(seed: u32) -> Fixture {
    let mut world = World::new(Passability::all_passable(), seed);
    let a = world.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats(25, 10));
    let b = world.spawn_unit(1, 1, CellCoord::new(40, 3), Facing(64), 300, stats(12, 3));
    let c = world.spawn_unit(2, 2, CellCoord::new(3, 40), Facing(128), 150, stats(30, 20));
    Fixture { world, a, b, c }
}

const TICKS: usize = 90;

/// A command log as a plain data structure: `log[t]` are the commands
/// applied at tick `t`. This is exactly the shape a save/replay or network
/// transport would persist (DESIGN.md §4.4: "A replay is just the command
/// log + initial seed"), independent of any particular run.
fn command_log(f: &Fixture) -> Vec<Vec<Command>> {
    let mut log = vec![Vec::new(); TICKS];
    log[0].push(Command::Move {
        unit: f.a,
        dest: CellCoord::new(60, 50),
        house: 1,
    });
    log[3].push(Command::Move {
        unit: f.b,
        dest: CellCoord::new(2, 2),
        house: 1,
    });
    log[10].push(Command::Move {
        unit: f.c,
        dest: CellCoord::new(45, 45),
        house: 2,
    });
    // Re-issue: stop `a` mid-flight, then send it somewhere else entirely.
    log[25].push(Command::Stop {
        unit: f.a,
        house: 1,
    });
    log[26].push(Command::Move {
        unit: f.a,
        dest: CellCoord::new(10, 55),
        house: 1,
    });
    // Wrong-house order: must be a no-op (ownership check), not a divergence
    // source — included so the replay proves rejected commands replay
    // identically too, not just accepted ones.
    log[40].push(Command::Move {
        unit: f.c,
        dest: CellCoord::new(0, 0),
        house: 1, // house 1 does not own `c` (house 2)
    });
    log
}

/// Run `world` through `log`, returning the per-tick hash chain
/// (`hashes[t]` is the state hash returned by `World::tick` for tick `t`).
fn run(world: &mut World, log: &[Vec<Command>]) -> Vec<u64> {
    log.iter().map(|cmds| world.tick(cmds)).collect()
}

// ---------------------------------------------------------------------
// 1a. Same-seed-twice, sim-level.
// ---------------------------------------------------------------------

#[test]
fn same_seed_twice_sim_level_identical_hash_chains() {
    let fa = build_fixture(0xC0FF_EE42);
    let fb = build_fixture(0xC0FF_EE42);
    // Handles are deterministic (arena insert order), so the same log,
    // built from each fixture's own handles, addresses the same units.
    let log_a = command_log(&fa);
    let log_b = command_log(&fb);
    assert_eq!(
        log_a, log_b,
        "handles should be identical across identically-built fixtures"
    );

    let mut wa = fa.world;
    let mut wb = fb.world;
    let chain_a = run(&mut wa, &log_a);
    let chain_b = run(&mut wb, &log_b);
    assert_eq!(
        chain_a, chain_b,
        "identical seed + command log must give an identical hash chain"
    );
    assert_eq!(wa.state_hash(), wb.state_hash());
}

// ---------------------------------------------------------------------
// 1b. Command-log replay: capture a log from a "live" run, replay it from a
// fresh World, assert the chain is reproduced exactly.
// ---------------------------------------------------------------------

#[test]
fn command_log_replay_matches_live_run() {
    let live_fixture = build_fixture(0x5EED_0001);
    let log = command_log(&live_fixture);
    let mut live = live_fixture.world;
    let live_chain = run(&mut live, &log);

    // Fresh world, same seed, replaying the *persisted* log (not re-deriving
    // it from a second fixture — this is the save/replay use case: only the
    // seed and the log are kept).
    let mut replay = World::new(Passability::all_passable(), 0x5EED_0001);
    replay.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats(25, 10));
    replay.spawn_unit(1, 1, CellCoord::new(40, 3), Facing(64), 300, stats(12, 3));
    replay.spawn_unit(2, 2, CellCoord::new(3, 40), Facing(128), 150, stats(30, 20));
    let replay_chain = run(&mut replay, &log);

    assert_eq!(
        live_chain, replay_chain,
        "replayed log must reproduce the live hash chain"
    );
    assert_eq!(live.state_hash(), replay.state_hash());
}

/// Same replay idea, but the log itself is generated by a `Random_Pick`-style
/// ranged-draw sequence (`RandomLcg::range`) choosing destinations for a
/// larger population of units — the closest this suite can get to "RNG
/// consumption in the scripted path" given that no `ra-sim` system draws
/// from `World`'s own RNG yet (see module docs). Two independently-seeded
/// generators producing the same script prove the *generator* replays
/// identically; two `World`s consuming that identical script prove the *sim*
/// replays identically. Composing both covers the same ground an
/// RNG-in-the-tick-loop design would, just via an external generator rather
/// than one owned by `World`.
#[test]
fn random_pick_generated_script_replays_identically() {
    fn build(seed_world: u32, seed_script: u32) -> (World, Vec<u64>) {
        let mut world = World::new(Passability::all_passable(), seed_world);
        let mut handles = Vec::new();
        for i in 0..8 {
            let cell = CellCoord::new(5 + i * 3, 5);
            handles.push(world.spawn_unit(0, 1, cell, Facing(0), 256, stats(20, 8)));
        }
        let mut rng = RandomLcg::new(seed_script);
        let mut log = vec![Vec::new(); 40];
        for _ in 0..20 {
            let tick = rng.range(0, 39) as usize;
            let who = handles[rng.range(0, 7) as usize];
            let dest = CellCoord::new(rng.range(0, 60), rng.range(0, 60));
            log[tick].push(Command::Move {
                unit: who,
                dest,
                house: 1,
            });
        }
        let chain = run(&mut world, &log);
        (world, chain)
    }

    let (w1, chain1) = build(0xAAAA_1111, 0xBBBB_2222);
    let (w2, chain2) = build(0xAAAA_1111, 0xBBBB_2222);
    assert_eq!(chain1, chain2, "RNG-scripted replay diverged");
    assert_eq!(w1.state_hash(), w2.state_hash());
}

// ---------------------------------------------------------------------
// 2. Hash sensitivity: flipping any one relevant field must change the hash.
// ---------------------------------------------------------------------

#[test]
fn hash_sensitivity_unit_coord() {
    let mut w = World::new(Passability::all_passable(), 1);
    let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    let before = w.state_hash();
    w.units.get_mut(h).unwrap().coord.x.0 += 1;
    assert_ne!(
        before,
        w.state_hash(),
        "a 1-lepton coord change must change the hash"
    );
}

#[test]
fn hash_sensitivity_unit_facing() {
    let mut w = World::new(Passability::all_passable(), 1);
    let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    let before = w.state_hash();
    w.units.get_mut(h).unwrap().facing = Facing(1);
    assert_ne!(
        before,
        w.state_hash(),
        "a facing change must change the hash"
    );
}

#[test]
fn hash_sensitivity_unit_health() {
    let mut w = World::new(Passability::all_passable(), 1);
    let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    let before = w.state_hash();
    w.units.get_mut(h).unwrap().health -= 1;
    assert_ne!(
        before,
        w.state_hash(),
        "a health change must change the hash"
    );
}

#[test]
fn hash_sensitivity_tick_count() {
    // Two idle worlds (no commands, all-passable map -> nothing moves), one
    // ticked once more than the other: only `tick_count` differs.
    let mut w0 = World::new(Passability::all_passable(), 7);
    w0.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    let mut w1 = w0.clone();
    w1.tick(&[]);
    assert_ne!(
        w0.state_hash(),
        w1.state_hash(),
        "advancing the tick counter alone must change the hash"
    );
}

#[test]
fn hash_sensitivity_rng_seed() {
    let mut wa = World::new(Passability::all_passable(), 1);
    let mut wb = World::new(Passability::all_passable(), 2);
    wa.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    wb.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    assert_ne!(
        wa.state_hash(),
        wb.state_hash(),
        "a different sim seed alone must change the hash (units are otherwise identical)"
    );
}

#[test]
fn hash_sensitivity_unit_path_and_dest() {
    // Same units, but one world has an in-flight order and the other
    // doesn't: the path/dest fields must be hashed (guards against a field
    // silently dropped from `Unit::hash_into`).
    let mut w0 = World::new(Passability::all_passable(), 1);
    let h0 = w0.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats(10, 10));
    let mut w1 = w0.clone();
    w1.tick(&[Command::Move {
        unit: h0,
        dest: CellCoord::new(20, 20),
        house: 1,
    }]);
    // Bring tick counts back in sync by also ticking w0 once (empty), so the
    // *only* structural difference remaining is the path/dest, not tick
    // count (already covered separately above).
    w0.tick(&[]);
    assert_ne!(
        w0.state_hash(),
        w1.state_hash(),
        "an in-flight path/dest must be part of the hash"
    );
}

// ---------------------------------------------------------------------
// 3. Divergence localization: one differing command diverges the chain at
// exactly the tick it applies, not before.
// ---------------------------------------------------------------------

#[test]
fn divergence_localizes_to_the_differing_tick() {
    let fa = build_fixture(0x1357_9BDF);
    let fb = build_fixture(0x1357_9BDF);

    let log_a = command_log(&fa);
    let mut log_b = command_log(&fb);
    assert_eq!(log_a, log_b);

    // Diverge exactly one command, at tick 26 (the re-issued move for `a`):
    // a genuinely different, still-reachable destination so the resulting
    // path really differs (not coincidentally identical).
    const DIVERGE_TICK: usize = 26;
    log_b[DIVERGE_TICK] = vec![Command::Move {
        unit: fb.b, // note: `.b`, not the reissued `.a` from the base log
        dest: CellCoord::new(50, 2),
        house: 1,
    }];

    let mut wa = fa.world;
    let mut wb = fb.world;
    let chain_a = run(&mut wa, &log_a);
    let chain_b = run(&mut wb, &log_b);

    for t in 0..DIVERGE_TICK {
        assert_eq!(
            chain_a[t], chain_b[t],
            "chains diverged at tick {t}, before the differing command at tick {DIVERGE_TICK}"
        );
    }
    assert_ne!(
        chain_a[DIVERGE_TICK], chain_b[DIVERGE_TICK],
        "chains should diverge at tick {DIVERGE_TICK}, the tick the differing command applies"
    );
}

/// Same idea, minimal case: two single-unit worlds, identical for the first
/// `K` ticks, then one gets a `Move` the other doesn't. Complements the
/// larger fixture above with the simplest possible repro.
#[test]
fn divergence_localizes_minimal_repro() {
    const K: usize = 5;
    let mut wa = World::new(Passability::all_passable(), 42);
    let mut wb = World::new(Passability::all_passable(), 42);
    let ha = wa.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 256, stats(20, 10));
    let hb = wb.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 256, stats(20, 10));

    let mut chain_a = Vec::new();
    let mut chain_b = Vec::new();
    for t in 0..(K + 10) {
        let cmds_a: &[Command] = &[];
        let cmds_b: Vec<Command> = if t == K {
            vec![Command::Move {
                unit: hb,
                dest: CellCoord::new(30, 10),
                house: 1,
            }]
        } else {
            Vec::new()
        };
        let _ = ha; // only used to keep symmetry readable
        chain_a.push(wa.tick(cmds_a));
        chain_b.push(wb.tick(&cmds_b));
    }

    for t in 0..K {
        assert_eq!(chain_a[t], chain_b[t], "diverged before tick {K}");
    }
    assert_ne!(
        chain_a[K], chain_b[K],
        "did not diverge exactly at tick {K}"
    );
}

// ---------------------------------------------------------------------
// 4. Real-scenario variant: scg01ea's 4 real starting units (3 Greek JEEPs +
// 1 USSR HARV). Skip-clean without assets; pins a golden hash-chain prefix.
// ---------------------------------------------------------------------

mod real {
    use ra_data::passability;
    use ra_data::rules::unit_stats;
    use ra_data::scenario::{parse_units, Scenario};
    use ra_formats::ini::Ini;
    use ra_formats::mix::MixArchive;
    use ra_sim::coords::{CellCoord, Facing};
    use ra_sim::{Handle, MoveStats, Passability, World};
    use std::path::PathBuf;

    fn assets_dir() -> PathBuf {
        std::env::var("RA_ASSETS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"))
    }

    /// Build a `World` from scg01ea's real `[UNITS]` placements, with real
    /// `Speed`/`ROT`/`Strength` resolved from `rules.ini`. This mirrors (a
    /// stripped, rendering-free copy of) the unit-spawning slice of
    /// `ra_client::assets::load_game_from_bytes`; `ra-sim` cannot depend on
    /// `ra-client` (that crate already depends on `ra-sim`, so the reverse
    /// would be a cycle), so this reimplements just what the sim needs:
    /// passability + spawns, no sprites/palette/remaps. `type_id` is left 0
    /// for every unit (opaque to the sim; only the client's renderer cares).
    pub fn load_scg01ea(seed: u32) -> Option<(World, Vec<Handle>)> {
        let dir = assets_dir();
        if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
            return None;
        }
        let main_bytes = std::fs::read(dir.join("main.mix")).ok()?;
        let redalert_bytes = std::fs::read(dir.join("redalert.mix")).ok()?;

        let main = MixArchive::parse(&main_bytes).ok()?;
        let general = main.open_nested("general.mix").ok()?;
        let ini_bytes = general.get("scg01ea.ini")?;
        let ini_text = String::from_utf8_lossy(ini_bytes);
        let ini = Ini::parse(&ini_text);
        let scenario = Scenario::from_ini(&ini).ok()?;
        let placements = parse_units(&ini);

        let redalert = MixArchive::parse(&redalert_bytes).ok()?;
        let local = redalert.open_nested("local.mix").ok()?;
        let rules_bytes = local.get("rules.ini")?;
        let rules = Ini::parse(&String::from_utf8_lossy(rules_bytes));

        let mask = passability::build(&scenario);
        let grid = Passability::new(128, 128, mask);
        let mut world = World::new(grid, seed);

        let mut handles = Vec::new();
        for p in &placements {
            let key = p.unit_type.to_ascii_uppercase();
            let Some(unit_stats) = unit_stats(&rules, &key) else {
                continue;
            };
            let cell = CellCoord::from_index(p.cell);
            let max_strength = unit_stats.strength.max(1);
            let health =
                ((p.strength as i32) * max_strength / 256).clamp(0, u16::MAX as i32) as u16;
            let h = world.spawn_unit(
                0,
                p.house,
                cell,
                Facing(p.facing),
                health,
                MoveStats {
                    max_speed: unit_stats.max_speed_leptons(),
                    rot: unit_stats.rot,
                },
            );
            handles.push(h);
        }
        Some((world, handles))
    }

    /// Same as [`load_scg01ea`], but additionally resolves and attaches each
    /// unit's real M4 combat stats (armor, primary weapon, turret) via
    /// `ra_data::combat`, mirroring (again, a rendering-free reimplementation
    /// of, for the same cross-crate-dependency reason) the combat-attach
    /// slice of `ra_client::assets::load_game_from_bytes`. Used by the M4
    /// combat-determinism suite's real-map variants.
    pub fn load_scg01ea_with_combat(seed: u32) -> Option<(World, Vec<Handle>)> {
        use ra_data::combat::resolve_unit_combat;

        let dir = assets_dir();
        if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
            return None;
        }
        let main_bytes = std::fs::read(dir.join("main.mix")).ok()?;
        let redalert_bytes = std::fs::read(dir.join("redalert.mix")).ok()?;

        let main = MixArchive::parse(&main_bytes).ok()?;
        let general = main.open_nested("general.mix").ok()?;
        let ini_bytes = general.get("scg01ea.ini")?;
        let ini_text = String::from_utf8_lossy(ini_bytes);
        let ini = Ini::parse(&ini_text);
        let scenario = Scenario::from_ini(&ini).ok()?;
        let placements = parse_units(&ini);

        let redalert = MixArchive::parse(&redalert_bytes).ok()?;
        let local = redalert.open_nested("local.mix").ok()?;
        let rules_bytes = local.get("rules.ini")?;
        let rules = Ini::parse(&String::from_utf8_lossy(rules_bytes));

        let mask = passability::build(&scenario);
        let grid = Passability::new(128, 128, mask);
        let mut world = World::new(grid, seed);

        let mut handles = Vec::new();
        for p in &placements {
            let key = p.unit_type.to_ascii_uppercase();
            let Some(unit_stats) = unit_stats(&rules, &key) else {
                continue;
            };
            let cell = CellCoord::from_index(p.cell);
            let max_strength = unit_stats.strength.max(1);
            let health =
                ((p.strength as i32) * max_strength / 256).clamp(0, u16::MAX as i32) as u16;
            let h = world.spawn_unit(
                0,
                p.house,
                cell,
                Facing(p.facing),
                health,
                MoveStats {
                    max_speed: unit_stats.max_speed_leptons(),
                    rot: unit_stats.rot,
                },
            );
            if let Some(combat) = resolve_unit_combat(&rules, &key) {
                world.set_unit_combat(
                    h,
                    combat.armor,
                    combat.weapon.as_ref().map(weapon_to_profile),
                    combat.has_turret,
                );
            }
            handles.push(h);
        }
        Some((world, handles))
    }

    /// `ra_data::combat::WeaponDef` -> `ra_sim::WeaponProfile`, field-for-
    /// field. Duplicated from `ra_client::assets::weapon_to_profile` for the
    /// same no-reverse-dependency reason as the rest of this module — kept
    /// tiny and mechanical on purpose so drift from the real conversion is
    /// obvious on read.
    pub fn weapon_to_profile(w: &ra_data::combat::WeaponDef) -> ra_sim::WeaponProfile {
        ra_sim::WeaponProfile {
            damage: w.damage,
            rof: w.rof,
            range: w.range,
            proj_speed: w.proj_speed,
            proj_rot: w.proj_rot,
            invisible: w.invisible,
            instant: w.instant,
            warhead: ra_sim::WarheadProfile {
                spread: w.spread,
                verses: w.verses,
            },
            warhead_ap: w.warhead_ap,
            arcing: w.arcing,
            ballistic_scatter: w.ballistic_scatter,
            homing_scatter: w.homing_scatter,
            min_damage: w.min_damage,
            max_damage: w.max_damage,
        }
    }
}

const REAL_SEED: u32 = 0x5CA1_AB1E;

#[test]
fn real_scg01ea_same_seed_twice_identical_chains() {
    let Some((mut wa, ha)) = real::load_scg01ea(REAL_SEED) else {
        eprintln!(
            "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix/redalert.mix into assets/ to run this test)"
        );
        return;
    };
    let Some((mut wb, hb)) = real::load_scg01ea(REAL_SEED) else {
        unreachable!("second load should succeed since the first did");
    };
    assert_eq!(
        ha.len(),
        4,
        "scg01ea should spawn its 4 real starting units"
    );
    assert_eq!(ha, hb, "identical loads should mint identical handles");

    // Same script against both: move every unit toward a shared destination.
    let dest = CellCoord::new(70, 55);
    let cmds: Vec<Command> = ha
        .iter()
        .map(|&unit| Command::Move {
            unit,
            dest,
            house: if unit == ha[1] { 2 } else { 1 }, // ha[1] is the HARV (house 2); see below
        })
        .collect();

    let chain_a: Vec<u64> = std::iter::once(wa.tick(&cmds))
        .chain((0..59).map(|_| wa.tick(&[])))
        .collect();
    let chain_b: Vec<u64> = std::iter::once(wb.tick(&cmds))
        .chain((0..59).map(|_| wb.tick(&[])))
        .collect();

    assert_eq!(
        chain_a, chain_b,
        "real scg01ea run diverged between two identical loads"
    );
}

/// Regression pin: the first 10 tick hashes of a fixed script (all 4 real
/// starting units ordered to a shared destination cell, seed
/// `0x5CA1_AB1E`) against scg01ea. Derived once by running this suite
/// against the real assets and reading back the computed values (same
/// policy as every other golden hash in this repo — not independently
/// re-verified against a second implementation); a change here means either
/// a real regression in movement/pathing/hashing, or a deliberate change
/// that should update the pin with a comment explaining why.
#[test]
fn real_scg01ea_hash_chain_prefix_golden() {
    let Some((mut world, handles)) = real::load_scg01ea(REAL_SEED) else {
        eprintln!(
            "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix/redalert.mix into assets/ to run this test)"
        );
        return;
    };
    assert_eq!(handles.len(), 4);

    let dest = CellCoord::new(70, 55);
    // house indices from scg01ea's own placement order: 3 Greece (house 1)
    // JEEPs then 1 USSR (house 2) HARV (see `ra_data::scenario` module docs
    // and `ra-client`'s `sim` subcommand output, which reports this exact
    // order for this scenario).
    let houses = [1u8, 2, 1, 1];
    let cmds: Vec<Command> = handles
        .iter()
        .zip(houses)
        .map(|(&unit, house)| Command::Move { unit, dest, house })
        .collect();

    let mut chain = vec![world.tick(&cmds)];
    for _ in 0..9 {
        chain.push(world.tick(&[]));
    }

    // Derived once via `cargo test -p ra-sim --test determinism
    // real_scg01ea_hash_chain_prefix_golden -- --nocapture` against the real
    // assets and copied from the printed output below (see the doc comment
    // above for the derivation policy).
    //
    // **Updated for M4 (combat), re-pinned for M5 (economy).** These values
    // changed because `World::state_hash` grew twice, each a deliberate state
    // extension and not a movement/pathing regression:
    //  - M4 folded in the per-unit combat fields (armor, turret facing, rearm
    //    timer, weapon presence, target) and the bullets arena;
    //  - M5 appends each unit's `is_harvester` byte and folds in the buildings
    //    arena, the houses vector, and the ore field.
    // In this test the scenario's units carry only default combat state, are not
    // harvesters, and there are no buildings/houses/ore, so every M5 addition
    // hashes its empty/default value — the *behavior* is byte-identical to M3.
    // The companion audit `m4_repin_is_justified_movement_unaffected_by_combat_fields`
    // proves the M4/M5 additions were formula-only.
    //
    // **Re-pinned for M7.6 (infantry + occupancy) — a real BEHAVIOR change, not a
    // hash-formula change.** This script moves all four units to the *same* cell
    // (70,55). M7.6 adds unit cell occupancy: vehicles are one-per-cell, and a
    // group ordered to one cell now **disperses** to distinct nearby free cells
    // (`Adjust_Dest` scatter) instead of stacking. So the four JEEPs/HARV settle
    // in different cells and the movement — hence the hash chain — legitimately
    // changes. This is the coordinator-authorised occupancy re-pin (see QUIRKS
    // Q5/Q6); the independent movement-only oracle chain in the companion audit is
    // re-pinned in the same pass. Re-derived deterministically (read back once).
    //
    // **Re-pinned again for M7.7 (P0a head-on tie-break) — another deliberate
    // movement change.** The four units contend as they disperse to one cell; the
    // new slot-order yield (a moving lower-index vehicle makes the higher-index one
    // hold a tick, breaking the symmetric-head-on livelock) alters their movement
    // from tick 5 onward. Ticks 0-4 are byte-identical to the M7.6 pin (the units
    // are still apart), so the change is exactly the contention resolution. Not a
    // hashing/formula change, not a single-unit regression (the synthetic
    // single-unit oracle golden below is unaffected — the tie-break only fires on a
    // vehicle-vehicle collision). QUIRKS Q5.
    let golden: [u64; 10] = [
        0xe6ce_37fb_c98b_9e8d,
        0x8f12_8151_a357_4fa6,
        0xedbc_01c3_1509_1f6b,
        0x443b_4be3_7df3_e8cc,
        0xebf9_01c4_2c38_fa89,
        0x94d8_1da3_c1b9_293a,
        0x8962_63c5_57b9_4f07,
        0x84e0_7a9e_9807_a639,
        0x737b_9cab_ffc6_8e0f,
        0x639b_2266_c1d2_ad01,
    ];
    assert_eq!(
        chain, golden,
        "scg01ea hash-chain prefix changed — either a real determinism regression \
         (movement/pathing/hashing) or a deliberate change; update the pin with a comment"
    );
}

// ---------------------------------------------------------------------
// 4b. Independent single-unit movement oracle (M7.6 re-pin audit).
// ---------------------------------------------------------------------
//
// `real_scg01ea_hash_chain_prefix_golden` above just moved for the second
// time in the project's history — first for the M4/M5 hash-formula growth
// (inert-field, not a behavior change), now for a genuine M7.6 movement
// *behavior* change (occupancy/dispersal). The M4-era companion audit
// (`repin_audit::m4_repin_is_justified_movement_unaffected_by_combat_fields`)
// used to be the project's only *independent* movement oracle protecting
// against silent movement regressions — but M7.6 proved that oracle is not
// immune to *movement* changes: its own 4-units-to-one-cell script disperses
// under the new occupancy rule, so it moved too (re-pinned alongside this
// test, see its doc comment and QUIRKS Q5).
//
// This is a **new** oracle, constructed to never need re-pinning again for
// any *future* multi-unit occupancy/collision/dispersal change, by
// construction: **exactly one unit** exists in this fixture. With no other
// unit on the map, `UnitGrid::vehicle_blocked_for` and `pick_dest`'s
// `dest_ok` can never observe a collision (there is nothing to collide
// with), so `move_units`'s occupancy gate/re-route/dispersal branches are
// dead code for this script by construction — not merely by accident of the
// current destinations chosen. Only a change to the *base* movement math
// itself (waypoint advance, facing rotation, path consumption) can ever
// move this golden. Kept purely synthetic (no real assets) so it always
// runs, and it exercises a re-issued order (stop + new destination) the
// same way the M3/M4-era fixtures do, so it is not trivially only a
// straight-line no-op case.
const SINGLE_UNIT_ORACLE_TICKS: usize = 40;

fn single_unit_oracle_log(unit: Handle) -> Vec<Vec<Command>> {
    let mut log = vec![Vec::new(); SINGLE_UNIT_ORACLE_TICKS];
    log[0].push(Command::Move {
        unit,
        dest: CellCoord::new(100, 90),
        house: 1,
    });
    // Re-issue mid-flight: stop, then send it somewhere else — exercises the
    // same "interrupted order" path the M3/M4 fixtures do, still solo.
    log[15].push(Command::Stop { unit, house: 1 });
    log[16].push(Command::Move {
        unit,
        dest: CellCoord::new(12, 80),
        house: 1,
    });
    log
}

/// A lone unit alone on an open synthetic map: the one-vehicle-per-cell gate,
/// `find_path_avoiding` re-route, and group-dispersal `pick_dest` scatter
/// (M7.6) can never trigger — there is no other unit to collide with. See the
/// module-doc comment above for why this makes the golden below immune to any
/// future occupancy/collision/dispersal change by construction.
#[test]
fn single_unit_no_collision_movement_oracle_golden() {
    let mut world = World::new(Passability::all_passable(), 0x0501_7E17);
    let unit = world.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(32), 500, stats(18, 6));
    let log = single_unit_oracle_log(unit);
    let chain = run(&mut world, &log);

    // Derived once via `cargo test -p ra-sim --test determinism \
    // single_unit_no_collision_movement_oracle_golden -- --nocapture` and
    // copied from the printed chain (same "computed once, read back, and
    // pinned" policy as every other golden hash in this repo).
    let golden: [u64; SINGLE_UNIT_ORACLE_TICKS] = [
        0xfb94_37e6_2da2_4d6d,
        0xc3f3_82fe_5777_725f,
        0xafef_6bf9_3b7b_4e65,
        0x8609_86ca_1722_0d4f,
        0x1d38_2221_1006_b859,
        0x91db_04cf_73bc_0a9c,
        0x3125_c2e9_4566_7a73,
        0x08dc_77dc_6a25_db7d,
        0x6611_63b5_f78f_9552,
        0xc9f2_019f_9a08_5d27,
        0x8974_b56d_0165_c8a0,
        0x80b2_4220_2c58_c0d1,
        0x070a_ca94_0fb6_06e6,
        0x76f9_5f77_8b57_c003,
        0xbd7f_a403_6326_a422,
        0x3fc0_74c7_b83e_2c3e,
        0x2532_aea9_89ee_2c24,
        0x49dc_cfbe_1018_cd7d,
        0xcda1_4d9a_4c0a_0c9a,
        0x64dd_7cae_a528_8a4f,
        0xc62d_4095_9845_a5f8,
        0x8540_712b_d7bc_2cb9,
        0xd06d_4475_80c2_2176,
        0x43ea_fc3b_3017_5be0,
        0xe405_b5d5_63d4_6dbf,
        0xd1bd_71c2_b239_e7db,
        0x7260_305e_0fc1_63a3,
        0x0b91_44b6_b7c9_76a7,
        0x64b5_505d_0a7f_ebcb,
        0x36eb_79fa_3f8c_1923,
        0x1da1_e9b4_c2b1_a4d6,
        0x23c0_f391_654e_8538,
        0x9c0e_d2e5_c1ff_bb61,
        0x9245_f1b3_3af6_26d0,
        0xc53b_8e06_fd03_b313,
        0xf73c_e08e_694a_2ebe,
        0x61db_0022_bc4e_bd4d,
        0x4d4e_e65d_f7cd_7c85,
        0xf24e_b3c9_eddc_8e46,
        0x597d_15db_7aa5_732b,
    ];
    assert_eq!(
        chain, golden,
        "single-unit no-collision movement oracle changed — this must ONLY ever move on a \
         change to the base movement math itself (waypoint advance / facing rotation / path \
         consumption), never on an occupancy, dispersal, or collision-avoidance change (there is \
         only one unit on the map, so those branches never fire here)"
    );
}

/// Same fixture, same-seed-twice — this oracle must itself be deterministic
/// before it's trustworthy as a regression pin.
#[test]
fn single_unit_no_collision_movement_oracle_is_deterministic() {
    let mut wa = World::new(Passability::all_passable(), 0x0501_7E17);
    let ua = wa.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(32), 500, stats(18, 6));
    let chain_a = run(&mut wa, &single_unit_oracle_log(ua));

    let mut wb = World::new(Passability::all_passable(), 0x0501_7E17);
    let ub = wb.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(32), 500, stats(18, 6));
    let chain_b = run(&mut wb, &single_unit_oracle_log(ub));

    assert_eq!(chain_a, chain_b);
}

// ---------------------------------------------------------------------
// 5. M4 combat determinism: unit-vs-unit and force-fire-at-cell battles,
//    synthetic (always-run) + real-map (skip-clean) variants. Complements
//    the movement-only suite above (sections 1-4) and the M4 combat unit
//    tests colocated in `world.rs` (which pin the same RNG asymmetry and
//    hash-chain equality at the *unit test* layer, over `World` directly);
//    this is the integration-layer echo the M4 test plan calls for
//    explicitly, using `ra_sim::world::Command`'s public surface exactly as
//    a real replay/net client would, plus the real-map variants a colocated
//    `#[cfg(test)]` module can't reach (no asset access by this repo's
//    policy for `src`-internal unit tests).
// ---------------------------------------------------------------------

use ra_sim::{Target, WarheadProfile, WeaponProfile};

fn pct5_det(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// 2TNK's 90mm: AP, non-instant, so its shots at a ground cell scatter (draw
/// the sim RNG) but its shots at a live enemy unit are accurate (no draw).
fn ninety_mm_det() -> WeaponProfile {
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
            verses: pct5_det([30, 75, 75, 100, 50]),
        },
        warhead_ap: true,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// A combat-capable `World` fixture: two house-1 tanks and two house-2
/// targets on an open synthetic map, positioned so both an in-range
/// unit-vs-unit fight and an in-range force-fire-at-cell fight are possible
/// from tick 0 (no approach phase needed — that's `firing_fsm.rs`'s job).
struct CombatFixture {
    world: World,
    tank_a: Handle, // house 1, will attack `victim` directly (unit target)
    tank_b: Handle, // house 1, will force-fire at `victim_cell` (Target::Cell)
    victim: Handle, // house 2, `tank_a`'s unit target
    victim_cell: CellCoord,
}

fn build_combat_fixture(seed: u32) -> CombatFixture {
    let mut world = World::new(Passability::all_passable(), seed);
    let tank_a = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats(25, 10));
    world.set_unit_combat(tank_a, 3, Some(ninety_mm_det()), true);
    let tank_b = world.spawn_unit(1, 1, CellCoord::new(30, 10), Facing(0), 400, stats(25, 10));
    world.set_unit_combat(tank_b, 3, Some(ninety_mm_det()), true);

    let victim = world.spawn_unit(2, 2, CellCoord::new(11, 10), Facing(0), 600, stats(20, 8));
    world.set_unit_combat(victim, 3, None, false);
    // Force-fire target cell for `tank_b`: ~4.5 cells away (within 90mm's
    // 4.75-cell range) and far enough that `scatterdist > 0`, so the AP
    // scatter branch genuinely draws the sim RNG every shot (matching
    // `world.rs`'s own `force_fire_at_cell_consumes_sim_rng_when_scattering`
    // unit test's distance choice).
    let victim_cell = CellCoord::new(34, 12);

    CombatFixture {
        world,
        tank_a,
        tank_b,
        victim,
        victim_cell,
    }
}

/// `log[t]` are the commands applied at tick `t`: `tank_a` attacks `victim`
/// (unit target, accurate) from tick 0; `tank_b` force-fires `victim_cell`
/// (ground target, AP scatter) starting a few ticks later; at tick 40,
/// `tank_b` is re-targeted from the cell to the live `victim` unit instead
/// (switching an in-progress force-fire to a unit attack mid-battle, the
/// same "re-issue" shape section 1-4's movement log exercises for `Move`).
const COMBAT_TICKS: usize = 220;

fn combat_command_log(f: &CombatFixture) -> Vec<Vec<Command>> {
    let mut log = vec![Vec::new(); COMBAT_TICKS];
    log[0].push(Command::Attack {
        unit: f.tank_a,
        target: Target::Unit(f.victim),
        house: 1,
    });
    log[5].push(Command::Attack {
        unit: f.tank_b,
        target: Target::Cell(f.victim_cell),
        house: 1,
    });
    log[40].push(Command::Attack {
        unit: f.tank_b,
        target: Target::Unit(f.victim),
        house: 1,
    });
    log
}

fn run_combat_log(world: &mut World, log: &[Vec<Command>]) -> Vec<u64> {
    log.iter().map(|cmds| world.tick(cmds)).collect()
}

#[test]
fn synthetic_combat_battle_same_seed_twice_identical_chains() {
    let fa = build_combat_fixture(0xBEEF_0001);
    let fb = build_combat_fixture(0xBEEF_0001);
    let log_a = combat_command_log(&fa);
    let log_b = combat_command_log(&fb);
    assert_eq!(log_a, log_b);

    let mut wa = fa.world;
    let mut wb = fb.world;
    let chain_a = run_combat_log(&mut wa, &log_a);
    let chain_b = run_combat_log(&mut wb, &log_b);
    assert_eq!(
        chain_a, chain_b,
        "identical seed + combat command log must give an identical hash chain \
         (covers unit-vs-unit fire, AP force-fire scatter, and the mid-battle re-target)"
    );
    assert_eq!(wa.state_hash(), wb.state_hash());
    // Sanity: the battle actually happened (both fire modes did damage), so
    // this is exercising real combat state, not an idle no-op script.
    assert!(
        wa.units.get(fa.victim).is_none_or(|u| u.health < 600),
        "victim should have taken damage from the unit-target attack"
    );
}

#[test]
fn synthetic_combat_battle_command_log_replay_matches_live_run() {
    let live_fixture = build_combat_fixture(0xBEEF_0002);
    let log = combat_command_log(&live_fixture);
    let mut live = live_fixture.world;
    let live_chain = run_combat_log(&mut live, &log);

    // Fresh world, same seed, replaying only the persisted log — the
    // save/replay use case (DESIGN.md §4.4: "a replay is just the command
    // log + initial seed").
    let mut replay = World::new(Passability::all_passable(), 0xBEEF_0002);
    let a = replay.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats(25, 10));
    replay.set_unit_combat(a, 3, Some(ninety_mm_det()), true);
    let b = replay.spawn_unit(1, 1, CellCoord::new(30, 10), Facing(0), 400, stats(25, 10));
    replay.set_unit_combat(b, 3, Some(ninety_mm_det()), true);
    let v = replay.spawn_unit(2, 2, CellCoord::new(11, 10), Facing(0), 600, stats(20, 8));
    replay.set_unit_combat(v, 3, None, false);
    let replay_chain = run_combat_log(&mut replay, &log);

    assert_eq!(
        live_chain, replay_chain,
        "replayed combat log must reproduce the live hash chain exactly"
    );
    assert_eq!(live.state_hash(), replay.state_hash());
}

// ---------------------------------------------------------------------
// 6. RNG-consumption asymmetry, pinned as a determinism-critical regression
// (per the M4 task brief: "assert the RNG seed ADVANCES on AP force-fire at
// a cell (scatter draw) and does NOT advance on accurate unit-target shots
// — pin this asymmetry, it's the original's behavior and a prime desync
// trap"). `world.rs` unit-tests this directly against `World`; this
// integration-level echo re-derives it independently by running the *whole*
// scripted battle above and inspecting `rng_seed()` at specific ticks, plus
// adds a real-map variant.
// ---------------------------------------------------------------------

#[test]
fn regression_accurate_unit_target_shot_never_advances_rng_seed() {
    // Isolate `tank_a`'s accurate unit-target attack only (no force-fire in
    // this script), so any seed change can only come from the accurate-shot
    // path — a stricter isolation than the combined battle script above.
    let mut w = World::new(Passability::all_passable(), 0x1357_ACE1);
    let a = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats(25, 10));
    w.set_unit_combat(a, 3, Some(ninety_mm_det()), true);
    let victim = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats(20, 8));
    w.set_unit_combat(victim, 3, None, false);

    let seed0 = w.rng_seed();
    w.tick(&[Command::Attack {
        unit: a,
        target: Target::Unit(victim),
        house: 1,
    }]);
    for _ in 0..150 {
        w.tick(&[]);
    }
    assert!(
        w.units.get(victim).unwrap().health < 600,
        "sanity: the accurate attack should have landed several shots"
    );
    assert_eq!(
        seed0,
        w.rng_seed(),
        "an all-accurate unit-vs-unit battle must never advance the sim RNG seed"
    );
}

#[test]
fn regression_ap_force_fire_scatter_always_advances_rng_seed() {
    let mut w = World::new(Passability::all_passable(), 0x1357_ACE2);
    let a = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats(25, 10));
    w.set_unit_combat(a, 3, Some(ninety_mm_det()), true);
    let cell = CellCoord::new(14, 12); // ~4.5 cells: in range, scatterdist > 0

    let seed0 = w.rng_seed();
    w.tick(&[Command::Attack {
        unit: a,
        target: Target::Cell(cell),
        house: 1,
    }]);
    let mut seeds_seen = vec![seed0];
    for _ in 0..150 {
        w.tick(&[]);
        seeds_seen.push(w.rng_seed());
    }
    assert_ne!(
        seed0,
        w.rng_seed(),
        "an AP force-fire-at-cell battle must advance the sim RNG seed"
    );
    // Stronger than just "changed once": since 90mm's ROF=50 gives 3 shots
    // in 150 ticks and every AP-at-cell shot scatters, the seed should
    // change at (at least) 3 distinct points, not just once coincidentally.
    let distinct = seeds_seen.windows(2).filter(|w| w[0] != w[1]).count();
    assert!(
        distinct >= 3,
        "expected the seed to change on every one of several scattering shots, saw {distinct} changes"
    );
}

/// Real-map variant: one of scg01ea's real JEEPs (SA/M60mg, not AP — see
/// below) attacking the scenario's real HARV must never advance the sim RNG,
/// exercised over the real passability grid + real spawn placements (not
/// just an all-passable synthetic map). Skips cleanly without assets.
///
/// M60mg is SA, not AP, so this is actually a *stronger* no-scatter case
/// than the synthetic AP-vs-unit test above (SA never scatters regardless of
/// target kind — `warhead_ap` gates the scatter branch in `fire()` — so this
/// also cross-checks that a non-AP weapon is unconditionally accurate, not
/// just "accurate when aimed at a unit").
#[test]
fn real_scg01ea_non_ap_weapon_never_advances_rng_seed() {
    let Some((mut world, handles)) = real::load_scg01ea_with_combat(REAL_SEED) else {
        eprintln!(
            "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix/redalert.mix into assets/ to run this test)"
        );
        return;
    };
    assert_eq!(handles.len(), 4);
    // scg01ea's placement order: 3 Greece (house 1) JEEPs then 1 USSR
    // (house 2) HARV (see `real_scg01ea_hash_chain_prefix_golden` above).
    let jeep = handles[0];
    let harv = handles[3];
    assert!(
        world.units.get(jeep).unwrap().weapon.is_some(),
        "scg01ea's JEEP should have resolved a weapon (M60mg) from real rules.ini"
    );

    let seed0 = world.rng_seed();
    world.tick(&[Command::Attack {
        unit: jeep,
        target: Target::Unit(harv),
        house: 1,
    }]);
    for _ in 0..100 {
        world.tick(&[]);
    }
    assert_eq!(
        seed0,
        world.rng_seed(),
        "a non-AP (SA) weapon must never advance the sim RNG, even over the real map"
    );
}

/// Real-map variant of the scatter side: attach a synthetic AP weapon (real
/// weapons in scg01ea's default loadout are all SA/M60mg, which never
/// scatters — see the test above) to one of the real JEEP spawns, force-fire
/// a nearby real-map cell, and confirm the seed advances — proving the
/// scatter path also fires correctly over real passability/spawn data, not
/// only the synthetic all-passable grid. Skips cleanly without assets.
#[test]
fn real_scg01ea_ap_force_fire_scatter_advances_rng_seed() {
    let Some((mut world, handles)) = real::load_scg01ea_with_combat(REAL_SEED) else {
        eprintln!(
            "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix/redalert.mix into assets/ to run this test)"
        );
        return;
    };
    let jeep = handles[0];
    world.set_unit_combat(jeep, 3, Some(ninety_mm_det()), true);
    let jeep_cell = world.units.get(jeep).unwrap().cell();
    // A force-fire cell a few cells away, clamped on-map, aiming to land
    // within 90mm's range with a nonzero scatter distance.
    let target_cell = CellCoord::new((jeep_cell.x + 4).min(126), (jeep_cell.y + 2).min(126));

    let seed0 = world.rng_seed();
    world.tick(&[Command::Attack {
        unit: jeep,
        target: Target::Cell(target_cell),
        house: world.units.get(jeep).unwrap().house,
    }]);
    let mut advanced = false;
    for _ in 0..80 {
        world.tick(&[]);
        if world.rng_seed() != seed0 {
            advanced = true;
            break;
        }
    }
    assert!(
        advanced,
        "AP force-fire at a real-map cell never drew the sim RNG (no scatter observed)"
    );
}

// ---------------------------------------------------------------------
// 7. M3 golden re-pin audit. `real_scg01ea_hash_chain_prefix_golden` above
// was re-pinned for M4 with the doc-comment claim "behavior is identical to
// M3; only the hashed field set grew". This section *re-derives* that claim
// from first principles instead of trusting the comment:
//
// (a) `run_combat` (`world.rs`) is a no-op for every unit in that golden's
//     script: it decrements `arm` only `if arm > 0` (starts and stays 0
//     here), then immediately `continue`s unless a unit has **both** a
//     `target` and a `weapon` — and `real::load_scg01ea` (used by that
//     golden, as opposed to `load_scg01ea_with_combat` used by section 5/6
//     above) never calls `set_unit_combat`, so every unit's `weapon` stays
//     `None` for the whole run. A no-op system cannot perturb movement.
// (b) The new hashed fields (`armor`, `has_turret`, `turret_facing`, `arm`,
//     `weapon` presence, `target`) are therefore constant — `0`, `false`,
//     equal to spawn `facing`, `0`, `0` (absent), `0` (absent) — for every
//     unit at every tick of that script. This test asserts that directly
//     (not just by code-reading) at every tick, over every unit, rather than
//     taking it on faith.
// (c) Independent evidence, not just "the combat fields don't move the
//     needle": this test pins its own hash chain over *only* the pre-M4
//     field set (type_id/house/coord/facing/health/max_health/stats/path/
//     dest — reimplemented here, not by calling `Unit::hash_into`), so a
//     future movement/pathing regression is caught by a mechanism entirely
//     independent of `state_hash`'s formula — exactly the failure mode a
//     "hash formula changed" re-pin could otherwise hide.
// ---------------------------------------------------------------------

mod repin_audit {
    use super::*;
    use ra_sim::hash::Fnv1a;

    /// Hash exactly the M3-era mutable field set of one unit — a
    /// from-scratch reimplementation (not a call into `Unit::hash_into`,
    /// which also hashes the M4 combat fields this audit is isolating away
    /// from) so agreement with the *movement* portion of the real hash is a
    /// genuine cross-check, not a tautology.
    fn hash_unit_m3_fields(h: &mut Fnv1a, u: &ra_sim::Unit) {
        h.write_u32(u.type_id);
        h.write_u8(u.house);
        h.write_i32(u.coord.x.0);
        h.write_i32(u.coord.y.0);
        h.write_u8(u.facing.0);
        h.write_u16(u.health);
        h.write_u16(u.max_health);
        h.write_i32(u.stats.max_speed);
        h.write_u8(u.stats.rot);
        h.write_u32(u.path.len() as u32);
        for cell in &u.path {
            h.write_i32(cell.x);
            h.write_i32(cell.y);
        }
        match u.dest {
            Some(c) => {
                h.write_u8(1);
                h.write_i32(c.x);
                h.write_i32(c.y);
            }
            None => h.write_u8(0),
        }
    }

    /// The M3-shaped whole-world hash: same shape as `World::state_hash`
    /// minus the bullets arena (didn't exist pre-M4) and minus each unit's
    /// M4 combat tail.
    fn state_hash_m3_shaped(world: &World) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u32(world.tick_count());
        h.write_u32(world.rng_seed());
        h.write_u32(world.units.len());
        for (handle, unit) in world.units.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            hash_unit_m3_fields(&mut h, unit);
        }
        h.finish()
    }

    #[test]
    fn m4_repin_is_justified_movement_unaffected_by_combat_fields() {
        let Some((mut world, handles)) = real::load_scg01ea(REAL_SEED) else {
            eprintln!(
                "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix/redalert.mix into assets/ to run this test)"
            );
            return;
        };
        assert_eq!(handles.len(), 4);

        // `turret_facing` is initialised equal to spawn `facing`
        // (`Unit::new`) but — since these units are never armed/turreted —
        // nothing in `run_combat` ever touches it again (that system
        // `continue`s immediately for a weaponless unit); it is `facing`
        // that changes as the unit turns to move. So the invariant these
        // units actually hold is "`turret_facing` stays pinned at its
        // *spawn* value", not "`turret_facing` tracks the current `facing`"
        // — captured here before the first tick so the per-tick loop below
        // can check the real invariant.
        let spawn_turret_facing: std::collections::HashMap<u32, ra_sim::coords::Facing> = world
            .units
            .iter()
            .map(|(h, u)| (h.index, u.turret_facing))
            .collect();

        // Exactly the golden test's script: same seed, same destination, same
        // per-unit house assignment, same tick count.
        let dest = CellCoord::new(70, 55);
        let houses = [1u8, 2, 1, 1];
        let cmds: Vec<Command> = handles
            .iter()
            .zip(houses)
            .map(|(&unit, house)| Command::Move { unit, dest, house })
            .collect();

        let mut m3_shaped_chain = Vec::new();
        let mut prod_chain = Vec::new();
        for t in 0..10 {
            let cmds_this_tick: &[Command] = if t == 0 { &cmds } else { &[] };
            world.tick(cmds_this_tick);

            // (b) Directly assert every M4 combat field is at its inert
            // default for every unit, every tick — the claim this whole
            // audit rests on, checked at runtime rather than by inspection.
            for (h, unit) in world.units.iter() {
                assert_eq!(unit.armor, 0, "tick {t}: armor should be untouched (0)");
                assert!(!unit.has_turret, "tick {t}: has_turret should stay false");
                assert_eq!(
                    unit.turret_facing, spawn_turret_facing[&h.index],
                    "tick {t}: an untouched (unarmed) unit's turret_facing must stay pinned at spawn"
                );
                assert_eq!(
                    unit.arm, 0,
                    "tick {t}: arm should never leave 0 (never fires)"
                );
                assert!(unit.weapon.is_none(), "tick {t}: weapon should stay unset");
                assert!(unit.target.is_none(), "tick {t}: target should stay unset");
            }

            m3_shaped_chain.push(state_hash_m3_shaped(&world));
            prod_chain.push(world.state_hash());
        }

        // (c) The independent M3-shaped chain, pinned as its own golden —
        // derived once against the real assets during this audit, same
        // "computed once, read back, and pinned" policy as every other
        // golden hash in this repo (see the module docs on that policy).
        // (c) The independent M3-shaped chain, pinned as its own golden —
        // derived once against the real assets during this audit (same
        // "computed once, read back, and pinned" policy as every other
        // golden hash in this repo — see `real_scg01ea_hash_chain_prefix_golden`'s
        // doc comment above).
        // Re-pinned for M7.6: the four-units-to-one-cell script now disperses
        // (unit cell occupancy — one vehicle per cell), a real movement change, so
        // this independent movement-only chain legitimately moves too. It still
        // serves its purpose — proving the M4/M5 combat/economy hash-formula
        // additions do not perturb movement — now baselined on the M7.6 movement
        // behavior. (Coordinator-authorised occupancy re-pin; QUIRKS Q5/Q6.)
        //
        // Re-pinned again for M7.7 (P0a head-on tie-break): the four units
        // contend while dispersing to one cell, and the new slot-order yield
        // changes their movement from tick 5 onward (ticks 0-4 are byte-identical
        // — the units are still apart). This is the *same* deliberate movement
        // change as the occupancy re-pin, now including the tie-break. The
        // per-tick combat-inertness asserts above still hold (armor/has_turret/
        // arm/weapon/target untouched), so the audit's conclusion — combat fields
        // do not perturb movement — is unchanged; only the movement baseline
        // moved. The synthetic single-unit oracle golden is unaffected (the
        // tie-break only fires on a vehicle-vehicle collision). QUIRKS Q5.
        let m3_shaped_golden: [u64; 10] = [
            0xcae7_fe64_e3cd_ae2d,
            0x1928_72a0_34f4_4186,
            0x6653_655a_b6a8_268b,
            0x8f06_f304_54b6_feec,
            0x5989_8d2f_e58b_6829,
            0x4faf_4f67_6f6a_219a,
            0x8119_19c5_e2c7_ef07,
            0x4b0a_f5b2_9d5d_8d19,
            0x6c2c_6edc_260e_efcf,
            0xd103_4520_70ca_aca1,
        ];
        assert_eq!(
            m3_shaped_chain, m3_shaped_golden,
            "movement-only (M3-field-shaped) hash chain changed — expected to move only on a \
             deliberate movement change (M7.6 occupancy/dispersal); otherwise a real regression"
        );

        // And finally: the production hash chain must equal the golden
        // pinned by `real_scg01ea_hash_chain_prefix_golden` above — tying
        // this audit's conclusion back to the actual re-pinned test.
        assert_eq!(
            prod_chain,
            [
                0xe6ce_37fb_c98b_9e8d,
                0x8f12_8151_a357_4fa6,
                0xedbc_01c3_1509_1f6b,
                0x443b_4be3_7df3_e8cc,
                0xebf9_01c4_2c38_fa89,
                0x94d8_1da3_c1b9_293a,
                0x8962_63c5_57b9_4f07,
                0x84e0_7a9e_9807_a639,
                0x737b_9cab_ffc6_8e0f,
                0x639b_2266_c1d2_ad01,
            ],
            "this audit's script diverged from real_scg01ea_hash_chain_prefix_golden's script"
        );
    }
}
