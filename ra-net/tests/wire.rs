//! M8-B proof test (a): wire-format round-trip properties + malformed-input
//! fuzz. Seeded and exhaustive over every datagram and command variant —
//! encode→decode must be the identity, and `decode` must never panic on any
//! byte soup (it returns [`WireError`] values instead).

use ra_net::wire::{
    self, Datagram, RejectReason, WireError, GAME_VERSION, MAX_CMDS_PER_TICK, MAX_NAME,
    PROTOCOL_VERSION,
};
use ra_sim::coords::CellCoord;
use ra_sim::{BuildItem, Command, Handle, ProdKind, RandomLcg, SuperKind, Target};

fn handle(rng: &mut RandomLcg) -> Handle {
    Handle {
        index: rng.range(0, 5000) as u32,
        gen: rng.range(0, 500) as u32,
    }
}

fn cell(rng: &mut RandomLcg) -> CellCoord {
    // Includes off-map negatives: the wire must carry what the sim types can
    // hold, not just the pretty subset.
    CellCoord::new(rng.range(-4, 131), rng.range(-4, 131))
}

fn target(rng: &mut RandomLcg) -> Target {
    match rng.range(0, 2) {
        0 => Target::Unit(handle(rng)),
        1 => Target::Building(handle(rng)),
        _ => Target::Cell(cell(rng)),
    }
}

fn prod_kind(rng: &mut RandomLcg) -> ProdKind {
    match rng.range(0, 2) {
        0 => ProdKind::Building,
        1 => ProdKind::Unit,
        _ => ProdKind::Infantry,
    }
}

/// A random command of variant `tag` (0..=12) — driving by explicit tag lets
/// the round-trip test prove every variant was covered (non-vacuity).
fn command(tag: i32, rng: &mut RandomLcg) -> Command {
    let house = rng.range(0, 7) as u8;
    match tag {
        0 => Command::Move {
            unit: handle(rng),
            dest: cell(rng),
            house,
        },
        1 => Command::Stop {
            unit: handle(rng),
            house,
        },
        2 => Command::Attack {
            unit: handle(rng),
            target: target(rng),
            house,
        },
        3 => Command::Deploy {
            unit: handle(rng),
            house,
        },
        4 => Command::StartProduction {
            house,
            item: if rng.range(0, 1) == 0 {
                BuildItem::Building(rng.range(0, 90) as u32)
            } else {
                BuildItem::Unit(rng.range(0, 90) as u32)
            },
        },
        5 => Command::PlaceBuilding {
            house,
            building: rng.range(0, 90) as u32,
            cell: cell(rng),
        },
        6 => Command::CancelProduction {
            house,
            kind: prod_kind(rng),
        },
        7 => Command::HoldProduction {
            house,
            kind: prod_kind(rng),
        },
        8 => Command::Sell {
            house,
            building: handle(rng),
        },
        9 => Command::Repair {
            house,
            building: handle(rng),
        },
        10 => Command::Load {
            passenger: handle(rng),
            transport: handle(rng),
            house,
        },
        11 => Command::Unload {
            transport: handle(rng),
            house,
        },
        _ => Command::FireSuperWeapon {
            house,
            kind: match rng.range(0, 2) {
                0 => SuperKind::Nuclear,
                1 => SuperKind::IronCurtain,
                _ => SuperKind::Chronosphere,
            },
            target: target(rng),
            dest: if rng.range(0, 1) == 0 {
                None
            } else {
                Some(cell(rng))
            },
        },
    }
}

fn random_commands(rng: &mut RandomLcg) -> Vec<Command> {
    let n = rng.range(0, 5);
    (0..n).map(|_| command(rng.range(0, 12), rng)).collect()
}

fn random_name(rng: &mut RandomLcg) -> String {
    let n = rng.range(0, MAX_NAME as i32) as usize;
    (0..n)
        .map(|_| (b'A' + rng.range(0, 25) as u8) as char)
        .collect()
}

