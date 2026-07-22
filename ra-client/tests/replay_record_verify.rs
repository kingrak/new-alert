//! M7.23 P4: the replay record → verify determinism proof, plus post-mortem
//! dump, recording-failure degradation, and a revert-drill — all over a real
//! AI skirmish (skipped when the archives are absent, the established pattern).
//!
//! The load-bearing claim (P1): in single player the AI's commands never cross
//! the transport — `run_ai` is a sim system drawing the `World`-owned seeded
//! RNG — so recording the **player** command stream + seed and re-simulating
//! reproduces the whole game, AI included. These tests assert that empirically:
//! every recorded hash record must match on a fresh re-simulation.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;

use ra_client::assets::{self, SkirmishGame};
use ra_client::replay::ReplayRecorder;
use ra_net::{
    EndReason, ReplayHeader, ReplayReader, ReplayRecord, ReplaySeat, ReplayTransport, TickBundle,
};
use ra_sim::Difficulty;

const SCENARIO: &str = "scm01ea.ini";
const CREDITS: i32 = 8000;
/// Enough ticks for the AI to deploy, build a base, and produce units — so the
/// re-sim is exercising real AI evolution, not an empty world.
const TICKS: u32 = 900;

/// A unique scratch path so parallel test binaries never collide.
fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ra-replay-test-{}-{name}-{nanos}",
        std::process::id()
    ));
    p
}

fn load_skirmish() -> SkirmishGame {
    assets::load_skirmish_from_dir(
        &support::assets_dir(),
        SCENARIO,
        CREDITS,
        Difficulty::Normal,
    )
    .expect("load skirmish")
}

fn header_for(game: &SkirmishGame) -> ReplayHeader {
    let w = game.core.world();
    ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: ra_net::wire::GAME_VERSION,
        protocol_version: ra_net::wire::PROTOCOL_VERSION,
        scenario: SCENARIO.to_string(),
        seed: w.rng_seed(),
        difficulty: 1,
        credits: CREDITS,
        catalog_hash: w.catalog().content_hash(),
        start_millis: 42,
        seats: vec![ReplaySeat {
            seat: game.player_house,
            house: game.player_house,
            color: game.player_house,
        }],
    }
}

/// Drive a scripted skirmish for `TICKS` ticks with the recorder installed,
/// injecting a couple of real player commands so the stream carries tick records
/// as well as the hash chain. Returns the recorder's file path.
fn record_scripted_game(path: PathBuf) -> PathBuf {
    let mut game = load_skirmish();
    let player_house = game.player_house;
    let header = header_for(&game);
    game.core
        .install_recorder(ReplayRecorder::create(path.clone(), &header));

    // Deploy the player's MCV (the one starting unit) on tick ~2 so a Deploy
    // command lands in the recorded stream (non-vacuous command recording).
    let mcv = game
        .core
        .world()
        .units
        .iter()
        .find(|(_, u)| u.house == player_house)
        .map(|(h, _)| h);

    for t in 0..TICKS {
        if t == 2 {
            if let Some(unit) = mcv {
                game.core.inject_command(ra_sim::Command::Deploy {
                    unit,
                    house: player_house,
                });
            }
        }
        game.core.update(67); // ~1 tick at 15 Hz
    }
    game.core.finish_recording(EndReason::Quit);
    path
}

/// Parse a replay file into (header, tick-bundle map, hash-record map, final).
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

