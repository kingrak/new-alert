//! M8-B depth audit, items 2/3/5: loss/reorder/duplication torture beyond
//! `lan_lockstep.rs`'s existing proxy tests, revert-sensitivity drills that
//! isolate NACK from redundant carry (proving the "layering" claim in
//! `lan.rs`'s module docs actually holds), and bounded-wall-clock timeout /
//! keepalive timing semantics (freeze-past-timeout vs. resume-just-under).
//!
//! Every spin loop carries a spin cap *and* a wall-clock guard (M7.20
//! lesson), same discipline as `lan_lockstep.rs`.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram};
use ra_net::{
    CommandTransport, LanTransport, LostReason, PollResult, DEFAULT_INPUT_DELAY, REDUNDANT_TICKS,
};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, RandomLcg, World};

const WALL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// Fixture (mirrors ra-net/tests/lan_lockstep.rs).
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
    let mut world = World::new(Passability::all_passable(), 0xC0FF_EE01);
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
        _ => {}
    }
    v
}

fn loopback_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

fn direct_pair(delay: u32) -> (LanTransport, LanTransport) {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let aa = sa.local_addr().unwrap();
    let ab = sb.local_addr().unwrap();
    let ta = LanTransport::new(sa, ab, 1, 2, delay, true).unwrap();
    let tb = LanTransport::new(sb, aa, 2, 1, delay, false).unwrap();
    (ta, tb)
}

struct Instance {
    world: World,
    tp: LanTransport,
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

/// Same shape as `lan_lockstep.rs`'s `drive`: submit, poll both sides to
/// `Ready` (stalling on `Waiting`, pumping the supplied medium between
/// polls), tick both worlds, exchange hashes. `Err(t)` if tick `t` could not
/// complete within `spin_cap` polls.
#[allow(clippy::too_many_arguments)]
fn drive(
    a: &mut Instance,
    b: &mut Instance,
    ticks: u32,
    spin_cap: u32,
    mut pump: impl FnMut(),
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
            pump();
            if bundle_a.is_none() {
                match a.tp.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("A desynced at tick {t}: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("A lost peer at tick {t}: {l:?}"),
                }
            } else {
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

fn assert_non_vacuous(chain: &[u64], min_distinct: usize) {
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(
        distinct.len() >= min_distinct,
        "hash chain suspiciously static ({} distinct of {})",
        distinct.len(),
        chain.len()
    );
}

// ---------------------------------------------------------------------------
// Proxies
// ---------------------------------------------------------------------------

/// A clean (lossless, non-reordering) relay that also exposes direct
/// injection into either side's "wire" — used by the stale-replay test to
/// impersonate the peer after the fact.
struct CleanProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    forwarded: u64,
}

impl CleanProxy {
    fn new(a_real: SocketAddr, b_real: SocketAddr) -> CleanProxy {
        CleanProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            forwarded: 0,
        }
    }
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            let _ = self.b_side.send_to(&buf[..n], self.b_real);
            self.forwarded += 1;
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            let _ = self.a_side.send_to(&buf[..n], self.a_real);
            self.forwarded += 1;
        }
    }
    /// Send raw bytes to A, impersonating B (i.e. from the address A treats
    /// as its peer).
    fn inject_to_a(&self, bytes: &[u8]) {
        self.a_side
            .send_to(bytes, self.a_real)
            .expect("inject to A");
    }
}

fn clean_pair(delay: u32) -> (LanTransport, LanTransport, CleanProxy) {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let proxy = CleanProxy::new(a_real, b_real);
    let ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, delay, true).unwrap();
    let tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, delay, false).unwrap();
    (ta, tb, proxy)
}

/// Every forwarded datagram is delivered **twice** (0% loss otherwise).
struct DuplicatingProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    duplicated_pairs: u64,
}

