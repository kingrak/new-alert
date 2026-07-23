//! `ra-server` — the M9 **relay sequencer** (SERVER-DESIGN.md). A std-only,
//! single-threaded, poll-driven state machine that orders clients' commands into
//! canonical tick bundles, broadcasts them, arbitrates hash disputes, and records
//! the canonical command log per game. It is an **authoritative sequencer, never
//! a sim host** (§1): it loads no map, runs no tick, and depends on no `ra-sim` —
//! command payloads are opaque validated bytes (§3/§7).
//!
//! # Testable core
//!
//! [`Server`] is pure with respect to I/O and the clock. Feed it datagrams with
//! [`Server::recv`], advance its timers with [`Server::advance_time`] (both take
//! `now: Instant` — **no wall-clock read ever happens inside the logic**, §9),
//! and drain its replies with [`Server::take_outgoing`]. `main.rs` wires these to
//! one non-blocking `UdpSocket`; CI wires them to in-process localhost sockets
//! and synthetic time (§11).

pub mod relay;
pub mod replay;
pub mod session;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram, RejectReason, GAME_VERSION};

use session::{leave_reason, Session};

/// Default per-connection datagram rate cap (§7.5); overridable via
/// [`ServerConfig::max_dgrams_per_sec`].
pub const MAX_DGRAMS_PER_SEC: usize = 60;
/// Drop a connection that has said nothing this long and holds no seat.
const CONN_IDLE_TIMEOUT: Duration = Duration::from_secs(130);
/// Default UDP port (§9, distinct from LAN discovery 21057).
pub const DEFAULT_PORT: u16 = 21058;
/// Fixed input delay for M9-A (no adaptive; §6.2). 6 ticks at 15 Hz ≈ 400 ms of
/// round-trip budget — sized for internet RTT (vs LAN's 3), where a command must
/// reach the server and the canonical bundle return before its execution tick.
/// M9-B replaces this with the runtime MaxAhead retune.
pub const RELAY_INPUT_DELAY: u8 = 6;

/// Live counters exported by `STATUS` (§9/§10).
#[derive(Clone, Debug, Default)]
pub struct Counters {
    /// Sessions ever created.
    pub sessions_created: u64,
    /// Games ever started (Lobby → Running).
    pub games_started: u64,
    /// Seats ever joined (excludes the creator's seat 0).
    pub seats_joined: u64,
    /// Commands/entries dropped by validation (over-cap, out-of-window, malformed).
    pub drops: u64,
    /// Late commands dropped after their tick had closed (§6.1).
    pub lates: u64,
    /// Seats kicked (flood / wrong-house binding).
    pub kicks: u64,
    /// Datagrams that failed to decode (ignored, never panic).
    pub decode_errors: u64,
}

/// Server configuration (ops surface, §9).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Max concurrent sessions (§9 "sessions ≤ 256").
    pub max_sessions: usize,
    /// Fixed input delay in ticks (M9-A).
    pub input_delay: u8,
    /// Directory for `<id>.rar1` replay logs, or `None` to disable logging.
    pub replay_dir: Option<PathBuf>,
    /// Recording start time (Unix epoch ms) stamped into replay headers; the
    /// shell layer supplies it (the core reads no clock).
    pub start_millis: u64,
    /// RNG seed for conn-id / nonce generation (deterministic in tests).
    pub rng_seed: u64,
    /// Per-connection datagram rate cap in a trailing 1s window (§7.5). Sustained
    /// flooding past twice this kicks the connection. Tests that run the poll
    /// loop faster than real time raise this so their bursty (but legitimate)
    /// traffic is not misread as a flood.
    pub max_dgrams_per_sec: usize,
}

impl Default for ServerConfig {
    fn default() -> ServerConfig {
        ServerConfig {
            max_sessions: 256,
            input_delay: RELAY_INPUT_DELAY,
            replay_dir: None,
            start_millis: 0,
            rng_seed: 0x9E37_79B9_7F4A_7C15,
            max_dgrams_per_sec: MAX_DGRAMS_PER_SEC,
        }
    }
}

/// A connected client (transport identity = source address).
#[derive(Debug)]
struct Conn {
    /// The id echoed in every C→S message (off-path spoof guard, §7.2).
    conn_id: u32,
    /// The session this conn is seated in, if any.
    session_id: Option<u32>,
    /// Datagram timestamps in the trailing 1s window (rate cap).
    window: Vec<Instant>,
    /// Last datagram time (idle GC).
    last_recv: Instant,
}

