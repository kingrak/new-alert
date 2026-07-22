//! M7.23 replay audit follow-up on `replay_record_verify.rs`.
//!
//! 1. The determinism proof re-run on a FRESH scenario + difficulty (the
//!    original test only ever exercises `scm01ea.ini` / `Difficulty::Normal`)
//!    — proves the record->verify contract isn't accidentally tied to that
//!    one scenario's specific seed/AI behaviour.
//! 2. The critical adversarial variant the original suite is missing
//!    entirely: record a game, TAMPER with one player command already in the
//!    file (valid encoding, different cell — not corruption, a *plausible*
//!    wrong command), and prove replay-verify FAILS at or before the next
//!    15-tick hash checkpoint. `revert_drill_broken_hash_record_fails_verify`
//!    in the original file corrupts a *hash record*; nothing there proves the
//!    checkpoints actually bind the *command stream* rather than just the
//!    seed (a bug that replayed the wrong commands but coincidentally landed
//!    on the same declared hash would sail through the existing suite).

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use ra_client::assets::{self, SkirmishGame};
use ra_client::replay::ReplayRecorder;
use ra_net::{
    EndReason, ReplayHeader, ReplayReader, ReplayRecord, ReplaySeat, TickBundle, HASH_INTERVAL,
};
use ra_sim::coords::CellCoord;
use ra_sim::{Command, Difficulty};

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ra-replay-adversarial-{}-{name}-{nanos}",
        std::process::id()
    ));
    p
}

fn load(scenario: &str, credits: i32, difficulty: Difficulty) -> SkirmishGame {
    assets::load_skirmish_from_dir(&support::assets_dir(), scenario, credits, difficulty)
        .expect("load skirmish")
}

fn header_for(game: &SkirmishGame, scenario: &str, credits: i32, difficulty: u8) -> ReplayHeader {
    let w = game.core.world();
    ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: ra_net::wire::GAME_VERSION,
        protocol_version: ra_net::wire::PROTOCOL_VERSION,
        scenario: scenario.to_string(),
        seed: w.rng_seed(),
        difficulty,
        credits,
        catalog_hash: w.catalog().content_hash(),
        start_millis: 42,
        seats: vec![ReplaySeat {
            seat: game.player_house,
            house: game.player_house,
            color: game.player_house,
        }],
    }
}

fn record_scripted_game(
    path: PathBuf,
    scenario: &str,
    credits: i32,
    difficulty: Difficulty,
    difficulty_byte: u8,
    ticks: u32,
) -> PathBuf {
    let mut game = load(scenario, credits, difficulty);
    let player_house = game.player_house;
    let header = header_for(&game, scenario, credits, difficulty_byte);
    game.core
        .install_recorder(ReplayRecorder::create(path.clone(), &header));

    // Deliberately NOT deployed: `Command::Deploy` consumes the MCV's handle
    // (it becomes a construction yard, a different arena slot), which would
    // silently no-op every later `Move` aimed at this handle — exactly the
    // kind of dead-command trap that makes a tamper test vacuous. Keeping the
    // MCV mobile for the whole run keeps every injected `Move` live.
    let mcv = game
        .core
        .world()
        .units
        .iter()
        .find(|(_, u)| u.house == player_house)
        .map(|(h, _)| h);

    for t in 0..ticks {
        // A handful of Move orders so the tamper test has more than one
        // command record to choose from, and the fresh-scenario determinism
        // test has a non-vacuous command stream (not just AI-driven hashes).
        if t == 20 || t == 40 || t == 60 {
            if let Some(unit) = mcv {
                game.core.inject_command(Command::Move {
                    unit,
                    dest: CellCoord::new(30 + t as i32 % 5, 30),
                    house: player_house,
                });
            }
        }
        game.core.update(67);
    }
    game.core.finish_recording(EndReason::Quit);
    path
}

fn parse(
    path: &PathBuf,
) -> (
    ReplayHeader,
    BTreeMap<u32, TickBundle>,
    BTreeMap<u32, u64>,
    u32,
) {
    let bytes = std::fs::read(path).expect("read replay");
    let (header, reader) = ReplayReader::open(&bytes).expect("open replay");
    let records = reader.collect_records().expect("records");
    let mut bundles = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut final_tick = 0;
    for rec in records {
        match rec {
            ReplayRecord::Tick { tick, bundle } => {
                final_tick = final_tick.max(tick);
                bundles.insert(tick, bundle);
            }
            ReplayRecord::Hash { tick, hash } => {
                final_tick = final_tick.max(tick);
                hashes.insert(tick, hash);
            }
            ReplayRecord::End { final_tick: ft, .. } => final_tick = final_tick.max(ft),
        }
    }
    (header, bundles, hashes, final_tick)
}

