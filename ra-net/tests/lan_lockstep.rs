//! M8-B proof tests over **real UDP sockets** (all on 127.0.0.1 with
//! OS-assigned ports — never a fixed port, per CI collision safety):
//!
//! - (b) the M8-A scripted proof battery re-run over real sockets:
//!   two independent `World`s, different commands per endpoint, per-tick
//!   hash-chain identity + non-vacuity.
//! - (c) loss injection through a deterministic in-process lossy proxy:
//!   redundant-bundle recovery keeps the game advancing hash-identically;
//!   burst loss beyond the redundancy window exercises the NACK backstop.
//! - (d) handshake negatives: protocol/game version mismatches rejected
//!   cleanly; join to a dead port times out cleanly.
//! - (e) peer disconnect mid-game surfaces `ConnectionLost` (timeout) and a
//!   clean quit surfaces `ConnectionLost` (peer-quit) — never `Desync`.
//! - (g) revert sensitivity: with the redundant carry AND the NACK backstop
//!   disabled, the loss-injection run stalls — proving both mechanisms are
//!   load-bearing, not decorative.
//! - plus the full ra-net-level lobby flow (announce → browse → join →
//!   ready → start) ending in a real lockstep run.
//!
//! Every spin loop carries both a spin cap and a wall-clock guard (M7.20
//! lesson: a networking bug must fail the test, not hang the suite).

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram, RejectReason, GAME_VERSION, PROTOCOL_VERSION};
use ra_net::{
    CommandTransport, DiscoveryConfig, HostLobby, JoinLobby, LanTransport, LostReason, PollResult,
    SessionBrowser, SessionSettings, DEFAULT_INPUT_DELAY, REDUNDANT_TICKS,
};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, RandomLcg, World};

const SEED: u32 = 0xC0FF_EE01;
/// Enough for every scripted move to arrive (same fixture as the M8-A
/// battery in `lockstep.rs`).
const TICKS: u32 = 450;
/// Hard wall guard for any single test's drive loop.
const WALL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Fixture (mirrors ra-net/tests/lockstep.rs — the M8-A proof scenario)
// ---------------------------------------------------------------------------

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

struct Fixture {
    a1: Handle,
    a2: Handle,
    b1: Handle,
    b2: Handle,
}

fn build_world() -> (World, Fixture) {
    let mut world = World::new(Passability::all_passable(), SEED);
    let a1 = world.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats(60, 10));
    let a2 = world.spawn_unit(1, 1, CellCoord::new(40, 3), Facing(64), 300, stats(30, 3));
    let b1 = world.spawn_unit(2, 2, CellCoord::new(3, 40), Facing(128), 150, stats(60, 20));
    let b2 = world.spawn_unit(3, 2, CellCoord::new(40, 40), Facing(192), 200, stats(60, 8));
    (world, Fixture { a1, a2, b1, b2 })
}

fn script_a(f: &Fixture, t: u32) -> Vec<Command> {
    let mut v = Vec::new();
    match t {
        0 => v.push(Command::Move {
            unit: f.a1,
            dest: CellCoord::new(60, 50),
            house: 1,
        }),
        5 => v.push(Command::Move {
            unit: f.a2,
            dest: CellCoord::new(30, 12),
            house: 1,
        }),
        40 => {
            v.push(Command::Stop {
                unit: f.a1,
                house: 1,
            });
            v.push(Command::Move {
                unit: f.a1,
                dest: CellCoord::new(10, 55),
                house: 1,
            });
        }
        _ => {}
    }
    v
}

fn script_b(f: &Fixture, t: u32) -> Vec<Command> {
    let mut v = Vec::new();
    match t {
        2 => v.push(Command::Move {
            unit: f.b1,
            dest: CellCoord::new(45, 8),
            house: 2,
        }),
        7 => v.push(Command::Move {
            unit: f.b2,
            dest: CellCoord::new(5, 5),
            house: 2,
        }),
        60 => v.push(Command::Move {
            unit: f.b1,
            dest: CellCoord::new(55, 55),
            house: 2,
        }),
        _ => {}
    }
    v
}

// ---------------------------------------------------------------------------
// Socket plumbing
// ---------------------------------------------------------------------------

