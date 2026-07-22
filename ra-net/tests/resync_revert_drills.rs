//! M8-C resync audit: revert drills for claims that are easy to state but
//! need to be proven, not assumed.
//!
//! (a) The loser's hash-verify-after-load (`declared_hash` compare, mirrored
//!     here from the real call site `ra-client/src/appcore.rs`'s
//!     `drive_net_resync` — the transport itself treats the hash as opaque
//!     per DESIGN.md §4.6, so the check necessarily lives in the caller, not
//!     in `ra-net`) is load-bearing: with it, a wrong-but-validly-shaped load
//!     is rejected and the session retries/falls back; without it, the wrong
//!     world is silently adopted.
//!
//! `RESYNC_CONFIRM_GRACE` (the host's optimistic-resume-without-DONE
//! backstop) has **no** test-injectable disable, unlike `peer_timeout`/
//! `carry`/`resume_clear_windows`/(implicitly, wire-level attempt behavior) —
//! so a true "disable it and watch the all-dropped-DONE case change
//! behavior" drill cannot be written without a production code change, which
//! is out of scope for this audit. What CAN be verified without one: that
//! the documented backstop actually fires and is bounded when every DONE is
//! lost (positive-side proof) — see
//! `all_done_dropped_still_resumes_via_the_optimistic_grace` below. This
//! asymmetry is called out explicitly in the audit report.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram};
use ra_net::{LanTransport, ResyncEvent, DEFAULT_INPUT_DELAY};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, World};

const SEED: u32 = 0x5E5C_00A1;
const WALL_SECS: u64 = 30;

fn loopback_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// A small world that genuinely evolves tick to tick (mirrors
/// `lan_resync.rs::build_world`, duplicated here to keep this file
/// self-contained per file — no dependency on another test binary).
fn build_world() -> (World, Handle) {
    let mut w = World::new(Passability::all_passable(), SEED);
    w.init_houses(2, 5000);
    let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 200, stats(50, 8));
    (w, h)
}

/// Advance `w` by `n` ticks with the mover walking a long diagonal, so
/// consecutive snapshots at different ticks are genuinely different (not
/// coincidentally hash-equal).
fn advance(w: &mut World, mover: Handle, n: u32) {
    if w.tick_count() == 0 {
        w.tick(&[Command::Move {
            unit: mover,
            dest: CellCoord::new(60, 60),
            house: 1,
        }]);
        for _ in 1..n {
            w.tick(&[]);
        }
    } else {
        for _ in 0..n {
            w.tick(&[]);
        }
    }
}

/// Drive a resync attempt to `NeedsLoad`, over real localhost UDP, for a
/// freshly begun host+loser pair. Returns the reassembled bytes plus both
/// transports, positioned right at the load decision point.
fn drive_to_needs_load(
    snapshot: Vec<u8>,
    resume_tick: u32,
    declared_hash: u64,
) -> (LanTransport, LanTransport, Vec<u8>) {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut ta = LanTransport::new(sa, b_real, 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, a_real, 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    ta.begin_resync_host(snapshot, resume_tick, declared_hash);
    tb.begin_resync_loser();

    let start = Instant::now();
    let mut got_bytes = None;
    while got_bytes.is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        if ta.resync_active() {
            ta.resync_poll();
        }
        if let ResyncEvent::NeedsLoad { bytes, .. } = tb.resync_poll() {
            got_bytes = Some(bytes);
        }
    }
    (ta, tb, got_bytes.unwrap())
}

/// Drain a transport's `resync_poll`/`poll` until either it resumes/fails or
/// the wall guard trips. Returns `Some(true)` for Resumed, `Some(false)` for
/// Failed, `None` if the wall guard tripped (test fails in that case).
///
/// A real (persistent) bug is retry-proof: it must keep failing verification
/// on every attempt, not just the first — so every `NeedsLoad` the loser
/// sees (including ones from retried attempts) is answered with
/// `resync_report_loaded(false)`, mirroring `lan_resync.rs`'s
/// `always_reject` drill knob.
fn drain_to_outcome(
    ta: &mut LanTransport,
    tb: &mut LanTransport,
    start: Instant,
) -> (Option<bool>, Option<bool>) {
    let mut outcome_a = None;
    let mut outcome_b = None;
    while outcome_a.is_none() || outcome_b.is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        if outcome_a.is_none() && ta.resync_active() {
            match ta.resync_poll() {
                ResyncEvent::Resumed { .. } => outcome_a = Some(true),
                ResyncEvent::Failed => outcome_a = Some(false),
                _ => {}
            }
        }
        if outcome_b.is_none() && tb.resync_active() {
            match tb.resync_poll() {
                ResyncEvent::Resumed { .. } => outcome_b = Some(true),
                ResyncEvent::Failed => outcome_b = Some(false),
                ResyncEvent::NeedsLoad { .. } => tb.resync_report_loaded(false),
                _ => {}
            }
        }
    }
    (outcome_a, outcome_b)
}

