//! M8-C P2: forced-desync **resync** over real localhost UDP.
//!
//! A two-player lockstep game is deliberately corrupted mid-match (the joiner's
//! world gains a phantom unit, so its state hash diverges); the peers detect the
//! desync, the host serves an authoritative world snapshot, the joiner loads and
//! hash-verifies it, and both resume lockstep from the snapshot tick — the game
//! CONTINUES instead of ending. The drills:
//!
//! - (c) clean localhost UDP: resync completes, `resyncs_completed` increments,
//!   both hash chains are byte-identical for many ticks after, clean end.
//! - (d) same through a 25%-loss proxy: the chunked transfer heals and recovers.
//! - (e) attempt-cap fallback: a loser that always rejects the load exhausts the
//!   cap and both fall back to the terminal `Desync` end — bounded, no hang.
//! - (f) revert drill: with the resume window re-stamp disabled, the post-resync
//!   chains diverge — proving the re-stamp is load-bearing.
//!
//! Every loop carries a spin cap and a wall-clock guard: a resync bug must fail
//! the test, not hang the suite.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ra_net::{CommandTransport, LanTransport, PollResult, ResyncEvent, DEFAULT_INPUT_DELAY};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, World};

const SEED: u32 = 0x5E5C_0001;
const WALL_SECS: u64 = 60;
const SPIN_CAP: u32 = 500_000;

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

fn loopback_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

/// A skirmish-ish world with houses + a spread of units, some moving, so the
/// hash chain is non-vacuous and a mid-game snapshot is substantial. Returns the
/// unit handles used for scripting (movement makes the chain genuinely evolve).
fn build_world() -> (World, Vec<Handle>) {
    let mut w = World::new(Passability::all_passable(), SEED);
    w.init_houses(2, 5000);
    w.enable_shroud();
    let mut movers = Vec::new();
    for i in 0..12u32 {
        let house = (i % 2) as u8 + 1;
        let cell = CellCoord::new(4 + (i as i32 % 6) * 3, 4 + (i as i32 / 6) * 3);
        let h = w.spawn_unit(
            i % 3,
            house,
            cell,
            Facing((i * 20) as u8),
            200,
            stats(50, 8),
        );
        w.reveal_shroud(house, cell, 4);
        if i < 4 {
            movers.push((h, house));
        }
    }
    let handles = movers.iter().map(|&(h, _)| h).collect();
    // Stash house ids alongside via the same order (index i → house).
    let _ = &movers;
    (w, handles)
}

/// House id for the i-th mover (matches `build_world`'s spawn order).
fn mover_house(i: usize) -> u8 {
    (i as u8 % 2) + 1
}

/// Scripted moves (identical intent on both peers) so the world genuinely
/// changes tick to tick.
fn script(handles: &[Handle], tick: u32) -> Vec<Command> {
    let mut v = Vec::new();
    if tick == 1 {
        for (i, &handle) in handles.iter().enumerate() {
            v.push(Command::Move {
                unit: handle,
                dest: CellCoord::new(20 + i as i32 * 4, 30),
                house: mover_house(i),
            });
        }
    }
    v
}

/// The 25%-loss proxy (mirrors `lan_torture.rs`), pumped between polls.
struct LossyProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    seed: u32,
    drop_pct: u32,
    forwarded: u64,
    dropped: u64,
}

impl LossyProxy {
    fn new(a_real: SocketAddr, b_real: SocketAddr, drop_pct: u32) -> LossyProxy {
        LossyProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            seed: 0x1234_5678,
            drop_pct,
            forwarded: 0,
            dropped: 0,
        }
    }
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }
    /// A cheap LCG bit of jitter to decide drops deterministically.
    fn roll(&mut self) -> u32 {
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        (self.seed >> 16) % 100
    }
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            if self.roll() >= self.drop_pct {
                let _ = self.b_side.send_to(&buf[..n], self.b_real);
                self.forwarded += 1;
            } else {
                self.dropped += 1;
            }
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            if self.roll() >= self.drop_pct {
                let _ = self.a_side.send_to(&buf[..n], self.a_real);
                self.forwarded += 1;
            } else {
                self.dropped += 1;
            }
        }
    }
}

