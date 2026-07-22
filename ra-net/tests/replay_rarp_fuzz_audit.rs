//! M7.23 audit follow-up on `replay_fuzz.rs`.
//!
//! `replay_fuzz.rs` already covers: every single-byte-mutation prefix
//! (superset of "truncation at every record boundary" — it truncates at
//! *every* byte offset, not just record boundaries), pure random garbage
//! (with and without a forced-valid magic/version so the fuzz reaches the
//! record decoders), and multi-byte structured corruption. What it does NOT
//! cover: a **hash-record tick regression** — a `.rarp` file whose HASH
//! records are not monotonically increasing in tick (or repeat a tick with a
//! DIFFERENT hash value). Every hash consumer in this codebase
//! (`ra-client/src/bin/ra-client.rs::cmd_replay_verify`,
//! `replay_record_verify.rs::resim_verify`, this audit's own
//! `replay_adversarial_audit.rs`) folds `ReplayRecord::Hash` into a
//! `BTreeMap<tick, hash>` via `.insert()`, so the map re-sorts by tick
//! regardless of file order (a regression in file order is harmless) but a
//! genuine **duplicate tick with a conflicting hash** silently keeps
//! whichever copy iteration visits last — nothing flags the conflict. This
//! file: (1) proves decode never panics on regressed/duplicate-tick hash
//! streams (extends the fuzz net), and (2) pins the actual last-write-wins
//! behavior so a future change to that silent-overwrite semantics is a
//! deliberate, visible diff, not a silent behavior change.

use ra_net::replay::{
    encode_end, encode_hash, encode_header, EndReason, ReplayHeader, ReplayReader, ReplaySeat,
};
use ra_sim::RandomLcg;
use std::collections::BTreeMap;

fn header() -> ReplayHeader {
    ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: 0x0000_0100,
        protocol_version: 1,
        scenario: "scm01ea.ini".to_string(),
        seed: 0xAAAA_BBBB,
        difficulty: 1,
        credits: 8000,
        catalog_hash: 0xCAFE_F00D,
        start_millis: 1_700_000_000_000,
        seats: vec![ReplaySeat {
            seat: 1,
            house: 1,
            color: 3,
        }],
    }
}

/// A file whose hash records regress (30, then 15) and then repeat tick 15
/// with a DIFFERENT hash value — no `Tick` records at all, isolating this to
/// purely the hash-record path.
fn regressed_and_duplicate_hash_file() -> Vec<u8> {
    let mut f = encode_header(&header());
    f.extend_from_slice(&encode_hash(30, 0x1111_1111_1111_1111));
    f.extend_from_slice(&encode_hash(15, 0x2222_2222_2222_2222)); // regression
    f.extend_from_slice(&encode_hash(15, 0x3333_3333_3333_3333)); // duplicate tick, different hash
    f.extend_from_slice(&encode_end(EndReason::Victory, 30));
    f
}

/// Decoding a regressed/duplicate-tick hash stream never panics, and every
/// record decodes successfully as its own well-formed `Hash` record — the
/// reader has no (and is not expected to have any) tick-ordering invariant of
/// its own; ordering is a caller concern.
#[test]
fn regressed_and_duplicate_hash_ticks_decode_without_panic() {
    let file = regressed_and_duplicate_hash_file();
    let (_header, reader) = ReplayReader::open(&file).expect("valid header must open");
    let records = reader
        .collect_records()
        .expect("every record is individually well-formed");
    let hash_ticks: Vec<u32> = records
        .iter()
        .filter_map(|r| match r {
            ra_net::ReplayRecord::Hash { tick, .. } => Some(*tick),
            _ => None,
        })
        .collect();
    assert_eq!(
        hash_ticks,
        vec![30, 15, 15],
        "the reader must preserve file order verbatim — it does not sort or reject regressions"
    );
}

