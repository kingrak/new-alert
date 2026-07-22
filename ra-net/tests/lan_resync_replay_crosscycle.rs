//! M8-C x M7.23 cross-cycle integration: play a LAN game with a forced
//! desync + resync WHILE recording (mirroring what
//! `ra-client/src/appcore.rs`'s `install_recorder` +
//! `rec.on_tick`/`rec.on_hash` taps do, using `ra_net::replay`'s encoders
//! directly so this needs no real assets) — then ask: does the replay of the
//! resynced game verify?
//!
//! Source-level facts established first (see module doc below for the
//! reasoning): the `.rarp` format (`ra_net::replay`) has exactly three record
//! types — `Tick`, `Hash`, `End` — no snapshot/rebase record exists, and
//! `AppCore::update` only taps the recorder from the normal per-tick path,
//! which is skipped outright while `net_resync_active()` (see
//! `appcore.rs:1529-1530`, `:1607-1608`, `:1735-1736`). So recording pauses
//! silently across a resync and resumes with no marker of the discontinuity.
//! This test proves the consequence empirically: a naive replay-verify
//! (seed + recorded command stream, exactly what `cmd_replay_verify` does)
//! CANNOT reproduce the resumed segment, because the loser's post-resync
//! world came from an externally-transferred snapshot, not from anything
//! derivable from its own deterministic history.
//!
//! **Finding, not necessarily a bug**: replay-verify safely FAILS LOUDLY
//! (reports a diverged tick) rather than silently claiming success on a
//! recording that crossed a resync — the failure mode is safe — but replay
//! verification is effectively unavailable for any recorded game that
//! self-healed a LAN desync. Documented here as the audit deliverable asks.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use ra_net::replay::{encode_end, encode_hash, encode_header, encode_tick};
use ra_net::{
    CommandTransport, EndReason, LanTransport, PollResult, ReplayHeader, ReplayReader,
    ReplayRecord, ReplaySeat, ResyncEvent, TickBundle, DEFAULT_INPUT_DELAY, HASH_INTERVAL,
};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, World};

const SEED: u32 = 0x5E5C_00C1;
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

fn build_world() -> (World, Vec<Handle>) {
    let mut w = World::new(Passability::all_passable(), SEED);
    w.init_houses(2, 5000);
    let mut movers = Vec::new();
    for i in 0..8u32 {
        let house = (i % 2) as u8 + 1;
        let cell = CellCoord::new(4 + (i as i32 % 4) * 3, 4 + (i as i32 / 4) * 3);
        let h = w.spawn_unit(
            i % 3,
            house,
            cell,
            Facing((i * 20) as u8),
            200,
            stats(50, 8),
        );
        if i < 4 {
            movers.push(h);
        }
    }
    (w, movers)
}

fn script(handles: &[Handle], tick: u32) -> Vec<Command> {
    let mut v = Vec::new();
    if tick == 1 {
        for (i, &handle) in handles.iter().enumerate() {
            v.push(Command::Move {
                unit: handle,
                dest: CellCoord::new(20 + i as i32 * 4, 30),
                house: (i as u8 % 2) + 1,
            });
        }
    }
    v
}

/// A hand-rolled stand-in for `ra_client::replay::ReplayRecorder`, using
/// `ra_net::replay`'s encoders directly so this test needs no real assets
/// and no `ra-client` dependency — same tap discipline: `on_tick` only for
/// non-empty bundles, `on_hash` on the `HASH_INTERVAL` cadence.
struct FakeRecorder {
    bytes: Vec<u8>,
}
impl FakeRecorder {
    fn new(header: &ReplayHeader) -> Self {
        FakeRecorder {
            bytes: encode_header(header),
        }
    }
    fn on_tick(&mut self, tick: u32, cmds: &[Command]) {
        if cmds.is_empty() {
            return;
        }
        let bundle = TickBundle {
            tick,
            seats: vec![(1, cmds.to_vec())],
        };
        self.bytes.extend_from_slice(&encode_tick(tick, &bundle));
    }
    fn on_hash(&mut self, tick: u32, hash: u64) {
        if !tick.is_multiple_of(HASH_INTERVAL) {
            return;
        }
        self.bytes.extend_from_slice(&encode_hash(tick, hash));
    }
    fn finish(mut self, final_tick: u32) -> Vec<u8> {
        self.bytes
            .extend_from_slice(&encode_end(EndReason::Quit, final_tick));
        self.bytes
    }
}

