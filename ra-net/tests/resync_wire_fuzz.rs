//! M8-C audit finding: the M8-B wire-fuzz pattern (`wire.rs`,
//! `wire_fuzz_deep.rs`) covers datagram tags 0x01-0x14 but was never extended
//! to the M8-C snapshot-resync messages `SNAPSHOT_OFFER/CHUNK/ACK/DONE`
//! (0x20-0x23, `T_SNAP_OFFER..T_SNAP_DONE` in `wire.rs`) — neither
//! `wire.rs::datagram()` nor `wire_fuzz_deep.rs::representative_datagrams()`
//! construct one. This file closes that gap: round-trip identity, truncation
//! safety, byte-flip fuzz, and the malicious-oversized-length rejections
//! specific to the snapshot messages (adversarial transfer decode per the
//! resync audit brief).

use ra_net::wire::{self, Datagram, WireError, MAX_SNAPSHOT_LEN, MAX_SNAP_CHUNK_DATA};
use ra_sim::RandomLcg;

fn snap_datagram(tag: i32, rng: &mut RandomLcg) -> Datagram {
    match tag {
        0 => Datagram::SnapshotOffer {
            attempt: rng.range(0, 255) as u8,
            resume_tick: rng.range(0, 100_000) as u32,
            declared_hash: ((rng.range(0, i32::MAX - 1) as u64) << 32)
                | rng.range(0, i32::MAX - 1) as u64,
            total_len: rng.range(0, MAX_SNAPSHOT_LEN as i32 - 1) as u32,
            chunk_size: rng.range(1, MAX_SNAP_CHUNK_DATA as i32) as u16,
        },
        1 => {
            let n = rng.range(0, MAX_SNAP_CHUNK_DATA as i32) as usize;
            Datagram::SnapshotChunk {
                attempt: rng.range(0, 255) as u8,
                seq: rng.range(0, 100_000) as u32,
                data: (0..n).map(|_| rng.range(0, 255) as u8).collect(),
            }
        }
        2 => {
            let n = rng.range(0, 16) as usize;
            Datagram::SnapshotAck {
                attempt: rng.range(0, 255) as u8,
                missing: (0..n).map(|_| rng.range(0, 100_000) as u32).collect(),
            }
        }
        _ => Datagram::SnapshotDone {
            attempt: rng.range(0, 255) as u8,
            ok: rng.range(0, 1) == 1,
        },
    }
}

/// Round-trip identity over all four snapshot variants (the same property
/// `encode_decode_round_trip_identity_over_all_variants` proves for the other
/// twelve — extended here per the audit finding).
#[test]
fn snapshot_messages_round_trip_identity() {
    let mut rng = RandomLcg::new(0x5A47_0001);
    let mut seen = [false; 4];
    for case in 0..400 {
        let tag = case % 4;
        let d = snap_datagram(tag, &mut rng);
        seen[tag as usize] = true;
        let bytes = wire::encode(&d);
        let back = wire::decode(&bytes)
            .unwrap_or_else(|e| panic!("case {case}: decode failed with {e:?} for {d:?}"));
        assert_eq!(back, d, "case {case}: round trip diverged");
    }
    assert!(seen.iter().all(|&s| s), "not every snapshot variant hit");
}

/// Every strict prefix of a valid snapshot-message encoding fails to decode
/// (error, never panic) — the truncation property from `wire.rs` extended to
/// 0x20-0x23.
#[test]
fn snapshot_messages_truncation_errors_without_panicking() {
    let mut rng = RandomLcg::new(0x5A47_0002);
    for case in 0..80 {
        let d = snap_datagram(case % 4, &mut rng);
        let bytes = wire::encode(&d);
        for len in 0..bytes.len() {
            let r = wire::decode(&bytes[..len]);
            assert!(
                r.is_err(),
                "case {case}: prefix len {len}/{} decoded Ok: {r:?}",
                bytes.len()
            );
        }
    }
}

/// Byte-flip fuzz on snapshot messages: never panics, and any Ok result must
/// still respect the chunk_size/total_len/missing-count caps (a flipped
/// length byte must be caught by the cap check, not silently honoured).
#[test]
fn snapshot_messages_byte_flip_fuzz_never_panics() {
    let mut rng = RandomLcg::new(0x5A47_0003);
    for case in 0..800 {
        let d = snap_datagram(case % 4, &mut rng);
        let mut bytes = wire::encode(&d);
        let flips = 1 + rng.range(0, 3) as usize;
        for _ in 0..flips {
            let pos = rng.range(0, bytes.len() as i32 - 1) as usize;
            let bit = rng.range(0, 7) as u8;
            bytes[pos] ^= 1 << bit;
        }
        if let Ok(Datagram::SnapshotOffer {
            total_len,
            chunk_size,
            ..
        }) = wire::decode(&bytes)
        {
            assert!(total_len as usize <= MAX_SNAPSHOT_LEN);
            assert!(chunk_size != 0 && chunk_size as usize <= MAX_SNAP_CHUNK_DATA);
        }
    }
}

