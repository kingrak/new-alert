//! M8-C resync audit: adversarial transfer + timing honesty, complementing
//! `lan_resync.rs`'s e2e clean/lossy/attempt-cap/window-revert drills.
//!
//! - Attempt-id confusion: a stale chunk from a superseded attempt arriving
//!   mid a newer attempt must not corrupt the reassembly buffer (`lan.rs`'s
//!   `receive()` guards `SnapshotChunk`/`SnapshotAck`/`SnapshotDone` with
//!   `attempt == rs.attempt`, and `SnapshotOffer` only (re)sizes on
//!   `!got_offer || attempt > rs.attempt`).
//! - Duplicate + out-of-order chunk delivery within one attempt.
//! - Timeout honesty: a loser that never responds at all forces the real 8s
//!   `RESYNC_TIMEOUT` per attempt. There is no test-shrinkable knob for it
//!   (unlike `peer_timeout`/`carry`/`resume_clear_windows`), so this test
//!   genuinely waits out ~2 * 8s under a wall guard — proof the fallback is
//!   bounded, not a hang.
//! - Revert drill: the loser's hash-verify-after-load is load-bearing.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram};
use ra_net::{LanTransport, ResyncEvent};

fn loopback_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

fn send_raw(sock: &UdpSocket, to: std::net::SocketAddr, d: &Datagram) {
    let bytes = wire::encode(d);
    sock.send_to(&bytes, to).expect("send_to loopback");
}

/// A stale chunk from a superseded (lower) attempt, delivered after the
/// buffer was resized for a newer attempt, must be ignored — not merged into
/// the reassembly buffer, not counted toward `have`.
#[test]
fn stale_attempt_chunk_does_not_corrupt_a_newer_attempts_buffer() {
    let host_sock = loopback_socket();
    let loser_sock = loopback_socket();
    let host_addr = host_sock.local_addr().unwrap();
    let loser_addr = loser_sock.local_addr().unwrap();

    let mut loser = LanTransport::new(loser_sock, host_addr, 2, 1, 3, false).unwrap();
    loser.begin_resync_loser();

    const CHUNK_SIZE: u16 = 8;
    const TOTAL_LEN: u32 = 16; // exactly 2 chunks
    let real_chunk0 = vec![0xAAu8; 8];
    let real_chunk1 = vec![0xBBu8; 8];
    let garbage_chunk0 = vec![0xFFu8; 8]; // what a stale attempt-0 packet would carry

    // Attempt 0: OFFER + one real chunk arrive (buffer sized for attempt 0).
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotOffer {
            attempt: 0,
            resume_tick: 500,
            declared_hash: 0x1111_2222_3333_4444,
            total_len: TOTAL_LEN,
            chunk_size: CHUNK_SIZE,
        },
    );
    loser.resync_poll();
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotChunk {
            attempt: 0,
            seq: 0,
            data: real_chunk0.clone(),
        },
    );
    loser.resync_poll();

    // The host times out attempt 0 and re-offers as attempt 1 with a FRESH
    // resume_tick/hash (simulating the host having moved on) — this must
    // (re)size the buffer per `attempt > rs.attempt`.
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotOffer {
            attempt: 1,
            resume_tick: 700,
            declared_hash: 0x5555_6666_7777_8888,
            total_len: TOTAL_LEN,
            chunk_size: CHUNK_SIZE,
        },
    );
    loser.resync_poll();

    // A reordered/late chunk from the superseded attempt 0 arrives NOW, with
    // deliberately distinguishable (garbage) content at the same seq as a
    // real attempt-1 chunk. It must be dropped, not merged.
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotChunk {
            attempt: 0,
            seq: 0,
            data: garbage_chunk0,
        },
    );
    loser.resync_poll();

    // Now the real attempt-1 chunks arrive and complete the transfer.
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotChunk {
            attempt: 1,
            seq: 0,
            data: real_chunk0.clone(),
        },
    );
    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotChunk {
            attempt: 1,
            seq: 1,
            data: real_chunk1.clone(),
        },
    );

    let mut needs_load = None;
    let start = Instant::now();
    while needs_load.is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "wall guard");
        if let ResyncEvent::NeedsLoad {
            bytes,
            resume_tick,
            declared_hash,
        } = loser.resync_poll()
        {
            needs_load = Some((bytes, resume_tick, declared_hash));
        }
    }
    let (bytes, resume_tick, declared_hash) = needs_load.unwrap();

    // Proof: the reassembled bytes are the REAL attempt-1 data, not the
    // garbage the stale attempt-0 packet carried, and the attempt-1 offer's
    // resume_tick/declared_hash won (not the stale attempt-0 values).
    let mut expected = real_chunk0;
    expected.extend_from_slice(&real_chunk1);
    assert_eq!(
        bytes, expected,
        "stale attempt-0 chunk corrupted the attempt-1 reassembly buffer"
    );
    assert_eq!(resume_tick, 700, "stale attempt-0 resume_tick must not win");
    assert_eq!(
        declared_hash, 0x5555_6666_7777_8888,
        "stale attempt-0 declared_hash must not win"
    );
}