fn resim_verify(
    scenario: &str,
    credits: i32,
    difficulty: Difficulty,
    bundles: &BTreeMap<u32, TickBundle>,
    hashes: &BTreeMap<u32, u64>,
    final_tick: u32,
) -> Result<usize, u32> {
    let mut game = load(scenario, credits, difficulty);
    let mut checked = 0;
    for t in 0..=final_tick {
        let cmds = bundles.get(&t).map(|b| b.flatten()).unwrap_or_default();
        let hash = game.core.world_mut().tick(&cmds);
        if let Some(&expected) = hashes.get(&t) {
            if hash != expected {
                return Err(t);
            }
            checked += 1;
        }
    }
    Ok(checked)
}

/// The determinism proof, re-run on a scenario/difficulty combination the
/// M7.23 agent never used (`scm02ea.ini` / `Difficulty::Hard`, vs. the
/// original suite's `scm01ea.ini` / `Difficulty::Normal`) — different map,
/// different AI pacing, different derived seed.
#[test]
fn record_then_verify_matches_ai_game_on_a_fresh_scenario_and_difficulty() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    const SCENARIO: &str = "scm02ea.ini";
    const CREDITS: i32 = 5000;
    const TICKS: u32 = 900;
    let path = record_scripted_game(
        scratch("fresh"),
        SCENARIO,
        CREDITS,
        Difficulty::Hard,
        2,
        TICKS,
    );
    let (header, bundles, hashes, final_tick) = parse(&path);

    assert_eq!(
        header.seed,
        load(SCENARIO, CREDITS, Difficulty::Hard)
            .core
            .world()
            .rng_seed(),
    );
    assert!(
        hashes.len() >= 10,
        "expected a substantial hash chain, got {}",
        hashes.len()
    );
    assert!(
        final_tick >= TICKS - 30,
        "final tick {final_tick} too small"
    );

    match resim_verify(
        SCENARIO,
        CREDITS,
        Difficulty::Hard,
        &bundles,
        &hashes,
        final_tick,
    ) {
        Ok(n) => assert_eq!(
            n,
            hashes.len(),
            "all {} recorded hash records must match on re-sim (fresh scenario/difficulty)",
            hashes.len()
        ),
        Err(t) => panic!(
            "replay diverged at tick {t} on scenario={SCENARIO} difficulty=Hard — \
             determinism contract violated outside the one scenario the agent tested"
        ),
    }
    let _ = std::fs::remove_file(&path);
}

/// The critical adversarial variant: record a game, then tamper with ONE
/// already-recorded player command — same valid wire encoding, a different
/// (but still in-bounds) destination cell — and prove replay-verify fails at
/// or before the next 15-tick hash checkpoint. This is the proof that
/// checkpoints bind the actual command stream, not merely the seed: a replay
/// reader that only checked "does the seed match" would sail through this
/// tamper undetected.
#[test]
fn tampered_player_command_fails_verify_at_or_before_the_next_checkpoint() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    const SCENARIO: &str = "scm01ea.ini";
    const CREDITS: i32 = 8000;
    const TICKS: u32 = 200;
    let path = record_scripted_game(
        scratch("tamper"),
        SCENARIO,
        CREDITS,
        Difficulty::Normal,
        1,
        TICKS,
    );
    let (_header, mut bundles, hashes, final_tick) = parse(&path);

    // Sanity: the clean (untampered) recording verifies first, so any failure
    // below is caused by the tamper, not a broken harness.
    assert!(
        resim_verify(
            SCENARIO,
            CREDITS,
            Difficulty::Normal,
            &bundles,
            &hashes,
            final_tick
        )
        .is_ok(),
        "the untampered recording must verify cleanly before the drill means anything"
    );

    // Pick the earliest recorded command tick and flip its Move destination to
    // a different, still in-bounds cell — valid encoding, wrong game.
    let tamper_tick = *bundles
        .keys()
        .find(|&&t| {
            bundles[&t]
                .flatten()
                .iter()
                .any(|c| matches!(c, Command::Move { .. }))
        })
        .expect("the scripted game must have recorded at least one Move command");
    {
        let bundle = bundles.get_mut(&tamper_tick).unwrap();
        for (_, cmds) in bundle.seats.iter_mut() {
            for c in cmds.iter_mut() {
                if let Command::Move { dest, .. } = c {
                    let tampered = CellCoord::new(dest.x + 7, dest.y + 7);
                    assert_ne!(
                        tampered, *dest,
                        "sanity: tamper must actually change the cell"
                    );
                    *dest = tampered;
                }
            }
        }
    }

    let next_checkpoint = tamper_tick.div_ceil(HASH_INTERVAL) * HASH_INTERVAL;
    match resim_verify(
        SCENARIO,
        CREDITS,
        Difficulty::Normal,
        &bundles,
        &hashes,
        final_tick,
    ) {
        Ok(_) => panic!(
            "verify must FAIL once a player command is tampered (tick {tamper_tick}) — \
             checkpoints are not actually binding the command stream"
        ),
        Err(t) => assert!(
            t <= next_checkpoint,
            "verify diverged at tick {t}, but the tamper was at tick {tamper_tick} and the \
             next checkpoint is {next_checkpoint} — detection is later than it should be"
        ),
    }
    let _ = std::fs::remove_file(&path);
}