/// Pure garbage fuzz restricted to the snapshot type-byte range (0x20-0x23):
/// forces the decoder down these specific payload parsers with fully random
/// trailing bytes. Must never panic.
#[test]
fn snapshot_type_byte_garbage_fuzz_never_panics() {
    let mut rng = RandomLcg::new(0x5A47_0004);
    for _ in 0..2000 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
        bytes.push([0x20u8, 0x21, 0x22, 0x23][rng.range(0, 3) as usize]);
        let len = rng.range(0, 64) as usize;
        for _ in 0..len {
            bytes.push(rng.range(0, 255) as u8);
        }
        let _ = wire::decode(&bytes); // must simply not panic
    }
}

/// Malicious `total_len` over the 16MB snapshot cap is rejected at decode,
/// never honoured with an unbounded allocation downstream.
#[test]
fn snapshot_offer_oversized_total_len_is_rejected() {
    let d = Datagram::SnapshotOffer {
        attempt: 0,
        resume_tick: 7,
        declared_hash: 0xDEAD_BEEF,
        total_len: (MAX_SNAPSHOT_LEN as u32) + 1,
        chunk_size: 1200,
    };
    let bytes = wire::encode(&d);
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "oversized total_len must be rejected, got {r:?}"
    );
}

/// A `total_len` at exactly the cap is accepted (boundary is not off-by-one).
#[test]
fn snapshot_offer_total_len_exactly_at_cap_is_accepted() {
    let d = Datagram::SnapshotOffer {
        attempt: 0,
        resume_tick: 7,
        declared_hash: 0xDEAD_BEEF,
        total_len: MAX_SNAPSHOT_LEN as u32,
        chunk_size: 1200,
    };
    let bytes = wire::encode(&d);
    assert_eq!(wire::decode(&bytes).unwrap(), d);
}

/// Malicious `chunk_size` of zero (would divide-by-zero / infinite-loop a
/// naive chunk-count calc) is rejected at decode.
#[test]
fn snapshot_offer_zero_chunk_size_is_rejected() {
    let d = Datagram::SnapshotOffer {
        attempt: 0,
        resume_tick: 7,
        declared_hash: 0,
        total_len: 5000,
        chunk_size: 0,
    };
    let bytes = wire::encode(&d);
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "zero chunk_size must be rejected, got {r:?}"
    );
}

/// Malicious `chunk_size` over `MAX_SNAP_CHUNK_DATA` is rejected at decode.
#[test]
fn snapshot_offer_oversized_chunk_size_is_rejected() {
    let d = Datagram::SnapshotOffer {
        attempt: 0,
        resume_tick: 7,
        declared_hash: 0,
        total_len: 5000,
        chunk_size: (MAX_SNAP_CHUNK_DATA as u16) + 1,
    };
    let bytes = wire::encode(&d);
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "oversized chunk_size must be rejected, got {r:?}"
    );
}

/// A hand-built SNAPSHOT_CHUNK datagram that lies about its data length
/// (claims more bytes than actually follow) errors as `Truncated`, never
/// panics or reads out of bounds.
#[test]
fn snapshot_chunk_lying_length_is_rejected_not_panicking() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
    bytes.push(0x21); // T_SNAP_CHUNK
    bytes.push(3); // attempt
    bytes.extend_from_slice(&9u32.to_le_bytes()); // seq
    bytes.extend_from_slice(&1000u16.to_le_bytes()); // claims 1000 bytes of data
    bytes.extend_from_slice(&[0xAA; 10]); // only 10 actually follow
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::Truncated)),
        "lying chunk length must be Truncated, got {r:?}"
    );
}

/// A hand-built SNAPSHOT_CHUNK claiming a data length over
/// `MAX_SNAP_CHUNK_DATA` is rejected by the cap check even before the
/// truncation check would fire.
#[test]
fn snapshot_chunk_over_cap_length_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
    bytes.push(0x21); // T_SNAP_CHUNK
    bytes.push(3); // attempt
    bytes.extend_from_slice(&9u32.to_le_bytes()); // seq
    bytes.extend_from_slice(&((MAX_SNAP_CHUNK_DATA as u16) + 1).to_le_bytes());
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "over-cap chunk length must be rejected, got {r:?}"
    );
}

/// A hand-built SNAPSHOT_ACK claiming an absurd missing-count is rejected by
/// the `MAX_SNAP_MISSING` cap, not honoured with a huge allocation.
#[test]
fn snapshot_ack_over_cap_missing_count_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
    bytes.push(0x22); // T_SNAP_ACK
    bytes.push(3); // attempt
    bytes.extend_from_slice(&(u32::MAX).to_le_bytes()); // absurd missing count
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_)) | Err(WireError::Truncated)),
        "lying missing count must be rejected, got {r:?}"
    );
}

/// A hand-built SNAPSHOT_DONE with an invalid `ok` byte (not 0 or 1) is
/// rejected rather than silently coerced.
#[test]
fn snapshot_done_invalid_ok_byte_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
    bytes.push(0x23); // T_SNAP_DONE
    bytes.push(3); // attempt
    bytes.push(2); // neither 0 nor 1
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "invalid ok byte must be rejected, got {r:?}"
    );
}