/// A tiny SplitMix64 for conn-id / nonce generation. The server is **not** the
/// deterministic sim, so ordinary (seeded, for test reproducibility) randomness
/// is fine here.
#[derive(Debug)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn u32_nonzero(&mut self) -> u32 {
        loop {
            let v = self.next() as u32;
            if v != 0 {
                return v;
            }
        }
    }
}

/// The relay sequencer.
pub struct Server {
    config: ServerConfig,
    rng: SplitMix64,
    conns: BTreeMap<SocketAddr, Conn>,
    sessions: BTreeMap<u32, Session>,
    next_session_id: u32,
    counters: Counters,
    /// Datagram replies staged this poll (encoded on drain).
    pending: Vec<(SocketAddr, Datagram)>,
    /// Raw (non-wire) replies — the plain-text `STATUS` answer.
    pending_raw: Vec<(SocketAddr, Vec<u8>)>,
}

impl Server {
    /// Build a server with the given config. No socket is bound (the caller owns
    /// I/O); state is entirely in memory.
    pub fn new(config: ServerConfig) -> Server {
        let rng = SplitMix64(config.rng_seed ^ 0xD1B5_4A32_D192_ED03);
        Server {
            config,
            rng,
            conns: BTreeMap::new(),
            sessions: BTreeMap::new(),
            next_session_id: 1,
            counters: Counters::default(),
            pending: Vec::new(),
            pending_raw: Vec::new(),
        }
    }

    /// A snapshot of the live counters.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Number of live sessions (Lobby + Running, before GC).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// The `STATUS` plain-text report (§9).
    pub fn status_text(&self) -> String {
        let c = &self.counters;
        let running = self
            .sessions
            .values()
            .filter(|s| s.phase() == wire::SessionPhase::Running)
            .count();
        format!(
            "ra-server status\n\
             sessions_live {}\n\
             sessions_running {}\n\
             sessions_created {}\n\
             games {}\n\
             seats {}\n\
             conns {}\n\
             drops {}\n\
             lates {}\n\
             kicks {}\n\
             decode_errors {}\n",
            self.sessions.len(),
            running,
            c.sessions_created,
            c.games_started,
            c.seats_joined,
            self.conns.len(),
            c.drops,
            c.lates,
            c.kicks,
            c.decode_errors,
        )
    }

