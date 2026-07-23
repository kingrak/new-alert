//! M8-B depth audit, item 1: adversarial wire-format fuzz **deeper** than the
//! existing `wire.rs` proof suite — exhaustive single-byte mutation at every
//! offset (not just a handful of seeded bit flips), explicit boundary values
//! at every length cap (exactly-at-cap and cap+1), zero-length strings,
//! duplicate/overlapping BUNDLES tick entries, and tick numbers in the
//! `u32::MAX` region. `decode` must never panic on any of this — a panic here
//! is a P0 finding per the audit brief.

use ra_net::wire::{
    self, BundleEntry, CtrlRecord, Datagram, HashVerdict, RejectReason, SessListEntry, SessSeat,
    SessionPhase, WireError, GAME_VERSION, MAX_CMDS_PER_TICK, MAX_MAP_NAME, MAX_NAME,
    MAX_TICK_ENTRIES, PROTOCOL_VERSION, WIRE_MAGIC,
};
use ra_sim::coords::CellCoord;
use ra_sim::{Command, Handle, ProdKind, RandomLcg};

/// One command as its opaque wire blob (the v2 TICK payload shape).
fn blob(house: u8, n: u32) -> Vec<u8> {
    wire::encode_command(&Command::Move {
        unit: Handle { index: n, gen: 1 },
        dest: CellCoord::new(n as i32, house as i32),
        house,
    })
}

fn handle(rng: &mut RandomLcg) -> Handle {
    Handle {
        index: rng.range(0, 5000) as u32,
        gen: rng.range(0, 500) as u32,
    }
}

fn sample_cmd(rng: &mut RandomLcg) -> Command {
    match rng.range(0, 3) {
        0 => Command::Move {
            unit: handle(rng),
            dest: CellCoord::new(rng.range(0, 100), rng.range(0, 100)),
            house: rng.range(0, 7) as u8,
        },
        1 => Command::CancelProduction {
            house: rng.range(0, 7) as u8,
            kind: ProdKind::Unit,
        },
        _ => Command::Stop {
            unit: handle(rng),
            house: rng.range(0, 7) as u8,
        },
    }
}

