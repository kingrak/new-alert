//! M7.23 P4: deep never-panic fuzz for the replay reader.
//!
//! The on-disk replay format inherits `wire`'s fuzz-safety contract: any byte
//! string — random garbage, a truncated valid file, a structurally-mutated one
//! — must decode to a `ReplayError` value, never a panic. This suite hammers
//! [`ReplayReader::open`] and full record iteration with deterministic
//! (seeded) corruption so a regression is reproducible.

use ra_net::replay::{
    encode_end, encode_hash, encode_header, encode_tick, EndReason, ReplayHeader, ReplayReader,
    ReplaySeat,
};
use ra_net::TickBundle;
use ra_sim::coords::CellCoord;
use ra_sim::{Command, Handle, ProdKind, RandomLcg};

fn sample_file() -> Vec<u8> {
    let header = ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: 0x0000_0100,
        protocol_version: 1,
        scenario: "scm01ea.ini".to_string(),
        seed: 0x1234_5678,
        difficulty: 1,
        credits: 8000,
        catalog_hash: 0xDEAD_BEEF,
        start_millis: 1_700_000_000_000,
        seats: vec![
            ReplaySeat {
                seat: 1,
                house: 1,
                color: 3,
            },
            ReplaySeat {
                seat: 2,
                house: 2,
                color: 5,
            },
        ],
    };
    let mut f = encode_header(&header);
    let mv = |house: u8, x: i32, y: i32| Command::Move {
        unit: Handle { index: 3, gen: 1 },
        dest: CellCoord::new(x, y),
        house,
    };
    let cancel = |house: u8| Command::CancelProduction {
        house,
        kind: ProdKind::Building,
    };
    for t in 0..40u32 {
        if t % 3 == 0 {
            let bundle = TickBundle {
                tick: t,
                seats: vec![
                    (1, vec![mv(1, t as i32, 2), cancel(1)]),
                    (2, vec![mv(2, 9, 9)]),
                ],
            };
            f.extend_from_slice(&encode_tick(t, &bundle));
        }
        if t % 15 == 0 {
            f.extend_from_slice(&encode_hash(t, 0xABCD_0000 ^ t as u64));
        }
    }
    f.extend_from_slice(&encode_end(EndReason::Victory, 40));
    f
}

/// Drive the reader to exhaustion; the only contract is "does not panic".
fn drain(bytes: &[u8]) {
    if let Ok((_h, reader)) = ReplayReader::open(bytes) {
        for rec in reader {
            if rec.is_err() {
                break;
            }
        }
    }
}

#[test]
fn every_prefix_and_single_byte_mutation_is_safe() {
    let file = sample_file();
    for cut in 0..=file.len() {
        drain(&file[..cut]);
        // Also each single-byte flip of that prefix.
        for i in 0..cut {
            let mut m = file[..cut].to_vec();
            m[i] ^= 0xFF;
            drain(&m);
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = RandomLcg::new(0x00FF_1234);
    for _ in 0..4000 {
        let len = (rng.range(0, 512)) as usize;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            buf.push(rng.range(0, 255) as u8);
        }
        drain(&buf);
    }
}

#[test]
fn random_bytes_after_valid_magic_never_panic() {
    // Force the magic + version to be correct so the fuzz reaches the header /
    // record decoders rather than bailing at the magic check.
    let mut rng = RandomLcg::new(0x0BAD_F00D);
    for _ in 0..4000 {
        let len = 6 + (rng.range(0, 400)) as usize;
        let mut buf = Vec::with_capacity(len);
        buf.extend_from_slice(b"RARP");
        buf.extend_from_slice(&ra_net::REPLAY_VERSION.to_le_bytes());
        for _ in 6..len {
            buf.push(rng.range(0, 255) as u8);
        }
        drain(&buf);
    }
}

#[test]
fn structured_multi_byte_corruption_of_valid_file_never_panics() {
    let base = sample_file();
    let mut rng = RandomLcg::new(0x5151_2323);
    for _ in 0..6000 {
        let mut m = base.clone();
        let flips = 1 + rng.range(0, 7) as usize;
        for _ in 0..flips {
            let idx = rng.range(0, m.len() as i32 - 1) as usize;
            m[idx] = rng.range(0, 255) as u8;
        }
        // Occasionally also truncate.
        if rng.range(0, 3) == 0 && !m.is_empty() {
            let keep = rng.range(0, m.len() as i32) as usize;
            m.truncate(keep);
        }
        drain(&m);
    }
}
