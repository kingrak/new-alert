//! Lobby sessions and live games — the server-authoritative state machine
//! (SERVER-DESIGN.md §5) and the tick sequencer (§6.1) that lives on a Running
//! session.
//!
//! ```text
//!   ∅ ──SESS_CREATE──▶ Lobby ──all seats full & READY──▶ Running ──all gone──▶ Closed
//!                        │ join/leave/ready (SESS_STATE rebroadcast)   │ resync (M9-B)
//!                        └ creator leaves → dissolve                   └ replay finalized
//! ```
//!
//! Lobby state is authoritative here; clients render `SESS_STATE` verbatim
//! (§5 — one brain, no client-side lobby truth). Seat 0 (the creator, house 1)
//! owns settings and is the arbitration tiebreak. The sequencer treats every
//! command as an **opaque validated blob**: it reads only the house byte (via
//! [`ra_net::wire::command_house`]) for the seat-house binding check (§7.3) and
//! never decodes a command — the structural "never a sim host" guarantee.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ra_net::wire::{
    command_house, BundleEntry, Datagram, HashVerdict, SessListEntry, SessSeat, SessionPhase,
    WireError, GAME_VERSION, MAX_CMD_BLOB, PROTOCOL_VERSION,
};
use ra_net::{EndReason, ReplayHeader, ReplaySeat, SeatId, Tick};

use crate::relay::arbitrate;
use crate::replay::ServerReplay;
use crate::Counters;

/// One tick's canonical bundle as opaque command blobs: `(seat/house, blobs)`
/// per seat in ascending order — what the server assembles, broadcasts, and logs.
type SeatBlobs = Vec<(SeatId, Vec<Vec<u8>>)>;

/// Per-seat rate cap (§7.5): a human peaks well under 64 commands in one tick.
pub const MAX_CMDS_PER_TICK_PER_SEAT: usize = 64;
/// Acceptance window for a client-stamped exec tick (§7.4).
pub const MAX_AHEAD_WINDOW: u32 = 128;
/// How many ticks each `TICK_BUNDLE` redundantly re-carries (loss tolerance,
/// mirrors the LAN [`ra_net::REDUNDANT_TICKS`] discipline).
const CARRY_TICKS: u32 = 8;
/// History kept for NACK re-sends / late detection (bounded memory).
const KEEP_TICKS: u32 = 64;
/// If a bundle cannot close (a seat's entry for it never arrived) for this long,
/// the server force-closes it with the missing seat empty and issues LATE
/// advisories (§6.2 "never waits for a slow seat beyond the deadline").
const BUNDLE_DEADLINE: Duration = Duration::from_secs(1);
/// Lobby idle timeout → dissolve (§5).
const LOBBY_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Running game where *all* seats have gone silent → Closed (§5).
const RUNNING_ALL_GONE_TIMEOUT: Duration = Duration::from_secs(60);
/// Server→seat keepalive cadence while a game is otherwise quiet.
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Leave/kick reason codes carried in `SESS_LEAVE` (§4 `reason u8`).
pub mod leave_reason {
    /// The player left cleanly.
    pub const PLAYER_LEFT: u8 = 0;
    /// Flood / sustained rate-cap violation (§7.5).
    pub const FLOOD: u8 = 1;
    /// A command bound to the wrong house (§7.3).
    pub const WRONG_HOUSE: u8 = 2;
    /// The session was dissolved (creator left the lobby, or GC).
    pub const DISSOLVED: u8 = 3;
    /// The server is shutting down (§9 SIGTERM).
    pub const SERVER_SHUTDOWN: u8 = 4;
}