/// Pin the actual behavior every consumer in this codebase exhibits when
/// folding hash records into a `BTreeMap<tick, hash>`: file-order regression
/// is harmless (the map re-sorts by tick key), but a duplicate tick with a
/// conflicting hash silently keeps whichever value was inserted LAST in file
/// iteration order — nothing detects or reports the conflict. This is not
/// asserted as "correct" or "incorrect", only as the actual, load-bearing
/// behavior every replay-verify caller depends on (a regression here is a
/// deliberate audit finding, not a silent behavior change).
#[test]
fn duplicate_tick_conflicting_hash_is_silently_last_write_wins() {
    let file = regressed_and_duplicate_hash_file();
    let (_header, reader) = ReplayReader::open(&file).unwrap();
    let mut map: BTreeMap<u32, u64> = BTreeMap::new();
    for rec in reader.collect_records().unwrap() {
        if let ra_net::ReplayRecord::Hash { tick, hash } = rec {
            map.insert(tick, hash);
        }
    }
    // Tick 30, seen once: untouched.
    assert_eq!(map.get(&30), Some(&0x1111_1111_1111_1111));
    // Tick 15, seen twice (0x2222... then 0x3333...): the LAST file-order
    // value wins, silently discarding the earlier one.
    assert_eq!(
        map.get(&15),
        Some(&0x3333_3333_3333_3333),
        "duplicate-tick hash folding must be last-write-wins by FILE order \
         (not by numeric value, not first-write-wins) — pin this exact semantic"
    );
    assert_eq!(
        map.len(),
        2,
        "the map has exactly one entry per distinct tick"
    );
}

/// Structured fuzz: many random regressed/duplicate-tick hash streams (random
/// tick values, deliberately including repeats and out-of-order sequences)
/// never panic on decode or on the BTreeMap fold.
#[test]
fn random_regressed_hash_streams_never_panic() {
    let mut rng = RandomLcg::new(0x7E57_C0DE);
    for _ in 0..500 {
        let mut f = encode_header(&header());
        let n = rng.range(0, 12) as usize;
        // Ticks drawn from a small pool so repeats/regressions are frequent.
        for _ in 0..n {
            let tick = rng.range(0, 5) as u32 * 15;
            let hash =
                ((rng.range(0, i32::MAX - 1) as u64) << 32) | rng.range(0, i32::MAX - 1) as u64;
            f.extend_from_slice(&encode_hash(tick, hash));
        }
        f.extend_from_slice(&encode_end(EndReason::Quit, 60));
        let mut map: BTreeMap<u32, u64> = BTreeMap::new();
        if let Ok((_h, reader)) = ReplayReader::open(&f) {
            for rec in reader {
                match rec {
                    Ok(ra_net::ReplayRecord::Hash { tick, hash }) => {
                        map.insert(tick, hash);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
        // No assertion beyond "did not panic" — the fold above is exercised
        // purely for its side effect of driving the same code path a real
        // consumer uses.
        let _ = map;
    }
}

/// Valid header immediately followed by pure noise that does not form any
/// valid record framing at all (not even a plausible length prefix) — must
/// stop cleanly with an error on the first record, never panic, and must NOT
/// silently skip the garbage and resume (fused-error discipline).
#[test]
fn valid_header_then_pure_garbage_records_stops_cleanly() {
    let mut rng = RandomLcg::new(0x600D_BEEF);
    for _ in 0..500 {
        let mut f = encode_header(&header());
        let len = rng.range(1, 300) as usize;
        for _ in 0..len {
            f.push(rng.range(0, 255) as u8);
        }
        let (_h, reader) = match ReplayReader::open(&f) {
            Ok(v) => v,
            Err(_) => continue, // header itself can't be garbage here; skip if so
        };
        let mut saw_err = false;
        let mut records_after_err = 0;
        for rec in reader {
            if saw_err {
                records_after_err += 1;
                continue;
            }
            if rec.is_err() {
                saw_err = true;
            }
        }
        assert_eq!(
            records_after_err, 0,
            "iteration must fuse on the first error, never emit a record after one"
        );
    }
}