/// Outcome of the forced-desync resync scenario.
struct Outcome {
    resynced: bool,
    resyncs_a: u64,
    resyncs_b: u64,
    /// Post-resync hash chains (host, joiner) — asserted identical on success.
    tail_a: Vec<u64>,
    tail_b: Vec<u64>,
    ended_desync: bool,
}

/// Run the full scenario. `drop_pct` = proxy loss (0 = direct); `always_reject`
/// makes the loser reject every load (attempt-cap drill); `clear_windows`
/// toggles the resume re-stamp (revert drill).
fn run_scenario(drop_pct: u32, always_reject: bool, clear_windows: bool) -> Outcome {
    // Sockets + optional lossy proxy.
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut proxy = if drop_pct > 0 {
        Some(LossyProxy::new(a_real, b_real, drop_pct))
    } else {
        None
    };
    let (a_peer, b_peer) = match &proxy {
        Some(p) => (p.a_addr(), p.b_addr()),
        None => (b_real, a_real),
    };
    let mut ta = LanTransport::new(sa, a_peer, 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, b_peer, 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    ta.set_resume_clear_windows_for_test(clear_windows);
    tb.set_resume_clear_windows_for_test(clear_windows);
    // Shrink the peer timeout so the attempt-cap drill can't hang the suite.
    ta.set_peer_timeout(Duration::from_secs(30));
    tb.set_peer_timeout(Duration::from_secs(30));

    let (mut wa, handles) = build_world();
    let (mut wb, _) = build_world();
    assert_eq!(
        wa.state_hash(),
        wb.state_hash(),
        "identical starts required"
    );

    let pump = move |proxy: &mut Option<LossyProxy>| {
        if let Some(p) = proxy {
            p.pump();
        }
    };

    let corrupt_at = 20u32;
    let post_ticks = 60u32; // ticks to run after resync
    let start = Instant::now();

    // Phase A: normal lockstep until the desync latches on both peers.
    let mut tick = 0u32;
    let mut a_desync = false;
    let mut b_desync = false;
    let mut ended_desync = false;
    let mut resynced = false;
    let mut corrupted = false;
    let mut tail_a = Vec::new();
    let mut tail_b = Vec::new();

    'outer: loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard tripped at tick {tick}"
        );

        // Inject the divergence just before tick `corrupt_at` on the joiner: a
        // phantom unit changes the joiner's state hash durably.
        if tick == corrupt_at && !corrupted {
            wb.spawn_unit(2, 2, CellCoord::new(60, 60), Facing(0), 100, stats(0, 0));
            corrupted = true;
        }

        // Submit scripted input (both peers, identical intent).
        for c in script(&handles, tick) {
            ta.submit(c);
        }
        for c in script(&handles, tick) {
            tb.submit(c);
        }

        // Poll both to Ready, stalling on Waiting, watching for Desync.
        let mut ba = None;
        let mut bb = None;
        let mut spins = 0u32;
        loop {
            pump(&mut proxy);
            if ba.is_none() && !a_desync {
                match ta.poll(tick) {
                    PollResult::Ready(x) => ba = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(_) => a_desync = true,
                    PollResult::ConnectionLost(l) => panic!("A lost peer: {l:?}"),
                }
            } else {
                ta.service();
            }
            if bb.is_none() && !b_desync {
                match tb.poll(tick) {
                    PollResult::Ready(x) => bb = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(_) => b_desync = true,
                    PollResult::ConnectionLost(l) => panic!("B lost peer: {l:?}"),
                }
            } else {
                tb.service();
            }
            if a_desync && b_desync {
                break;
            }
            if (ba.is_some() || a_desync) && (bb.is_some() || b_desync) {
                break;
            }
            spins += 1;
            assert!(spins < SPIN_CAP, "spin cap at tick {tick}");
            assert!(
                start.elapsed() < Duration::from_secs(WALL_SECS),
                "wall guard tripped polling tick {tick}"
            );
        }

        if a_desync && b_desync {
            break 'outer; // move to the resync phase
        }

        // Both Ready: apply, exchange hashes, advance.
        let ba = ba.unwrap();
        let bb = bb.unwrap();
        let ha = wa.tick(&ba.flatten());
        let hb = wb.tick(&bb.flatten());
        ta.report_hash(tick, ha);
        tb.report_hash(tick, hb);
        tick += 1;
    }

    // Phase B: resync. Host is authoritative (§4.6).
    let snapshot = wa.save_snapshot();
    ta.begin_resync_host(snapshot, wa.tick_count(), wa.state_hash());
    tb.begin_resync_loser();

    let mut resume_tick = 0u32;
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard tripped in resync"
        );
        pump(&mut proxy);

        if ta.resync_active() {
            match ta.resync_poll() {
                ResyncEvent::Resumed { resume_tick: rt } => resume_tick = rt,
                ResyncEvent::Failed => {
                    ended_desync = true;
                    break;
                }
                _ => {}
            }
        }
        if tb.resync_active() {
            match tb.resync_poll() {
                ResyncEvent::NeedsLoad {
                    bytes,
                    resume_tick: rt,
                    declared_hash,
                } => {
                    if always_reject {
                        tb.resync_report_loaded(false);
                    } else {
                        let loaded = World::load_snapshot(
                            &bytes,
                            wb.catalog().clone(),
                            wb.passability().clone(),
                        )
                        .expect("snapshot must decode");
                        let ok = loaded.state_hash() == declared_hash;
                        assert!(ok, "loaded snapshot hash must match host's declared hash");
                        wb = loaded;
                        resume_tick = rt;
                        tb.resync_report_loaded(true);
                    }
                }
                ResyncEvent::Resumed { resume_tick: rt } => resume_tick = rt,
                ResyncEvent::Failed => {
                    ended_desync = true;
                    break;
                }
                _ => {}
            }
        }

        if !ta.resync_active() && !tb.resync_active() {
            resynced = true;
            break;
        }
    }

    // Phase C: resume normal lockstep for a stretch, recording both chains.
    if resynced {
        assert_eq!(
            wa.tick_count(),
            resume_tick,
            "host world must sit at the resume tick"
        );
        assert_eq!(
            wb.tick_count(),
            resume_tick,
            "joiner world must sit at the resume tick"
        );
        let mut t = resume_tick;
        for _ in 0..post_ticks {
            assert!(
                start.elapsed() < Duration::from_secs(WALL_SECS),
                "wall guard tripped post-resync at {t}"
            );
            let mut ba = None;
            let mut bb = None;
            let mut spins = 0u32;
            while ba.is_none() || bb.is_none() {
                pump(&mut proxy);
                if ba.is_none() {
                    match ta.poll(t) {
                        PollResult::Ready(x) => ba = Some(x),
                        PollResult::Waiting => {}
                        // A revert drill legitimately re-desyncs here.
                        PollResult::Desync(_) => {
                            ended_desync = true;
                            return Outcome {
                                resynced,
                                resyncs_a: ta.resyncs_completed(),
                                resyncs_b: tb.resyncs_completed(),
                                tail_a,
                                tail_b,
                                ended_desync,
                            };
                        }
                        PollResult::ConnectionLost(l) => panic!("A lost peer post-resync: {l:?}"),
                    }
                } else {
                    ta.service();
                }
                if bb.is_none() {
                    match tb.poll(t) {
                        PollResult::Ready(x) => bb = Some(x),
                        PollResult::Waiting => {}
                        PollResult::Desync(_) => {
                            ended_desync = true;
                            return Outcome {
                                resynced,
                                resyncs_a: ta.resyncs_completed(),
                                resyncs_b: tb.resyncs_completed(),
                                tail_a,
                                tail_b,
                                ended_desync,
                            };
                        }
                        PollResult::ConnectionLost(l) => panic!("B lost peer post-resync: {l:?}"),
                    }
                } else {
                    tb.service();
                }
                spins += 1;
                assert!(spins < SPIN_CAP, "spin cap post-resync at {t}");
                assert!(
                    start.elapsed() < Duration::from_secs(WALL_SECS),
                    "wall guard post-resync at {t}"
                );
            }
            let ba = ba.unwrap();
            let bb = bb.unwrap();
            let ha = wa.tick(&ba.flatten());
            let hb = wb.tick(&bb.flatten());
            ta.report_hash(t, ha);
            tb.report_hash(t, hb);
            tail_a.push(ha);
            tail_b.push(hb);
            t += 1;
        }
    }

    Outcome {
        resynced,
        resyncs_a: ta.resyncs_completed(),
        resyncs_b: tb.resyncs_completed(),
        tail_a,
        tail_b,
        ended_desync,
    }
}