fn loopback_socket() -> UdpSocket {
    // OS-assigned port on loopback — never a fixed port in tests.
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

/// Two directly-connected LAN endpoints (seats 1 and 2).
fn direct_pair(delay: u32) -> (LanTransport, LanTransport) {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let aa = sa.local_addr().unwrap();
    let ab = sb.local_addr().unwrap();
    let ta = LanTransport::new(sa, ab, 1, 2, delay, true).unwrap();
    let tb = LanTransport::new(sb, aa, 2, 1, delay, false).unwrap();
    (ta, tb)
}

/// A deterministic lossy UDP proxy between the two endpoints, in-process.
/// Endpoint A's "peer" is the proxy's A-side socket and vice versa; every
/// forwarded datagram is re-sent from the opposite side's socket, so each
/// endpoint sees exactly one peer address. The drop pattern is a seeded LCG —
/// the same seed drops the same packets in the same order every run.
struct LossyProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    rng: RandomLcg,
    /// Percent of datagrams to drop (0..100).
    drop_pct: i32,
    /// When true, drop EVERYTHING (burst blackout mode).
    blackout: bool,
    dropped: u64,
    forwarded: u64,
}

impl LossyProxy {
    fn new(a_real: SocketAddr, b_real: SocketAddr, seed: u32, drop_pct: i32) -> LossyProxy {
        LossyProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            rng: RandomLcg::new(seed),
            drop_pct,
            blackout: false,
            dropped: 0,
            forwarded: 0,
        }
    }

    /// The address endpoint A must use as its peer.
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }

    /// The address endpoint B must use as its peer.
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }

    fn keep(&mut self) -> bool {
        if self.blackout {
            return false;
        }
        self.rng.range(0, 99) >= self.drop_pct
    }

    /// Forward everything pending, applying the drop pattern.
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            if self.keep() {
                self.forwarded += 1;
                let _ = self.b_side.send_to(&buf[..n], self.b_real);
            } else {
                self.dropped += 1;
            }
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            if self.keep() {
                self.forwarded += 1;
                let _ = self.a_side.send_to(&buf[..n], self.a_real);
            } else {
                self.dropped += 1;
            }
        }
    }
}

/// Two LAN endpoints joined through a lossy proxy.
fn proxied_pair(delay: u32, seed: u32, drop_pct: i32) -> (LanTransport, LanTransport, LossyProxy) {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut proxy = LossyProxy::new(a_real, b_real, seed, drop_pct);
    // Learn nothing dynamically: everything is wired explicitly.
    proxy.a_real = a_real;
    proxy.b_real = b_real;
    let ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, delay, true).unwrap();
    let tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, delay, false).unwrap();
    (ta, tb, proxy)
}

// ---------------------------------------------------------------------------
// Drive loop
// ---------------------------------------------------------------------------

struct Instance {
    world: World,
    tp: LanTransport,
}

/// Drive both instances through `ticks` of the full protocol, pumping
/// `pump` (the proxy, or nothing) between polls. Returns the shared hash
/// chain, or Err(tick) if a tick could not complete within `spin_cap` polls
/// (the revert-drill's stall detector).
#[allow(clippy::too_many_arguments)]
fn drive(
    a: &mut Instance,
    b: &mut Instance,
    ticks: u32,
    spin_cap: u32,
    mut pump: impl FnMut(u32),
    mut submit: impl FnMut(u32, &mut Instance, &mut Instance),
) -> Result<Vec<u64>, u32> {
    let start = Instant::now();
    let mut chain = Vec::new();
    for t in 0..ticks {
        submit(t, a, b);
        let mut bundle_a = None;
        let mut bundle_b = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            pump(t);
            if bundle_a.is_none() {
                match a.tp.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("A desynced at tick {t}: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("A lost peer at tick {t}: {l:?}"),
                }
            } else {
                // Ready side keeps the connection serviced (answers the
                // stalled peer's NACKs) — exactly what a real client does by
                // polling the *next* tick every frame.
                a.tp.service();
            }
            if bundle_b.is_none() {
                match b.tp.poll(t) {
                    PollResult::Ready(x) => bundle_b = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("B desynced at tick {t}: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("B lost peer at tick {t}: {l:?}"),
                }
            } else {
                b.tp.service();
            }
            spins += 1;
            if spins >= spin_cap {
                return Err(t);
            }
            assert!(
                start.elapsed() < Duration::from_secs(WALL_SECS),
                "wall-clock guard tripped at tick {t} (spins {spins})"
            );
        }
        let bundle_a = bundle_a.unwrap();
        let bundle_b = bundle_b.unwrap();
        assert_eq!(bundle_a, bundle_b, "bundle mismatch at tick {t}");
        let ha = a.world.tick(&bundle_a.flatten());
        let hb = b.world.tick(&bundle_b.flatten());
        assert_eq!(ha, hb, "hash mismatch at tick {t}");
        a.tp.report_hash(t, ha);
        b.tp.report_hash(t, hb);
        chain.push(ha);
    }
    Ok(chain)
}