/// One representative instance of every datagram variant (12 total),
/// including a BUNDLES/HASHES payload with real content so mutation exercises
/// the command-decode path too.
fn representative_datagrams(rng: &mut RandomLcg) -> Vec<(&'static str, Datagram)> {
    vec![
        (
            "announce",
            Datagram::Announce {
                game_version: GAME_VERSION,
                game_port: 12345,
                name: "HOSTY".to_string(),
                map: "scm01ea.ini".to_string(),
            },
        ),
        (
            "join",
            Datagram::Join {
                game_version: GAME_VERSION,
                name: "JOINER".to_string(),
            },
        ),
        (
            "welcome",
            Datagram::Welcome {
                game_version: GAME_VERSION,
                seat: 1,
                host_seat: 0,
                delay: 3,
                seed: 0xDEAD_BEEF,
                credits: 5000,
                map: "scm01ea.ini".to_string(),
                host_name: "HOSTY".to_string(),
            },
        ),
        (
            "reject",
            Datagram::Reject {
                reason: RejectReason::SessionFull,
            },
        ),
        ("ready", Datagram::Ready),
        ("start", Datagram::Start),
        ("leave", Datagram::Leave),
        (
            "bundles",
            Datagram::Bundles {
                entries: (0..5)
                    .map(|i| (100 + i, (0..3).map(|_| sample_cmd(rng)).collect()))
                    .collect(),
            },
        ),
        (
            "hashes",
            Datagram::Hashes {
                entries: (0..5)
                    .map(|i| (100 + i, 0x1122_3344_5566_7788 + i as u64))
                    .collect(),
            },
        ),
        ("nack", Datagram::Nack { from: 42 }),
        ("keepalive", Datagram::KeepAlive { tick: 42 }),
        ("quit", Datagram::Quit),
        // --- wire v2 (M9-A relay): every new message (audit: fuzz EVERY one) ---
        (
            "srv_hello",
            Datagram::SrvHello {
                game_version: GAME_VERSION,
                client_nonce: 0xABCD_1234,
            },
        ),
        (
            "srv_welcome",
            Datagram::SrvWelcome {
                server_nonce: 0x1111_2222,
                conn_id: 0x3333_4444,
            },
        ),
        (
            "sess_create",
            Datagram::SessCreate {
                conn_id: 0xDEAD_BEEF,
                name: "GAME".to_string(),
                map: "scm01ea.ini".to_string(),
                seats: 4,
                credits: 8000,
                seed: 0x5EED,
                catalog_hash: 0xCAFE_F00D_1234_5678,
            },
        ),
        ("sess_list_req", Datagram::SessListReq { conn_id: 0x99 }),
        (
            "sess_list",
            Datagram::SessList {
                entries: (0..3)
                    .map(|i| SessListEntry {
                        session_id: 100 + i,
                        name: format!("s{i}"),
                        map: "m.ini".to_string(),
                        seats_taken: i as u8,
                        seats: 4,
                        in_progress: i % 2 == 0,
                    })
                    .collect(),
            },
        ),
        (
            "sess_join",
            Datagram::SessJoin {
                conn_id: 0x1234,
                session_id: 7,
                name: "JOINER".to_string(),
            },
        ),
        (
            "sess_state",
            Datagram::SessState {
                session_id: 7,
                phase: SessionPhase::Lobby,
                host_seat: 1,
                delay: 6,
                seed: 0x5EED,
                credits: 8000,
                map: "scm01ea.ini".to_string(),
                seats: vec![
                    SessSeat {
                        seat: 1,
                        name: "host".to_string(),
                        house: 1,
                        ready: true,
                    },
                    SessSeat {
                        seat: 2,
                        name: "p2".to_string(),
                        house: 2,
                        ready: false,
                    },
                ],
            },
        ),
        (
            "sess_ready",
            Datagram::SessReady {
                conn_id: 0x1234,
                ready: true,
            },
        ),
        (
            "sess_leave",
            Datagram::SessLeave {
                conn_id: 0x1234,
                reason: 2,
            },
        ),
        (
            "sess_start",
            Datagram::SessStart {
                session_id: 7,
                start_tick: 0,
                input_delay: 6,
                seat_map: vec![(1, 1), (2, 2)],
            },
        ),
        (
            "tick_cmds",
            Datagram::TickCmds {
                conn_id: 0x1234,
                entries: (0..4)
                    .map(|t| (10 + t, vec![blob(1, t), blob(1, t + 100)]))
                    .collect(),
            },
        ),
        (
            "tick_bundle",
            Datagram::TickBundle {
                entries: (0..3)
                    .map(|t| BundleEntry {
                        tick: 10 + t,
                        ctrl: if t == 0 {
                            vec![
                                CtrlRecord::Timing {
                                    new_delay: 8,
                                    effective_tick: 20,
                                },
                                CtrlRecord::Unknown {
                                    tag: 99,
                                    bytes: vec![1, 2, 3],
                                },
                            ]
                        } else {
                            Vec::new()
                        },
                        seats: vec![(1, vec![blob(1, t)]), (2, vec![blob(2, t)])],
                    })
                    .collect(),
            },
        ),
        (
            "tick_hash",
            Datagram::TickHash {
                conn_id: 0x1234,
                entries: (0..4)
                    .map(|t| (10 + t, 0xAABB_CCDD_0000_0000 + t as u64))
                    .collect(),
            },
        ),
        (
            "hash_verdict",
            Datagram::HashVerdictMsg {
                tick: 42,
                verdict: HashVerdict::YouDiverged,
                majority_hash: 0x1234_5678_9ABC_DEF0,
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// (1) Exhaustive single-byte mutation at every offset, every message type.
// ---------------------------------------------------------------------------

/// For every datagram variant, mutate **every byte position** of the encoded
/// buffer to several representative replacement values, and confirm `decode`
/// never panics. This is materially deeper than the existing seeded 1-3-bit
/// flip fuzz in `wire.rs`: every offset is hit, not a random sample.
#[test]
fn exhaustive_single_byte_mutation_at_every_offset_never_panics() {
    let mut rng = RandomLcg::new(0x0FF5_E7A0);
    let datagrams = representative_datagrams(&mut rng);
    let replacement_values: [u8; 5] = [0x00, 0xFF, 0x7F, 0x80, 0x01];
    let mut mutations_tried: u64 = 0;
    for (name, d) in &datagrams {
        let bytes = wire::encode(d);
        assert!(!bytes.is_empty(), "{name}: encoding must not be empty");
        for pos in 0..bytes.len() {
            for &val in &replacement_values {
                let mut mutated = bytes.clone();
                if mutated[pos] == val {
                    continue; // not actually a mutation
                }
                mutated[pos] = val;
                // Must not panic. Ok or Err are both acceptable outcomes.
                let _ = wire::decode(&mutated);
                mutations_tried += 1;
            }
        }
    }
    assert!(
        mutations_tried > 500,
        "suspiciously few mutations tried ({mutations_tried}) — coverage regressed"
    );
}

/// Same exhaustive-offset mutation, but two simultaneous byte flips at every
/// (i, j) pair for the *shorter* datagrams (Ready/Start/Leave/Nack/KeepAlive/
/// Quit/Reject) — cheap enough to do pairwise, and two-byte corruption is a
/// more realistic wire-noise model than one bit.
#[test]
fn pairwise_byte_mutation_on_short_datagrams_never_panics() {
    let short: Vec<Datagram> = vec![
        Datagram::Ready,
        Datagram::Start,
        Datagram::Leave,
        Datagram::Quit,
        Datagram::Nack { from: 7 },
        Datagram::KeepAlive { tick: 7 },
        Datagram::Reject {
            reason: RejectReason::AlreadyStarted,
        },
    ];
    let mut pairs_tried = 0u64;
    for d in &short {
        let bytes = wire::encode(d);
        for i in 0..bytes.len() {
            for j in (i + 1)..bytes.len() {
                let mut mutated = bytes.clone();
                mutated[i] ^= 0xFF;
                mutated[j] ^= 0xAA;
                let _ = wire::decode(&mutated);
                pairs_tried += 1;
            }
        }
    }
    assert!(pairs_tried > 20, "too few pairs tried ({pairs_tried})");
}

// ---------------------------------------------------------------------------
// (2) Truncation at every length — reconfirmed with the richer datagram set.
// ---------------------------------------------------------------------------

#[test]
fn truncation_at_every_length_for_every_representative_datagram_errors_cleanly() {
    let mut rng = RandomLcg::new(0x0FF5_E7A1);
    for (name, d) in representative_datagrams(&mut rng) {
        let bytes = wire::encode(&d);
        for len in 0..bytes.len() {
            let r = wire::decode(&bytes[..len]);
            assert!(
                r.is_err(),
                "{name}: truncation to {len}/{} bytes decoded Ok unexpectedly: {r:?}",
                bytes.len()
            );
        }
        // The full buffer, by contrast, must decode.
        assert!(
            wire::decode(&bytes).is_ok(),
            "{name}: full buffer must decode"
        );
    }
}

// ---------------------------------------------------------------------------
// (3) Cap boundary values: exactly at cap, and cap+1.
// ---------------------------------------------------------------------------

fn header(buf: &mut Vec<u8>, msg_type: u8) {
    buf.extend_from_slice(&WIRE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    buf.push(msg_type);
}

/// BUNDLES entry count exactly at `MAX_TICK_ENTRIES` must decode fine;
/// `MAX_TICK_ENTRIES + 1` must be rejected — the exact boundary, not just an
/// absurd oversized value like the existing `over_cap_command_count_is_rejected`.
#[test]
fn bundles_entry_count_exactly_at_cap_and_cap_plus_one() {
    // At cap: build via the real API so we don't hand-rewrite the command
    // encoding — MAX_TICK_ENTRIES empty-command entries.
    let entries: Vec<(u32, Vec<Command>)> = (0..MAX_TICK_ENTRIES as u32)
        .map(|t| (t, Vec::new()))
        .collect();
    let d = Datagram::Bundles { entries };
    let bytes = wire::encode(&d);
    let decoded = wire::decode(&bytes).expect("exactly-at-cap BUNDLES must decode");
    match decoded {
        Datagram::Bundles { entries } => assert_eq!(entries.len(), MAX_TICK_ENTRIES),
        other => panic!("wrong variant decoded: {other:?}"),
    }

    // Cap + 1: hand-build the header since `encode` clamps to the cap
    // (encode never fails per its contract) — the count byte itself must be
    // the lie the decoder catches.
    let mut buf = Vec::new();
    header(&mut buf, 0x10); // T_BUNDLES
    buf.push((MAX_TICK_ENTRIES + 1) as u8);
    // No entry bodies needed: the count check must fire before consuming any.
    let r = wire::decode(&buf);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "cap+1 BUNDLES entry count must be rejected as BadValue, got {r:?}"
    );
}

/// Same exact-boundary probe for HASHES.
#[test]
fn hashes_entry_count_exactly_at_cap_and_cap_plus_one() {
    let entries: Vec<(u32, u64)> = (0..MAX_TICK_ENTRIES as u32)
        .map(|t| (t, t as u64))
        .collect();
    let d = Datagram::Hashes { entries };
    let bytes = wire::encode(&d);
    let decoded = wire::decode(&bytes).expect("exactly-at-cap HASHES must decode");
    match decoded {
        Datagram::Hashes { entries } => assert_eq!(entries.len(), MAX_TICK_ENTRIES),
        other => panic!("wrong variant: {other:?}"),
    }

    let mut buf = Vec::new();
    header(&mut buf, 0x11); // T_HASHES
    buf.push((MAX_TICK_ENTRIES + 1) as u8);
    let r = wire::decode(&buf);
    assert!(
        matches!(r, Err(WireError::BadValue(_))),
        "cap+1 HASHES entry count must be rejected, got {r:?}"
    );
}

/// Command-per-tick count exactly at `MAX_CMDS_PER_TICK` (not just
/// `u16::MAX` like the existing test) — the real boundary a legitimate-looking
/// but hostile peer would probe first.
#[test]
fn commands_per_tick_exactly_at_cap_and_cap_plus_one() {
    let mut buf = Vec::new();
    header(&mut buf, 0x10); // T_BUNDLES
    buf.push(1); // one tick entry
    buf.extend_from_slice(&7u32.to_le_bytes()); // tick
    buf.extend_from_slice(&(MAX_CMDS_PER_TICK as u16).to_le_bytes()); // == cap
                                                                      // No command bodies follow: MAX_CMDS_PER_TICK is huge, so this will hit
                                                                      // Truncated while reading the first command tag — still must be an error,
                                                                      // never a panic, and specifically not silently accepted as 0 commands.
    let r = wire::decode(&buf);
    assert!(
        r.is_err(),
        "at-cap count with no bodies must still error (truncated), got {r:?}"
    );

    let mut buf2 = Vec::new();
    header(&mut buf2, 0x10);
    buf2.push(1);
    buf2.extend_from_slice(&7u32.to_le_bytes());
    buf2.extend_from_slice(&(MAX_CMDS_PER_TICK as u16 + 1).to_le_bytes()); // cap + 1
    let r2 = wire::decode(&buf2);
    assert!(
        matches!(r2, Err(WireError::BadValue(_))),
        "cap+1 command count must be rejected as BadValue before any command is parsed, got {r2:?}"
    );
}

/// String length exactly at `MAX_NAME`/`MAX_MAP_NAME` must decode; cap+1 must
/// be `BadValue`, mirroring the exact boundary the `str8` decoder checks.
#[test]
fn string_length_exactly_at_cap_and_cap_plus_one() {
    // At cap: MAX_NAME bytes of 'A', which the real encoder would truncate
    // to MAX_NAME anyway (str8 caps to MAX_NAME) — construct by hand to
    // pin the *decoder's* boundary independent of the encoder's clamp.
    let mut buf = Vec::new();
    header(&mut buf, 0x02); // T_JOIN
    buf.extend_from_slice(&GAME_VERSION.to_le_bytes());
    buf.push(MAX_NAME as u8); // length == cap
    buf.extend_from_slice(&[b'A'; MAX_NAME]);
    let r = wire::decode(&buf);
    assert!(
        r.is_ok(),
        "name length exactly at MAX_NAME must decode, got {r:?}"
    );
    match r.unwrap() {
        Datagram::Join { name, .. } => assert_eq!(name.len(), MAX_NAME),
        other => panic!("wrong variant: {other:?}"),
    }

    // Cap + 1: BadValue, and specifically *before* the decoder tries to
    // consume MAX_NAME+1 bytes it doesn't have (so this must not be
    // reported as Truncated — cap violation is checked first).
    let mut buf2 = Vec::new();
    header(&mut buf2, 0x02);
    buf2.extend_from_slice(&GAME_VERSION.to_le_bytes());
    buf2.push((MAX_NAME + 1) as u8);
    buf2.extend_from_slice(&[b'A'; MAX_NAME + 1]); // even with enough bytes present
    let r2 = wire::decode(&buf2);
    assert!(
        matches!(r2, Err(WireError::BadValue(_))),
        "name length cap+1 (even with enough bytes present) must be BadValue, got {r2:?}"
    );

    // Map name cap boundary in WELCOME, same idea.
    let mut buf3 = Vec::new();
    header(&mut buf3, 0x03); // T_WELCOME
    buf3.extend_from_slice(&GAME_VERSION.to_le_bytes());
    buf3.push(1); // seat
    buf3.push(0); // host_seat
    buf3.push(3); // delay
    buf3.extend_from_slice(&0u32.to_le_bytes()); // seed
    buf3.extend_from_slice(&0i32.to_le_bytes()); // credits
    buf3.push((MAX_MAP_NAME + 1) as u8); // map length cap+1
    buf3.extend_from_slice(&[b'M'; MAX_MAP_NAME + 1]);
    let r3 = wire::decode(&buf3);
    assert!(
        matches!(r3, Err(WireError::BadValue(_))),
        "map length cap+1 in WELCOME must be BadValue, got {r3:?}"
    );
}

// ---------------------------------------------------------------------------
// (4) Zero-length strings.
// ---------------------------------------------------------------------------

#[test]
fn zero_length_strings_round_trip_identity() {
    let cases = vec![
        Datagram::Announce {
            game_version: GAME_VERSION,
            game_port: 1,
            name: String::new(),
            map: String::new(),
        },
        Datagram::Join {
            game_version: GAME_VERSION,
            name: String::new(),
        },
        Datagram::Welcome {
            game_version: GAME_VERSION,
            seat: 1,
            host_seat: 0,
            delay: 3,
            seed: 0,
            credits: 0,
            map: String::new(),
            host_name: String::new(),
        },
    ];
    for d in cases {
        let bytes = wire::encode(&d);
        let back =
            wire::decode(&bytes).unwrap_or_else(|e| panic!("zero-length string case: {e:?}"));
        assert_eq!(back, d);
    }
}

// ---------------------------------------------------------------------------
// (5) Duplicate / overlapping tick ranges in BUNDLES.
// ---------------------------------------------------------------------------

/// The wire layer imposes no uniqueness or ordering constraint on BUNDLES
/// entries — a hostile or buggy peer could send duplicate or out-of-order
/// tick numbers. `decode` must accept it verbatim (dedup/ordering is a
/// transport-layer concern, proven separately against `LanTransport`); this
/// pins that the wire layer itself does not choke on it.
#[test]
fn duplicate_and_overlapping_bundle_ticks_decode_without_panic() {
    let entries = vec![
        (
            5u32,
            vec![Command::Stop {
                unit: Handle { index: 1, gen: 0 },
                house: 1,
            }],
        ),
        (
            5u32, // duplicate tick, different payload
            vec![Command::Stop {
                unit: Handle { index: 2, gen: 0 },
                house: 2,
            }],
        ),
        (3u32, vec![]), // out of ascending order vs. the two 5's above
        (5u32, vec![]), // a third copy of tick 5, now empty
    ];
    let d = Datagram::Bundles {
        entries: entries.clone(),
    };
    let bytes = wire::encode(&d);
    let back = wire::decode(&bytes).expect("duplicate/overlapping ticks must still decode");
    match back {
        Datagram::Bundles { entries: got } => assert_eq!(
            got, entries,
            "entries must round-trip verbatim, duplicates and all"
        ),
        other => panic!("wrong variant: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// (6) Tick-number overflow region (u32::MAX).
// ---------------------------------------------------------------------------

#[test]
fn tick_numbers_near_u32_max_round_trip_without_panic() {
    let extreme_ticks = [
        0u32,
        1,
        u32::MAX,
        u32::MAX - 1,
        u32::MAX / 2,
        u32::MAX - 100,
    ];
    let entries: Vec<(u32, Vec<Command>)> = extreme_ticks
        .iter()
        .map(|&t| {
            (
                t,
                vec![Command::Stop {
                    unit: Handle { index: 0, gen: 0 },
                    house: 0,
                }],
            )
        })
        .collect();
    let d = Datagram::Bundles {
        entries: entries.clone(),
    };
    let bytes = wire::encode(&d);
    let back = wire::decode(&bytes).expect("extreme tick numbers must decode");
    match back {
        Datagram::Bundles { entries: got } => assert_eq!(got, entries),
        other => panic!("wrong variant: {other:?}"),
    }

    // Same for HASHES and NACK/KEEPALIVE (every datagram carrying a raw Tick).
    let hd = Datagram::Hashes {
        entries: extreme_ticks.iter().map(|&t| (t, u64::MAX)).collect(),
    };
    let hbytes = wire::encode(&hd);
    assert_eq!(wire::decode(&hbytes).unwrap(), hd);

    for &t in &extreme_ticks {
        let n = Datagram::Nack { from: t };
        assert_eq!(wire::decode(&wire::encode(&n)).unwrap(), n);
        let k = Datagram::KeepAlive { tick: t };
        assert_eq!(wire::decode(&wire::encode(&k)).unwrap(), k);
    }
}

// ---------------------------------------------------------------------------
// (7) Oversized length prefix on an otherwise-empty buffer (no body bytes at
//     all following the lying length byte) — the classic "claims 255 bytes,
//     delivers zero" adversarial shape.
// ---------------------------------------------------------------------------

#[test]
fn string_length_prefix_claims_more_than_the_buffer_holds() {
    let mut buf = Vec::new();
    header(&mut buf, 0x02); // T_JOIN
    buf.extend_from_slice(&GAME_VERSION.to_le_bytes());
    buf.push(200); // claims 200 bytes of name (over MAX_NAME=24 anyway)
                   // zero body bytes follow at all.
    let r = wire::decode(&buf);
    assert!(
        r.is_err(),
        "oversized length prefix with no body must error, got {r:?}"
    );

    // Within-cap length byte but body still short (truncated mid-string).
    let mut buf2 = Vec::new();
    header(&mut buf2, 0x02);
    buf2.extend_from_slice(&GAME_VERSION.to_le_bytes());
    buf2.push(10); // within MAX_NAME
    buf2.extend_from_slice(b"abc"); // only 3 of the promised 10 bytes
    let r2 = wire::decode(&buf2);
    assert!(
        matches!(r2, Err(WireError::Truncated)),
        "short body must be Truncated, got {r2:?}"
    );
}

// ---------------------------------------------------------------------------
// (8) Exact-consumption load-bearing check (audit item 3c): appending extra
// bytes after an otherwise-fully-valid encoding must be rejected as
// `TrailingBytes`, never silently accepted. A manual revert-drill (Reader::done
// commented out in `decode`) confirmed that *none* of the pre-existing wire
// tests noticed its absence — every one of them either truncates a valid
// buffer or mutates bytes in place, never appends past the end. This test was
// added specifically to close that gap and is the one that actually fails
// under the revert-drill, proving the check is load-bearing rather than
// vacuously present.
// ---------------------------------------------------------------------------

#[test]
fn trailing_bytes_after_a_fully_valid_datagram_are_rejected() {
    let mut rng = RandomLcg::new(0x0FF5_E7A2);
    let mut checked = 0u32;
    for (name, d) in representative_datagrams(&mut rng) {
        let mut bytes = wire::encode(&d);
        // A clean, fully-valid encoding must decode as-is.
        assert!(
            wire::decode(&bytes).is_ok(),
            "{name}: baseline encoding must decode"
        );
        // Append 1..=4 extra bytes (garbage tail, e.g. a coalesced second
        // datagram or a stray padding byte from a buggy sender) — must now
        // be rejected, specifically as TrailingBytes, not silently ignored.
        for extra in 1..=4 {
            bytes.push((0xA0 + extra) as u8);
            let r = wire::decode(&bytes);
            assert!(
                matches!(r, Err(WireError::TrailingBytes)),
                "{name}: {extra} trailing byte(s) after a fully-valid datagram must be \
                 rejected as TrailingBytes, got {r:?} — a peer padding or coalescing \
                 datagrams could otherwise have its extra bytes silently swallowed"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 40,
        "too few trailing-byte cases checked ({checked})"
    );
}

// ---------------------------------------------------------------------------
// (9) Wire v2 round-trip identity for EVERY new relay message, plus the
// control-record forward-compatibility contract.
// ---------------------------------------------------------------------------

/// Every v2 relay message encodes → decodes back to itself byte-for-byte
/// (identity), including nested command blobs and the embedded control records.
#[test]
fn v2_messages_round_trip_identity() {
    let mut rng = RandomLcg::new(0x0FF5_E7A3);
    for (name, d) in representative_datagrams(&mut rng) {
        let bytes = wire::encode(&d);
        let back = wire::decode(&bytes).unwrap_or_else(|e| panic!("{name}: decode failed: {e:?}"));
        assert_eq!(back, d, "{name}: must round-trip to an identical value");
    }
}

/// An unknown control-record tag decodes opaquely as [`CtrlRecord::Unknown`] and
/// re-encodes byte-for-byte — the forward-compatibility slot (§6.2) that lets a
/// later server add control records without a wire bump.
#[test]
fn unknown_control_record_round_trips_opaquely() {
    let d = Datagram::TickBundle {
        entries: vec![BundleEntry {
            tick: 5,
            ctrl: vec![CtrlRecord::Unknown {
                tag: 0x7E,
                bytes: vec![9, 8, 7, 6, 5],
            }],
            seats: vec![(1, vec![])],
        }],
    };
    let bytes = wire::encode(&d);
    assert_eq!(wire::decode(&bytes).unwrap(), d);
}

/// The seat-house accessor reads the issuing house of an opaque blob without a
/// sim dependency at the call site (§7.3), for every command shape.
#[test]
fn command_house_accessor_reads_every_command_shape() {
    for house in 0u8..=6 {
        assert_eq!(wire::command_house(&blob(house, 3)).unwrap(), house);
    }
    // Malformed blob → error, never a panic.
    assert!(wire::command_house(&[0xFF, 0x00]).is_err());
    assert!(wire::command_house(&[]).is_err());
}