/// One occupied seat.
#[derive(Debug)]
struct Seat {
    /// The connection that owns this seat (transport identity).
    addr: SocketAddr,
    /// The connection id bound to this seat (spoof guard, §7.2).
    conn_id: u32,
    /// Display name.
    name: String,
    /// House id (== [`SeatId`] for canonical ordering). Seat index + 1.
    house: u8,
    /// READY toggle.
    ready: bool,
    /// Last datagram time from this seat.
    last_recv: Instant,
    /// Received command entries per exec tick (present entry — even empty — means
    /// "this seat reached this tick"). The barrier closes a tick once every seat
    /// has an entry for it.
    pending: BTreeMap<Tick, Vec<Vec<u8>>>,
    /// Reported state hashes per tick (for arbitration).
    hashes: BTreeMap<Tick, u64>,
}

/// A lobby session and, once started, its live game + sequencer.
#[derive(Debug)]
pub struct Session {
    /// Server-assigned id.
    pub id: u32,
    name: String,
    map: String,
    seed: u32,
    credits: i32,
    catalog_hash: u64,
    /// Seat capacity (2..=8).
    cap: u8,
    phase: SessionPhase,
    input_delay: u8,
    /// The creator's address (settings authority; leaving in lobby dissolves).
    creator: SocketAddr,
    seats: Vec<Seat>,
    /// Last lobby/keepalive activity (idle-timeout clock).
    last_activity: Instant,

    // --- sequencer state (Running only) ---------------------------------
    /// Next tick to close and broadcast (starts at `input_delay` — ticks
    /// `0..input_delay` carry no commands and the client synthesises them).
    next_bundle_tick: Tick,
    /// Broadcast bundles kept for redundant carry, NACK re-send, and late
    /// detection.
    sent: BTreeMap<Tick, SeatBlobs>,
    /// Arbitrated winning hash per tick (also the replay hash chain).
    winning_hash: BTreeMap<Tick, u64>,
    /// Ticks whose hashes have been arbitrated (so each is decided once).
    arbitrated: BTreeSet<Tick>,
    /// When the last bundle closed (deadline clock).
    last_bundle_activity: Instant,
    /// Last server→seat keepalive.
    last_keepalive: Instant,
    /// This game's replay log, or `None` if logging is disabled/off.
    replay: Option<ServerReplay>,
    /// The highest tick broadcast (for the End record's `final_tick`).
    final_tick: Tick,
}

impl Session {
    /// Create a Lobby session; the creator takes seat 0 (house 1, authority).
    #[allow(clippy::too_many_arguments)]
    pub fn create_lobby(
        id: u32,
        creator: SocketAddr,
        conn_id: u32,
        name: String,
        map: String,
        mut cap: u8,
        credits: i32,
        seed: u32,
        catalog_hash: u64,
        input_delay: u8,
        creator_name: String,
        now: Instant,
    ) -> Session {
        cap = cap.clamp(2, ra_net::wire::MAX_SEATS as u8);
        Session {
            id,
            name,
            map,
            seed,
            credits,
            catalog_hash,
            cap,
            phase: SessionPhase::Lobby,
            input_delay,
            creator,
            seats: vec![Seat {
                addr: creator,
                conn_id,
                name: creator_name,
                house: 1,
                ready: false,
                last_recv: now,
                pending: BTreeMap::new(),
                hashes: BTreeMap::new(),
            }],
            last_activity: now,
            next_bundle_tick: input_delay as u32,
            sent: BTreeMap::new(),
            winning_hash: BTreeMap::new(),
            arbitrated: BTreeSet::new(),
            last_bundle_activity: now,
            last_keepalive: now,
            replay: None,
            final_tick: 0,
        }
    }

    /// The session's current phase.
    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Whether the session is Closed (ready for GC).
    pub fn is_closed(&self) -> bool {
        self.phase == SessionPhase::Closed
    }

    /// A `SESS_LIST` entry for this session.
    pub fn list_entry(&self) -> SessListEntry {
        SessListEntry {
            session_id: self.id,
            name: self.name.clone(),
            map: self.map.clone(),
            seats_taken: self.seats.len() as u8,
            seats: self.cap,
            in_progress: self.phase != SessionPhase::Lobby,
        }
    }