fn scripted_instances(ta: LanTransport, tb: LanTransport) -> (Instance, Instance, Fixture) {
    let (wa, f) = build_world();
    let (wb, _) = build_world();
    assert_eq!(
        wa.state_hash(),
        wb.state_hash(),
        "identical starts required"
    );
    (
        Instance { world: wa, tp: ta },
        Instance { world: wb, tp: tb },
        f,
    )
}

/// The scripted submit hook shared by the socketed runs.
fn scripted_submit(f: &Fixture) -> impl FnMut(u32, &mut Instance, &mut Instance) + '_ {
    move |t, ia, ib| {
        for c in script_a(f, t) {
            ia.tp.submit(c);
        }
        for c in script_b(f, t) {
            ib.tp.submit(c);
        }
    }
}

/// Non-vacuity (M7.19): the sim advanced, both scripts applied, and they
/// applied *through the sockets* (each world moved the OTHER side's units).
fn assert_non_vacuous(chain: &[u64], a: &Instance, b: &Instance, f: &Fixture) {
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(
        distinct.len() > 50,
        "hash chain suspiciously static ({} distinct of {})",
        distinct.len(),
        chain.len()
    );
    for (name, w) in [("A", &a.world), ("B", &b.world)] {
        let a1 = w.units.get(f.a1).expect("a1 alive");
        assert_eq!(
            a1.cell(),
            CellCoord::new(10, 55),
            "instance {name}: house-1 unit did not reach its scripted dest"
        );
        let b2 = w.units.get(f.b2).expect("b2 alive");
        assert_eq!(
            b2.cell(),
            CellCoord::new(5, 5),
            "instance {name}: house-2 unit did not reach its scripted dest"
        );
    }
}

// ---------------------------------------------------------------------------
// (b) the M8-A scripted battery over real sockets
// ---------------------------------------------------------------------------

/// Proof (b, scripted half): the exact M8-A scenario, but every command and
/// hash crosses a real UDP socket pair. Chains must match tick for tick.
#[test]
fn scripted_lockstep_over_real_udp_is_hash_identical() {
    let (ta, tb) = direct_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(&mut a, &mut b, TICKS, 200_000, |_| {}, scripted_submit(&f))
        .expect("clean localhost sockets must never stall out");
    assert_non_vacuous(&chain, &a, &b, &f);
    assert_eq!(chain.len(), TICKS as usize);
    assert_eq!(a.tp.decode_errors(), 0, "clean run must decode everything");
    assert_eq!(b.tp.decode_errors(), 0);
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
    assert!(a.tp.connection_lost().is_none() && b.tp.connection_lost().is_none());
}

// ---------------------------------------------------------------------------
// (c) loss injection
// ---------------------------------------------------------------------------