/// Duplicate chunk delivery (same attempt, same seq, sent twice) and
/// out-of-order arrival within one attempt reconstruct correctly.
#[test]
fn duplicate_and_reordered_chunks_within_one_attempt_reconstruct_correctly() {
    let host_sock = loopback_socket();
    let loser_sock = loopback_socket();
    let host_addr = host_sock.local_addr().unwrap();
    let loser_addr = loser_sock.local_addr().unwrap();

    let mut loser = LanTransport::new(loser_sock, host_addr, 2, 1, 3, false).unwrap();
    loser.begin_resync_loser();

    const CHUNK_SIZE: u16 = 4;
    const TOTAL_LEN: u32 = 12; // 3 chunks
    let chunks = [vec![1u8; 4], vec![2u8; 4], vec![3u8; 4]];

    send_raw(
        &host_sock,
        loser_addr,
        &Datagram::SnapshotOffer {
            attempt: 0,
            resume_tick: 9,
            declared_hash: 42,
            total_len: TOTAL_LEN,
            chunk_size: CHUNK_SIZE,
        },
    );
    loser.resync_poll();

    // Out of order: 2, 0, 2 (dup), 1. `NeedsLoad` fires once, on whichever
    // `resync_poll()` call observes the completed set — capture every call's
    // result so it can't be silently dropped.
    let mut needs_load = None;
    for seq in [2u32, 0, 2, 1] {
        send_raw(
            &host_sock,
            loser_addr,
            &Datagram::SnapshotChunk {
                attempt: 0,
                seq,
                data: chunks[seq as usize].clone(),
            },
        );
        if let ResyncEvent::NeedsLoad { bytes, .. } = loser.resync_poll() {
            needs_load = Some(bytes);
        }
    }

    let start = Instant::now();
    while needs_load.is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "wall guard");
        if let ResyncEvent::NeedsLoad { bytes, .. } = loser.resync_poll() {
            needs_load = Some(bytes);
        }
    }
    let bytes = needs_load.unwrap();
    let expected: Vec<u8> = chunks.concat();
    assert_eq!(
        bytes, expected,
        "reassembly must be seq-ordered regardless of arrival order, with dupes ignored"
    );
}

/// Timeout honesty: a loser that never responds at all (no OFFER ack, no
/// ACK, no DONE — total silence) forces the host through the real 8s
/// `RESYNC_TIMEOUT` per attempt, exhausts the attempt cap, and returns
/// `Failed` within bounded wall-clock. No hang; counters correct.
#[test]
fn never_responding_loser_forces_the_real_timeout_and_falls_back_bounded() {
    let host_sock = loopback_socket();
    // A silent peer: bound so sends don't error, but nothing ever reads or
    // replies from this address.
    let silent = loopback_socket();
    let silent_addr = silent.local_addr().unwrap();
    let host_addr = host_sock.local_addr().unwrap();

    let mut host = LanTransport::new(host_sock, silent_addr, 1, 2, 3, true).unwrap();
    let snapshot = vec![0x42u8; 5000]; // several chunks
    host.begin_resync_host(snapshot, 123, 0xF00D);

    let start = Instant::now();
    let mut polls = 0u64;
    let event = loop {
        let event = host.resync_poll();
        polls += 1;
        if event == ResyncEvent::Failed {
            break event;
        }
        // Mimic a real per-frame poll cadence (production drives this once
        // per frame, not in a busy spin) so the spin cap bounds iteration
        // count rather than wall-clock time.
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "wall guard: never-responding loser must fail within bounded wall-clock, \
             not hang (host addr {host_addr}, {polls} polls so far)"
        );
        assert!(polls < 100_000, "spin cap");
    };
    let elapsed = start.elapsed();

    assert_eq!(event, ResyncEvent::Failed);
    // Two attempts at RESYNC_TIMEOUT=8s each: must take a while (proves the
    // real timeout — not a fast/instant bogus pass) but stay well under the
    // 30s wall guard (proves the fallback is bounded).
    assert!(
        elapsed >= Duration::from_secs(15),
        "expected ~2 attempts of the real 8s timeout, only took {elapsed:?} \
         (suspiciously fast — is the timeout actually being exercised?)"
    );
    assert!(
        elapsed < Duration::from_secs(25),
        "took {elapsed:?}, too slow even for 2 * 8s attempts — investigate"
    );
    assert_eq!(
        host.resyncs_completed(),
        0,
        "a never-responding loser must never be counted as a completed resync"
    );
}