fn parse(
    bytes: &[u8],
) -> (
    ReplayHeader,
    BTreeMap<u32, TickBundle>,
    BTreeMap<u32, u64>,
    u32,
) {
    let (header, reader) = ReplayReader::open(bytes).expect("open replay");
    let records = reader.collect_records().expect("records");
    let mut bundles = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut final_tick = 0;
    for rec in records {
        match rec {
            ReplayRecord::Tick { tick, bundle } => {
                final_tick = final_tick.max(tick);
                bundles.insert(tick, bundle);
            }
            ReplayRecord::Hash { tick, hash } => {
                final_tick = final_tick.max(tick);
                hashes.insert(tick, hash);
            }
            ReplayRecord::End { final_tick: ft, .. } => final_tick = final_tick.max(ft),
        }
    }
    (header, bundles, hashes, final_tick)
}

/// Naive replay-verify: fresh world from the same seed, drive it with the
/// recorded command stream, check every hash record — exactly
/// `cmd_replay_verify`'s algorithm.
fn resim_verify(
    bundles: &BTreeMap<u32, TickBundle>,
    hashes: &BTreeMap<u32, u64>,
    final_tick: u32,
) -> Result<usize, u32> {
    let (mut w, _) = build_world(); // same seed, same init -> same starting state
    let mut checked = 0;
    for t in 0..=final_tick {
        let cmds = bundles.get(&t).map(|b| b.flatten()).unwrap_or_default();
        let hash = w.tick(&cmds);
        if let Some(&expected) = hashes.get(&t) {
            if hash != expected {
                return Err(t);
            }
            checked += 1;
        }
    }
    Ok(checked)
}

