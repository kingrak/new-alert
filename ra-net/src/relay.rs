//! Stage 3 (M9-A): the **client side** of the relay — `RelayClient` (the lobby
//! handshake over the internet) and [`RelayTransport`] (the tick pipeline),
//! implementing [`CommandTransport`] against a server address.
//!
//! Unlike [`crate::LanTransport`], where each peer stamps and executes its own
//! commands and the barrier stalls on the *other* peer, here **everything
//! round-trips through the sequencer**: the client stamps its own commands
//! (sender-clock-pure, QUEUE.CPP:2526 — identical to LAN), ships them as opaque
//! blobs in `TICK_CMDS`, and executes the server's canonical `TICK_BUNDLE` for
//! *every* seat including itself. This is the same `PollResult::Ready(TickBundle)`
//! seam the sim already consumes; only the counterpart changed (SERVER-DESIGN.md
//! §6.1). The redundant-carry + NACK loss discipline is reused verbatim.
//!
//! Timing is transport-layer only (keepalive/timeout/NACK pacing); execution
//! ticks are fixed by the sender stamp, so the sim stays deterministic (§4.2).

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
use crate::wire::{
    self, Datagram, HashVerdict, SessListEntry, SessSeat, SessionPhase, GAME_VERSION,
};

/// Redundant TICK carry window (mirrors [`crate::REDUNDANT_TICKS`]).
const CARRY_TICKS: u32 = 8;
/// History kept for redundant carry / NACK re-send.
const KEEP_TICKS: u32 = 64;
/// Handshake re-send / keepalive cadence.
const HANDSHAKE_INTERVAL: Duration = Duration::from_millis(400);
/// In-game keepalive cadence.
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(250);
/// Declare the server gone after this long without a datagram.
pub const SERVER_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshake step timeout (no server answer).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// While stalled at the barrier, NACK every this-many consecutive Waiting polls.
const NACK_EVERY_STALL_POLLS: u32 = 16;

// ---------------------------------------------------------------------------
// Lobby handshake: RelayClient
// ---------------------------------------------------------------------------

/// What the client wants to do once connected.
#[derive(Clone, Debug)]
pub enum RelayIntent {
    /// Create a new session (creator gets seat 0 / house 1, settings authority).
    Create {
        /// Session display name.
        name: String,
        /// Scenario filename.
        map: String,
        /// Seat count (2..=8).
        seats: u8,
        /// Starting credits.
        credits: i32,
        /// World RNG seed.
        seed: u32,
        /// Content-catalog hash.
        catalog_hash: u64,
    },
    /// Join an existing session by id.
    Join {
        /// The session to join.
        session_id: u32,
    },
    /// Just browse the session list (no create/join).
    Browse,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    /// SRV_HELLO sent, awaiting SRV_WELCOME.
    Hello,
    /// Connected; issuing the create/join and awaiting SESS_STATE.
    Connecting,
    /// In the lobby (SESS_STATE received); may set ready; awaiting SESS_START.
    Lobby,
    /// Browsing the session list (no create/join intent).
    Browsing,
    /// SESS_START received — [`RelayClient::into_transport`] may be called.
    Started,
    /// Terminal failure.
    Failed,
}

/// The client's lobby driver: SRV_HELLO → (create|join) → ready → START, every
/// wait bounded. Poll-driven from the menu loop; no threads (§4.7).
#[derive(Debug)]
pub struct RelayClient {
    sock: UdpSocket,
    server: SocketAddr,
    name: String,
    intent: RelayIntent,
    phase: Phase,
    conn_id: Option<u32>,
    session_id: Option<u32>,
    my_house: Option<SeatId>,
    delay: Option<u8>,
    seed: u32,
    credits: i32,
    map: String,
    seats: Vec<SessSeat>,
    seat_map: Vec<(SeatId, u8)>,
    ready: bool,
    sessions: Vec<SessListEntry>,
    started_at: Instant,
    last_send: Instant,
    last_recv: Instant,
    timeout: Duration,
    error: Option<String>,
}

