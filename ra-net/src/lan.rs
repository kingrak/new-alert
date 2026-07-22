//! M8-B P1: [`LanTransport`] — the same lockstep protocol
//! [`crate::PairTransport`] proved in-process, with the medium swapped for a
//! non-blocking `std::net::UdpSocket`. The scheduler, tick barrier, canonical
//! seat ordering, and hash-exchange semantics are identical; only delivery
//! and liveness are new.
//!
//! **Loss tolerance (lockstep classic).** Every BUNDLES datagram redundantly
//! carries the last [`REDUNDANT_TICKS`] ticks' bundles, so an isolated drop
//! never stalls the barrier — the next tick's datagram re-delivers the lost
//! tick. The same applies to HASHES. For bursts longer than the redundancy
//! window, a stalled endpoint periodically NACKs ("re-send everything from
//! tick T") and re-pushes its own recent window, so both directions heal.
//! This mirrors the original's posture: its comm layer re-sent unacked
//! packets while `Wait_For_Players` sat in the frame-sync loop resending
//! FRAMESYNC packets (QUEUE.CPP:1748-1817) rather than ever advancing
//! without data.
//!
//! **Timing discipline (determinism note).** All wall-clock here is
//! transport-layer only — keepalive cadence, peer timeout, NACK pacing. None
//! of it ever reschedules a command: execution ticks are fixed by the
//! *sender's* stamp ([`InputScheduler`], QUEUE.CPP:2526) and arrival timing
//! can only stall the barrier or end the session. The sim stays
//! sender-clock-pure exactly as the M8-A pins require.
//!
//! **Runtime MaxAhead retuning (QUEUE.CPP:1440-1461) is deliberately NOT
//! ported.** The original recomputed MaxAhead from measured response time
//! because its send cadence (`FrameSendRate` batching, QUEUE.CPP:2754-2759)
//! and IPX-era latency made a fixed value wasteful. We send every tick on a
//! LAN, where a fixed small delay (DESIGN.md §4.6: "2–3 ticks at 15 Hz is
//! imperceptible") is both sufficient and simpler; retuning would add a
//! TIMING event that changes `delay` mid-game for both peers — deferred
//! until a transport with real latency variance (M8-C/relay) needs it.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ra_sim::Command;

use crate::scheduler::InputScheduler;
use crate::transport::{
    CommandTransport, ConnectionLost, DesyncDetected, LostReason, PollResult, SeatId, Tick,
    TickBundle,
};
use crate::wire::{self, Datagram, MAX_DATAGRAM, MAX_TICK_ENTRIES};

/// How many ticks' bundles every BUNDLES datagram redundantly re-carries.
/// Must exceed the input-delay window ([`crate::DEFAULT_INPUT_DELAY`] = 3):
/// a bundle for exec tick `T` is first sent at stamp tick `T - delay`, and
/// each of the next `REDUNDANT_TICKS - 1` stamped ticks re-carries it — so
/// any single datagram (or any run shorter than the window) can be lost with
/// zero effect. 8 = the delay window with >2x margin, at a cost of a few
/// hundred bytes per datagram in the worst case.
pub const REDUNDANT_TICKS: u32 = 8;

/// While stalled at the barrier, send a NACK + re-push our own recent window
/// every this many consecutive `Waiting` polls (the burst-loss backstop).
/// The client polls at frame rate while stalled, so 16 polls is roughly a
/// quarter-second of real stall — fast enough to heal a burst quickly,
/// slow enough not to spam a link that is merely slow.
const NACK_EVERY_STALL_POLLS: u32 = 16;

/// Keepalive cadence while nothing else is being sent.
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Declare the peer gone after this long without receiving anything
/// (keepalives flow every 250ms, so this is ~40 consecutive losses — the
/// peer's process is dead or the link is severed, not lossy).
pub const PEER_TIMEOUT: Duration = Duration::from_secs(10);

/// How many stamped ticks of history we keep for NACK re-sends. The barrier
/// keeps peers within `delay` ticks of each other, so anything older than a
/// few windows can never legitimately be re-requested.
const SENT_KEEP_TICKS: u32 = 64;