/// (a-verify-on) A wrong-but-validly-shaped snapshot (a stale earlier-tick
/// save of the SAME world, not the one actually transferred) is loaded, its
/// hash is checked against `declared_hash` exactly as
/// `appcore.rs::drive_net_resync` does, and — because it fails — the loser
/// reports `ok=false`. With `RESYNC_MAX_ATTEMPTS=2`, both peers exhaust the
/// cap and fall back to `Failed`. This is the behaviour the real desync end
/// depends on.
#[test]
fn revert_drill_hash_verify_rejects_a_wrong_load_and_falls_back() {
    let (mut w, mover) = build_world();
    advance(&mut w, mover, 5);
    let stale_snapshot = w.save_snapshot(); // "wrong" world: an earlier state
    let stale_hash = w.state_hash();
    advance(&mut w, mover, 10);
    let real_snapshot = w.save_snapshot();
    let declared_hash = w.state_hash();
    assert_ne!(
        stale_hash, declared_hash,
        "the stale and real snapshots must genuinely differ for this drill to mean anything"
    );

    let (mut ta, mut tb, bytes) =
        drive_to_needs_load(real_snapshot.clone(), w.tick_count(), declared_hash);
    // Sanity: the wire transfer itself is faithful (proves the corruption
    // below is deliberate, not an artifact of a broken transfer).
    assert_eq!(bytes, real_snapshot, "transfer must be byte-exact");

    // Simulate a wrong load (the stale snapshot) and verify it exactly as
    // `appcore.rs` does.
    let loaded = World::load_snapshot(
        &stale_snapshot,
        w.catalog().clone(),
        w.passability().clone(),
    )
    .expect("stale snapshot must still decode (it's a valid save)");
    let ok = loaded.state_hash() == declared_hash;
    assert!(!ok, "the stale load must NOT verify against declared_hash");
    tb.resync_report_loaded(ok);

    let start = Instant::now();
    let (outcome_a, outcome_b) = drain_to_outcome(&mut ta, &mut tb, start);
    assert_eq!(
        outcome_a,
        Some(false),
        "host must fall back to Failed when the loser keeps rejecting the load"
    );
    assert_eq!(
        outcome_b,
        Some(false),
        "loser must fall back to Failed when its own load never verifies"
    );
}

/// (a-verify-off) The same wrong load, but the verification step is skipped
/// (the test unconditionally reports `ok=true`, standing in for "the check
/// wasn't there") — the wrong world is silently adopted, `resyncs_completed`
/// is (wrongly) incremented, and the internal tick counter is left at the
/// STALE tick, not `resume_tick`: the session continues in an objectively
/// corrupted state instead of failing loudly. This is the proof that the
/// check is load-bearing, not theater — remove it and a real bug (loading
/// the wrong save) goes undetected instead of triggering the documented
/// fallback.
#[test]
fn revert_drill_without_hash_verify_a_wrong_load_is_silently_adopted() {
    let (mut w, mover) = build_world();
    advance(&mut w, mover, 5);
    let stale_snapshot = w.save_snapshot();
    let stale_hash = w.state_hash();
    advance(&mut w, mover, 10);
    let real_snapshot = w.save_snapshot();
    let declared_hash = w.state_hash();
    let resume_tick = w.tick_count();
    assert_ne!(stale_hash, declared_hash);

    let (mut ta, mut tb, _bytes) = drive_to_needs_load(real_snapshot, resume_tick, declared_hash);

    let loaded = World::load_snapshot(
        &stale_snapshot,
        w.catalog().clone(),
        w.passability().clone(),
    )
    .expect("stale snapshot must still decode");
    let actually_matches = loaded.state_hash() == declared_hash;
    assert!(!actually_matches, "sanity: this load is genuinely wrong");

    // The disabled-check stand-in: report success regardless.
    tb.resync_report_loaded(true);

    let start = Instant::now();
    let (outcome_a, outcome_b) = drain_to_outcome(&mut ta, &mut tb, start);
    assert_eq!(
        outcome_a,
        Some(true),
        "host has no way to know the loser's load was wrong: it resumes normally"
    );
    assert_eq!(
        outcome_b,
        Some(true),
        "without the check, the loser's wrong load is reported as a success"
    );
    assert_eq!(
        tb.resyncs_completed(),
        1,
        "the (wrong) resync is counted as completed — exactly what makes this dangerous"
    );
    // The smoking gun: `loaded` (what a real caller would swap into `self.world`)
    // sits at the STALE tick, not `resume_tick` — the world and the transport's
    // resumed tick counter are now inconsistent. A real caller that skipped the
    // check would proceed to tick this mismatched pair.
    assert_ne!(
        loaded.tick_count(),
        resume_tick,
        "the adopted world's tick sits at the wrong point in history — silent corruption"
    );
}

