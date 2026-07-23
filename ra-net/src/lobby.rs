//! M8-B P2: LAN discovery + lobby, host-authoritative and deliberately dumb.
//!
//! Flow (all datagrams from [`crate::wire`], all sockets non-blocking, all
//! objects poll-driven from the menu loop — no threads):
//!
//! ```text
//!  Host                                    Joiner
//!  ────                                    ──────
//!  HostLobby::create ── ANNOUNCE (1/s) ──▶ SessionBrowser (fixed port)
//!                    ◀───── JOIN ───────── JoinLobby::join
//!  validate versions ── WELCOME/REJECT ──▶ show lobby / show error
//!                    ◀───── READY ──────── player clicks READY (re-sent)
//!  player clicks START ──── START ───────▶ both build the same world
//!  HostLobby::start → LanTransport         JoinLobby::into_transport
//! ```
//!
//! The host is the authority on every setting (map, seed, credits, seats,
//! input delay); the joiner only echoes READY. Both must confirm before
//! START (the host's START button stays disabled until the joiner's READY
//! arrives). Loss tolerance: JOIN and READY re-send on a timer until
//! answered; a START lost on the wire is re-answered by the host's
//! *transport* when the joiner's re-sent READY arrives in-game
//! (see [`LanTransport`]); every wait has a timeout so a vanished peer
//! surfaces as a state, never a hang.
//!
//! This replaces the original's IPX session enumeration + dialog loop
//! (IPXCONN.CPP / the netdlg session lists) with the minimal modern
//! equivalent: fixed-port UDP broadcast discovery, then a three-way
//! JOIN/WELCOME/READY handshake on the host's game socket.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::lan::LanTransport;
use crate::platform;
use crate::transport::SeatId;
use crate::wire::{self, Datagram, RejectReason, GAME_VERSION};

/// How often the host re-broadcasts its session announcement.
const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(500);

/// Lobby-side keepalive cadence (host → joiner and joiner → host while
/// waiting; also the READY re-send cadence).
const LOBBY_KEEPALIVE: Duration = Duration::from_millis(400);

/// Drop a lobby peer that has been silent this long.
const LOBBY_TIMEOUT: Duration = Duration::from_secs(5);

/// Default JOIN answer timeout (dead host / wrong address).
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A browsed session ages out of the list after this long without a fresh
/// announcement.
const SESSION_TTL: Duration = Duration::from_secs(3);

/// Discovery wiring — the two knobs tests must be able to redirect so that
/// **no test ever binds a fixed port** (CI collision safety): where
/// announcements are sent, and which port the browser listens on.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    /// Where the host sends ANNOUNCE datagrams. Default: limited broadcast +
    /// loopback on [`platform::DISCOVERY_PORT`].
    pub announce_targets: Vec<SocketAddr>,
    /// The browser's listen port. Default [`platform::DISCOVERY_PORT`];
    /// tests pass 0 (OS-assigned) and point `announce_targets` at the real
    /// bound port.
    pub listen_port: u16,
}

impl Default for DiscoveryConfig {
    fn default() -> DiscoveryConfig {
        DiscoveryConfig {
            announce_targets: platform::default_announce_targets(platform::DISCOVERY_PORT),
            listen_port: platform::DISCOVERY_PORT,
        }
    }
}

/// The host-chosen session parameters — everything both sides need to build
/// the identical world (M8-B P2: "host is authority on settings").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSettings {
    /// Scenario filename both sides load (e.g. `"scm01ea.ini"`).
    pub map: String,
    /// World RNG seed.
    pub seed: u32,
    /// Starting credits for both houses.
    pub credits: i32,
    /// The host's seat (house id).
    pub host_seat: SeatId,
    /// The joiner's seat (house id).
    pub join_seat: SeatId,
    /// Lockstep input delay in ticks.
    pub delay: u32,
}

/// One discovered session in the joiner's browser list.
#[derive(Clone, Debug)]
pub struct DiscoveredSession {
    /// The host's game-socket address (send JOIN here).
    pub addr: SocketAddr,
    /// Host/session display name.
    pub name: String,
    /// Scenario filename the host selected.
    pub map: String,
    /// Whether our build can play this session (game version match; a
    /// *protocol* mismatch can't even be decoded and is listed as
    /// incompatible with empty name/map).
    pub compatible: bool,
    last_seen: Instant,
}