    /// The addresses of every seated connection (for GC / conn unbinding).
    pub fn seat_addrs(&self) -> Vec<SocketAddr> {
        self.seats.iter().map(|s| s.addr).collect()
    }

    fn seat_index_of(&self, addr: SocketAddr) -> Option<usize> {
        self.seats.iter().position(|s| s.addr == addr)
    }

    /// The `SESS_STATE` datagram for this session.
    fn state_msg(&self) -> Datagram {
        Datagram::SessState {
            session_id: self.id,
            phase: self.phase,
            host_seat: 1, // creator = house 1
            delay: self.input_delay,
            seed: self.seed,
            credits: self.credits,
            map: self.map.clone(),
            seats: self
                .seats
                .iter()
                .map(|s| SessSeat {
                    seat: s.house,
                    name: s.name.clone(),
                    house: s.house,
                    ready: s.ready,
                })
                .collect(),
        }
    }

    /// Broadcast the initial lobby state right after creation (creator seated).
    pub fn broadcast_initial(&self, out: &mut Vec<(SocketAddr, Datagram)>) {
        self.broadcast_state(out);
    }

    /// Rebroadcast authoritative lobby state to every seat.
    fn broadcast_state(&self, out: &mut Vec<(SocketAddr, Datagram)>) {
        let msg = self.state_msg();
        for s in &self.seats {
            out.push((s.addr, msg.clone()));
        }
    }

    /// Handle a join (SERVER-DESIGN.md §5): assign the next free seat, or — for a
    /// conn already seated here — refresh idempotently (double-JOIN precedent).
    /// Rejects if full or already started.
    pub fn on_join(
        &mut self,
        addr: SocketAddr,
        conn_id: u32,
        name: String,
        out: &mut Vec<(SocketAddr, Datagram)>,
        counters: &mut Counters,
        now: Instant,
    ) {
        self.last_activity = now;
        if let Some(i) = self.seat_index_of(addr) {
            // Idempotent re-join while still in Lobby: refresh liveness.
            self.seats[i].last_recv = now;
            self.seats[i].conn_id = conn_id;
            self.broadcast_state(out);
            return;
        }
        if self.phase != SessionPhase::Lobby {
            out.push((
                addr,
                Datagram::Reject {
                    reason: ra_net::wire::RejectReason::AlreadyStarted,
                },
            ));
            return;
        }
        if self.seats.len() as u8 >= self.cap {
            out.push((
                addr,
                Datagram::Reject {
                    reason: ra_net::wire::RejectReason::SessionFull,
                },
            ));
            return;
        }
        let house = self.seats.len() as u8 + 1; // next house, ascending join order
        self.seats.push(Seat {
            addr,
            conn_id,
            name,
            house,
            ready: false,
            last_recv: now,
            pending: BTreeMap::new(),
            hashes: BTreeMap::new(),
        });
        counters.seats_joined += 1;
        self.broadcast_state(out);
    }

    /// Toggle a seat's READY and rebroadcast; then attempt to start (§5).
    #[allow(clippy::too_many_arguments)]
    pub fn on_ready(
        &mut self,
        addr: SocketAddr,
        ready: bool,
        out: &mut Vec<(SocketAddr, Datagram)>,
        counters: &mut Counters,
        now: Instant,
        replay_dir: Option<&PathBuf>,
        start_millis: u64,
    ) {
        self.last_activity = now;
        if let Some(i) = self.seat_index_of(addr) {
            self.seats[i].ready = ready;
            self.seats[i].last_recv = now;
        } else {
            return;
        }
        self.broadcast_state(out);
        self.maybe_start(out, counters, now, replay_dir, start_millis);
    }