/// One endpoint of a two-player UDP lockstep session.
#[derive(Debug)]
pub struct LanTransport {
    sock: UdpSocket,
    peer: SocketAddr,
    seat: SeatId,
    peer_seat: SeatId,
    delay: u32,
    sched: InputScheduler,
    /// The tick currently being assembled (polls must be sequential).
    current: Tick,
    /// Whether `current` has been stamped/sent (Waiting re-polls must not
    /// re-stamp — one scheduling decision per tick, ever).
    stamped: bool,
    /// Local commands due at `current`, held across Waiting re-polls.
    local_due: Option<Vec<Command>>,
    /// Delivered peer bundles keyed by exec tick. Redundant deliveries are
    /// deduped by insert-if-absent (first arrival wins; all copies carry the
    /// same sender-stamped payload).
    remote: BTreeMap<Tick, Vec<Command>>,
    /// Our stamped bundles, kept for redundant carry + NACK re-send.
    sent_bundles: BTreeMap<Tick, Vec<Command>>,
    /// Our reported hashes, kept for redundant carry + NACK re-send.
    sent_hashes: BTreeMap<Tick, u64>,
    /// Own reported hashes awaiting the peer's report for the same tick.
    my_hashes: BTreeMap<Tick, u64>,
    /// Peer hashes awaiting our own report for the same tick.
    peer_hashes: BTreeMap<Tick, u64>,
    /// Latched divergence state (sticky).
    desync: Option<DesyncDetected>,
    /// Latched peer-gone state (sticky).
    lost: Option<ConnectionLost>,
    /// Whether this endpoint hosts the session: a stray lobby READY received
    /// in-game is answered with START (a lost START must not strand the
    /// joiner in the lobby).
    is_host: bool,
    /// Wall-clock of the last datagram received from the peer.
    last_recv: Instant,
    /// Wall-clock of our last send (keepalive pacing).
    last_send: Instant,
    /// Peer-gone threshold (shrunk by tests; default [`PEER_TIMEOUT`]).
    peer_timeout: Duration,
    /// Consecutive `Waiting` polls for the current tick (NACK pacing + the
    /// client's "waiting for player" overlay).
    stall_polls: u32,
    /// Total `Waiting` polls over the session (observability).
    stalls: u64,
    /// Redundant-carry window size (revert-drill knob; default
    /// [`REDUNDANT_TICKS`]).
    carry: u32,
    /// Whether the NACK backstop runs (revert-drill knob; default true).
    nack_enabled: bool,
    /// NACKs sent (test observability: proves the burst path ran).
    nacks_sent: u64,
    /// NACKs received → windows re-sent (test observability).
    nacks_answered: u64,
    /// Datagrams that failed to decode (ignored, counted).
    decode_errors: u64,
}

impl LanTransport {
    /// Wrap an already-bound socket into a session endpoint talking to
    /// `peer`. `seat`/`peer_seat` are the two players' house ids (canonical
    /// bundle order — same contract as [`crate::PairTransport::pair`]);
    /// `delay` is the session's shared input delay in ticks; `is_host`
    /// selects the lost-START re-answer behavior (see field docs).
    ///
    /// The socket is switched to non-blocking; the lobby (or a test) is
    /// expected to have exchanged real addresses already.
    pub fn new(
        sock: UdpSocket,
        peer: SocketAddr,
        seat: SeatId,
        peer_seat: SeatId,
        delay: u32,
        is_host: bool,
    ) -> io::Result<LanTransport> {
        assert_ne!(seat, peer_seat, "lockstep seats must be distinct");
        sock.set_nonblocking(true)?;
        let now = Instant::now();
        Ok(LanTransport {
            sock,
            peer,
            seat,
            peer_seat,
            delay,
            sched: InputScheduler::new(delay),
            current: 0,
            stamped: false,
            local_due: None,
            remote: BTreeMap::new(),
            sent_bundles: BTreeMap::new(),
            sent_hashes: BTreeMap::new(),
            my_hashes: BTreeMap::new(),
            peer_hashes: BTreeMap::new(),
            desync: None,
            lost: None,
            is_host,
            last_recv: now,
            last_send: now,
            peer_timeout: PEER_TIMEOUT,
            stall_polls: 0,
            stalls: 0,
            carry: REDUNDANT_TICKS,
            nack_enabled: true,
            nacks_sent: 0,
            nacks_answered: 0,
            decode_errors: 0,
        })
    }

    /// This endpoint's seat id.
    pub fn seat(&self) -> SeatId {
        self.seat
    }

    /// The latched divergence state, if any.
    pub fn desync(&self) -> Option<DesyncDetected> {
        self.desync
    }

    /// The latched peer-gone state, if any.
    pub fn connection_lost(&self) -> Option<ConnectionLost> {
        self.lost
    }