/// (c) Forced desync over clean localhost UDP self-heals; the game continues.
#[test]
fn forced_desync_resyncs_and_continues_clean_udp() {
    let o = run_scenario(0, false, true);
    assert!(o.resynced, "resync must complete");
    assert_eq!(o.resyncs_a, 1, "host must record one completed resync");
    assert_eq!(o.resyncs_b, 1, "joiner must record one completed resync");
    assert!(!o.ended_desync, "must not fall back to the desync end");
    assert_eq!(o.tail_a.len(), 60);
    assert_eq!(o.tail_a, o.tail_b, "post-resync chains must be identical");
    // Non-vacuity: the world genuinely evolves after resync (not a frozen chain).
    let distinct: std::collections::BTreeSet<u64> = o.tail_a.iter().copied().collect();
    assert!(distinct.len() > 5, "post-resync chain suspiciously static");
}

/// (d) The same drill through a 25%-loss proxy: chunked transfer recovers.
#[test]
fn forced_desync_resyncs_through_25pct_loss() {
    let o = run_scenario(25, false, true);
    assert!(o.resynced, "resync must complete under loss");
    assert_eq!(o.resyncs_a, 1);
    assert_eq!(o.resyncs_b, 1);
    assert!(!o.ended_desync);
    assert_eq!(
        o.tail_a, o.tail_b,
        "post-resync chains must be identical under loss"
    );
    assert_eq!(o.tail_a.len(), 60);
}

/// (e) A loser that always rejects the load exhausts the attempt cap and both
/// peers fall back to the terminal desync end — bounded, no hang.
#[test]
fn resync_failure_falls_back_to_desync_end() {
    let o = run_scenario(0, true, true);
    assert!(
        !o.resynced,
        "resync must NOT complete when the load always fails"
    );
    assert!(o.ended_desync, "must fall back to the terminal desync end");
    assert_eq!(
        o.resyncs_a, 0,
        "no resync should be recorded on the cap fallback"
    );
    assert_eq!(o.resyncs_b, 0);
}

/// (f) Revert drill: disable the resume window re-stamp → the post-resync chains
/// diverge (or re-desync), proving the re-stamp is load-bearing.
#[test]
fn revert_drill_no_window_clear_diverges() {
    let o = run_scenario(0, false, false);
    // The transfer itself still completes (resync machinery runs)...
    assert!(
        o.resynced,
        "the transfer completes; only the resume is sabotaged"
    );
    // ...but without the window re-stamp the chains must NOT stay identical.
    let diverged = o.ended_desync || o.tail_a != o.tail_b;
    assert!(
        diverged,
        "with the resume re-stamp disabled the chains must diverge (they did not — the re-stamp is not actually load-bearing?)"
    );
}