    /// Fire START when the session is full and every seat is READY (§5, M9-A
    /// policy — seat-0 explicit early-start is an M9-B refinement).
    fn maybe_start(
        &mut self,
        out: &mut Vec<(SocketAddr, Datagram)>,
        counters: &mut Counters,
        now: Instant,
        replay_dir: Option<&PathBuf>,
        start_millis: u64,
    ) {
        if self.phase != SessionPhase::Lobby {
            return;
        }
        let full = self.seats.len() as u8 == self.cap && self.seats.len() >= 2;
        if !full || !self.seats.iter().all(|s| s.ready) {
            return;
        }
        self.phase = SessionPhase::Running;
        self.next_bundle_tick = self.input_delay as u32;
        self.last_bundle_activity = now;
        self.last_keepalive = now;
        counters.games_started += 1;

        let seat_map: Vec<(SeatId, u8)> = self.seats.iter().map(|s| (s.house, s.house)).collect();
        let start = Datagram::SessStart {
            session_id: self.id,
            start_tick: 0,
            input_delay: self.input_delay,
            seat_map,
        };
        for s in &self.seats {
            out.push((s.addr, start.clone()));
        }

        // Open the canonical replay log (§8). Failure degrades (no log).
        if let Some(dir) = replay_dir {
            let path = dir.join(format!("{}.rar1", self.id));
            let header = ReplayHeader {
                replay_version: ra_net::REPLAY_VERSION,
                game_version: GAME_VERSION,
                protocol_version: PROTOCOL_VERSION,
                scenario: self.map.clone(),
                seed: self.seed,
                difficulty: 1, // no AI difficulty in an MP relay game
                credits: self.credits,
                catalog_hash: self.catalog_hash,
                start_millis,
                seats: self
                    .seats
                    .iter()
                    .map(|s| ReplaySeat {
                        seat: s.house,
                        house: s.house,
                        color: s.house,
                    })
                    .collect(),
            };
            self.replay = Some(ServerReplay::create(path, &header));
        }
    }

    /// Remove a seat (leave/kick). Returns `true` if the session dissolved (the
    /// caller GCs it and unbinds every conn). The creator leaving a *lobby*
    /// dissolves it; any other leave just frees the seat.
    pub fn remove_seat(
        &mut self,
        addr: SocketAddr,
        reason: u8,
        out: &mut Vec<(SocketAddr, Datagram)>,
        now: Instant,
    ) -> bool {
        self.last_activity = now;
        let Some(i) = self.seat_index_of(addr) else {
            return false;
        };
        // Tell the leaver (kick path) explicitly.
        out.push((addr, Datagram::SessLeave { conn_id: 0, reason }));

        let was_creator = self.seats[i].addr == self.creator;
        self.seats.remove(i);

        if self.phase == SessionPhase::Lobby && (was_creator || self.seats.is_empty()) {
            // Dissolve: notify remaining seats and mark Closed.
            for s in &self.seats {
                out.push((
                    s.addr,
                    Datagram::SessLeave {
                        conn_id: 0,
                        reason: leave_reason::DISSOLVED,
                    },
                ));
            }
            self.phase = SessionPhase::Closed;
            return true;
        }
        if self.seats.is_empty() {
            // A Running game with no seats left finalizes and closes.
            self.close(EndReason::Quit);
            return true;
        }
        if self.phase == SessionPhase::Lobby {
            self.broadcast_state(out);
        }
        false
    }

    fn close(&mut self, reason: EndReason) {
        if let Some(r) = self.replay.as_mut() {
            r.finalize(reason, self.final_tick);
        }
        self.phase = SessionPhase::Closed;
    }

    // -- sequencer (§6.1) --------------------------------------------------