    /// Total polls that returned [`PollResult::Waiting`].
    pub fn stall_count(&self) -> u64 {
        self.stalls
    }

    /// Consecutive `Waiting` polls for the tick currently being assembled
    /// (resets to 0 the moment the barrier opens) — the client's
    /// "waiting for player" overlay reads this.
    pub fn stalled_polls_current_tick(&self) -> u32 {
        self.stall_polls
    }

    /// The tick this endpoint is currently assembling.
    pub fn current_tick(&self) -> Tick {
        self.current
    }

    /// NACKs sent so far (proves the burst-loss backstop actually ran).
    pub fn nacks_sent(&self) -> u64 {
        self.nacks_sent
    }

    /// NACKs answered with a re-sent window.
    pub fn nacks_answered(&self) -> u64 {
        self.nacks_answered
    }

    /// Datagrams received that failed to decode (ignored per fuzz-safety).
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }

    /// Shrink the peer timeout (tests: a real 10s wait per case is hostile
    /// to CI). Production code never calls this.
    pub fn set_peer_timeout(&mut self, timeout: Duration) {
        self.peer_timeout = timeout;
    }

    /// Revert-drill knob (proof test g): shrink the redundant-carry window
    /// and/or disable the NACK backstop, so tests can prove the loss
    /// machinery is load-bearing (carry=1, nack=false must stall under
    /// loss). Production code never calls this.
    pub fn set_loss_recovery_for_test(&mut self, carry_ticks: u32, nack_enabled: bool) {
        self.carry = carry_ticks.max(1);
        self.nack_enabled = nack_enabled;
    }

    /// Service the connection **without** advancing the lockstep protocol:
    /// drain the socket (answering NACKs, filing bundles/hashes, noticing a
    /// QUIT), send a keepalive if one is due, and run the peer-timeout
    /// check. Call this whenever the game loop is alive but not polling —
    /// the local player paused, a menu is up, or (in tests) this endpoint
    /// finished the tick its peer is still stalled on. Without it, a
    /// non-polling endpoint goes silent: its peer's NACKs get no answer and
    /// its keepalives stop, so the peer would eventually latch a spurious
    /// timeout.
    pub fn service(&mut self) {
        if self.desync.is_some() || self.lost.is_some() {
            return;
        }
        self.pump();
        if self.lost.is_some() {
            return;
        }
        if self.last_send.elapsed() >= KEEPALIVE_INTERVAL {
            let tick = self.current;
            self.send(&Datagram::KeepAlive { tick });
        }
        if self.last_recv.elapsed() >= self.peer_timeout {
            self.lost = Some(ConnectionLost {
                tick: self.current,
                reason: LostReason::Timeout,
            });
        }
    }

    /// Send the clean-exit QUIT (best effort, a few copies — it is the
    /// last thing we ever say, so a drop only degrades "player left" into
    /// the keepalive timeout on the peer). Call when the local player
    /// leaves an in-progress game.
    pub fn send_quit(&mut self) {
        let bytes = wire::encode(&Datagram::Quit);
        for _ in 0..3 {
            let _ = self.sock.send_to(&bytes, self.peer);
        }
    }

    // -- internals ----------------------------------------------------------

    fn send(&mut self, d: &Datagram) {
        let bytes = wire::encode(d);
        debug_assert!(bytes.len() <= MAX_DATAGRAM);
        let _ = self.sock.send_to(&bytes, self.peer);
        self.last_send = Instant::now();
    }

    /// The redundant BUNDLES window ending at `end` (the last `carry` ticks
    /// we have stamped), size-capped to [`MAX_DATAGRAM`] by dropping the
    /// oldest entries first (the newest tick always ships).
    fn bundles_window(&self, end: Tick) -> Datagram {
        let lo = end.saturating_sub(self.carry.saturating_sub(1));
        let mut entries: Vec<(Tick, Vec<Command>)> = self
            .sent_bundles
            .range(lo..=end)
            .map(|(&t, c)| (t, c.clone()))
            .collect();
        // Size cap: estimate by encoding; drop oldest until it fits.
        while entries.len() > 1
            && wire::encode(&Datagram::Bundles {
                entries: entries.clone(),
            })
            .len()
                > MAX_DATAGRAM
        {
            entries.remove(0);
        }
        Datagram::Bundles { entries }
    }

    /// The redundant HASHES window ending at the newest reported tick.
    fn hashes_window(&self) -> Option<Datagram> {
        let (&end, _) = self.sent_hashes.iter().next_back()?;
        let lo = end.saturating_sub(self.carry.saturating_sub(1));
        let entries: Vec<(Tick, u64)> = self
            .sent_hashes
            .range(lo..=end)
            .map(|(&t, &h)| (t, h))
            .collect();
        Some(Datagram::Hashes { entries })
    }

    /// Answer a NACK: re-send everything we still hold from `from` on, in
    /// window-sized chunks.
    fn answer_nack(&mut self, from: Tick) {
        self.nacks_answered += 1;
        let bundle_chunks: Vec<Vec<(Tick, Vec<Command>)>> = {
            let all: Vec<(Tick, Vec<Command>)> = self
                .sent_bundles
                .range(from..)
                .map(|(&t, c)| (t, c.clone()))
                .collect();
            all.chunks(MAX_TICK_ENTRIES.min(self.carry.max(1) as usize))
                .map(|c| c.to_vec())
                .collect()
        };
        for entries in bundle_chunks {
            self.send(&Datagram::Bundles { entries });
        }
        let hash_chunks: Vec<Vec<(Tick, u64)>> = {
            let all: Vec<(Tick, u64)> = self
                .sent_hashes
                .range(from..)
                .map(|(&t, &h)| (t, h))
                .collect();
            all.chunks(MAX_TICK_ENTRIES).map(|c| c.to_vec()).collect()
        };
        for entries in hash_chunks {
            self.send(&Datagram::Hashes { entries });
        }
    }

    fn flag_desync(&mut self, d: DesyncDetected) {
        match self.desync {
            Some(e) if e.tick <= d.tick => {}
            _ => self.desync = Some(d),
        }
    }

    fn on_peer_hash(&mut self, tick: Tick, hash: u64) {
        // Ignore reports for ticks long since pruned (late redundant copies).
        if tick.saturating_add(SENT_KEEP_TICKS) < self.current {
            return;
        }
        match self.my_hashes.get(&tick) {
            Some(&mine) if mine != hash => {
                let peer = self.peer_seat;
                self.flag_desync(DesyncDetected {
                    tick,
                    local_hash: mine,
                    remote_hash: hash,
                    peer,
                });
            }
            Some(_) => {
                // Confirmed in sync for `tick`; prune (bounded memory).
                self.my_hashes.remove(&tick);
            }
            None => {
                // Redundant copies are idempotent: first insert wins; stale
                // entries are swept by `prune()`.
                self.peer_hashes.entry(tick).or_insert(hash);
            }
        }
    }

    fn receive(&mut self, d: Datagram) {
        match d {
            Datagram::Bundles { entries } => {
                for (tick, cmds) in entries {
                    // Already consumed ticks are done; redundant copies of a
                    // pending tick are deduped (insert-if-absent — every copy
                    // carries the identical sender-stamped payload anyway).
                    if tick >= self.current {
                        self.remote.entry(tick).or_insert(cmds);
                    }
                }
            }
            Datagram::Hashes { entries } => {
                for (tick, hash) in entries {
                    self.on_peer_hash(tick, hash);
                }
            }
            Datagram::Nack { from } => self.answer_nack(from),
            Datagram::KeepAlive { .. } => {}
            Datagram::Quit => {
                let tick = self.current;
                self.lost.get_or_insert(ConnectionLost {
                    tick,
                    reason: LostReason::PeerQuit,
                });
            }
            // A joiner whose START was lost re-sends READY: answer it so the
            // lobby handoff cannot strand them (host side only).
            Datagram::Ready => {
                if self.is_host {
                    self.send(&Datagram::Start);
                }
            }
            // Lobby leftovers / stray discovery traffic: ignore in-game.
            Datagram::Announce { .. }
            | Datagram::Join { .. }
            | Datagram::Welcome { .. }
            | Datagram::Reject { .. }
            | Datagram::Start
            | Datagram::Leave => {}
        }
    }

    /// Drain the socket, dispatching every decodable datagram from the peer.
    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    // Only the session peer may drive session state.
                    if src != self.peer {
                        continue;
                    }
                    self.last_recv = Instant::now();
                    match wire::decode(&buf[..n]) {
                        Ok(d) => self.receive(d),
                        Err(_) => self.decode_errors += 1,
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                // Connection-refused style errors surface on some platforms
                // after sending to a dead port; the keepalive timeout is the
                // arbiter of peer death, not transient ICMP.
                Err(_) => break,
            }
        }
    }

    /// Bounded-memory sweep of the history maps.
    fn prune(&mut self) {
        let cutoff = self.current.saturating_sub(SENT_KEEP_TICKS);
        self.sent_bundles = self.sent_bundles.split_off(&cutoff);
        self.sent_hashes = self.sent_hashes.split_off(&cutoff);
        self.my_hashes = self.my_hashes.split_off(&cutoff);
        self.peer_hashes = self.peer_hashes.split_off(&cutoff);
    }
}

