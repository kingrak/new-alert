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
//! FRAMESYNC packets on a retry timer to break wait deadlocks
//! (QUEUE.CPP:960-976, "Resend a frame-sync packet if longer than one
//! propagation delay goes by; this prevents a 'deadlock'") rather than ever
//! advancing without data.
//!
//! **Layering (audit-verified):** the redundant carry is a pure *jitter/loss
//! absorber* for in-flight traffic — it provides ZERO stall recovery on its
//! own, because a stalled pair sends nothing new to carry history on. NACK
//! is the sole stall-recovery mechanism; disabling it makes blackout
//! recovery impossible, not merely slower (M8-B depth audit, lan_torture.rs
//! revert drills).
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

/// Max resync attempts before falling back to the terminal "OUT OF SYNC" end
/// (DESIGN.md §3.6 "never an infinite resync loop"). Attempt ids run `0..CAP`.
pub const RESYNC_MAX_ATTEMPTS: u8 = 2;

/// Wall-clock budget for one resync attempt (transfer + load). Exceeding it
/// counts as a failed attempt: retry, or — past the cap — fail over to the
/// desync end. Never a hang.
const RESYNC_TIMEOUT: Duration = Duration::from_secs(8);

/// Pacing for resync re-sends (OFFER/CHUNK re-transmit, ACK cadence). The
/// appcore drives `resync_poll` at frame rate; this throttles the actual wire
/// traffic so a slow load doesn't flood the link.
const RESYNC_ACTION_INTERVAL: Duration = Duration::from_millis(40);

/// Once the loser has ACKed a complete chunk set, the host waits this long for
/// the explicit DONE before resuming optimistically — so an all-dropped DONE
/// burst still completes the resync (a genuine load failure sends DONE{ok=false}
/// first, which arrives well inside this grace).
const RESYNC_CONFIRM_GRACE: Duration = Duration::from_millis(600);

/// How many copies of the terminal DONE the loser bursts (loss tolerance).
const DONE_BURST: usize = 5;

/// What [`LanTransport::resync_poll`] tells the appcore to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResyncEvent {
    /// Snapshot transfer under way — keep the "RESYNCHRONIZING..." overlay up.
    Transferring,
    /// Loser only, emitted once: the full snapshot has arrived. The caller must
    /// `World::load_snapshot` it, verify the loaded hash equals `declared_hash`,
    /// and report the outcome via [`LanTransport::resync_report_loaded`].
    NeedsLoad {
        /// The reassembled snapshot bytes.
        bytes: Vec<u8>,
        /// The tick both peers resume lockstep from.
        resume_tick: Tick,
        /// The host's declared state hash to verify the load against.
        declared_hash: u64,
    },
    /// Resync succeeded: normal lockstep resumes at `resume_tick`. The caller
    /// sets its loop tick to this (the host keeps its world; the loser uses the
    /// world it just loaded) and shows the "GAME RESYNCED" toast.
    Resumed {
        /// The resumed tick.
        resume_tick: Tick,
    },
    /// Resync failed past the attempt cap — fall back to the terminal desync
    /// end ([`PollResult::Desync`] is still latched).
    Failed,
}

/// The host's or loser's private transfer state.
#[derive(Debug)]
enum ResyncSide {
    /// Authoritative peer: owns the snapshot, chunked; serves it on demand.
    Host {
        chunks: Vec<Vec<u8>>,
        chunk_size: u16,
        total_len: u32,
        /// Chunk seqs still to (re)send — all of them until the loser ACKs.
        pending: Vec<u32>,
        /// When the loser first reported a complete set (grace timer start).
        all_acked_at: Option<Instant>,
        /// Set when a DONE (ok or fail) arrives.
        done: Option<bool>,
    },
    /// Desynced peer: fills the chunk buffer, then loads + verifies.
    Loser {
        got_offer: bool,
        chunk_size: u16,
        n_chunks: u32,
        received: Vec<Option<Vec<u8>>>,
        have: u32,
        /// Set by [`LanTransport::resync_report_loaded`].
        loaded: Option<bool>,
        /// Whether `NeedsLoad` has been emitted (emit once per attempt).
        emitted_needs_load: bool,
    },
}