impl DuplicatingProxy {
    fn new(a_real: SocketAddr, b_real: SocketAddr) -> DuplicatingProxy {
        DuplicatingProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            duplicated_pairs: 0,
        }
    }
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            let _ = self.b_side.send_to(&buf[..n], self.b_real);
            let _ = self.b_side.send_to(&buf[..n], self.b_real);
            self.duplicated_pairs += 1;
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            let _ = self.a_side.send_to(&buf[..n], self.a_real);
            let _ = self.a_side.send_to(&buf[..n], self.a_real);
            self.duplicated_pairs += 1;
        }
    }
}

/// Buffers datagrams per direction and releases them in **reverse** arrival
/// order once enough have accumulated across both directions combined (or a
/// pump-call staleness bound is hit, guaranteeing eventual delivery so the
/// run can always finish) — reordering well beyond the redundant-carry
/// window, with zero loss.
struct ReorderingProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    buf_a_to_b: Vec<Vec<u8>>,
    buf_b_to_a: Vec<Vec<u8>>,
    batch: usize,
    stale_calls: u32,
    calls_since_flush: u32,
    reordered: u64,
}

impl ReorderingProxy {
    fn new(a_real: SocketAddr, b_real: SocketAddr, batch: usize) -> ReorderingProxy {
        ReorderingProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            buf_a_to_b: Vec::new(),
            buf_b_to_a: Vec::new(),
            batch,
            stale_calls: 40,
            calls_since_flush: 0,
            reordered: 0,
        }
    }
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            self.buf_a_to_b.push(buf[..n].to_vec());
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            self.buf_b_to_a.push(buf[..n].to_vec());
        }
        self.calls_since_flush += 1;
        let total = self.buf_a_to_b.len() + self.buf_b_to_a.len();
        if total >= self.batch || (total > 0 && self.calls_since_flush >= self.stale_calls) {
            self.reordered += total as u64;
            self.calls_since_flush = 0;
            for pkt in self.buf_a_to_b.drain(..).rev() {
                let _ = self.b_side.send_to(&pkt, self.b_real);
            }
            for pkt in self.buf_b_to_a.drain(..).rev() {
                let _ = self.a_side.send_to(&pkt, self.a_real);
            }
        }
    }
}

/// Independent, differently-seeded drop rates per direction.
struct AsymmetricLossyProxy {
    a_side: UdpSocket,
    b_side: UdpSocket,
    a_real: SocketAddr,
    b_real: SocketAddr,
    rng: RandomLcg,
    /// A → B drop percent.
    drop_a_to_b: i32,
    /// B → A drop percent.
    drop_b_to_a: i32,
    dropped_a_to_b: u64,
    dropped_b_to_a: u64,
    forwarded: u64,
}