impl CommandTransport for LanTransport {
    fn submit(&mut self, cmd: Command) {
        if self.desync.is_some() || self.lost.is_some() {
            return; // session is over; drop input
        }
        self.sched.submit(cmd);
    }

    fn poll(&mut self, tick: Tick) -> PollResult {
        if let Some(d) = self.desync {
            return PollResult::Desync(d);
        }
        if let Some(l) = self.lost {
            return PollResult::ConnectionLost(l);
        }
        assert_eq!(
            tick, self.current,
            "lockstep ticks must be polled sequentially (expected {}, got {})",
            self.current, tick
        );

        // First poll of this tick: stamp staged input for `tick + delay`
        // (QUEUE.CPP:2526 — the sender-side stamp) and ship the redundant
        // window ending at the new exec tick.
        if !self.stamped {
            let (exec_tick, cmds) = self.sched.stamp(tick);
            self.sent_bundles.insert(exec_tick, cmds);
            self.local_due = Some(self.sched.take_due(tick));
            self.stamped = true;
            self.prune();
            let window = self.bundles_window(exec_tick);
            self.send(&window);
        }

        // Deliver whatever has arrived.
        self.pump();
        if let Some(d) = self.desync {
            return PollResult::Desync(d);
        }
        if let Some(l) = self.lost {
            return PollResult::ConnectionLost(l);
        }

        // Liveness: keepalive out, timeout in.
        if self.last_send.elapsed() >= KEEPALIVE_INTERVAL {
            self.send(&Datagram::KeepAlive { tick });
        }
        if self.last_recv.elapsed() >= self.peer_timeout {
            let l = ConnectionLost {
                tick,
                reason: LostReason::Timeout,
            };
            self.lost = Some(l);
            return PollResult::ConnectionLost(l);
        }

        // Tick barrier: we need the peer's bundle for `tick`. Ticks below
        // the input delay are empty by protocol definition (no stamp can
        // land there — same rule as PairTransport).
        let remote_cmds = if tick < self.delay {
            Some(Vec::new())
        } else {
            self.remote.remove(&tick)
        };
        let Some(remote_cmds) = remote_cmds else {
            self.stalls += 1;
            self.stall_polls += 1;
            // Burst-loss backstop: while stalled, periodically NACK the
            // missing run and re-push our own window (the peer may be
            // stalled missing OURS — both directions heal).
            if self.nack_enabled && self.stall_polls.is_multiple_of(NACK_EVERY_STALL_POLLS) {
                self.nacks_sent += 1;
                self.send(&Datagram::Nack { from: tick });
                if let Some((&newest, _)) = self.sent_bundles.iter().next_back() {
                    let window = self.bundles_window(newest);
                    self.send(&window);
                }
                if let Some(hashes) = self.hashes_window() {
                    self.send(&hashes);
                }
            }
            return PollResult::Waiting;
        };

        let local_cmds = self.local_due.take().unwrap_or_default();
        let mut seats = vec![(self.seat, local_cmds), (self.peer_seat, remote_cmds)];
        seats.sort_by_key(|&(s, _)| s); // canonical house order, QUEUE.CPP:3286-3290
        self.current += 1;
        self.stamped = false;
        self.stall_polls = 0;
        PollResult::Ready(TickBundle { tick, seats })
    }

    fn report_hash(&mut self, tick: Tick, hash: u64) {
        if self.desync.is_some() || self.lost.is_some() {
            return;
        }
        self.sent_hashes.insert(tick, hash);
        if let Some(window) = self.hashes_window() {
            self.send(&window);
        }
        match self.peer_hashes.remove(&tick) {
            Some(theirs) if theirs != hash => {
                let peer = self.peer_seat;
                self.flag_desync(DesyncDetected {
                    tick,
                    local_hash: hash,
                    remote_hash: theirs,
                    peer,
                });
            }
            Some(_) => {} // confirmed in sync; both sides pruned
            None => {
                self.my_hashes.insert(tick, hash);
            }
        }
    }
}