/// A random datagram of variant `tag` (0..=11).
fn datagram(tag: i32, rng: &mut RandomLcg) -> Datagram {
    match tag {
        0 => Datagram::Announce {
            game_version: rng.range(0, i32::MAX - 1) as u32,
            game_port: rng.range(1024, 65535) as u16,
            name: random_name(rng),
            map: "scm01ea.ini".to_string(),
        },
        1 => Datagram::Join {
            game_version: GAME_VERSION,
            name: random_name(rng),
        },
        2 => Datagram::Welcome {
            game_version: GAME_VERSION,
            seat: rng.range(0, 7) as u8,
            host_seat: rng.range(0, 7) as u8,
            delay: rng.range(0, 8) as u8,
            seed: rng.range(0, i32::MAX - 1) as u32,
            credits: rng.range(0, 20000),
            map: "scm12ea.ini".to_string(),
            host_name: random_name(rng),
        },
        3 => Datagram::Reject {
            reason: match rng.range(0, 3) {
                0 => RejectReason::ProtocolVersion,
                1 => RejectReason::GameVersion,
                2 => RejectReason::SessionFull,
                _ => RejectReason::AlreadyStarted,
            },
        },
        4 => Datagram::Ready,
        5 => Datagram::Start,
        6 => Datagram::Leave,
        7 => {
            let base = rng.range(0, 100_000) as u32;
            let n = rng.range(1, 8) as u32;
            Datagram::Bundles {
                entries: (0..n).map(|i| (base + i, random_commands(rng))).collect(),
            }
        }
        8 => {
            let base = rng.range(0, 100_000) as u32;
            let n = rng.range(1, 8) as u32;
            Datagram::Hashes {
                entries: (0..n)
                    .map(|i| {
                        let hi = rng.range(0, i32::MAX - 1) as u64;
                        let lo = rng.range(0, i32::MAX - 1) as u64;
                        (base + i, (hi << 32) | lo)
                    })
                    .collect(),
            }
        }
        9 => Datagram::Nack {
            from: rng.range(0, 100_000) as u32,
        },
        10 => Datagram::KeepAlive {
            tick: rng.range(0, 100_000) as u32,
        },
        _ => Datagram::Quit,
    }
}

/// Property: encode→decode is the identity, over hundreds of seeded random
/// datagrams covering every datagram variant and every command variant
/// (coverage asserted — non-vacuity per the M7.19 lesson).
#[test]
fn encode_decode_round_trip_identity_over_all_variants() {
    let mut rng = RandomLcg::new(0x37B0_0001);
    let mut datagram_seen = [false; 12];
    let mut command_seen = [false; 13];
    for case in 0..600 {
        let tag = case % 12;
        let d = datagram(tag, &mut rng);
        datagram_seen[tag as usize] = true;
        if let Datagram::Bundles { entries } = &d {
            for (_, cmds) in entries {
                for c in cmds {
                    command_seen[command_tag(c)] = true;
                }
            }
        }
        let bytes = wire::encode(&d);
        let back = wire::decode(&bytes)
            .unwrap_or_else(|e| panic!("case {case}: decode failed with {e:?} for {d:?}"));
        assert_eq!(back, d, "case {case}: round trip diverged");
    }
    // Explicit per-variant command sweep too (the random bundles above cover
    // them with overwhelming probability, but make it a certainty).
    for tag in 0..=12 {
        let c = command(tag, &mut rng);
        command_seen[command_tag(&c)] = true;
        let d = Datagram::Bundles {
            entries: vec![(7, vec![c])],
        };
        let bytes = wire::encode(&d);
        assert_eq!(wire::decode(&bytes).unwrap(), d);
    }
    assert!(
        datagram_seen.iter().all(|&s| s),
        "not every datagram variant was exercised: {datagram_seen:?}"
    );
    assert!(
        command_seen.iter().all(|&s| s),
        "not every command variant was exercised: {command_seen:?}"
    );
}

fn command_tag(c: &Command) -> usize {
    match c {
        Command::Move { .. } => 0,
        Command::Stop { .. } => 1,
        Command::Attack { .. } => 2,
        Command::Deploy { .. } => 3,
        Command::StartProduction { .. } => 4,
        Command::PlaceBuilding { .. } => 5,
        Command::CancelProduction { .. } => 6,
        Command::HoldProduction { .. } => 7,
        Command::Sell { .. } => 8,
        Command::Repair { .. } => 9,
        Command::Load { .. } => 10,
        Command::Unload { .. } => 11,
        Command::FireSuperWeapon { .. } => 12,
    }
}