// ---------------------------------------------------------------------------
// Host side
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Joiner {
    addr: SocketAddr,
    name: String,
    ready: bool,
    last_seen: Instant,
}

/// The host's lobby: announces the session, admits (at most) one joiner,
/// tracks its READY, and converts into a [`LanTransport`] on START.
#[derive(Debug)]
pub struct HostLobby {
    sock: UdpSocket,
    name: String,
    settings: SessionSettings,
    targets: Vec<SocketAddr>,
    joiner: Option<Joiner>,
    last_announce: Option<Instant>,
    last_lobby_send: Instant,
    /// Set when the joiner vanished (silent > [`LOBBY_TIMEOUT`]) — the UI
    /// shows "player left" once, then clears.
    joiner_lost: bool,
}

impl HostLobby {
    /// Bind the game socket and start announcing `name`'s session.
    pub fn create(
        name: &str,
        settings: SessionSettings,
        cfg: &DiscoveryConfig,
    ) -> io::Result<HostLobby> {
        let sock = platform::bind_host_socket()?;
        Ok(HostLobby {
            sock,
            name: name.to_string(),
            settings,
            targets: cfg.announce_targets.clone(),
            joiner: None,
            last_announce: None,
            last_lobby_send: Instant::now(),
            joiner_lost: false,
        })
    }

    /// The game socket's bound port (carried in announcements).
    pub fn port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// The session settings (authority copy).
    pub fn settings(&self) -> &SessionSettings {
        &self.settings
    }

    /// The joined player's display name, if one is present.
    pub fn joiner_name(&self) -> Option<&str> {
        self.joiner.as_ref().map(|j| j.name.as_str())
    }

    /// Whether the joined player has confirmed READY.
    pub fn joiner_ready(&self) -> bool {
        self.joiner.as_ref().map(|j| j.ready).unwrap_or(false)
    }

    /// Whether START may be pressed (both-confirm rule: joiner present AND
    /// ready; the host's own confirmation is the button press itself).
    pub fn can_start(&self) -> bool {
        self.joiner_ready()
    }

    /// True once, if the joiner vanished mid-lobby (then clears).
    pub fn take_joiner_lost(&mut self) -> bool {
        std::mem::take(&mut self.joiner_lost)
    }