    /// Drain every staged reply as encoded bytes ready to send.
    pub fn take_outgoing(&mut self) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out: Vec<(SocketAddr, Vec<u8>)> =
            Vec::with_capacity(self.pending.len() + self.pending_raw.len());
        for (addr, d) in self.pending.drain(..) {
            out.push((addr, wire::encode(&d)));
        }
        out.append(&mut self.pending_raw);
        out
    }

    /// Process one inbound datagram: rate-cap, validate, dispatch. Never panics
    /// on malformed input (fuzz-safety, §7.1).
    pub fn recv(&mut self, src: SocketAddr, bytes: &[u8], now: Instant) {
        // Localhost-only STATUS probe (§9): a plain-text request, not a wire
        // datagram, answered before any decode.
        if src.ip().is_loopback() && bytes == b"STATUS" {
            let text = self.status_text();
            self.pending_raw.push((src, text.into_bytes()));
            return;
        }

        // Rate cap (§7.5): maintain the trailing-1s window for this source.
        {
            let conn = self.conns.entry(src).or_insert_with(|| Conn {
                conn_id: 0,
                session_id: None,
                window: Vec::new(),
                last_recv: now,
            });
            conn.last_recv = now;
            let cutoff = now.checked_sub(Duration::from_secs(1));
            if let Some(cutoff) = cutoff {
                conn.window.retain(|&t| t >= cutoff);
            }
            conn.window.push(now);
            let cap = self.config.max_dgrams_per_sec;
            if conn.window.len() > 2 * cap {
                // Sustained flood → kick the connection off any seat and drop it.
                self.kick(src, leave_reason::FLOOD, now);
                return;
            }
            if conn.window.len() > cap {
                // Over cap but not yet a sustained flood: drop this datagram.
                self.counters.drops += 1;
                return;
            }
        }

        let d = match wire::decode(bytes) {
            Ok(d) => d,
            Err(_) => {
                self.counters.decode_errors += 1;
                return;
            }
        };
        self.dispatch(src, d, now);
    }

    /// Verify a C→S message's echoed `conn_id` against the bound connection.
    fn conn_ok(&self, src: SocketAddr, conn_id: u32) -> bool {
        self.conns
            .get(&src)
            .map(|c| c.conn_id != 0 && c.conn_id == conn_id)
            .unwrap_or(false)
    }

    fn dispatch(&mut self, src: SocketAddr, d: Datagram, now: Instant) {
        match d {
            Datagram::SrvHello { game_version, .. } => {
                if game_version != GAME_VERSION {
                    self.pending.push((
                        src,
                        Datagram::Reject {
                            reason: RejectReason::GameVersion,
                        },
                    ));
                    return;
                }
                if self.conns.len() > self.config.max_sessions * ra_net::wire::MAX_SEATS
                    && !self.conns.contains_key(&src)
                {
                    self.pending.push((
                        src,
                        Datagram::Reject {
                            reason: RejectReason::ServerFull,
                        },
                    ));
                    return;
                }
                let server_nonce = self.rng.u32_nonzero();
                let conn = self.conns.get_mut(&src).expect("conn created in recv");
                if conn.conn_id == 0 {
                    conn.conn_id = self.rng.u32_nonzero();
                }
                let conn_id = conn.conn_id;
                self.pending.push((
                    src,
                    Datagram::SrvWelcome {
                        server_nonce,
                        conn_id,
                    },
                ));
            }

            Datagram::SessListReq { conn_id } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                let entries = self
                    .sessions
                    .values()
                    .filter(|s| !s.is_closed())
                    .take(ra_net::wire::MAX_SESSIONS_IN_LIST)
                    .map(|s| s.list_entry())
                    .collect();
                self.pending.push((src, Datagram::SessList { entries }));
            }

            Datagram::SessCreate {
                conn_id,
                name,
                map,
                seats,
                credits,
                seed,
                catalog_hash,
            } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                // One conn = one session: an already-seated conn cannot create.
                if self.conns.get(&src).and_then(|c| c.session_id).is_some() {
                    return;
                }
                if self.sessions.len() >= self.config.max_sessions {
                    self.pending.push((
                        src,
                        Datagram::Reject {
                            reason: RejectReason::ServerFull,
                        },
                    ));
                    return;
                }
                let id = self.next_session_id;
                self.next_session_id += 1;
                let creator_name = if name.is_empty() {
                    "host".to_string()
                } else {
                    name.clone()
                };
                let session = Session::create_lobby(
                    id,
                    src,
                    conn_id,
                    name,
                    map,
                    seats,
                    credits,
                    seed,
                    catalog_hash,
                    self.config.input_delay,
                    creator_name,
                    now,
                );
                self.counters.sessions_created += 1;
                if let Some(c) = self.conns.get_mut(&src) {
                    c.session_id = Some(id);
                }
                // Broadcast initial state (creator is the only seat so far).
                session.broadcast_initial(&mut self.pending);
                self.sessions.insert(id, session);
            }

            Datagram::SessJoin {
                conn_id,
                session_id,
                name,
            } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                // A conn already in another session cannot join a second.
                if let Some(existing) = self.conns.get(&src).and_then(|c| c.session_id) {
                    if existing != session_id {
                        return;
                    }
                }
                let mut out = std::mem::take(&mut self.pending);
                let mut counters = std::mem::take(&mut self.counters);
                let joined = if let Some(s) = self.sessions.get_mut(&session_id) {
                    s.on_join(src, conn_id, name, &mut out, &mut counters, now);
                    s.seat_addrs().contains(&src)
                } else {
                    false
                };
                self.pending = out;
                self.counters = counters;
                if joined {
                    if let Some(c) = self.conns.get_mut(&src) {
                        c.session_id = Some(session_id);
                    }
                }
            }

            Datagram::SessReady { conn_id, ready } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                let Some(sid) = self.conns.get(&src).and_then(|c| c.session_id) else {
                    return;
                };
                let mut out = std::mem::take(&mut self.pending);
                let mut counters = std::mem::take(&mut self.counters);
                let replay_dir = self.config.replay_dir.clone();
                let start_millis = self.config.start_millis;
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.on_ready(
                        src,
                        ready,
                        &mut out,
                        &mut counters,
                        now,
                        replay_dir.as_ref(),
                        start_millis,
                    );
                }
                self.pending = out;
                self.counters = counters;
            }

            Datagram::SessLeave { conn_id, .. } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                self.kick(src, leave_reason::PLAYER_LEFT, now);
            }

            Datagram::TickCmds { conn_id, entries } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                let Some(sid) = self.conns.get(&src).and_then(|c| c.session_id) else {
                    return;
                };
                let mut out = std::mem::take(&mut self.pending);
                let mut counters = std::mem::take(&mut self.counters);
                let kick = self
                    .sessions
                    .get_mut(&sid)
                    .and_then(|s| s.on_tick_cmds(src, entries, &mut out, &mut counters, now));
                self.pending = out;
                self.counters = counters;
                if let Some(addr) = kick {
                    self.kick(addr, leave_reason::WRONG_HOUSE, now);
                }
            }

            Datagram::TickHash { conn_id, entries } => {
                if !self.conn_ok(src, conn_id) {
                    return;
                }
                let Some(sid) = self.conns.get(&src).and_then(|c| c.session_id) else {
                    return;
                };
                let mut out = std::mem::take(&mut self.pending);
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.on_tick_hash(src, entries, &mut out, now);
                }
                self.pending = out;
            }

            Datagram::Nack { from } => {
                let Some(sid) = self.conns.get(&src).and_then(|c| c.session_id) else {
                    return;
                };
                let mut out = std::mem::take(&mut self.pending);
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.on_nack(src, from, &mut out);
                }
                self.pending = out;
            }

            Datagram::KeepAlive { .. } => {
                // Liveness only: `last_recv` already refreshed in `recv`.
            }

            // Server-origin / LAN-only messages have no meaning inbound here.
            _ => {}
        }
    }

    /// Remove a connection from its seat with `reason`, GCing a dissolved
    /// session, and drop the connection.
    fn kick(&mut self, src: SocketAddr, reason: u8, now: Instant) {
        if reason == leave_reason::FLOOD || reason == leave_reason::WRONG_HOUSE {
            self.counters.kicks += 1;
        }
        let sid = self.conns.get(&src).and_then(|c| c.session_id);
        if let Some(sid) = sid {
            let mut out = std::mem::take(&mut self.pending);
            let dissolved = self
                .sessions
                .get_mut(&sid)
                .map(|s| s.remove_seat(src, reason, &mut out, now))
                .unwrap_or(false);
            self.pending = out;
            if dissolved {
                self.gc_session(sid);
            }
        } else {
            // Not seated: still answer the kick to the offender.
            self.pending
                .push((src, Datagram::SessLeave { conn_id: 0, reason }));
        }
        self.conns.remove(&src);
    }

    /// Remove a Closed/dissolved session and unbind its conns.
    fn gc_session(&mut self, sid: u32) {
        if let Some(s) = self.sessions.remove(&sid) {
            for addr in s.seat_addrs() {
                if let Some(c) = self.conns.get_mut(&addr) {
                    c.session_id = None;
                }
            }
        }
    }

    /// Advance every timer: bundle deadlines, idle timeouts, keepalives, and
    /// GC (§9 run-loop `advance_time`). Injected `now` — no wall-clock read.
    pub fn advance_time(&mut self, now: Instant) {
        let ids: Vec<u32> = self.sessions.keys().copied().collect();
        let mut out = std::mem::take(&mut self.pending);
        let mut counters = std::mem::take(&mut self.counters);
        let mut to_gc: Vec<u32> = Vec::new();
        for id in ids {
            if let Some(s) = self.sessions.get_mut(&id) {
                if s.advance(&mut out, &mut counters, now) {
                    to_gc.push(id);
                }
            }
        }
        self.pending = out;
        self.counters = counters;
        for id in to_gc {
            self.gc_session(id);
        }

        // Drop idle, seatless connections (bounded memory).
        let stale: Vec<SocketAddr> = self
            .conns
            .iter()
            .filter(|(_, c)| {
                c.session_id.is_none() && now.duration_since(c.last_recv) >= CONN_IDLE_TIMEOUT
            })
            .map(|(a, _)| *a)
            .collect();
        for a in stale {
            self.conns.remove(&a);
        }
    }

    /// Broadcast shutdown to every session and finalize replays (§9 SIGTERM).
    pub fn shutdown(&mut self) {
        let mut out = std::mem::take(&mut self.pending);
        for s in self.sessions.values_mut() {
            s.shutdown(&mut out);
        }
        self.pending = out;
    }
}