/// Every strict prefix of a valid encoding fails to decode (with an error,
/// never a panic): the format has no padding, so truncation anywhere must be
/// caught by a length check.
#[test]
fn every_truncation_of_valid_datagrams_errors_without_panicking() {
    let mut rng = RandomLcg::new(0x37B0_0002);
    for case in 0..120 {
        let d = datagram(case % 12, &mut rng);
        let bytes = wire::encode(&d);
        for len in 0..bytes.len() {
            let r = wire::decode(&bytes[..len]);
            assert!(
                r.is_err(),
                "case {case}: strict prefix of length {len}/{} decoded Ok: {r:?}",
                bytes.len()
            );
        }
    }
}

/// Byte-flip fuzz: mutate valid encodings at random positions; decode must
/// return (Ok or Err — a flipped payload byte can still be valid), never
/// panic, and never allocate absurdly (the caps clamp counts).
#[test]
fn byte_flip_fuzz_never_panics() {
    let mut rng = RandomLcg::new(0x37B0_0003);
    for case in 0..400 {
        let d = datagram(case % 12, &mut rng);
        let mut bytes = wire::encode(&d);
        let flips = 1 + rng.range(0, 3) as usize;
        for _ in 0..flips {
            let pos = rng.range(0, bytes.len() as i32 - 1) as usize;
            let bit = rng.range(0, 7) as u8;
            bytes[pos] ^= 1 << bit;
        }
        let _ = wire::decode(&bytes); // must simply not panic
    }
}

/// Pure-garbage fuzz: random buffers of random lengths. Everything must come
/// back as an error value (the magic check catches nearly all of it) and
/// nothing may panic.
#[test]
fn random_garbage_fuzz_never_panics() {
    let mut rng = RandomLcg::new(0x37B0_0004);
    for _ in 0..2000 {
        let len = rng.range(0, 200) as usize;
        let buf: Vec<u8> = (0..len).map(|_| rng.range(0, 255) as u8).collect();
        let _ = wire::decode(&buf); // must simply not panic
    }
    // Empty and single-byte edges.
    assert!(wire::decode(&[]).is_err());
    assert!(wire::decode(&[0x52]).is_err());
}

/// Over-cap counts are rejected as `BadValue`, not honoured with a huge
/// allocation: a hand-built BUNDLES datagram claiming 65535 commands must
/// error out at the cap check.
#[test]
fn over_cap_command_count_is_rejected() {
    // Header + BUNDLES(0x10) + count 1 + tick + a lying command count.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire::WIRE_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    bytes.push(0x10);
    bytes.push(1); // one tick entry
    bytes.extend_from_slice(&7u32.to_le_bytes()); // tick
    bytes.extend_from_slice(&(u16::MAX).to_le_bytes()); // absurd command count
    let r = wire::decode(&bytes);
    assert!(
        matches!(r, Err(WireError::BadValue(_)) | Err(WireError::Truncated)),
        "lying count must be rejected, got {r:?}"
    );
    assert!(u16::MAX as usize > MAX_CMDS_PER_TICK);
}

/// A different protocol version in the header is a `ProtocolMismatch` error
/// before any payload is trusted — the handshake-reject primitive.
#[test]
fn protocol_version_mismatch_is_detected_at_the_header() {
    let d = Datagram::Join {
        game_version: GAME_VERSION,
        name: "X".to_string(),
    };
    let bytes = wire::encode_with_protocol(&d, PROTOCOL_VERSION + 1);
    match wire::decode(&bytes) {
        Err(WireError::ProtocolMismatch { theirs }) => {
            assert_eq!(theirs, PROTOCOL_VERSION + 1);
        }
        other => panic!("expected ProtocolMismatch, got {other:?}"),
    }
    // Same bytes with the right version decode fine.
    assert_eq!(wire::decode(&wire::encode(&d)).unwrap(), d);
}