    /// Drive the lobby: announce, admit, track liveness. Call every frame.
    pub fn poll(&mut self) {
        // Periodic announcement (only while the seat is open — a full lobby
        // stops advertising, exactly like pulling the session from the list).
        let announce_due = self
            .last_announce
            .map(|t| t.elapsed() >= ANNOUNCE_INTERVAL)
            .unwrap_or(true);
        if self.joiner.is_none() && announce_due {
            self.last_announce = Some(Instant::now());
            let d = Datagram::Announce {
                game_version: GAME_VERSION,
                game_port: self.port(),
                name: self.name.clone(),
                map: self.settings.map.clone(),
            };
            let bytes = wire::encode(&d);
            for t in &self.targets {
                let _ = self.sock.send_to(&bytes, t);
            }
        }

        // Pump the socket.
        let mut buf = [0u8; 2048];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, src)) => match wire::decode(&buf[..n]) {
                    Ok(d) => self.receive(d, src),
                    Err(wire::WireError::ProtocolMismatch { .. }) => {
                        // A different-protocol joiner knocked: refuse
                        // explicitly (they interpret ANY undecodable answer
                        // from us as a version mismatch, but sending the
                        // reject keeps the intent on the wire).
                        let bytes = wire::encode(&Datagram::Reject {
                            reason: RejectReason::ProtocolVersion,
                        });
                        let _ = self.sock.send_to(&bytes, src);
                    }
                    Err(_) => {}
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        // Joiner liveness + lobby keepalive.
        if let Some(j) = &self.joiner {
            if j.last_seen.elapsed() >= LOBBY_TIMEOUT {
                self.joiner = None;
                self.joiner_lost = true;
            } else if self.last_lobby_send.elapsed() >= LOBBY_KEEPALIVE {
                self.last_lobby_send = Instant::now();
                let bytes = wire::encode(&Datagram::KeepAlive { tick: 0 });
                let addr = j.addr;
                let _ = self.sock.send_to(&bytes, addr);
            }
        }
    }

    fn receive(&mut self, d: Datagram, src: SocketAddr) {
        match d {
            Datagram::Join { game_version, name } => {
                if game_version != GAME_VERSION {
                    let bytes = wire::encode(&Datagram::Reject {
                        reason: RejectReason::GameVersion,
                    });
                    let _ = self.sock.send_to(&bytes, src);
                    return;
                }
                match &mut self.joiner {
                    // Same joiner re-sending (lost WELCOME): refresh + re-welcome.
                    Some(j) if j.addr == src => j.last_seen = Instant::now(),
                    Some(_) => {
                        let bytes = wire::encode(&Datagram::Reject {
                            reason: RejectReason::SessionFull,
                        });
                        let _ = self.sock.send_to(&bytes, src);
                        return;
                    }
                    None => {
                        self.joiner = Some(Joiner {
                            addr: src,
                            name,
                            ready: false,
                            last_seen: Instant::now(),
                        });
                        self.joiner_lost = false;
                    }
                }
                let welcome = Datagram::Welcome {
                    game_version: GAME_VERSION,
                    seat: self.settings.join_seat,
                    host_seat: self.settings.host_seat,
                    delay: self.settings.delay.min(255) as u8,
                    seed: self.settings.seed,
                    credits: self.settings.credits,
                    map: self.settings.map.clone(),
                    host_name: self.name.clone(),
                };
                let bytes = wire::encode(&welcome);
                let _ = self.sock.send_to(&bytes, src);
            }
            Datagram::Ready => {
                if let Some(j) = &mut self.joiner {
                    if j.addr == src {
                        j.ready = true;
                        j.last_seen = Instant::now();
                    }
                }
            }
            Datagram::KeepAlive { .. } => {
                if let Some(j) = &mut self.joiner {
                    if j.addr == src {
                        j.last_seen = Instant::now();
                    }
                }
            }
            Datagram::Leave => {
                if self.joiner.as_ref().map(|j| j.addr) == Some(src) {
                    self.joiner = None;
                    self.joiner_lost = true;
                }
            }
            _ => {}
        }
    }

    /// Fire START and become the host-side transport. Only valid when
    /// [`HostLobby::can_start`]; the transport keeps re-answering the
    /// joiner's READY with START, so a lost START self-heals.
    pub fn start(self) -> io::Result<LanTransport> {
        let joiner = self
            .joiner
            .as_ref()
            .expect("start() requires a ready joiner (check can_start)");
        let bytes = wire::encode(&Datagram::Start);
        let _ = self.sock.send_to(&bytes, joiner.addr);
        LanTransport::new(
            self.sock,
            joiner.addr,
            self.settings.host_seat,
            self.settings.join_seat,
            self.settings.delay,
            true,
        )
    }

    /// Cancel the session (host backs out): tell the joiner, drop the socket.
    pub fn cancel(self) {
        if let Some(j) = &self.joiner {
            let bytes = wire::encode(&Datagram::Leave);
            let _ = self.sock.send_to(&bytes, j.addr);
        }
    }
}

// ---------------------------------------------------------------------------
// Joiner side: discovery
// ---------------------------------------------------------------------------

/// The joiner's session browser: listens for announcements and keeps a
/// TTL-pruned list.
#[derive(Debug)]
pub struct SessionBrowser {
    sock: UdpSocket,
    sessions: Vec<DiscoveredSession>,
}

impl SessionBrowser {
    /// Bind the discovery listener per `cfg` (the fixed port by default; 0 in
    /// tests). Fails if another process already owns the port — the UI
    /// surfaces that as "another joiner is already browsing on this machine".
    pub fn bind(cfg: &DiscoveryConfig) -> io::Result<SessionBrowser> {
        let sock = platform::bind_discovery_listener(cfg.listen_port)?;
        Ok(SessionBrowser {
            sock,
            sessions: Vec::new(),
        })
    }