/// Proof (c1): a deterministically lossy link (25% drops, seeded pattern).
/// The redundant-bundle carry keeps the game advancing and hash-identical;
/// the drop counter proves losses genuinely happened (non-vacuity).
#[test]
fn loss_injection_recovers_and_stays_hash_identical() {
    let (ta, tb, mut proxy) = proxied_pair(DEFAULT_INPUT_DELAY, 0x10_55_10_55, 25);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(
        &mut a,
        &mut b,
        300,
        400_000,
        |_| proxy.pump(),
        scripted_submit(&f),
    )
    .expect("redundant carry + NACK must ride out 25% loss");
    assert_eq!(chain.len(), 300);
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(distinct.len() > 50, "sim did not really advance");
    assert!(
        proxy.dropped > 100,
        "only {} drops — the lossy proxy was vacuous",
        proxy.dropped
    );
    assert!(
        proxy.forwarded > 300,
        "only {} forwards — traffic did not flow",
        proxy.forwarded
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

/// Proof (c2): a total blackout longer than the redundancy window. Both
/// endpoints drain the bundles already in flight (at most the input-delay
/// window), stall at the barrier, and NACK into the void; when the link
/// returns, the NACK backstop re-requests the lost run and the game resumes,
/// still hash-identical.
///
/// Resumable tick runner: submissions are guarded by `submitted_through` so
/// a tick interrupted by the blackout is not double-submitted on resume.
struct BurstDriver {
    a: Instance,
    b: Instance,
    f: Fixture,
    submitted_through: u32,
    chain: Vec<u64>,
}

impl BurstDriver {
    /// Run ticks `from..to`; Err(t) if tick `t` cannot complete within
    /// `spin_cap` polls.
    fn run(
        &mut self,
        from: u32,
        to: u32,
        spin_cap: u32,
        proxy: &mut LossyProxy,
    ) -> Result<(), u32> {
        let start = Instant::now();
        for t in from..to {
            if t >= self.submitted_through {
                for c in script_a(&self.f, t) {
                    self.a.tp.submit(c);
                }
                for c in script_b(&self.f, t) {
                    self.b.tp.submit(c);
                }
                self.submitted_through = t + 1;
            }
            let mut ba = None;
            let mut bb = None;
            let mut spins = 0u32;
            while ba.is_none() || bb.is_none() {
                proxy.pump();
                if ba.is_none() {
                    match self.a.tp.poll(t) {
                        PollResult::Ready(x) => ba = Some(x),
                        PollResult::Waiting => {}
                        other => panic!("A at tick {t}: unexpected {other:?}"),
                    }
                } else {
                    self.a.tp.service();
                }
                if bb.is_none() {
                    match self.b.tp.poll(t) {
                        PollResult::Ready(x) => bb = Some(x),
                        PollResult::Waiting => {}
                        other => panic!("B at tick {t}: unexpected {other:?}"),
                    }
                } else {
                    self.b.tp.service();
                }
                spins += 1;
                if spins >= spin_cap {
                    return Err(t);
                }
                assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
            }
            let ba = ba.unwrap();
            let bb = bb.unwrap();
            assert_eq!(ba, bb, "bundle mismatch at tick {t}");
            let ha = self.a.world.tick(&ba.flatten());
            let hb = self.b.world.tick(&bb.flatten());
            assert_eq!(ha, hb, "hash mismatch at tick {t}");
            self.a.tp.report_hash(t, ha);
            self.b.tp.report_hash(t, hb);
            self.chain.push(ha);
        }
        Ok(())
    }
}

#[test]
fn burst_loss_beyond_redundancy_window_recovers_via_nack() {
    let (ta, tb, mut proxy) = proxied_pair(DEFAULT_INPUT_DELAY, 0xB1AC_0007, 0);
    let (a, b, f) = scripted_instances(ta, tb);
    let mut d = BurstDriver {
        a,
        b,
        f,
        submitted_through: 0,
        chain: Vec::new(),
    };

    // Phase 1: 50 clean ticks.
    d.run(0, 50, 200_000, &mut proxy).expect("clean phase");
    assert_eq!(d.chain.len(), 50);

    // Phase 2: blackout. In-flight bundles cover at most the input-delay
    // window, so the run must stall within a few ticks of 50 — well beyond
    // the redundancy window in lost datagrams — while NACK attempts fire
    // into the void.
    proxy.blackout = true;
    let nacks_before = d.a.tp.nacks_sent() + d.b.tp.nacks_sent();
    let stalled_at = d
        .run(50, 120, 3_000, &mut proxy)
        .expect_err("a total blackout must stall the barrier");
    assert!(
        stalled_at <= 50 + DEFAULT_INPUT_DELAY + 1,
        "stall must come within the in-flight window (stalled at {stalled_at})"
    );
    let nacks_during = d.a.tp.nacks_sent() + d.b.tp.nacks_sent() - nacks_before;
    assert!(
        nacks_during > 5,
        "expected NACK attempts during the blackout, got {nacks_during}"
    );
    let dropped = proxy.dropped;
    assert!(
        dropped > REDUNDANT_TICKS as u64,
        "burst must exceed the redundancy window ({dropped} dropped)"
    );

    // Phase 3: link restored — the next NACK exchange heals both directions
    // and the run continues, hash-identical, to tick 120.
    proxy.blackout = false;
    d.run(stalled_at, 120, 400_000, &mut proxy)
        .expect("NACK recovery must un-stall the session");
    assert_eq!(d.chain.len(), 120);
    assert!(
        d.a.tp.nacks_answered() + d.b.tp.nacks_answered() > 0,
        "recovery must have flowed through the NACK answer path"
    );
    assert!(d.a.tp.desync().is_none() && d.b.tp.desync().is_none());
    // Non-vacuity: the sim genuinely advanced through and past the outage
    // (120 ticks is too short for the scripted end-positions; the 450-tick
    // runs pin those).
    let distinct: std::collections::BTreeSet<u64> = d.chain.iter().copied().collect();
    assert!(distinct.len() > 40, "sim did not really advance");
}

// ---------------------------------------------------------------------------
// (g) revert sensitivity
// ---------------------------------------------------------------------------

/// Proof (g): disable the redundant carry (window = 1) AND the NACK
/// backstop, keep the same seeded 25% loss — the run must stall (a dropped
/// bundle is then simply never re-delivered). This is the revert-drill that
/// proves the loss machinery of (c1) is load-bearing.
#[test]
fn revert_drill_without_redundancy_and_nack_the_lossy_run_stalls() {
    let (mut ta, mut tb, mut proxy) = proxied_pair(DEFAULT_INPUT_DELAY, 0x10_55_10_55, 25);
    ta.set_loss_recovery_for_test(1, false);
    tb.set_loss_recovery_for_test(1, false);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    // Modest spin cap: a genuine stall means no progress no matter how long
    // we spin (nothing re-sends), so 30k spins on one tick is decisive.
    let r = drive(
        &mut a,
        &mut b,
        300,
        30_000,
        |_| proxy.pump(),
        scripted_submit(&f),
    );
    let stalled_at = r.expect_err(
        "without redundant carry and NACK, a 25% lossy link must stall the barrier \
         — if this completes, the loss-injection test has lost its teeth",
    );
    assert!(
        proxy.dropped > 0,
        "the stall must come from real drops (got none)"
    );
    // The stall is at or after the input-delay horizon (ticks below the
    // delay need no peer bundle by protocol definition).
    assert!(stalled_at >= DEFAULT_INPUT_DELAY, "stalled at {stalled_at}");
}

// ---------------------------------------------------------------------------
// (e) disconnect vs desync
// ---------------------------------------------------------------------------

/// Proof (e1): kill one endpoint mid-game — the survivor gets
/// `ConnectionLost` with `LostReason::Timeout`, and **not** `Desync` (the
/// two verdicts must stay distinguishable).
#[test]
fn peer_disconnect_raises_connection_lost_not_desync() {
    let (ta, tb) = direct_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(&mut a, &mut b, 30, 200_000, |_| {}, scripted_submit(&f))
        .expect("clean phase must complete");
    assert_eq!(chain.len(), 30);

    // B vanishes without a word (process kill / cable pull).
    a.tp.set_peer_timeout(Duration::from_millis(150));
    drop(b);

    let start = Instant::now();
    let lost = loop {
        // B's final in-flight bundles may still complete a few ticks; always
        // poll the transport's own current tick (sequential-poll contract).
        let t = a.tp.current_tick();
        match a.tp.poll(t) {
            PollResult::Waiting => {}
            PollResult::ConnectionLost(l) => break l,
            PollResult::Desync(d) => {
                panic!("a dead peer must surface as ConnectionLost, not Desync ({d:?})")
            }
            PollResult::Ready(b) => {
                let h = a.world.tick(&b.flatten());
                a.tp.report_hash(t, h);
                continue;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "timeout never latched"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(lost.reason, LostReason::Timeout);
    assert!(
        a.tp.desync().is_none(),
        "desync must not be latched by a disconnect"
    );
    // Sticky: every later poll returns the same state.
    assert!(matches!(
        a.tp.poll(lost.tick),
        PollResult::ConnectionLost(l) if l == lost
    ));
}

/// Proof (e2): a clean quit surfaces `ConnectionLost` with
/// `LostReason::PeerQuit` — "player left", distinguishable from both the
/// timeout and a desync.
#[test]
fn clean_quit_surfaces_player_left() {
    let (ta, tb) = direct_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    drive(&mut a, &mut b, 30, 200_000, |_| {}, scripted_submit(&f))
        .expect("clean phase must complete");

    b.tp.send_quit();
    let start = Instant::now();
    let lost = loop {
        let t = a.tp.current_tick();
        match a.tp.poll(t) {
            PollResult::ConnectionLost(l) => break l,
            PollResult::Waiting => {}
            PollResult::Ready(bundle) => {
                let h = a.world.tick(&bundle.flatten());
                a.tp.report_hash(t, h);
                continue;
            }
            PollResult::Desync(d) => panic!("quit must not read as desync ({d:?})"),
        }
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
    };
    assert_eq!(lost.reason, LostReason::PeerQuit);
    assert!(a.tp.desync().is_none());
}

// ---------------------------------------------------------------------------
// (d) handshake negatives
// ---------------------------------------------------------------------------

fn ephemeral_discovery() -> DiscoveryConfig {
    // No fixed ports anywhere: announcements go nowhere unless a test
    // explicitly aims them at a bound browser.
    DiscoveryConfig {
        announce_targets: Vec::new(),
        listen_port: 0,
    }
}

fn test_settings() -> SessionSettings {
    SessionSettings {
        map: "scm01ea.ini".to_string(),
        seed: 0xD00D_F00D,
        credits: 5000,
        host_seat: 1,
        join_seat: 2,
        delay: DEFAULT_INPUT_DELAY,
    }
}

/// Proof (d1): a JOIN speaking a different **protocol** version is answered
/// with a clean Reject and never seats the joiner.
#[test]
fn protocol_version_mismatch_join_is_rejected() {
    let mut host = HostLobby::create("HOSTY", test_settings(), &ephemeral_discovery()).unwrap();
    let host_addr: SocketAddr = format!("127.0.0.1:{}", host.port()).parse().unwrap();

    let probe = loopback_socket();
    let join = Datagram::Join {
        game_version: GAME_VERSION,
        name: "EVIL".to_string(),
    };
    let bytes = wire::encode_with_protocol(&join, PROTOCOL_VERSION + 1);
    probe.send_to(&bytes, host_addr).unwrap();

    let start = Instant::now();
    let mut buf = [0u8; 2048];
    let reply = loop {
        host.poll();
        match probe.recv_from(&mut buf) {
            Ok((n, src)) => {
                assert_eq!(src, host_addr);
                break wire::decode(&buf[..n]).expect("host reply must decode");
            }
            Err(_) => {
                assert!(
                    start.elapsed() < Duration::from_secs(WALL_SECS),
                    "host never answered the mismatched JOIN"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(
        reply,
        Datagram::Reject {
            reason: RejectReason::ProtocolVersion
        }
    );
    assert!(
        host.joiner_name().is_none(),
        "mismatch must not seat anyone"
    );
}

/// Proof (d2): same protocol, different **game** version → Reject with the
/// game-version reason (the startup scenario-CRC compare analogue).
#[test]
fn game_version_mismatch_join_is_rejected() {
    let mut host = HostLobby::create("HOSTY", test_settings(), &ephemeral_discovery()).unwrap();
    let host_addr: SocketAddr = format!("127.0.0.1:{}", host.port()).parse().unwrap();

    let probe = loopback_socket();
    let join = Datagram::Join {
        game_version: GAME_VERSION ^ 0xFF,
        name: "OLDBUILD".to_string(),
    };
    probe.send_to(&wire::encode(&join), host_addr).unwrap();

    let start = Instant::now();
    let mut buf = [0u8; 2048];
    let reply = loop {
        host.poll();
        match probe.recv_from(&mut buf) {
            Ok((n, _)) => break wire::decode(&buf[..n]).expect("host reply must decode"),
            Err(_) => {
                assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(
        reply,
        Datagram::Reject {
            reason: RejectReason::GameVersion
        }
    );
    assert!(host.joiner_name().is_none());
}

/// Proof (d3): joining a dead port times out cleanly — an error state within
/// the configured timeout, never a hang.
#[test]
fn join_to_dead_port_times_out_cleanly() {
    // Grab a port the OS just released: bind, read, drop. Nothing listens.
    let dead_addr = {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap()
    };
    let mut join = JoinLobby::join(dead_addr, "NOBODY").unwrap();
    join.set_timeout(Duration::from_millis(200));
    let start = Instant::now();
    while join.error().is_none() {
        join.poll();
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "dead-port join never surfaced its timeout"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let msg = join.error().unwrap();
    assert!(
        msg.contains("timed out"),
        "expected a timeout message, got: {msg}"
    );
    assert!(!join.started());
}

// ---------------------------------------------------------------------------
// Full ra-net-level lobby flow ending in lockstep
// ---------------------------------------------------------------------------

/// The whole P2 pipeline at the ra-net level: announce → browse → join →
/// welcome (settings authority crosses) → ready (both-confirm) → start →
/// a real 100-tick lockstep run, hash-identical. All localhost, all
/// OS-assigned ports.
#[test]
fn full_lobby_flow_announce_join_ready_start_then_lockstep() {
    // Browser first (OS-assigned port), then aim the host's announcements at
    // it — the test-safe inversion of the fixed-port broadcast.
    let browser_cfg = ephemeral_discovery();
    let mut browser = SessionBrowser::bind(&browser_cfg).unwrap();
    let host_cfg = DiscoveryConfig {
        announce_targets: vec![format!("127.0.0.1:{}", browser.port()).parse().unwrap()],
        listen_port: 0,
    };
    let settings = test_settings();
    let mut host = HostLobby::create("HOSTPLAYER", settings.clone(), &host_cfg).unwrap();

    // Discovery.
    let start = Instant::now();
    let session = loop {
        host.poll();
        browser.poll();
        if let Some(s) = browser.sessions().first() {
            break s.clone();
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "announcement never reached the browser"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(session.compatible);
    assert_eq!(session.map, settings.map);
    assert_eq!(session.name, "HOSTPLAYER");

    // Join → welcome.
    let mut join = JoinLobby::join(session.addr, "JOINER").unwrap();
    let start = Instant::now();
    while join.welcome().is_none() {
        host.poll();
        join.poll();
        assert!(join.error().is_none(), "join failed: {:?}", join.error());
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(2));
    }
    {
        let w = join.welcome().unwrap();
        assert_eq!(w.map, settings.map);
        assert_eq!(w.seed, settings.seed);
        assert_eq!(w.credits, settings.credits);
        assert_eq!(w.seat, settings.join_seat);
        assert_eq!(w.host_seat, settings.host_seat);
        assert_eq!(w.delay, settings.delay);
        assert_eq!(w.host_name, "HOSTPLAYER");
    }
    assert_eq!(host.joiner_name(), Some("JOINER"));
    assert!(!host.can_start(), "START must wait for the joiner's READY");

    // Ready (both-confirm) → start.
    join.set_ready();
    let start = Instant::now();
    while !host.can_start() {
        host.poll();
        join.poll();
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(2));
    }
    let ta = host.start().unwrap();
    let start = Instant::now();
    while !join.started() {
        join.poll();
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(2));
    }
    let tb = join.into_transport().unwrap();

    // And now the actual game: the scripted lockstep run over the very
    // sockets the lobby handed us.
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(&mut a, &mut b, 100, 200_000, |_| {}, scripted_submit(&f))
        .expect("post-lobby lockstep must run clean");
    assert_eq!(chain.len(), 100);
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(distinct.len() > 20, "sim did not really advance");
}