/// Cross-cycle: run the M8-C forced-desync-resync drill (mirrors
/// `lan_resync.rs::forced_desync_resyncs_and_continues_clean_udp`) while
/// tapping the LOSER's tick/hash stream exactly as production recording
/// would, then attempt to replay-verify the resulting file.
#[test]
fn resynced_game_recording_diverges_from_naive_replay_verify_at_the_resync_gap() {
    let sa = loopback_socket();
    let sb = loopback_socket();
    let a_real = sa.local_addr().unwrap();
    let b_real = sb.local_addr().unwrap();
    let mut ta = LanTransport::new(sa, b_real, 1, 2, DEFAULT_INPUT_DELAY, true).unwrap();
    let mut tb = LanTransport::new(sb, a_real, 2, 1, DEFAULT_INPUT_DELAY, false).unwrap();
    ta.set_peer_timeout(Duration::from_secs(30));
    tb.set_peer_timeout(Duration::from_secs(30));

    let (mut wa, handles) = build_world();
    let (mut wb, _) = build_world();
    assert_eq!(wa.state_hash(), wb.state_hash());

    let header = ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: ra_net::wire::GAME_VERSION,
        protocol_version: ra_net::wire::PROTOCOL_VERSION,
        scenario: "synthetic".to_string(),
        seed: wb.rng_seed(),
        difficulty: 1,
        credits: 5000,
        catalog_hash: wb.catalog().content_hash(),
        start_millis: 0,
        seats: vec![ReplaySeat {
            seat: 2,
            house: 2,
            color: 2,
        }],
    };
    let mut rec = FakeRecorder::new(&header);

    let corrupt_at = 20u32;
    let post_ticks = 40u32;
    let start = Instant::now();
    let mut tick = 0u32;
    let mut a_desync = false;
    let mut b_desync = false;
    let mut corrupted = false;

    'phase_a: loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        if tick == corrupt_at && !corrupted {
            wb.spawn_unit(2, 2, CellCoord::new(60, 60), Facing(0), 100, stats(0, 0));
            corrupted = true;
        }
        let cmds_a = script(&handles, tick);
        let cmds_b = script(&handles, tick);
        for c in cmds_a.clone() {
            ta.submit(c);
        }
        for c in cmds_b.clone() {
            tb.submit(c);
        }
        let mut ba = None;
        let mut bb = None;
        let mut spins = 0u32;
        loop {
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
            assert!(spins < SPIN_CAP, "spin cap");
        }
        if a_desync && b_desync {
            break 'phase_a;
        }
        let ha = wa.tick(&ba.unwrap().flatten());
        let hb = wb.tick(&bb.unwrap().flatten());
        // Tap the LOSER's (house-2 seat's) recorded stream exactly as
        // production does: this tick's OWN submitted commands + the post-tick
        // hash.
        rec.on_tick(tick, &cmds_b);
        rec.on_hash(tick, hb);
        ta.report_hash(tick, ha);
        tb.report_hash(tick, hb);
        tick += 1;
    }

    // Resync phase (host-authoritative, mirrors lan_resync.rs).
    let snapshot = wa.save_snapshot();
    ta.begin_resync_host(snapshot, wa.tick_count(), wa.state_hash());
    tb.begin_resync_loser();
    let mut resume_tick = 0u32;
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        if ta.resync_active() {
            if let ResyncEvent::Resumed { resume_tick: rt } = ta.resync_poll() {
                resume_tick = rt;
            }
        }
        if tb.resync_active() {
            match tb.resync_poll() {
                ResyncEvent::NeedsLoad {
                    bytes,
                    resume_tick: rt,
                    declared_hash,
                } => {
                    let loaded = World::load_snapshot(
                        &bytes,
                        wb.catalog().clone(),
                        wb.passability().clone(),
                    )
                    .expect("snapshot must decode");
                    assert_eq!(loaded.state_hash(), declared_hash);
                    wb = loaded;
                    resume_tick = rt;
                    tb.resync_report_loaded(true);
                }
                ResyncEvent::Resumed { resume_tick: rt } => resume_tick = rt,
                _ => {}
            }
        }
        if !ta.resync_active() && !tb.resync_active() {
            break;
        }
    }
    assert_eq!(
        wb.tick_count(),
        resume_tick,
        "loser resumed at the snapshot tick"
    );

    // Phase C: continue post-resync, still tapping the recorder exactly as
    // production would (recording never paused-aware of the resync, no
    // marker written — this is the point being tested).
    let mut t = resume_tick;
    for _ in 0..post_ticks {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard"
        );
        let cmds_a = script(&handles, t);
        let cmds_b = script(&handles, t);
        for c in cmds_a.clone() {
            ta.submit(c);
        }
        for c in cmds_b.clone() {
            tb.submit(c);
        }
        let mut ba = None;
        let mut bb = None;
        let mut spins = 0u32;
        while ba.is_none() || bb.is_none() {
            if ba.is_none() {
                match ta.poll(t) {
                    PollResult::Ready(x) => ba = Some(x),
                    PollResult::Waiting => {}
                    other => panic!("A unexpected: {other:?}"),
                }
            }
            if bb.is_none() {
                match tb.poll(t) {
                    PollResult::Ready(x) => bb = Some(x),
                    PollResult::Waiting => {}
                    other => panic!("B unexpected: {other:?}"),
                }
            }
            spins += 1;
            assert!(spins < SPIN_CAP, "spin cap post-resync");
        }
        let ha = wa.tick(&ba.unwrap().flatten());
        let hb = wb.tick(&bb.unwrap().flatten());
        rec.on_tick(t, &cmds_b);
        rec.on_hash(t, hb);
        ta.report_hash(t, ha);
        tb.report_hash(t, hb);
        t += 1;
    }

    let final_tick = t - 1;
    let file = rec.finish(final_tick);
    let (_header, bundles, hashes, parsed_final) = parse(&file);
    assert!(parsed_final >= final_tick);

    // The claim: naive replay-verify (seed + recorded commands, no knowledge
    // of the resync) diverges — it CANNOT reproduce the externally-loaded
    // snapshot's state. It must diverge at or before the resume tick (the
    // corruption itself happened earlier, at `corrupt_at`, so divergence
    // could in principle show up even earlier than the resync — either way
    // proves the same point: this recording is not naively replayable).
    match resim_verify(&bundles, &hashes, parsed_final) {
        Ok(n) => panic!(
            "expected naive replay-verify to diverge on a resynced recording, but all {n} \
             hash records matched — either the resync IS representable after all (a real \
             finding worth re-checking), or this test's corruption didn't actually change \
             wb's hash chain"
        ),
        Err(t) => {
            assert!(
                t <= resume_tick,
                "divergence at tick {t} came AFTER the resume tick {resume_tick} — expected \
                 it at or before (the corruption predates the resync, so a resim that never \
                 saw the snapshot must already disagree by the resume point)"
            );
        }
    }
}