/// (b) Positive-side proof for the host's optimistic-resume grace
/// (`RESYNC_CONFIRM_GRACE` = 600ms, `lan.rs`): with every `SnapshotDone`
/// datagram dropped (simulating total loss of the terminal burst), the host
/// still resumes — bounded near the grace window, never hanging out to the
/// 8s `RESYNC_TIMEOUT`. (See the module doc for why this can't also be run
/// as a true disable-and-compare revert drill without a production code
/// change.)
#[test]
fn all_done_dropped_still_resumes_via_the_optimistic_grace() {
    let sa = loopback_socket();
    // A relay that forwards everything EXCEPT SnapshotDone (0x23) datagrams.
    let relay_a = loopback_socket(); // faces the host
    let relay_b = loopback_socket(); // faces the loser
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let relay_a_addr = relay_a.local_addr().unwrap();
    let relay_b_addr = relay_b.local_addr().unwrap();

    let mut ta = LanTransport::new(sa, relay_a_addr, 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, relay_b_addr, 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();

    let (mut w, mover) = build_world();
    advance(&mut w, mover, 3);
    let snapshot = w.save_snapshot();
    let declared_hash = w.state_hash();
    let resume_tick = w.tick_count();
    ta.begin_resync_host(snapshot, resume_tick, declared_hash);
    tb.begin_resync_loser();

    let mut buf = [0u8; 65536];
    let mut pump_relay = || {
        // Host -> loser: arrives at relay_a (that's the host's configured
        // peer); forward via relay_b so the loser sees it from ITS
        // configured peer address. Forward everything (the host never sends
        // DONE).
        while let Ok((n, _)) = relay_a.recv_from(&mut buf) {
            let _ = relay_b.send_to(&buf[..n], b_real);
        }
        // Loser -> host: arrives at relay_b; forward via relay_a so the host
        // sees it from ITS configured peer address. Forward everything
        // EXCEPT SnapshotDone (0x23) — simulating total loss of the
        // terminal DONE burst.
        while let Ok((n, _)) = relay_b.recv_from(&mut buf) {
            match wire::decode(&buf[..n]) {
                Ok(Datagram::SnapshotDone { .. }) => {} // dropped: the whole point of this drill
                _ => {
                    let _ = relay_a.send_to(&buf[..n], a_real);
                }
            }
        }
    };

    let start = Instant::now();
    let mut resumed_a = None;
    let mut got_needs_load = false;
    while resumed_a.is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        pump_relay();
        if let ResyncEvent::Resumed { resume_tick: rt } = ta.resync_poll() {
            resumed_a = Some(rt);
        }
        if tb.resync_active() {
            if let ResyncEvent::NeedsLoad {
                bytes,
                declared_hash: dh,
                ..
            } = tb.resync_poll()
            {
                got_needs_load = true;
                let loaded =
                    World::load_snapshot(&bytes, w.catalog().clone(), w.passability().clone())
                        .expect("must decode");
                tb.resync_report_loaded(loaded.state_hash() == dh);
            }
        }
    }
    let elapsed = start.elapsed();

    assert!(got_needs_load, "loser must have reached the load step");
    assert_eq!(resumed_a, Some(resume_tick));
    // Bounded near the grace window (600ms), not out at the 8s attempt
    // timeout — proves the backstop is what resumed the host, not a
    // coincidental DONE getting through some other path.
    assert!(
        elapsed < Duration::from_secs(4),
        "took {elapsed:?} — suspiciously close to the 8s RESYNC_TIMEOUT; \
         is the optimistic grace actually firing, or did this fall through \
         to the per-attempt timeout instead?"
    );
}