    /// Ingest a client's own commands (redundant window). Validates seat-house
    /// binding (§7.3), per-tick rate cap and tick window (§7.4/§7.5), files the
    /// entries, and closes any now-complete bundles. Returns `Some(addr)` if the
    /// seat must be kicked (wrong-house binding violation).
    pub fn on_tick_cmds(
        &mut self,
        addr: SocketAddr,
        entries: Vec<(Tick, Vec<Vec<u8>>)>,
        out: &mut Vec<(SocketAddr, Datagram)>,
        counters: &mut Counters,
        now: Instant,
    ) -> Option<SocketAddr> {
        if self.phase != SessionPhase::Running {
            return None;
        }
        let i = self.seat_index_of(addr)?;
        self.seats[i].last_recv = now;
        self.last_activity = now;
        let house = self.seats[i].house;
        let current = self.next_bundle_tick;

        for (tick, blobs) in entries {
            // Tick-window sanity: a stamp too far in the future is abusive.
            if tick > current.saturating_add(MAX_AHEAD_WINDOW) {
                counters.drops += 1;
                continue;
            }
            // Per-tick rate cap.
            if blobs.len() > MAX_CMDS_PER_TICK_PER_SEAT {
                counters.drops += 1;
                continue;
            }
            // Seat-house binding: every command must be for this seat's house.
            let mut ok = true;
            for b in &blobs {
                if b.len() > MAX_CMD_BLOB {
                    ok = false;
                    break;
                }
                match command_house(b) {
                    Ok(h) if h == house => {}
                    Ok(_) => return Some(addr), // wrong-house → kick
                    Err(_) => {
                        ok = false; // malformed blob: drop the entry
                        break;
                    }
                }
            }
            if !ok {
                counters.drops += 1;
                continue;
            }

            if tick < current {
                // Tick already closed. A non-empty payload we did not include is
                // a genuine LATE (§6.1): drop, count, advise. Redundant copies of
                // an already-sequenced tick are ignored silently.
                if !blobs.is_empty() {
                    let we_had_it = self
                        .sent
                        .get(&tick)
                        .and_then(|bs| bs.iter().find(|(s, _)| *s == house))
                        .map(|(_, b)| !b.is_empty())
                        .unwrap_or(false);
                    if !we_had_it {
                        counters.lates += 1;
                        // HASH_VERDICT{WAIT}-style LATE advisory (§6.1).
                        out.push((
                            addr,
                            Datagram::HashVerdictMsg {
                                tick,
                                verdict: HashVerdict::Wait,
                                majority_hash: 0,
                            },
                        ));
                    }
                }
                continue;
            }

            // Open tick: file the entry (present == "seat reached this tick"),
            // deduping redundant copies (first arrival wins).
            self.seats[i].pending.entry(tick).or_insert(blobs);
        }

        self.try_close_bundles(out, now);
        None
    }

    /// Close every bundle whose barrier is satisfied (all seats have filed an
    /// entry for it), broadcast the redundant window, and record to the replay.
    fn try_close_bundles(&mut self, out: &mut Vec<(SocketAddr, Datagram)>, now: Instant) {
        let mut closed_any = false;
        loop {
            let t = self.next_bundle_tick;
            let all_have = self.seats.iter().all(|s| s.pending.contains_key(&t));
            if !all_have {
                break;
            }
            self.close_tick(t, now);
            closed_any = true;
        }
        if closed_any {
            self.broadcast_bundle_window(out);
        }
    }

    /// Assemble and record one tick's canonical bundle (seat-ascending).
    fn close_tick(&mut self, t: Tick, now: Instant) {
        let mut seats_blobs: SeatBlobs = Vec::with_capacity(self.seats.len());
        for s in &mut self.seats {
            let blobs = s.pending.remove(&t).unwrap_or_default();
            seats_blobs.push((s.house, blobs));
        }
        seats_blobs.sort_by_key(|(h, _)| *h); // canonical house-ascending order
        if let Some(r) = self.replay.as_mut() {
            r.on_bundle(t, &seats_blobs);
        }
        self.sent.insert(t, seats_blobs);
        self.final_tick = self.final_tick.max(t);
        self.next_bundle_tick = t + 1;
        self.last_bundle_activity = now;
        self.prune();
    }