impl AsymmetricLossyProxy {
    fn new(
        a_real: SocketAddr,
        b_real: SocketAddr,
        seed: u32,
        drop_a_to_b: i32,
        drop_b_to_a: i32,
    ) -> AsymmetricLossyProxy {
        AsymmetricLossyProxy {
            a_side: loopback_socket(),
            b_side: loopback_socket(),
            a_real,
            b_real,
            rng: RandomLcg::new(seed),
            drop_a_to_b,
            drop_b_to_a,
            dropped_a_to_b: 0,
            dropped_b_to_a: 0,
            forwarded: 0,
        }
    }
    fn a_addr(&self) -> SocketAddr {
        self.a_side.local_addr().unwrap()
    }
    fn b_addr(&self) -> SocketAddr {
        self.b_side.local_addr().unwrap()
    }
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        while let Ok((n, _)) = self.a_side.recv_from(&mut buf) {
            if self.rng.range(0, 99) >= self.drop_a_to_b {
                let _ = self.b_side.send_to(&buf[..n], self.b_real);
                self.forwarded += 1;
            } else {
                self.dropped_a_to_b += 1;
            }
        }
        while let Ok((n, _)) = self.b_side.recv_from(&mut buf) {
            if self.rng.range(0, 99) >= self.drop_b_to_a {
                let _ = self.a_side.send_to(&buf[..n], self.a_real);
                self.forwarded += 1;
            } else {
                self.dropped_b_to_a += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// (2a) Duplicated datagrams: every packet delivered twice.
// ---------------------------------------------------------------------------

#[test]
fn duplicated_datagrams_dedup_and_stay_hash_identical() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut proxy = DuplicatingProxy::new(a_real, b_real);
    let ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    let chain = drive(
        &mut a,
        &mut b,
        200,
        200_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("every-packet-doubled run must complete: dedup must hold");
    assert_eq!(chain.len(), 200);
    assert_non_vacuous(&chain, 20);
    assert!(
        proxy.duplicated_pairs > 100,
        "only {} duplicated forwards — the duplicating proxy was vacuous",
        proxy.duplicated_pairs
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
    // Idempotency, concretely: both sides' hash chains match tick for tick
    // (already enforced inside `drive`), and no decode errors — a duplicate
    // is a perfectly valid datagram, just redundant.
    assert_eq!(a.tp.decode_errors(), 0);
    assert_eq!(b.tp.decode_errors(), 0);
}

// ---------------------------------------------------------------------------
// (2b) Reordering beyond the redundancy window.
// ---------------------------------------------------------------------------

#[test]
fn reordering_beyond_redundancy_window_stays_hash_identical() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    // Batch size well beyond REDUNDANT_TICKS: packets get held and released
    // out of send order across a span wider than the window a single
    // redundant-carry datagram could cover on its own.
    let batch = (REDUNDANT_TICKS as usize) * 3;
    let mut proxy = ReorderingProxy::new(a_real, b_real, batch);
    let ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    let chain = drive(
        &mut a,
        &mut b,
        200,
        400_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("pure reordering (no loss) must never stall the barrier");
    assert_eq!(chain.len(), 200);
    assert_non_vacuous(&chain, 20);
    assert!(
        proxy.reordered > 100,
        "only {} packets went through the reorder buffer — the proxy was vacuous",
        proxy.reordered
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

// ---------------------------------------------------------------------------
// (2c) Asymmetric loss: one direction 40%, the other clean.
// ---------------------------------------------------------------------------

#[test]
fn asymmetric_loss_one_direction_heavy_other_clean_recovers() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    // A→B clean, B→A 40% loss.
    let mut proxy = AsymmetricLossyProxy::new(a_real, b_real, 0xA5A5_0001, 0, 40);
    let ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    let chain = drive(
        &mut a,
        &mut b,
        300,
        400_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("asymmetric loss must still recover via redundant carry + NACK");
    assert_eq!(chain.len(), 300);
    assert_non_vacuous(&chain, 40);
    assert!(
        proxy.dropped_b_to_a > 50,
        "only {} B→A drops — the asymmetric loss was too light to be a real test",
        proxy.dropped_b_to_a
    );
    assert_eq!(proxy.dropped_a_to_b, 0, "A→B was supposed to be clean");
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

// ---------------------------------------------------------------------------
// (2d) Stale-datagram replay: re-delivering long-past ticks must be ignored.
// ---------------------------------------------------------------------------

#[test]
fn stale_datagram_replay_is_ignored_not_reexecuted() {
    let (ta, tb, mut proxy) = clean_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    // Clean run through a fair number of ticks, well beyond SENT_KEEP_TICKS
    // (64) past any tick we're about to replay.
    let chain1 = drive(
        &mut a,
        &mut b,
        90,
        200_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("clean phase must complete");
    assert_eq!(chain1.len(), 90);
    assert!(a.tp.current_tick() >= 90);

    // Replay a stale BUNDLES for tick 5 carrying a command that, if it were
    // ever (re-)applied, would move a1 somewhere the real run never sent it —
    // impersonating B, straight to A's socket.
    let bogus_move = Command::Move {
        unit: f.a1,
        dest: CellCoord::new(1, 1),
        house: 1,
    };
    let stale_bundles = Datagram::Bundles {
        entries: vec![(5, vec![bogus_move])],
    };
    proxy.inject_to_a(&wire::encode(&stale_bundles));

    // Replay a stale HASHES for the same long-past tick with a *deliberately
    // wrong* hash — if the stale guard were missing, this could falsely latch
    // Desync even though the real tick-5 hashes agreed ages ago.
    let stale_hashes = Datagram::Hashes {
        entries: vec![(5, 0xBAD_BAD_BAD_BAD_BADu64)],
    };
    proxy.inject_to_a(&wire::encode(&stale_hashes));

    // Drain without advancing the protocol tick (service(), not poll()) so
    // the injected datagrams are actually processed before we check state.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        a.tp.service();
        proxy.pump();
    }

    assert!(
        a.tp.desync().is_none(),
        "a stale, deliberately-wrong hash replay must not latch a false Desync"
    );
    assert!(a.tp.connection_lost().is_none());

    // The run must continue normally afterward, hash-identical, proving the
    // stale bundle was never (re-)applied (a re-application would have moved
    // a1 to (1,1) at some point and/or produced a bundle mismatch panic
    // inside `drive`, which asserts equality every tick).
    let chain2 = drive_clean_range(&mut a, &mut b, 90, 95, 200_000, Some(&mut proxy))
        .expect("session must continue cleanly after the stale replay");
    assert_eq!(chain2.len(), 5);
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

/// Small helper overload used only by the stale-replay test above: drive a
/// specific `from..to` tick range (the plain `drive` always starts at 0).
#[allow(clippy::too_many_arguments)]
fn drive_clean_range(
    a: &mut Instance,
    b: &mut Instance,
    from: u32,
    to: u32,
    spin_cap: u32,
    proxy: Option<&mut CleanProxy>,
) -> Result<Vec<u64>, u32> {
    let start = Instant::now();
    let mut chain = Vec::new();
    let mut proxy = proxy;
    for t in from..to {
        let mut bundle_a = None;
        let mut bundle_b = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            if let Some(p) = proxy.as_deref_mut() {
                p.pump();
            }
            if bundle_a.is_none() {
                match a.tp.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("A desynced at tick {t}: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("A lost peer at tick {t}: {l:?}"),
                }
            } else {
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
            assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
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

// ---------------------------------------------------------------------------
// (3a) Revert drill: disable NACK alone (redundant carry intact) — blackout
// recovery must fail.
// ---------------------------------------------------------------------------

#[test]
fn revert_drill_nack_disabled_alone_blackout_recovery_fails() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut proxy = AsymmetricLossyProxy::new(a_real, b_real, 0xB1AC_0099, 0, 0);
    let mut ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    // Redundant carry stays at its default (>1); NACK alone is disabled.
    ta.set_loss_recovery_for_test(REDUNDANT_TICKS, false);
    tb.set_loss_recovery_for_test(REDUNDANT_TICKS, false);
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    // Clean phase.
    let chain = drive(
        &mut a,
        &mut b,
        30,
        200_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("clean phase must complete");
    assert_eq!(chain.len(), 30);

    // Now a total blackout for longer than the redundancy window: nothing
    // gets through, and — because NACK is the *only* mechanism that ever
    // retransmits while stalled — once every in-flight redundant copy is
    // exhausted, neither side ever sends anything new for the stalled tick,
    // so lifting the blackout changes nothing. This must stay stalled
    // forever (bounded here by a spin cap, not a real hang).
    proxy.drop_a_to_b = 100;
    proxy.drop_b_to_a = 100;
    let mut submitted_through = 30u32;
    let mut submit_from = |t: u32, ia: &mut Instance, ib: &mut Instance| {
        if t >= submitted_through {
            for c in script_a(&f, t) {
                ia.tp.submit(c);
            }
            for c in script_b(&f, t) {
                ib.tp.submit(c);
            }
            submitted_through = t + 1;
        }
    };
    let r = drive_range(
        &mut a,
        &mut b,
        30,
        200,
        20_000,
        || proxy.pump(),
        &mut submit_from,
    );
    let stalled_at = r.expect_err(
        "with NACK disabled and no other retransmit path, a total blackout must never \
         self-heal even after it lifts — if this completes, the layering claim in lan.rs's \
         module docs (redundant carry vs. NACK backstop) is wrong",
    );

    // Lift the blackout and try to make more progress — must still fail:
    // nothing on either side ever proactively resends while stalled once
    // NACK is off.
    proxy.drop_a_to_b = 0;
    proxy.drop_b_to_a = 0;
    let r2 = drive_range(
        &mut a,
        &mut b,
        stalled_at,
        stalled_at + 50,
        20_000,
        || proxy.pump(),
        &mut submit_from,
    );
    assert!(
        r2.is_err(),
        "recovery after lifting the blackout must still fail without NACK — got {r2:?}"
    );
}

// ---------------------------------------------------------------------------
// (3b) Revert drill: shrink REDUNDANT_TICKS to 1, keep NACK — burst loss
// must still recover (proves NACK alone, without a meaningful carry window,
// really does the backstop job the docs claim).
// ---------------------------------------------------------------------------

#[test]
fn revert_drill_redundant_carry_shrunk_to_one_nack_intact_still_recovers() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut proxy = AsymmetricLossyProxy::new(a_real, b_real, 0xB1AC_0007, 0, 0);
    let mut ta = LanTransport::new(sa, proxy.a_addr(), 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, proxy.b_addr(), 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    ta.set_loss_recovery_for_test(1, true);
    tb.set_loss_recovery_for_test(1, true);
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    let chain = drive(
        &mut a,
        &mut b,
        30,
        200_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("clean phase must complete");
    assert_eq!(chain.len(), 30);

    // Total blackout, then lift it: with carry=1 the redundant window can't
    // help at all, so recovery — if it happens — must come entirely from
    // NACK re-sending `sent_bundles`/`sent_hashes` history (which is kept
    // independent of `carry`, see `answer_nack`).
    proxy.drop_a_to_b = 100;
    proxy.drop_b_to_a = 100;
    let mut submitted_through = 30u32;
    let mut submit_from = |t: u32, ia: &mut Instance, ib: &mut Instance| {
        if t >= submitted_through {
            for c in script_a(&f, t) {
                ia.tp.submit(c);
            }
            for c in script_b(&f, t) {
                ib.tp.submit(c);
            }
            submitted_through = t + 1;
        }
    };
    let stalled_at = drive_range(
        &mut a,
        &mut b,
        30,
        100,
        3_000,
        || proxy.pump(),
        &mut submit_from,
    )
    .expect_err("blackout must stall while active");

    proxy.drop_a_to_b = 0;
    proxy.drop_b_to_a = 0;
    let chain2 = drive_range(
        &mut a,
        &mut b,
        stalled_at,
        150,
        400_000,
        || proxy.pump(),
        &mut submit_from,
    )
    .expect("NACK alone (carry=1) must still recover once the blackout lifts");
    assert!(chain2.len() > 30, "must have made real additional progress");
    assert!(
        a.tp.nacks_answered() + b.tp.nacks_answered() > 0,
        "recovery must have flowed through the NACK answer path, not the (nearly useless) \
         carry=1 window"
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

/// `drive` overload taking `from..to` plus a submit closure, used by the two
/// revert drills above (they resume mid-run after a blackout).
#[allow(clippy::too_many_arguments)]
fn drive_range(
    a: &mut Instance,
    b: &mut Instance,
    from: u32,
    to: u32,
    spin_cap: u32,
    mut pump: impl FnMut(),
    mut submit: impl FnMut(u32, &mut Instance, &mut Instance),
) -> Result<Vec<u64>, u32> {
    let start = Instant::now();
    let mut chain = Vec::new();
    for t in from..to {
        submit(t, a, b);
        let mut bundle_a = None;
        let mut bundle_b = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            pump();
            if bundle_a.is_none() {
                match a.tp.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    other => panic!("A at tick {t}: unexpected {other:?}"),
                }
            } else {
                a.tp.service();
            }
            if bundle_b.is_none() {
                match b.tp.poll(t) {
                    PollResult::Ready(x) => bundle_b = Some(x),
                    PollResult::Waiting => {}
                    other => panic!("B at tick {t}: unexpected {other:?}"),
                }
            } else {
                b.tp.service();
            }
            spins += 1;
            if spins >= spin_cap {
                return Err(t);
            }
            assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
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

// ---------------------------------------------------------------------------
// (5a) Mid-game freeze past PEER_TIMEOUT: ConnectionLost at approximately
// the right time.
// ---------------------------------------------------------------------------

#[test]
fn mid_game_freeze_raises_connection_lost_at_approximately_peer_timeout() {
    let (ta, tb) = direct_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(&mut a, &mut b, 20, 200_000, || {}, scripted_submit(&f))
        .expect("clean phase must complete");
    assert_eq!(chain.len(), 20);

    let timeout = Duration::from_millis(400);
    a.tp.set_peer_timeout(timeout);

    // B freezes: we simply stop calling poll()/service() on it at all (its
    // socket is still bound and alive — this is "peer stopped responding",
    // not "peer process died", though the wire-level effect on A is
    // identical: silence).
    let freeze_start = Instant::now();
    let lost = loop {
        let t = a.tp.current_tick();
        match a.tp.poll(t) {
            PollResult::Waiting => {}
            PollResult::ConnectionLost(l) => break l,
            PollResult::Desync(d) => panic!("freeze must not read as desync: {d:?}"),
            PollResult::Ready(bundle) => {
                let h = a.world.tick(&bundle.flatten());
                a.tp.report_hash(t, h);
                continue;
            }
        }
        assert!(
            freeze_start.elapsed() < Duration::from_secs(WALL_SECS),
            "timeout never latched"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let elapsed = freeze_start.elapsed();

    assert_eq!(lost.reason, LostReason::Timeout);
    assert!(
        elapsed >= timeout,
        "latched before the configured timeout elapsed ({elapsed:?} < {timeout:?}) — a false \
         positive under normal jitter"
    );
    // Generous upper slack (poll cadence is a 5ms sleep in this harness, not
    // the production frame rate) — this must not be drastically late either.
    let slack = timeout * 5;
    assert!(
        elapsed < timeout + slack,
        "latched far later than the configured timeout ({elapsed:?}, timeout {timeout:?}) — \
         the keepalive-timeout check is not actually running promptly"
    );
    assert!(a.tp.desync().is_none());
}

// ---------------------------------------------------------------------------
// (5b) Resumed polling just under the timeout: no false ConnectionLost.
// ---------------------------------------------------------------------------

#[test]
fn resumed_polling_just_under_timeout_avoids_false_connection_lost() {
    let (ta, tb) = direct_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);
    let chain = drive(&mut a, &mut b, 20, 200_000, || {}, scripted_submit(&f))
        .expect("clean phase must complete");
    assert_eq!(chain.len(), 20);

    let timeout = Duration::from_millis(500);
    a.tp.set_peer_timeout(timeout);
    b.tp.set_peer_timeout(timeout);

    // B "freezes" (no poll/service calls at all) for comfortably under the
    // timeout, while A keeps polling. A may legitimately race a few ticks
    // ahead first — the redundant-carry window means B's *last* active send
    // already pre-delivered a handful of future ticks' worth of (empty)
    // bundles — but once that pre-buffered slack is exhausted A must settle
    // into genuine `Waiting` (the stall-overlay path) for the rest of the
    // freeze, and never `ConnectionLost` while still under the timeout.
    let freeze_for = Duration::from_millis(250); // 50% of the timeout
    let freeze_start = Instant::now();
    let mut saw_waiting = false;
    let mut a_ready_count = 0u32;
    while freeze_start.elapsed() < freeze_for {
        let t = a.tp.current_tick();
        match a.tp.poll(t) {
            PollResult::Waiting => saw_waiting = true,
            PollResult::ConnectionLost(l) => {
                panic!(
                    "false ConnectionLost while still under the timeout: {l:?} (elapsed {:?})",
                    freeze_start.elapsed()
                )
            }
            PollResult::Desync(d) => panic!("must not desync: {d:?}"),
            PollResult::Ready(bundle) => {
                // Pre-buffered redundant-carry tick: apply it and keep
                // going — this is expected for a few ticks, not a bug.
                let h = a.world.tick(&bundle.flatten());
                a.tp.report_hash(t, h);
                a_ready_count += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        saw_waiting,
        "expected the stall-overlay path (Waiting) once A exhausted B's pre-buffered ticks \
         (A advanced {a_ready_count} ticks on buffered data alone before this point)"
    );
    // The pre-buffer is bounded (input delay + redundant carry, both small
    // constants) — A must not have been able to run away indefinitely on
    // buffered data alone.
    assert!(
        a_ready_count < 50,
        "A advanced implausibly far ({a_ready_count} ticks) on B's frozen pre-buffer alone"
    );
    assert!(a.tp.connection_lost().is_none());

    // Resume B. The two transports' own tick counters may now differ (A ran
    // ahead on the pre-buffer); poll each at *its own* current tick — the
    // API's only ordering requirement — until both reach a common target
    // tick well past where the frozen A would have stalled forever without
    // B's cooperation.
    let target = a.tp.current_tick().max(b.tp.current_tick()) + 20;
    let start = Instant::now();
    while a.tp.current_tick() < target || b.tp.current_tick() < target {
        let ta = a.tp.current_tick();
        match a.tp.poll(ta) {
            PollResult::Ready(bundle) => {
                let h = a.world.tick(&bundle.flatten());
                a.tp.report_hash(ta, h);
            }
            PollResult::Waiting => a.tp.service(),
            other => panic!("A: unexpected {other:?} at tick {ta}"),
        }
        let tb = b.tp.current_tick();
        match b.tp.poll(tb) {
            PollResult::Ready(bundle) => {
                let h = b.world.tick(&bundle.flatten());
                b.tp.report_hash(tb, h);
            }
            PollResult::Waiting => b.tp.service(),
            other => panic!("B: unexpected {other:?} at tick {tb}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "resume after under-timeout freeze never converged (a={}, b={}, target={target})",
            a.tp.current_tick(),
            b.tp.current_tick()
        );
    }
    assert!(
        a.tp.connection_lost().is_none() && b.tp.connection_lost().is_none(),
        "resuming just under the timeout must never have latched ConnectionLost on either side"
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
}

// ---------------------------------------------------------------------------
// Coverage gap-fill: `decode_errors()` is asserted == 0 in every existing
// clean-run test, but nothing proves it actually increments when a live
// transport receives garbage — pin that the counter is genuinely load-bearing
// observability, not a dead field.
// ---------------------------------------------------------------------------

#[test]
fn garbage_datagrams_to_a_live_transport_increment_decode_errors_and_session_continues() {
    let (ta, tb, mut proxy) = clean_pair(DEFAULT_INPUT_DELAY);
    let (mut a, mut b, f) = scripted_instances(ta, tb);

    let chain1 = drive(
        &mut a,
        &mut b,
        20,
        200_000,
        || proxy.pump(),
        scripted_submit(&f),
    )
    .expect("clean phase must complete");
    assert_eq!(chain1.len(), 20);
    assert_eq!(a.tp.decode_errors(), 0);

    // Inject pure garbage, impersonating the peer, several times.
    let mut rng = RandomLcg::new(0x9A12_5BAD_u32 ^ 0xABCD);
    for _ in 0..10 {
        let len = 1 + rng.range(0, 40) as usize;
        let garbage: Vec<u8> = (0..len).map(|_| rng.range(0, 255) as u8).collect();
        proxy.inject_to_a(&garbage);
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        a.tp.service();
        proxy.pump();
    }
    assert!(
        a.tp.decode_errors() > 0,
        "decode_errors must have incremented for at least some of the 10 garbage injections"
    );
    assert!(a.tp.desync().is_none() && a.tp.connection_lost().is_none());

    // Session must still be fully usable afterward.
    let chain2 = drive_clean_range(&mut a, &mut b, 20, 50, 200_000, Some(&mut proxy))
        .expect("session must continue cleanly after absorbing garbage");
    assert_eq!(chain2.len(), 30);
}