impl RelayClient {
    /// Bind a fresh socket and open the connection to `server`, kicking off the
    /// SRV_HELLO handshake.
    pub fn connect(server: SocketAddr, name: &str, intent: RelayIntent) -> io::Result<RelayClient> {
        let sock = crate::platform::bind_join_socket()?;
        sock.set_nonblocking(true)?;
        let now = Instant::now();
        let mut c = RelayClient {
            sock,
            server,
            name: name.to_string(),
            intent,
            phase: Phase::Hello,
            conn_id: None,
            session_id: None,
            my_house: None,
            delay: None,
            seed: 0,
            credits: 0,
            map: String::new(),
            seats: Vec::new(),
            seat_map: Vec::new(),
            ready: false,
            sessions: Vec::new(),
            started_at: now,
            last_send: now,
            last_recv: now,
            timeout: HANDSHAKE_TIMEOUT,
            error: None,
        };
        c.send_hello();
        Ok(c)
    }

    /// Shrink the handshake timeout (tests).
    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    /// The bound local port (tests aim the server's replies here — but the
    /// server learns it from the source address anyway).
    pub fn local_port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// Terminal error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether START has arrived.
    pub fn started(&self) -> bool {
        self.phase == Phase::Started
    }

    /// Whether we are in the lobby (may toggle ready).
    pub fn in_lobby(&self) -> bool {
        self.phase == Phase::Lobby
    }

    /// The authoritative lobby seats (rendered verbatim by the UI, §5).
    pub fn seats(&self) -> &[SessSeat] {
        &self.seats
    }

    /// The discovered session list (Browse intent).
    pub fn sessions(&self) -> &[SessListEntry] {
        &self.sessions
    }

    /// This client's assigned house, once known.
    pub fn my_house(&self) -> Option<SeatId> {
        self.my_house
    }

    /// Confirm READY (re-sent until START).
    pub fn set_ready(&mut self) {
        if self.phase == Phase::Lobby {
            self.ready = true;
            if let Some(conn_id) = self.conn_id {
                self.send(&Datagram::SessReady {
                    conn_id,
                    ready: true,
                });
            }
        }
    }

    /// Request a fresh session list (Browse).
    pub fn request_list(&mut self) {
        if let Some(conn_id) = self.conn_id {
            self.send(&Datagram::SessListReq { conn_id });
        }
    }

    fn send(&mut self, d: &Datagram) {
        let bytes = wire::encode(d);
        let _ = self.sock.send_to(&bytes, self.server);
        self.last_send = Instant::now();
    }

    fn send_hello(&mut self) {
        self.send(&Datagram::SrvHello {
            game_version: GAME_VERSION,
            client_nonce: 0x5EED_1234,
        });
    }

    fn fail(&mut self, msg: &str) {
        if self.phase != Phase::Failed {
            self.phase = Phase::Failed;
            self.error = Some(msg.to_string());
        }
    }

    fn issue_intent(&mut self) {
        let Some(conn_id) = self.conn_id else { return };
        match self.intent.clone() {
            RelayIntent::Create {
                name,
                map,
                seats,
                credits,
                seed,
                catalog_hash,
            } => {
                self.my_house = Some(1); // creator is always house 1
                self.send(&Datagram::SessCreate {
                    conn_id,
                    name,
                    map,
                    seats,
                    credits,
                    seed,
                    catalog_hash,
                });
                self.phase = Phase::Connecting;
            }
            RelayIntent::Join { session_id } => {
                let name = self.name.clone();
                self.send(&Datagram::SessJoin {
                    conn_id,
                    session_id,
                    name,
                });
                self.phase = Phase::Connecting;
            }
            RelayIntent::Browse => {
                self.send(&Datagram::SessListReq { conn_id });
                self.phase = Phase::Browsing;
            }
        }
    }