/// Re-simulate a fresh world from the stream, checking every hash record.
/// `Ok(n)` = n records matched; `Err(tick)` = first divergent tick.
fn resim_verify(
    bundles: &BTreeMap<u32, TickBundle>,
    hashes: &BTreeMap<u32, u64>,
    final_tick: u32,
) -> Result<usize, u32> {
    let mut game = load_skirmish();
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

#[test]
fn record_then_verify_matches_ai_game() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    let path = record_scripted_game(scratch("verify"));
    let (header, bundles, hashes, final_tick) = parse(&path);

    // Non-vacuity: the stream must carry a real hash chain and at least the
    // injected Deploy command, and the game must have progressed.
    assert_eq!(header.seed, load_skirmish().core.world().rng_seed());
    assert!(
        hashes.len() >= 10,
        "expected a substantial hash chain, got {}",
        hashes.len()
    );
    assert!(
        !bundles.is_empty(),
        "expected at least one recorded command tick (the injected Deploy)"
    );
    assert!(
        final_tick >= TICKS - 30,
        "final tick {final_tick} too small"
    );

    match resim_verify(&bundles, &hashes, final_tick) {
        Ok(n) => assert_eq!(
            n,
            hashes.len(),
            "all {} recorded hash records must match on re-sim",
            hashes.len()
        ),
        Err(t) => panic!("replay diverged at tick {t} — determinism contract violated"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn replay_transport_replays_the_same_chain() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    // Prove the recorded stream drives a `ReplayTransport` to the same hashes —
    // the "a replay IS a CommandTransport" contract (SERVER-DESIGN §8).
    let path = record_scripted_game(scratch("transport"));
    let (header, _b, hashes, final_tick) = parse(&path);

    let bytes = std::fs::read(&path).unwrap();
    let (h2, reader) = ReplayReader::open(&bytes).unwrap();
    let mut tp = ReplayTransport::from_reader(&h2, reader).unwrap();
    assert_eq!(h2.seed, header.seed);

    let mut game = load_skirmish();
    use ra_net::CommandTransport;
    use ra_net::PollResult;
    let mut checked = 0;
    for t in 0..=final_tick {
        let cmds = match tp.poll(t) {
            PollResult::Ready(b) => b.flatten(),
            other => panic!("ReplayTransport must always be Ready, got {other:?}"),
        };
        let hash = game.core.world_mut().tick(&cmds);
        if let Some(&expected) = hashes.get(&t) {
            assert_eq!(
                hash, expected,
                "ReplayTransport re-sim diverged at tick {t}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, hashes.len());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn replay_dump_reports_a_known_end_state() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    let path = record_scripted_game(scratch("dump"));
    let (_header, bundles, _hashes, final_tick) = parse(&path);

    // Re-simulate to the end and inspect the world (what replay-dump renders).
    let mut game = load_skirmish();
    for t in 0..final_tick {
        let cmds = bundles.get(&t).map(|b| b.flatten()).unwrap_or_default();
        game.core.world_mut().tick(&cmds);
    }
    let w = game.core.world();
    let player_house = game.player_house;
    let ai_house = game.ai_house;

    // The player deployed its MCV → a construction yard exists.
    let player_has_cy = w
        .buildings
        .iter()
        .any(|(_, b)| b.house == player_house && b.is_construction_yard);
    assert!(
        player_has_cy,
        "player should have deployed to a construction yard"
    );

    // The AI acted: it owns buildings and/or units by tick 900.
    let ai_objects = w
        .buildings
        .iter()
        .filter(|(_, b)| b.house == ai_house)
        .count()
        + w.units.iter().filter(|(_, u)| u.house == ai_house).count();
    assert!(
        ai_objects > 1,
        "AI should have built/produced (got {ai_objects})"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn recording_failure_degrades_gracefully() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    // Point the recorder at an impossible path: a file used as a directory. The
    // recorder must latch disabled, and the game must run unaffected.
    let blocker = scratch("blocker-file");
    std::fs::write(&blocker, b"x").expect("write blocker file");
    let bad_path = blocker.join("nested").join("game.rarp");

    let mut game = load_skirmish();
    let header = header_for(&game);
    let rec = ReplayRecorder::create(bad_path.clone(), &header);
    assert!(
        !rec.is_recording(),
        "recorder must disable itself when the path is unwritable"
    );
    game.core.install_recorder(rec);

    let before = game.core.world().tick_count();
    for _ in 0..30 {
        game.core.update(67);
    }
    let after = game.core.world().tick_count();
    assert!(
        after > before,
        "the game must advance despite recording failure"
    );
    game.core.finish_recording(EndReason::Quit);
    assert!(!bad_path.exists(), "no file should have been created");

    let _ = std::fs::remove_file(&blocker);
}

#[test]
fn revert_drill_broken_hash_record_fails_verify() {
    if !support::real_assets_available() {
        eprintln!("skip: real assets not present");
        return;
    }
    // Record a good game; a clean verify must pass.
    let path = record_scripted_game(scratch("revert"));
    let (_h, bundles, mut hashes, final_tick) = parse(&path);
    assert!(resim_verify(&bundles, &hashes, final_tick).is_ok());

    // Corrupt one recorded hash (the revert-drill: break the hash chain) and the
    // same re-sim must now detect the divergence.
    let &(some_tick, some_hash) = &hashes.iter().next().map(|(t, h)| (*t, *h)).unwrap();
    hashes.insert(some_tick, some_hash ^ 0xFFFF_FFFF);
    match resim_verify(&bundles, &hashes, final_tick) {
        Ok(_) => panic!("verify must fail once a hash record is corrupted"),
        Err(t) => assert_eq!(t, some_tick, "should flag the corrupted tick first"),
    }

    let _ = std::fs::remove_file(&path);
}