    /// The actually-bound listen port (tests read this to aim the host).
    pub fn port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// Pump announcements and prune stale sessions. Call every frame.
    pub fn poll(&mut self) {
        let mut buf = [0u8; 2048];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    let (addr, name, map, compatible) = match wire::decode(&buf[..n]) {
                        Ok(Datagram::Announce {
                            game_version,
                            game_port,
                            name,
                            map,
                        }) => {
                            let addr = SocketAddr::new(src.ip(), game_port);
                            (addr, name, map, game_version == GAME_VERSION)
                        }
                        // A host speaking a different protocol still shows
                        // up — as an explicitly incompatible entry (better
                        // UX than silently invisible sessions).
                        Err(wire::WireError::ProtocolMismatch { .. }) => (
                            src,
                            "?".to_string(),
                            "INCOMPATIBLE VERSION".to_string(),
                            false,
                        ),
                        _ => continue,
                    };
                    match self.sessions.iter_mut().find(|s| s.addr == addr) {
                        Some(s) => {
                            s.name = name;
                            s.map = map;
                            s.compatible = compatible;
                            s.last_seen = Instant::now();
                        }
                        None => self.sessions.push(DiscoveredSession {
                            addr,
                            name,
                            map,
                            compatible,
                            last_seen: Instant::now(),
                        }),
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        self.sessions
            .retain(|s| s.last_seen.elapsed() < SESSION_TTL);
    }

    /// The live session list, freshest announcements first retained in
    /// arrival order.
    pub fn sessions(&self) -> &[DiscoveredSession] {
        &self.sessions
    }
}

// ---------------------------------------------------------------------------
// Joiner side: lobby
// ---------------------------------------------------------------------------

/// What both-sides-agree parameters the joiner received in WELCOME.
#[derive(Clone, Debug)]
pub struct WelcomeInfo {
    /// The joiner's assigned seat (house id).
    pub seat: SeatId,
    /// The host's seat.
    pub host_seat: SeatId,
    /// Session input delay in ticks.
    pub delay: u32,
    /// World seed.
    pub seed: u32,
    /// Starting credits.
    pub credits: i32,
    /// Scenario filename to load.
    pub map: String,
    /// Host display name.
    pub host_name: String,
}

#[derive(Debug, PartialEq, Eq)]
enum JoinPhase {
    /// JOIN sent, awaiting WELCOME.
    Joining,
    /// WELCOME received; waiting for the local player's READY and/or START.
    InLobby,
    /// START received — [`JoinLobby::into_transport`] may be called.
    Started,
    /// Terminal failure; see [`JoinLobby::error`].
    Failed,
}

/// The joiner's lobby: JOIN → WELCOME → READY → START, every wait bounded.
#[derive(Debug)]
pub struct JoinLobby {
    sock: UdpSocket,
    host: SocketAddr,
    name: String,
    phase: JoinPhase,
    welcome: Option<WelcomeInfo>,
    ready: bool,
    started_at: Instant,
    host_last_seen: Instant,
    last_send: Instant,
    timeout: Duration,
    error: Option<String>,
}

impl JoinLobby {
    /// Bind a fresh game socket and send the first JOIN to `host`.
    pub fn join(host: SocketAddr, name: &str) -> io::Result<JoinLobby> {
        let sock = platform::bind_join_socket()?;
        let now = Instant::now();
        let mut j = JoinLobby {
            sock,
            host,
            name: name.to_string(),
            phase: JoinPhase::Joining,
            welcome: None,
            ready: false,
            started_at: now,
            host_last_seen: now,
            last_send: now,
            timeout: JOIN_TIMEOUT,
            error: None,
        };
        j.send_join();
        Ok(j)
    }

    /// Shrink the JOIN answer timeout (tests). Production uses
    /// [`JOIN_TIMEOUT`].
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The WELCOME parameters, once received.
    pub fn welcome(&self) -> Option<&WelcomeInfo> {
        self.welcome.as_ref()
    }

    /// Terminal failure message, if the join failed (timeout, reject,
    /// version mismatch, host cancelled/vanished).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the local player has confirmed READY.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether START has arrived (call [`JoinLobby::into_transport`]).
    pub fn started(&self) -> bool {
        self.phase == JoinPhase::Started
    }

    /// Confirm READY (re-sent on a timer until START arrives).
    pub fn set_ready(&mut self) {
        if self.phase == JoinPhase::InLobby {
            self.ready = true;
            self.send_to_host(&Datagram::Ready);
        }
    }