    /// Broadcast the redundant `TICK_BUNDLE` window (last [`CARRY_TICKS`] closed
    /// bundles) to every seat.
    fn broadcast_bundle_window(&self, out: &mut Vec<(SocketAddr, Datagram)>) {
        let Some((&end, _)) = self.sent.iter().next_back() else {
            return;
        };
        let lo = end.saturating_sub(CARRY_TICKS.saturating_sub(1));
        let entries: Vec<BundleEntry> = self
            .sent
            .range(lo..=end)
            .map(|(&tick, seats)| BundleEntry {
                tick,
                ctrl: Vec::new(), // SESS_TIMING hook reserved (M9-B); none in M9-A
                seats: seats.clone(),
            })
            .collect();
        let msg = Datagram::TickBundle { entries };
        for s in &self.seats {
            out.push((s.addr, msg.clone()));
        }
    }

    /// Answer a client NACK: re-send the bundle window it is missing from.
    pub fn on_nack(&mut self, addr: SocketAddr, from: Tick, out: &mut Vec<(SocketAddr, Datagram)>) {
        if self.phase != SessionPhase::Running || self.seat_index_of(addr).is_none() {
            return;
        }
        let entries: Vec<BundleEntry> = self
            .sent
            .range(from..)
            .map(|(&tick, seats)| BundleEntry {
                tick,
                ctrl: Vec::new(),
                seats: seats.clone(),
            })
            .collect();
        if !entries.is_empty() {
            // Chunk to the tick-entry cap so a long re-send stays under the
            // datagram size bound.
            for chunk in entries.chunks(CARRY_TICKS as usize) {
                out.push((
                    addr,
                    Datagram::TickBundle {
                        entries: chunk.to_vec(),
                    },
                ));
            }
        }
    }

    /// Ingest per-tick state hashes and arbitrate any now-complete ticks (§6.3).
    pub fn on_tick_hash(
        &mut self,
        addr: SocketAddr,
        entries: Vec<(Tick, u64)>,
        out: &mut Vec<(SocketAddr, Datagram)>,
        now: Instant,
    ) {
        if self.phase != SessionPhase::Running {
            return;
        }
        let Some(i) = self.seat_index_of(addr) else {
            return;
        };
        self.seats[i].last_recv = now;
        self.last_activity = now;
        let mut ticks_touched: Vec<Tick> = Vec::new();
        for (tick, hash) in entries {
            if self.arbitrated.contains(&tick) {
                continue;
            }
            self.seats[i].hashes.entry(tick).or_insert(hash);
            ticks_touched.push(tick);
        }
        for tick in ticks_touched {
            self.maybe_arbitrate(tick, out);
        }
        self.prune();
    }

    /// If every live seat has reported `tick`'s hash, decide the winner, record
    /// it to the replay chain, and notify divergent seats (§6.3).
    fn maybe_arbitrate(&mut self, tick: Tick, out: &mut Vec<(SocketAddr, Datagram)>) {
        if self.arbitrated.contains(&tick) {
            return;
        }
        let mut reports: Vec<(SeatId, u64)> = Vec::with_capacity(self.seats.len());
        for s in &self.seats {
            match s.hashes.get(&tick) {
                Some(&h) => reports.push((s.house, h)),
                None => return, // not everyone has reported yet
            }
        }
        let Some(winner) = arbitrate(&reports) else {
            return;
        };
        self.arbitrated.insert(tick);
        self.winning_hash.insert(tick, winner);
        if let Some(r) = self.replay.as_mut() {
            r.on_winning_hash(tick, winner);
        }
        // Silence on unanimity; verdicts only to the divergent (§6.3).
        if reports.iter().any(|(_, h)| *h != winner) {
            for s in &self.seats {
                if s.hashes.get(&tick) != Some(&winner) {
                    out.push((
                        s.addr,
                        Datagram::HashVerdictMsg {
                            tick,
                            verdict: HashVerdict::YouDiverged,
                            majority_hash: winner,
                        },
                    ));
                }
            }
        }
    }

