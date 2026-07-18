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
    let golden: [u64; 10] = [
        0x71ff_e80a_6812_e5c7,
        0x109c_a760_d46d_51f4,
        0xffd7_165b_7d9e_58fd,
        0x7b28_06d6_7c3d_818a,
        0x5dc4_c299_1695_dfeb,
        0x441f_e883_6728_f380,
        0x8b0d_9169_0892_8fea,
        0x54c0_f314_6a8a_cb94,
        0x56ef_3120_22fc_3aa2,
        0xec88_27fc_6168_a408,
    ];
    assert_eq!(
        chain, golden,
        "scg01ea hash-chain prefix changed — either a real determinism regression \
         (movement/pathing/hashing) or a deliberate change; update the pin with a comment"
    );
}