/// An in-progress resync (M8-C P1). Lives beside the lockstep state; the sim is
/// paused while it runs.
#[derive(Debug)]
struct Resync {
    side: ResyncSide,
    attempt: u8,
    resume_tick: Tick,
    declared_hash: u64,
    /// Attempt start (timeout clock).
    started: Instant,
    /// Last wire action (pacing).
    last_action: Instant,
    /// Terminal flags.
    failed: bool,
}

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
    /// In-progress resync, if any (M8-C P1). `None` during normal play.
    resync: Option<Resync>,
    /// Completed resyncs (observability: the e2e drill asserts this incremented,
    /// proving the game self-healed rather than vacuously never desyncing).
    resyncs_completed: u64,
    /// Revert-drill knob (proof test f): when `false`, [`LanTransport::resume_at`]
    /// keeps the stale scheduler/command windows instead of clearing them, so a
    /// test can prove the window re-stamp is load-bearing (chains diverge without
    /// it). Production always resumes with cleared windows.
    resume_clear_windows: bool,
    /// The tick lockstep last (re)started from: `0` at game start, or the resume
    /// tick after a resync. The first `delay` ticks from here carry no peer
    /// bundle by protocol (a fresh scheduler's earliest stamp lands at
    /// `resume_base + delay`), exactly as ticks `0..delay` are empty at start.
    resume_base: Tick,
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
            resync: None,
            resyncs_completed: 0,
            resume_clear_windows: true,
            resume_base: 0,
        })
    }

    /// This endpoint's seat id.
    pub fn seat(&self) -> SeatId {
        self.seat
    }

    /// Whether this endpoint hosts the session — the authoritative peer on a
    /// 2-player desync (it serves its snapshot; the joiner resyncs to it, §4.6).
    pub fn is_host(&self) -> bool {
        self.is_host
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

    // -- resync (M8-C P1) ---------------------------------------------------

    /// Whether a resync is in progress (the sim is paused; drive it with
    /// [`LanTransport::resync_poll`], not [`CommandTransport::poll`]).
    pub fn resync_active(&self) -> bool {
        self.resync.is_some()
    }

    /// Resyncs that completed successfully (self-heals). The forced-desync drill
    /// asserts this incremented.
    pub fn resyncs_completed(&self) -> u64 {
        self.resyncs_completed
    }

    /// Begin an authoritative (host-side) resync: split `snapshot` into chunks
    /// and start serving them to the desynced loser, which resumes lockstep at
    /// `resume_tick` and verifies against `declared_hash`. The transport treats
    /// all three as opaque (DESIGN.md §4.6). Call after [`PollResult::Desync`]
    /// on the host (host is authoritative on a 2-player desync, §4.6).
    pub fn begin_resync_host(&mut self, snapshot: Vec<u8>, resume_tick: Tick, declared_hash: u64) {
        let chunk_size = wire::MAX_SNAP_CHUNK_DATA as u16;
        let total_len = snapshot.len() as u32;
        let chunks: Vec<Vec<u8>> = snapshot
            .chunks(wire::MAX_SNAP_CHUNK_DATA)
            .map(|c| c.to_vec())
            .collect();
        let pending: Vec<u32> = (0..chunks.len() as u32).collect();
        let now = Instant::now();
        self.resync = Some(Resync {
            side: ResyncSide::Host {
                chunks,
                chunk_size,
                total_len,
                pending,
                all_acked_at: None,
                done: None,
            },
            attempt: 0,
            resume_tick,
            declared_hash,
            started: now,
            last_action: now - RESYNC_ACTION_INTERVAL,
            failed: false,
        });
    }

    /// Begin a loser-side resync: wait for the host's offer, fill the chunk
    /// buffer, then surface the bytes for the caller to load + verify. Call after
    /// [`PollResult::Desync`] on the non-host peer.
    pub fn begin_resync_loser(&mut self) {
        let now = Instant::now();
        self.resync = Some(Resync {
            side: ResyncSide::Loser {
                got_offer: false,
                chunk_size: 0,
                n_chunks: 0,
                received: Vec::new(),
                have: 0,
                loaded: None,
                emitted_needs_load: false,
            },
            attempt: 0,
            resume_tick: 0,
            declared_hash: 0,
            started: now,
            last_action: now - RESYNC_ACTION_INTERVAL,
            failed: false,
        });
    }

    /// Report the outcome of loading the snapshot handed back by
    /// [`ResyncEvent::NeedsLoad`]: `true` = loaded and hash-verified against
    /// `declared_hash`, `false` = load/verify failed (triggers a retry or, past
    /// the cap, the fallback).
    pub fn resync_report_loaded(&mut self, ok: bool) {
        if let Some(rs) = &mut self.resync {
            if let ResyncSide::Loser { loaded, .. } = &mut rs.side {
                *loaded = Some(ok);
            }
        }
    }

    /// Drive the in-progress resync one step: service the socket, (re)send the
    /// next transfer traffic, and report progress. See [`ResyncEvent`].
    pub fn resync_poll(&mut self) -> ResyncEvent {
        self.pump(); // files OFFER/CHUNK/ACK/DONE into self.resync

        let Some(mut rs) = self.resync.take() else {
            return ResyncEvent::Failed;
        };
        if rs.failed {
            self.resync = Some(rs);
            return ResyncEvent::Failed;
        }

        // Per-attempt timeout → retry, or fail past the cap.
        if rs.started.elapsed() >= RESYNC_TIMEOUT {
            if rs.attempt + 1 >= RESYNC_MAX_ATTEMPTS {
                rs.failed = true;
                self.resync = Some(rs);
                return ResyncEvent::Failed;
            }
            rs.attempt += 1;
            rs.started = Instant::now();
            rs.last_action = rs.started - RESYNC_ACTION_INTERVAL;
            match &mut rs.side {
                ResyncSide::Host {
                    chunks,
                    pending,
                    all_acked_at,
                    done,
                    ..
                } => {
                    *pending = (0..chunks.len() as u32).collect();
                    *all_acked_at = None;
                    *done = None;
                }
                ResyncSide::Loser {
                    got_offer,
                    loaded,
                    emitted_needs_load,
                    ..
                } => {
                    *got_offer = false;
                    *loaded = None;
                    *emitted_needs_load = false;
                }
            }
        }

        let pace = rs.last_action.elapsed() >= RESYNC_ACTION_INTERVAL;
        let mut outbound: Vec<Datagram> = Vec::new();
        let mut resume: Option<Tick> = None;
        let mut needs_load: Option<(Vec<u8>, Tick, u64)> = None;
        let attempt = rs.attempt;
        let resume_tick = rs.resume_tick;
        let declared_hash = rs.declared_hash;

        match &mut rs.side {
            ResyncSide::Host {
                chunks,
                chunk_size,
                total_len,
                pending,
                all_acked_at,
                done,
            } => match *done {
                Some(true) => resume = Some(resume_tick),
                Some(false) => {
                    if attempt + 1 >= RESYNC_MAX_ATTEMPTS {
                        rs.failed = true;
                    } else {
                        rs.attempt += 1;
                        rs.started = Instant::now();
                        *pending = (0..chunks.len() as u32).collect();
                        *all_acked_at = None;
                        *done = None;
                    }
                }
                None => {
                    // Optimistic-resume backstop (all-dropped DONE burst).
                    if all_acked_at.map(|t| t.elapsed() >= RESYNC_CONFIRM_GRACE) == Some(true) {
                        resume = Some(resume_tick);
                    } else if pace {
                        rs.last_action = Instant::now();
                        outbound.push(Datagram::SnapshotOffer {
                            attempt,
                            resume_tick,
                            declared_hash,
                            total_len: *total_len,
                            chunk_size: *chunk_size,
                        });
                        for &seq in pending.iter() {
                            outbound.push(Datagram::SnapshotChunk {
                                attempt,
                                seq,
                                data: chunks[seq as usize].clone(),
                            });
                        }
                    }
                }
            },
            ResyncSide::Loser {
                got_offer,
                n_chunks,
                received,
                have,
                loaded,
                emitted_needs_load,
                ..
            } => {
                if !*got_offer {
                    // Await the host's OFFER.
                } else if *have < *n_chunks {
                    if pace {
                        rs.last_action = Instant::now();
                        let missing: Vec<u32> = received
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| s.is_none())
                            .map(|(i, _)| i as u32)
                            .take(wire::MAX_SNAP_MISSING)
                            .collect();
                        outbound.push(Datagram::SnapshotAck { attempt, missing });
                    }
                } else {
                    // Complete: keep telling the host we have every chunk (arms
                    // its optimistic-resume grace even if the DONE burst is lost).
                    if pace {
                        rs.last_action = Instant::now();
                        outbound.push(Datagram::SnapshotAck {
                            attempt,
                            missing: Vec::new(),
                        });
                    }
                    match *loaded {
                        None => {
                            if !*emitted_needs_load {
                                *emitted_needs_load = true;
                                let mut bytes = Vec::new();
                                for chunk in received.iter().flatten() {
                                    bytes.extend_from_slice(chunk);
                                }
                                needs_load = Some((bytes, resume_tick, declared_hash));
                            }
                        }
                        Some(true) => {
                            for _ in 0..DONE_BURST {
                                outbound.push(Datagram::SnapshotDone { attempt, ok: true });
                            }
                            resume = Some(resume_tick);
                        }
                        Some(false) => {
                            for _ in 0..DONE_BURST {
                                outbound.push(Datagram::SnapshotDone { attempt, ok: false });
                            }
                            // The host owns the attempt id: reset and await its
                            // re-offer (a higher attempt), or time out at the cap.
                            *got_offer = false;
                            *loaded = None;
                            *emitted_needs_load = false;
                        }
                    }
                }
            }
        }

        let failed = rs.failed;
        // Re-install (or drop) the resync before touching the socket / resuming.
        self.resync = Some(rs);
        for d in &outbound {
            self.send(d);
        }

        if failed {
            return ResyncEvent::Failed;
        }
        if let Some(t) = resume {
            self.resyncs_completed += 1;
            self.resume_at(t); // sets self.resync = None
            return ResyncEvent::Resumed { resume_tick: t };
        }
        if let Some((bytes, rt, dh)) = needs_load {
            return ResyncEvent::NeedsLoad {
                bytes,
                resume_tick: rt,
                declared_hash: dh,
            };
        }
        ResyncEvent::Transferring
    }

    /// Reset the lockstep state to resume at `tick` (M8-C): clear the desync
    /// latch and — the load-bearing step, proven by the revert drill — re-stamp
    /// the input windows (fresh scheduler + cleared bundle/hash maps) so no stale
    /// pre-desync command replays into the resumed world.
    fn resume_at(&mut self, tick: Tick) {
        self.current = tick;
        self.resume_base = tick;
        self.stamped = false;
        self.desync = None;
        self.resync = None;
        self.stall_polls = 0;
        let now = Instant::now();
        self.last_recv = now;
        self.last_send = now;
        if self.resume_clear_windows {
            self.local_due = None;
            self.remote.clear();
            self.sent_bundles.clear();
            self.sent_hashes.clear();
            self.my_hashes.clear();
            self.peer_hashes.clear();
            self.sched = InputScheduler::new(self.delay);
        }
    }

    /// Revert-drill knob (proof test f): when `false`, [`LanTransport::resume_at`]
    /// leaves the stale command windows in place, so a test can prove the
    /// re-stamp is load-bearing (post-resync chains diverge). Production never
    /// calls this.
    pub fn set_resume_clear_windows_for_test(&mut self, clear: bool) {
        self.resume_clear_windows = clear;
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
            // --- Resync (M8-C P1): file transfer data into the resync state. ---
            Datagram::SnapshotOffer {
                attempt,
                resume_tick,
                declared_hash,
                total_len,
                chunk_size,
            } => {
                if let Some(rs) = &mut self.resync {
                    if let ResyncSide::Loser {
                        got_offer,
                        chunk_size: cs,
                        n_chunks,
                        received,
                        have,
                        loaded,
                        emitted_needs_load,
                    } = &mut rs.side
                    {
                        // First offer, or a fresh (higher-numbered) retry: (re)size
                        // the buffer. Duplicate offers of the current attempt are
                        // idempotent.
                        if !*got_offer || attempt > rs.attempt {
                            let n = (total_len as usize).div_ceil(chunk_size as usize) as u32;
                            rs.attempt = attempt;
                            rs.resume_tick = resume_tick;
                            rs.declared_hash = declared_hash;
                            *cs = chunk_size;
                            *n_chunks = n;
                            *received = (0..n).map(|_| None).collect();
                            *have = 0;
                            *loaded = None;
                            *emitted_needs_load = false;
                            *got_offer = true;
                            rs.started = Instant::now();
                        }
                    }
                }
            }
            Datagram::SnapshotChunk { attempt, seq, data } => {
                if let Some(rs) = &mut self.resync {
                    if let ResyncSide::Loser {
                        got_offer,
                        n_chunks,
                        received,
                        have,
                        ..
                    } = &mut rs.side
                    {
                        if *got_offer && attempt == rs.attempt && seq < *n_chunks {
                            let slot = &mut received[seq as usize];
                            if slot.is_none() {
                                *slot = Some(data);
                                *have += 1;
                            }
                        }
                    }
                }
            }
            Datagram::SnapshotAck { attempt, missing } => {
                if let Some(rs) = &mut self.resync {
                    if let ResyncSide::Host {
                        pending,
                        all_acked_at,
                        ..
                    } = &mut rs.side
                    {
                        if attempt == rs.attempt {
                            if missing.is_empty() {
                                pending.clear();
                                if all_acked_at.is_none() {
                                    *all_acked_at = Some(Instant::now());
                                }
                            } else {
                                *pending = missing;
                                *all_acked_at = None;
                            }
                        }
                    }
                }
            }
            Datagram::SnapshotDone { attempt, ok } => {
                if let Some(rs) = &mut self.resync {
                    if let ResyncSide::Host { done, .. } = &mut rs.side {
                        if attempt == rs.attempt {
                            *done = Some(ok);
                        }
                    }
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

        // Tick barrier: we need the peer's bundle for `tick`. The first `delay`
        // ticks from the lockstep (re)start point are empty by protocol
        // definition (no stamp can land there — same rule as PairTransport, and
        // after a resync the fresh scheduler's earliest stamp lands at
        // `resume_base + delay`).
        let remote_cmds = if tick < self.resume_base.saturating_add(self.delay) {
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