    /// Bounded-memory sweep of sequencer history.
    fn prune(&mut self) {
        let cutoff = self.next_bundle_tick.saturating_sub(KEEP_TICKS);
        self.sent = self.sent.split_off(&cutoff);
        self.winning_hash = self.winning_hash.split_off(&cutoff);
        self.arbitrated = self.arbitrated.split_off(&cutoff);
        for s in &mut self.seats {
            s.hashes = s.hashes.split_off(&cutoff);
            // pending entries below the cutoff can never close a future bundle.
            s.pending = s.pending.split_off(&cutoff);
        }
    }

    /// Per-tick maintenance: bundle deadline, idle timeouts, keepalives.
    /// Returns `true` if the session became Closed (caller GCs + unbinds conns).
    pub fn advance(
        &mut self,
        out: &mut Vec<(SocketAddr, Datagram)>,
        counters: &mut Counters,
        now: Instant,
    ) -> bool {
        match self.phase {
            SessionPhase::Lobby => {
                if now.duration_since(self.last_activity) >= LOBBY_IDLE_TIMEOUT {
                    for s in &self.seats {
                        out.push((
                            s.addr,
                            Datagram::SessLeave {
                                conn_id: 0,
                                reason: leave_reason::DISSOLVED,
                            },
                        ));
                    }
                    self.phase = SessionPhase::Closed;
                    return true;
                }
                false
            }
            SessionPhase::Running => {
                // Bundle deadline: force-close a stalled tick with missing seats
                // empty + LATE advisories (§6.2), so one slow seat can't hang all.
                if now.duration_since(self.last_bundle_activity) >= BUNDLE_DEADLINE {
                    let t = self.next_bundle_tick;
                    let all_have = self.seats.iter().all(|s| s.pending.contains_key(&t));
                    if !all_have {
                        for s in &self.seats {
                            if !s.pending.contains_key(&t) {
                                counters.drops += 1;
                                out.push((
                                    s.addr,
                                    Datagram::HashVerdictMsg {
                                        tick: t,
                                        verdict: HashVerdict::Wait,
                                        majority_hash: 0,
                                    },
                                ));
                            }
                        }
                        self.close_tick(t, now);
                        self.broadcast_bundle_window(out);
                    }
                }
                // All seats gone silent → close.
                let all_gone = self
                    .seats
                    .iter()
                    .all(|s| now.duration_since(s.last_recv) >= RUNNING_ALL_GONE_TIMEOUT);
                if all_gone {
                    // Timeout is not a kick — no counter bump; just close.
                    self.close(EndReason::Quit);
                    return true;
                }
                // Keepalive so idle clients don't time the server out.
                if now.duration_since(self.last_keepalive) >= KEEPALIVE_INTERVAL {
                    self.last_keepalive = now;
                    for s in &self.seats {
                        out.push((
                            s.addr,
                            Datagram::KeepAlive {
                                tick: self.next_bundle_tick,
                            },
                        ));
                    }
                }
                false
            }
            SessionPhase::Closed => true,
        }
    }

    /// Force-close on server shutdown (§9): notify seats, finalize the replay.
    pub fn shutdown(&mut self, out: &mut Vec<(SocketAddr, Datagram)>) {
        for s in &self.seats {
            out.push((
                s.addr,
                Datagram::SessLeave {
                    conn_id: 0,
                    reason: leave_reason::SERVER_SHUTDOWN,
                },
            ));
        }
        if self.phase == SessionPhase::Running {
            self.close(EndReason::Quit);
        } else {
            self.phase = SessionPhase::Closed;
        }
    }
}

/// The house byte an encoded command blob claims — re-exported convenience so
/// tests can build wrong-house payloads without reaching into `ra_net::wire`.
pub fn blob_house(blob: &[u8]) -> Result<u8, WireError> {
    command_house(blob)
}