    fn send_join(&mut self) {
        let name = self.name.clone();
        self.send_to_host(&Datagram::Join {
            game_version: GAME_VERSION,
            name,
        });
    }

    fn send_to_host(&mut self, d: &Datagram) {
        let bytes = wire::encode(d);
        let _ = self.sock.send_to(&bytes, self.host);
        self.last_send = Instant::now();
    }

    fn fail(&mut self, msg: &str) {
        if self.phase != JoinPhase::Failed {
            self.phase = JoinPhase::Failed;
            self.error = Some(msg.to_string());
        }
    }

    /// Drive the handshake. Call every frame.
    pub fn poll(&mut self) {
        if matches!(self.phase, JoinPhase::Failed | JoinPhase::Started) {
            return;
        }

        // Re-send / keepalive pacing.
        if self.last_send.elapsed() >= LOBBY_KEEPALIVE {
            match self.phase {
                JoinPhase::Joining => self.send_join(),
                JoinPhase::InLobby => {
                    if self.ready {
                        self.send_to_host(&Datagram::Ready);
                    } else {
                        self.send_to_host(&Datagram::KeepAlive { tick: 0 });
                    }
                }
                _ => {}
            }
        }

        // Timeouts.
        match self.phase {
            JoinPhase::Joining => {
                if self.started_at.elapsed() >= self.timeout {
                    self.fail("no response from host (timed out)");
                    return;
                }
            }
            JoinPhase::InLobby => {
                if self.host_last_seen.elapsed() >= LOBBY_TIMEOUT {
                    self.fail("lost contact with host");
                    return;
                }
            }
            _ => {}
        }

        // Pump.
        let mut buf = [0u8; 2048];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if src != self.host {
                        continue;
                    }
                    self.host_last_seen = Instant::now();
                    match wire::decode(&buf[..n]) {
                        Ok(d) => {
                            self.receive(d);
                            if matches!(self.phase, JoinPhase::Failed | JoinPhase::Started) {
                                return;
                            }
                        }
                        Err(wire::WireError::ProtocolMismatch { theirs }) => {
                            self.fail(&format!(
                                "protocol version mismatch (host {theirs}, ours {})",
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
            Datagram::Welcome {
                game_version,
                seat,
                host_seat,
                delay,
                seed,
                credits,
                map,
                host_name,
            } => {
                if game_version != GAME_VERSION {
                    self.fail("game version mismatch");
                    return;
                }
                if self.phase == JoinPhase::Joining {
                    self.phase = JoinPhase::InLobby;
                    self.welcome = Some(WelcomeInfo {
                        seat,
                        host_seat,
                        delay: delay as u32,
                        seed,
                        credits,
                        map,
                        host_name,
                    });
                }
            }
            Datagram::Reject { reason } => {
                let msg = match reason {
                    RejectReason::ProtocolVersion => "rejected: protocol version mismatch",
                    RejectReason::GameVersion => "rejected: game version mismatch",
                    RejectReason::SessionFull => "rejected: session is full",
                    RejectReason::AlreadyStarted => "rejected: game already started",
                    RejectReason::ServerFull => "rejected: server is full",
                };
                self.fail(msg);
            }
            Datagram::Start => {
                if self.welcome.is_some() {
                    self.phase = JoinPhase::Started;
                }
            }
            // The host is already in-game (our START was lost but its
            // first bundles/keepalives beat the re-answer): treat any
            // in-game traffic as the START.
            Datagram::Bundles { .. } | Datagram::Hashes { .. } => {
                if self.welcome.is_some() && self.ready {
                    self.phase = JoinPhase::Started;
                }
            }
            Datagram::Leave => self.fail("host cancelled the session"),
            Datagram::KeepAlive { .. } => {}
            _ => {}
        }
    }

    /// Become the joiner-side transport (call once [`JoinLobby::started`]).
    pub fn into_transport(self) -> io::Result<LanTransport> {
        let w = self
            .welcome
            .as_ref()
            .expect("into_transport requires a completed handshake");
        LanTransport::new(self.sock, self.host, w.seat, w.host_seat, w.delay, false)
    }

    /// Back out of the lobby (tell the host, drop the socket).
    pub fn leave(mut self) {
        self.send_to_host(&Datagram::Leave);
    }
}