    /// Drive the handshake. Call every frame.
    pub fn poll(&mut self) {
        if matches!(self.phase, Phase::Failed | Phase::Started) {
            return;
        }

        // Re-send / keepalive pacing (loss tolerance).
        if self.last_send.elapsed() >= HANDSHAKE_INTERVAL {
            match self.phase {
                Phase::Hello => self.send_hello(),
                Phase::Connecting => self.issue_intent(),
                Phase::Lobby => {
                    if let Some(conn_id) = self.conn_id {
                        if self.ready {
                            self.send(&Datagram::SessReady {
                                conn_id,
                                ready: true,
                            });
                        } else {
                            self.send(&Datagram::KeepAlive { tick: 0 });
                        }
                    }
                }
                Phase::Browsing => self.request_list(),
                _ => {}
            }
        }

        // Timeouts (only while awaiting a first answer / lobby liveness).
        match self.phase {
            Phase::Hello | Phase::Connecting => {
                if self.started_at.elapsed() >= self.timeout && self.error.is_none() {
                    self.fail("no response from server (timed out)");
                    return;
                }
            }
            Phase::Lobby => {
                if self.last_recv.elapsed() >= SERVER_TIMEOUT {
                    self.fail("lost contact with server");
                    return;
                }
            }
            _ => {}
        }

        // Pump.
        let mut buf = [0u8; 65536];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, srcaddr)) => {
                    if srcaddr != self.server {
                        continue;
                    }
                    self.last_recv = Instant::now();
                    match wire::decode(&buf[..n]) {
                        Ok(d) => {
                            self.receive(d);
                            if matches!(self.phase, Phase::Failed | Phase::Started) {
                                return;
                            }
                        }
                        Err(wire::WireError::ProtocolMismatch { theirs }) => {
                            self.fail(&format!(
                                "protocol mismatch (server {theirs}, ours {})",
                                wire::PROTOCOL_VERSION
                            ));
                            return;
                        }
                        Err(_) => {}
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn receive(&mut self, d: Datagram) {
        match d {
            Datagram::SrvWelcome { conn_id, .. } => {
                if self.conn_id.is_none() {
                    self.conn_id = Some(conn_id);
                    self.started_at = Instant::now();
                    self.issue_intent();
                }
            }
            Datagram::Reject { reason } => {
                self.fail(&format!("server rejected the connection: {reason:?}"));
            }
            Datagram::SessList { entries } => {
                self.sessions = entries;
            }
            Datagram::SessState {
                session_id,
                phase,
                delay,
                seed,
                credits,
                map,
                seats,
                ..
            } => {
                if phase == SessionPhase::Closed {
                    self.fail("session closed");
                    return;
                }
                self.session_id = Some(session_id);
                self.delay = Some(delay);
                self.seed = seed;
                self.credits = credits;
                self.map = map;
                // Learn our house: creator is always house 1; a joiner matches by
                // name (callers use distinct names, § M9-A note).
                if self.my_house.is_none() {
                    if let Some(s) = seats.iter().find(|s| s.name == self.name) {
                        self.my_house = Some(s.house);
                    }
                }
                self.seats = seats;
                if self.phase == Phase::Connecting {
                    self.phase = Phase::Lobby;
                }
            }
            Datagram::SessStart {
                session_id,
                input_delay,
                seat_map,
                ..
            } => {
                self.session_id = Some(session_id);
                self.delay = Some(input_delay);
                self.seat_map = seat_map;
                if self.my_house.is_some() {
                    self.phase = Phase::Started;
                }
            }
            Datagram::SessLeave { reason, .. } => {
                self.fail(&format!("removed from session (reason {reason})"));
            }
            Datagram::KeepAlive { .. } => {}
            _ => {}
        }
    }

    /// Become the in-game transport (call once [`RelayClient::started`]).
    pub fn into_transport(self) -> io::Result<RelayTransport> {
        let conn_id = self.conn_id.expect("into_transport requires SRV_WELCOME");
        let my_house = self.my_house.expect("into_transport requires a seat");
        let delay = self.delay.unwrap_or(crate::DEFAULT_INPUT_DELAY as u8) as u32;
        let houses: Vec<SeatId> = if self.seat_map.is_empty() {
            vec![my_house]
        } else {
            let mut h: Vec<SeatId> = self.seat_map.iter().map(|(_, house)| *house).collect();
            h.sort_unstable();
            h.dedup();
            h
        };
        RelayTransport::new(self.sock, self.server, conn_id, my_house, houses, delay)
    }

    /// Leave / cancel (best-effort).
    pub fn leave(mut self) {
        if let Some(conn_id) = self.conn_id {
            self.send(&Datagram::SessLeave { conn_id, reason: 0 });
        }
    }
}

// ---------------------------------------------------------------------------
// In-game: RelayTransport
// ---------------------------------------------------------------------------

/// One client endpoint of a server-sequenced game (implements
/// [`CommandTransport`]). Ships its own commands to the server and executes the
/// server's canonical bundles for every seat.
#[derive(Debug)]
pub struct RelayTransport {
    sock: UdpSocket,
    server: SocketAddr,
    conn_id: u32,
    my_house: SeatId,
    /// All seats' houses (for synthesising the empty prologue ticks).
    houses: Vec<SeatId>,
    delay: u32,
    sched: InputScheduler,
    current: Tick,
    stamped: bool,
    /// Canonical bundles delivered by the server, keyed by exec tick (decoded).
    bundles: BTreeMap<Tick, TickBundle>,
    /// Our own stamped command blobs per exec tick (redundant carry).
    sent_cmds: BTreeMap<Tick, Vec<Vec<u8>>>,
    /// Our reported hashes per tick (redundant carry + local record for verdict).
    sent_hashes: BTreeMap<Tick, u64>,
    desync: Option<DesyncDetected>,
    lost: Option<ConnectionLost>,
    last_recv: Instant,
    last_send: Instant,
    server_timeout: Duration,
    stall_polls: u32,
    stalls: u64,
    nacks_sent: u64,
    decode_errors: u64,
}

impl RelayTransport {
    fn new(
        sock: UdpSocket,
        server: SocketAddr,
        conn_id: u32,
        my_house: SeatId,
        houses: Vec<SeatId>,
        delay: u32,
    ) -> io::Result<RelayTransport> {
        sock.set_nonblocking(true)?;
        let now = Instant::now();
        Ok(RelayTransport {
            sock,
            server,
            conn_id,
            my_house,
            houses,
            delay,
            sched: InputScheduler::new(delay),
            current: 0,
            stamped: false,
            bundles: BTreeMap::new(),
            sent_cmds: BTreeMap::new(),
            sent_hashes: BTreeMap::new(),
            desync: None,
            lost: None,
            last_recv: now,
            last_send: now,
            server_timeout: SERVER_TIMEOUT,
            stall_polls: 0,
            stalls: 0,
            nacks_sent: 0,
            decode_errors: 0,
        })
    }

    /// This endpoint's seat (house) id.
    pub fn seat(&self) -> SeatId {
        self.my_house
    }

    /// The latched divergence state, if any (server said YOU_DIVERGED).
    pub fn desync(&self) -> Option<DesyncDetected> {
        self.desync
    }

    /// The latched server-gone state, if any.
    pub fn connection_lost(&self) -> Option<ConnectionLost> {
        self.lost
    }

    /// Total polls that returned [`PollResult::Waiting`].
    pub fn stall_count(&self) -> u64 {
        self.stalls
    }

    /// NACKs sent (proves the loss backstop ran).
    pub fn nacks_sent(&self) -> u64 {
        self.nacks_sent
    }

    /// Datagrams that failed to decode (ignored per fuzz-safety).
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }

    /// Shrink the server timeout (tests).
    pub fn set_server_timeout(&mut self, t: Duration) {
        self.server_timeout = t;
    }

    /// Clean exit: tell the server we are leaving (best-effort burst).
    pub fn send_leave(&mut self) {
        let d = Datagram::SessLeave {
            conn_id: self.conn_id,
            reason: 0,
        };
        let bytes = wire::encode(&d);
        for _ in 0..3 {
            let _ = self.sock.send_to(&bytes, self.server);
        }
    }

    fn send(&mut self, d: &Datagram) {
        let bytes = wire::encode(d);
        let _ = self.sock.send_to(&bytes, self.server);
        self.last_send = Instant::now();
    }

    /// The redundant TICK_CMDS window ending at `end`.
    fn cmds_window(&self, end: Tick) -> Datagram {
        let lo = end.saturating_sub(CARRY_TICKS.saturating_sub(1));
        let entries: Vec<(Tick, Vec<Vec<u8>>)> = self
            .sent_cmds
            .range(lo..=end)
            .map(|(&t, blobs)| (t, blobs.clone()))
            .collect();
        Datagram::TickCmds {
            conn_id: self.conn_id,
            entries,
        }
    }

    /// The redundant TICK_HASH window ending at the newest reported tick.
    fn hashes_window(&self) -> Option<Datagram> {
        let (&end, _) = self.sent_hashes.iter().next_back()?;
        let lo = end.saturating_sub(CARRY_TICKS.saturating_sub(1));
        let entries: Vec<(Tick, u64)> = self
            .sent_hashes
            .range(lo..=end)
            .map(|(&t, &h)| (t, h))
            .collect();
        Some(Datagram::TickHash {
            conn_id: self.conn_id,
            entries,
        })
    }

    fn empty_bundle(&self, tick: Tick) -> TickBundle {
        TickBundle {
            tick,
            seats: self.houses.iter().map(|&h| (h, Vec::new())).collect(),
        }
    }

    fn receive(&mut self, d: Datagram) {
        match d {
            Datagram::TickBundle { entries } => {
                for e in entries {
                    if e.tick < self.current {
                        continue; // already executed
                    }
                    if self.bundles.contains_key(&e.tick) {
                        continue; // redundant copy (first wins)
                    }
                    let mut seats: Vec<(SeatId, Vec<Command>)> = Vec::with_capacity(e.seats.len());
                    for (seat, blobs) in e.seats {
                        let mut cmds = Vec::with_capacity(blobs.len());
                        for b in blobs {
                            match wire::decode_command(&b) {
                                Ok(c) => cmds.push(c),
                                Err(_) => self.decode_errors += 1,
                            }
                        }
                        seats.push((seat, cmds));
                    }
                    seats.sort_by_key(|(s, _)| *s); // canonical order (server already sorts)
                    self.bundles.insert(
                        e.tick,
                        TickBundle {
                            tick: e.tick,
                            seats,
                        },
                    );
                }
            }
            Datagram::HashVerdictMsg {
                tick,
                verdict,
                majority_hash,
            } => {
                if verdict == HashVerdict::YouDiverged {
                    let local = self.sent_hashes.get(&tick).copied().unwrap_or(0);
                    let d = DesyncDetected {
                        tick,
                        local_hash: local,
                        remote_hash: majority_hash,
                        peer: self.my_house,
                    };
                    match self.desync {
                        Some(e) if e.tick <= d.tick => {}
                        _ => self.desync = Some(d),
                    }
                }
                // HashVerdict::Wait is the LATE advisory (§6.1): informational.
            }
            Datagram::SessLeave { .. } => {
                let tick = self.current;
                self.lost.get_or_insert(ConnectionLost {
                    tick,
                    reason: LostReason::PeerQuit,
                });
            }
            Datagram::KeepAlive { .. } => {}
            _ => {}
        }
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, srcaddr)) => {
                    if srcaddr != self.server {
                        continue;
                    }
                    self.last_recv = Instant::now();
                    match wire::decode(&buf[..n]) {
                        Ok(d) => self.receive(d),
                        Err(_) => self.decode_errors += 1,
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn prune(&mut self) {
        let cutoff = self.current.saturating_sub(KEEP_TICKS);
        self.sent_cmds = self.sent_cmds.split_off(&cutoff);
        self.sent_hashes = self.sent_hashes.split_off(&cutoff);
    }

    /// Service the connection without advancing the protocol (keepalive + pump),
    /// for menus/pauses — mirrors [`crate::LanTransport::service`].
    pub fn service(&mut self) {
        if self.desync.is_some() || self.lost.is_some() {
            return;
        }
        self.pump();
        if self.last_send.elapsed() >= KEEPALIVE_INTERVAL {
            let d = Datagram::KeepAlive { tick: self.current };
            self.send(&d);
        }
        if self.last_recv.elapsed() >= self.server_timeout {
            self.lost = Some(ConnectionLost {
                tick: self.current,
                reason: LostReason::Timeout,
            });
        }
    }
}

impl CommandTransport for RelayTransport {
    fn submit(&mut self, cmd: Command) {
        if self.desync.is_some() || self.lost.is_some() {
            return;
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
            "relay ticks must be polled sequentially (expected {}, got {})",
            self.current, tick
        );

        // First poll of this tick: stamp staged input for `tick + delay` and ship
        // the redundant TICK_CMDS window (even an empty tick ships an entry so the
        // server learns this seat reached the tick — the barrier depends on it).
        if !self.stamped {
            let (exec_tick, cmds) = self.sched.stamp(tick);
            let blobs: Vec<Vec<u8>> = cmds.iter().map(wire::encode_command).collect();
            self.sent_cmds.insert(exec_tick, blobs);
            self.stamped = true;
            self.prune();
            let window = self.cmds_window(exec_tick);
            self.send(&window);
        }

        self.pump();
        if let Some(d) = self.desync {
            return PollResult::Desync(d);
        }
        if let Some(l) = self.lost {
            return PollResult::ConnectionLost(l);
        }

        if self.last_send.elapsed() >= KEEPALIVE_INTERVAL {
            let d = Datagram::KeepAlive { tick };
            self.send(&d);
        }
        if self.last_recv.elapsed() >= self.server_timeout {
            let l = ConnectionLost {
                tick,
                reason: LostReason::Timeout,
            };
            self.lost = Some(l);
            return PollResult::ConnectionLost(l);
        }

        // Barrier: ticks `0..delay` are the empty prologue (the server sequences
        // from `delay` onward; the client synthesises the prologue locally, same
        // rule as LAN's `resume_base + delay`).
        let bundle = if tick < self.delay {
            Some(self.empty_bundle(tick))
        } else {
            self.bundles.remove(&tick)
        };

        let Some(bundle) = bundle else {
            self.stalls += 1;
            self.stall_polls += 1;
            if self.stall_polls.is_multiple_of(NACK_EVERY_STALL_POLLS) {
                self.nacks_sent += 1;
                self.send(&Datagram::Nack { from: tick });
                // Re-push our own window in case the server is missing ours.
                if let Some((&newest, _)) = self.sent_cmds.iter().next_back() {
                    let window = self.cmds_window(newest);
                    self.send(&window);
                }
            }
            return PollResult::Waiting;
        };

        self.current += 1;
        self.stamped = false;
        self.stall_polls = 0;
        PollResult::Ready(bundle)
    }

    fn report_hash(&mut self, tick: Tick, hash: u64) {
        if self.desync.is_some() || self.lost.is_some() {
            return;
        }
        self.sent_hashes.insert(tick, hash);
        if let Some(window) = self.hashes_window() {
            self.send(&window);
        }
    }
}
