//! M8-B P0: the versioned LAN datagram format — hand-rolled little-endian
//! byte layout with explicit per-field encode/decode (no serde, per the §4.6
//! "prefer plain code" rule and the M8-B brief).
//!
//! # Layout
//!
//! Every datagram starts with a fixed 5-byte header:
//!
//! | offset | size | field                                  |
//! |--------|------|----------------------------------------|
//! | 0      | 2    | magic `0x4152` (LE bytes `"RA"`)       |
//! | 2      | 2    | [`PROTOCOL_VERSION`]                   |
//! | 4      | 1    | message type                           |
//!
//! followed by the type-specific payload. All integers are little-endian.
//! Strings are length-prefixed (`u8` length, then that many bytes) and
//! length-capped. A decoder **never panics on malformed input**: every read is
//! length-checked, every enum tag validated, every count capped, and the
//! whole buffer must be consumed exactly — anything else is a [`WireError`].
//!
//! Handshake datagrams (ANNOUNCE / JOIN / WELCOME) additionally carry
//! [`GAME_VERSION`] so a same-protocol but different-build peer is rejected at
//! the handshake, mirroring the original's startup scenario-CRC compare in
//! FRAMESYNC packets (`Send_FrameSync`, QUEUE.CPP:1748-1758: "FRAMESYNC
//! packets contain a scenario-based CRC ... to let the games compare
//! scenario CRC's on startup").
//!
//! Peers whose [`PROTOCOL_VERSION`] differs cannot decode each other's
//! datagrams **at all** (the header check fails first) — which is the point:
//! version mismatch is detected before any payload is trusted.

use ra_sim::{coords::CellCoord, BuildItem, Command, Handle, ProdKind, SuperKind, Target};

use crate::transport::{SeatId, Tick};

/// Wire magic: little-endian `0x4152` = the bytes `"RA"`.
pub const WIRE_MAGIC: u16 = 0x4152;

/// The wire protocol version. Bump on ANY change to the datagram layout or to
/// the meaning of a field; peers with different values reject each other at
/// the handshake (and cannot decode each other's datagrams at all).
pub const PROTOCOL_VERSION: u16 = 2;

/// The game build version carried in handshake datagrams (major.minor.patch
/// packed as `major << 16 | minor << 8 | patch`). Two builds with the same
/// protocol but different game versions could still diverge in sim behavior,
/// so they refuse to play each other — the M8-B analogue of the original's
/// startup CRC compare (QUEUE.CPP:1748-1758).
pub const GAME_VERSION: u32 = 0x0000_0100; // 0.1.0, matching the workspace version

/// Hard cap on an encoded datagram (stays under the 65,507-byte UDP payload
/// limit with margin; on a LAN, IP fragmentation of large bundles is fine —
/// a lost fragment loses one datagram, which the redundancy/NACK machinery
/// already covers).
pub const MAX_DATAGRAM: usize = 60_000;

/// Caps on decoded variable-length fields — a malformed length byte must
/// never cause a huge allocation.
pub const MAX_NAME: usize = 24;
/// Cap on the scenario/map filename.
pub const MAX_MAP_NAME: usize = 64;
/// Cap on ticks carried per BUNDLES/HASHES datagram (the redundancy window
/// is far smaller; NACK re-sends chunk to this).
pub const MAX_TICK_ENTRIES: usize = 64;
/// Cap on commands in one tick's bundle (the original's DoList holds
/// `MAX_EVENTS * 64` total; one tick from one seat never legitimately nears
/// this).
pub const MAX_CMDS_PER_TICK: usize = 4096;

// Message type bytes.
const T_ANNOUNCE: u8 = 0x01;
const T_JOIN: u8 = 0x02;
const T_WELCOME: u8 = 0x03;
const T_REJECT: u8 = 0x04;
const T_READY: u8 = 0x05;
const T_START: u8 = 0x06;
const T_LEAVE: u8 = 0x07;
const T_BUNDLES: u8 = 0x10;
const T_HASHES: u8 = 0x11;
const T_NACK: u8 = 0x12;
const T_KEEPALIVE: u8 = 0x13;
const T_QUIT: u8 = 0x14;
// M8-C resync (snapshot transfer). Opaque bytes: the transport never interprets
// the snapshot payload or `declared_hash` — it only moves them (DESIGN.md §4.6).
const T_SNAP_OFFER: u8 = 0x20;
const T_SNAP_CHUNK: u8 = 0x21;
const T_SNAP_ACK: u8 = 0x22;
const T_SNAP_DONE: u8 = 0x23;
// Wire v2 (M9-A relay, SERVER-DESIGN.md §4). C→S messages after SRV_WELCOME echo
// `conn_id` as a cheap off-path spoof guard (§7.2). `Reject` (0x04) is reused for
// the SRV_HELLO refusal path.
const T_SRV_HELLO: u8 = 0x30;
const T_SRV_WELCOME: u8 = 0x31;
const T_SESS_CREATE: u8 = 0x33;
const T_SESS_LIST_REQ: u8 = 0x34;
const T_SESS_LIST: u8 = 0x35;
const T_SESS_JOIN: u8 = 0x36;
const T_SESS_STATE: u8 = 0x37;
const T_SESS_READY: u8 = 0x38;
const T_SESS_LEAVE: u8 = 0x39;
const T_SESS_START: u8 = 0x3A;
const T_TICK_CMDS: u8 = 0x3B;
const T_TICK_BUNDLE: u8 = 0x3C;
const T_TICK_HASH: u8 = 0x3D;
const T_HASH_VERDICT: u8 = 0x3E;

/// Embedded control-record tag: `SESS_TIMING` (§6.2 adaptive delay). Reserved in
/// M9-A (never emitted; fixed delay); decoded generically so M9-B can populate
/// it without a wire bump.
const CTRL_TIMING: u8 = 1;

/// Max snapshot payload a chunk carries — kept comfortably under a 1500-byte
/// Ethernet MTU (minus IP/UDP + our header) so a CHUNK datagram is not IP
/// fragmented, giving clean per-chunk loss behaviour.
pub const MAX_SNAP_CHUNK_DATA: usize = 1200;
/// Hard cap on a reassembled snapshot (matches `ra_sim::snapshot::MAX_SNAPSHOT`):
/// a malformed `total_len` can never trigger an unbounded allocation.
pub const MAX_SNAPSHOT_LEN: usize = 16 * 1024 * 1024;
/// Cap on missing-chunk seqs reported in one ACK (a corrupt count must not
/// over-allocate; the receiver re-ACKs across several datagrams if it is missing
/// more than this, which never happens at realistic loss rates).
pub const MAX_SNAP_MISSING: usize = 2048;

// --- Wire v2 (M9-A relay) caps (SERVER-DESIGN.md §4) --------------------------

/// Cap on sessions listed in one `SESS_LIST` page (§4: "capped page (≤ 32)").
pub const MAX_SESSIONS_IN_LIST: usize = 32;
/// Cap on seats named in a session (`SESS_STATE`/`SESS_START`; the original tops
/// out at 8 MP houses — SERVER-DESIGN.md §9 "seats ≤ 8/game").
pub const MAX_SEATS: usize = 8;
/// Hard decode cap on one encoded command blob carried opaquely in a
/// `TICK_CMDS`/`TICK_BUNDLE` (the largest `Command` — `FireSuperWeapon` with a
/// cell dest — encodes well under this; a malformed length must never
/// over-read). Command *semantic* validity is the sim's job; the relay only
/// moves these bytes and reads the house field (§7).
pub const MAX_CMD_BLOB: usize = 64;
/// Cap on control records embedded in one tick's bundle (the `SESS_TIMING` hook
/// slot, §6.2). M9-A never emits any; the slot exists so M9-B's adaptive-delay
/// control record needs no wire bump.
pub const MAX_CTRL_PER_TICK: usize = 8;
/// Cap on one control record's opaque payload.
pub const MAX_CTRL_PAYLOAD: usize = 64;

/// Why a JOIN was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The joiner's wire protocol version differs (its JOIN did not decode).
    ProtocolVersion,
    /// Same protocol, different game build ([`GAME_VERSION`] mismatch).
    GameVersion,
    /// The session already has its second player.
    SessionFull,
    /// The session already started.
    AlreadyStarted,
    /// Relay server is at capacity (max sessions/connections, §9). Wire v2.
    ServerFull,
}

impl RejectReason {
    fn to_byte(self) -> u8 {
        match self {
            RejectReason::ProtocolVersion => 1,
            RejectReason::GameVersion => 2,
            RejectReason::SessionFull => 3,
            RejectReason::AlreadyStarted => 4,
            RejectReason::ServerFull => 5,
        }
    }

    fn from_byte(b: u8) -> Result<RejectReason, WireError> {
        Ok(match b {
            1 => RejectReason::ProtocolVersion,
            2 => RejectReason::GameVersion,
            3 => RejectReason::SessionFull,
            4 => RejectReason::AlreadyStarted,
            5 => RejectReason::ServerFull,
            _ => return Err(WireError::BadValue("reject reason")),
        })
    }
}

/// A relay session's lifecycle phase (SERVER-DESIGN.md §5), carried in
/// `SESS_STATE`. `u8` on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    /// Accepting joins/ready toggles; seat-0 owns settings.
    Lobby,
    /// The game is running (sequencing tick bundles).
    Running,
    /// Finished / dissolved (replay finalized).
    Closed,
}

impl SessionPhase {
    fn to_byte(self) -> u8 {
        match self {
            SessionPhase::Lobby => 0,
            SessionPhase::Running => 1,
            SessionPhase::Closed => 2,
        }
    }
    fn from_byte(b: u8) -> Result<SessionPhase, WireError> {
        Ok(match b {
            0 => SessionPhase::Lobby,
            1 => SessionPhase::Running,
            2 => SessionPhase::Closed,
            _ => return Err(WireError::BadValue("session phase")),
        })
    }
}

/// Hash-arbitration verdict (SERVER-DESIGN.md §6.3), carried in `HASH_VERDICT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashVerdict {
    /// This seat is in the majority (only sent on an explicit query).
    Ok,
    /// This seat diverged from the majority — enter resync (M9-B) / terminal
    /// desync end (M9-A).
    YouDiverged,
    /// Arbitration pending (not enough seats have reported yet).
    Wait,
}

impl HashVerdict {
    fn to_byte(self) -> u8 {
        match self {
            HashVerdict::Ok => 0,
            HashVerdict::YouDiverged => 1,
            HashVerdict::Wait => 2,
        }
    }
    fn from_byte(b: u8) -> Result<HashVerdict, WireError> {
        Ok(match b {
            0 => HashVerdict::Ok,
            1 => HashVerdict::YouDiverged,
            2 => HashVerdict::Wait,
            _ => return Err(WireError::BadValue("hash verdict")),
        })
    }
}

/// One entry in a `SESS_LIST` page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessListEntry {
    /// Server-assigned session id.
    pub session_id: u32,
    /// Session display name.
    pub name: String,
    /// Scenario filename.
    pub map: String,
    /// Seats currently occupied.
    pub seats_taken: u8,
    /// Total seats.
    pub seats: u8,
    /// Whether the session has already started (Running/Closed).
    pub in_progress: bool,
}

/// One seat's authoritative lobby state in a `SESS_STATE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessSeat {
    /// The seat id (== house id: the canonical bundle order).
    pub seat: SeatId,
    /// The player's display name.
    pub name: String,
    /// The house this seat plays.
    pub house: u8,
    /// Whether the seat has confirmed READY.
    pub ready: bool,
}

/// A control record embedded in a `TICK_BUNDLE` tick (SERVER-DESIGN.md §6.2).
/// Ordered with the command stream so every client applies it at the same tick.
/// M9-A only reserves the slot; the sole typed record is the adaptive-delay
/// `Timing` hook M9-B will emit. Unknown tags decode opaquely so a newer server
/// can add records without a wire bump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtrlRecord {
    /// Adaptive input-delay retune (§6.2, QUEUE.CPP:1440-1461): raise every
    /// client's scheduler delay to `new_delay` at `effective_tick`.
    Timing {
        /// The new input delay in ticks.
        new_delay: u8,
        /// The tick the new delay takes effect on (all clients shift together).
        effective_tick: Tick,
    },
    /// A control record whose tag this build does not know — carried opaquely
    /// (forward compatibility for later relay control records).
    Unknown {
        /// The record's tag byte.
        tag: u8,
        /// Its opaque payload.
        bytes: Vec<u8>,
    },
}

/// One tick's canonical bundle inside a `TICK_BUNDLE` datagram: the tick, any
/// embedded control records, and every seat's opaque command blobs in
/// seat-ascending order. The relay assembles this; the client decodes the blobs
/// into `Command`s to feed the sim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleEntry {
    /// The execution tick.
    pub tick: Tick,
    /// Ordered control records (empty in M9-A).
    pub ctrl: Vec<CtrlRecord>,
    /// Per-seat command blobs (`(seat, blobs)`), ascending by seat.
    pub seats: Vec<(SeatId, Vec<Vec<u8>>)>,
}

/// Everything that crosses the wire: LAN v1 (discovery/lobby/lockstep/resync)
/// and the v2 relay message set (SRV/SESS/TICK/HASH). LAN v1 layouts are
/// unchanged from M8; the whole set shares one never-panic decode path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datagram {
    /// Host → broadcast: "a session exists here". `game_port` is the UDP port
    /// the host's game/lobby socket listens on (the announce's source IP +
    /// this port is the join address).
    Announce {
        /// Host build version (joiners flag mismatches in the session list).
        game_version: u32,
        /// The host's game-socket port.
        game_port: u16,
        /// Host player / session display name.
        name: String,
        /// Scenario filename the host selected (e.g. `"scm01ea.ini"`).
        map: String,
    },
    /// Joiner → host: request the open seat.
    Join {
        /// Joiner build version (host rejects mismatches).
        game_version: u32,
        /// Joiner display name.
        name: String,
    },
    /// Host → joiner: seat granted; everything needed to build the identical
    /// world. The host is authority on all of it (M8-B P2).
    Welcome {
        /// Host build version (final cross-check on the joiner side).
        game_version: u32,
        /// The joiner's seat (house id).
        seat: u8,
        /// The host's seat (house id).
        host_seat: u8,
        /// Lockstep input delay in ticks (protocol constant for the session).
        delay: u8,
        /// World RNG seed.
        seed: u32,
        /// Starting credits for both houses.
        credits: i32,
        /// Scenario filename both sides load.
        map: String,
        /// Host display name (shown in the joiner's lobby).
        host_name: String,
    },
    /// Host → joiner: seat refused.
    Reject {
        /// Why.
        reason: RejectReason,
    },
    /// Joiner → host: settings acknowledged, ready to start. Re-sent until
    /// START arrives (loss tolerance).
    Ready,
    /// Host → joiner: the game begins now, at tick 0. Re-sent by the host's
    /// transport whenever a stray READY arrives in-game (a lost START must
    /// not strand the joiner in the lobby).
    Start,
    /// Either side, lobby only: leaving / cancelling the session.
    Leave,
    /// In-game: one seat's command bundles for a run of execution ticks.
    /// Redundantly carries the last K ticks (lockstep-classic loss tolerance:
    /// an isolated drop never stalls, because the next datagram re-carries
    /// the lost tick).
    Bundles {
        /// `(execution tick, that tick's commands)`, ascending by tick.
        entries: Vec<(Tick, Vec<Command>)>,
    },
    /// In-game: post-tick state hashes, redundantly carrying the last K
    /// reports (same loss tolerance as bundles).
    Hashes {
        /// `(tick, state hash)`, ascending by tick.
        entries: Vec<(Tick, u64)>,
    },
    /// In-game backstop for burst loss: "re-send every bundle/hash you still
    /// hold from `from` on".
    Nack {
        /// First tick the sender is missing.
        from: Tick,
    },
    /// Liveness while nothing else is flowing (lobby waits, barrier stalls).
    KeepAlive {
        /// The sender's current tick (0 in the lobby); diagnostic only.
        tick: Tick,
    },
    /// Clean in-game exit: the peer's client shows "player left" rather than
    /// waiting out the keepalive timeout.
    Quit,
    /// Resync (M8-C): the authoritative (host) peer offers a world snapshot to
    /// the desynced loser. Re-sent until the loser acknowledges completion. The
    /// snapshot bytes follow in [`Datagram::SnapshotChunk`]s.
    SnapshotOffer {
        /// Retry counter (0-based). Chunks/acks/done for a stale attempt are
        /// ignored, so a re-offer cannot be corrupted by leftovers of the last.
        attempt: u8,
        /// The tick both peers resume lockstep from (the host's authoritative
        /// tick at snapshot time).
        resume_tick: Tick,
        /// The host's declared state hash at `resume_tick` — the loser verifies
        /// its loaded world against this. **Opaque to the transport.**
        declared_hash: u64,
        /// Total reassembled snapshot length in bytes.
        total_len: u32,
        /// Bytes carried per chunk (last chunk may be shorter).
        chunk_size: u16,
    },
    /// One chunk of the offered snapshot (host → loser).
    SnapshotChunk {
        /// The attempt this chunk belongs to.
        attempt: u8,
        /// Zero-based chunk index.
        seq: u32,
        /// The chunk's snapshot bytes (`<= MAX_SNAP_CHUNK_DATA`).
        data: Vec<u8>,
    },
    /// Loser → host: which chunk seqs are still missing (empty = have them all).
    /// Drives selective re-send under loss.
    SnapshotAck {
        /// The attempt being acknowledged.
        attempt: u8,
        /// Still-missing chunk seqs (capped; empty means complete).
        missing: Vec<u32>,
    },
    /// Loser → host: the transfer resolved — `ok` = loaded and hash-verified,
    /// so both resume; `!ok` = load/verify failed, triggering a retry or, past
    /// the attempt cap, the fallback to the terminal desync end.
    SnapshotDone {
        /// The attempt being reported.
        attempt: u8,
        /// Whether the loser loaded and hash-verified the snapshot.
        ok: bool,
    },

    // --- Wire v2 (M9-A relay, SERVER-DESIGN.md §4) --------------------------
    /// C→S first contact. The server replies [`Datagram::SrvWelcome`] or
    /// [`Datagram::Reject`].
    SrvHello {
        /// Client build version (server rejects mismatches).
        game_version: u32,
        /// Client-chosen nonce (echoed context; diagnostic).
        client_nonce: u32,
    },
    /// S→C: connection accepted. `conn_id` is echoed in every later C→S message
    /// (off-path spoof guard, §7.2).
    SrvWelcome {
        /// Server-chosen nonce.
        server_nonce: u32,
        /// The connection id the client must echo.
        conn_id: u32,
    },
    /// C→S: create a lobby session; the creator gets seat 0 (settings authority).
    SessCreate {
        /// The client's `conn_id` (spoof guard).
        conn_id: u32,
        /// Session display name.
        name: String,
        /// Scenario filename.
        map: String,
        /// Seat count.
        seats: u8,
        /// Starting credits.
        credits: i32,
        /// World RNG seed.
        seed: u32,
        /// Content-catalog hash (rejects content-mismatched joiners, §4).
        catalog_hash: u64,
    },
    /// C→S: request the session list.
    SessListReq {
        /// The client's `conn_id`.
        conn_id: u32,
    },
    /// S→C: a capped page of open/known sessions.
    SessList {
        /// Up to [`MAX_SESSIONS_IN_LIST`] entries.
        entries: Vec<SessListEntry>,
    },
    /// C→S: join a session; the server assigns the next free seat.
    SessJoin {
        /// The client's `conn_id`.
        conn_id: u32,
        /// The session to join.
        session_id: u32,
        /// The joiner's display name.
        name: String,
    },
    /// S→C: authoritative lobby state, re-broadcast on every change (idempotent,
    /// loss-tolerant — clients render it verbatim, §5).
    SessState {
        /// The session id.
        session_id: u32,
        /// Lifecycle phase.
        phase: SessionPhase,
        /// The settings-authority seat (seat 0 / creator).
        host_seat: SeatId,
        /// Lockstep input delay in ticks.
        delay: u8,
        /// World seed.
        seed: u32,
        /// Starting credits.
        credits: i32,
        /// Scenario filename.
        map: String,
        /// Every occupied seat's state, ascending by seat.
        seats: Vec<SessSeat>,
    },
    /// C→S: ready toggle.
    SessReady {
        /// The client's `conn_id`.
        conn_id: u32,
        /// Ready or not.
        ready: bool,
    },
    /// C→S or S→C: leave / kick / dissolve. `reason` reuses the leave/kick codes.
    SessLeave {
        /// The client's `conn_id` (0 when server-originated).
        conn_id: u32,
        /// Why (0 = player left; other codes = kick/dissolve reasons).
        reason: u8,
    },
    /// S→C: all-ready → the game begins. Carries the seat→house map and the
    /// initial (fixed, M9-A) input delay.
    SessStart {
        /// The session id.
        session_id: u32,
        /// The tick the game starts on (0 in M9-A).
        start_tick: Tick,
        /// Initial input delay in ticks.
        input_delay: u8,
        /// `(seat, house)` for every seat, ascending by seat.
        seat_map: Vec<(SeatId, u8)>,
    },
    /// C→S: this client's own commands (redundant window), each command an
    /// opaque blob stamped by the client's `InputScheduler` exactly as on LAN.
    TickCmds {
        /// The client's `conn_id`.
        conn_id: u32,
        /// `(tick, command blobs)` ascending by tick.
        entries: Vec<(Tick, Vec<Vec<u8>>)>,
    },
    /// S→C: the canonical sequenced bundle (redundant window), all seats,
    /// seat-ascending, with the embedded control-record slot (§6.1/§6.2).
    TickBundle {
        /// One [`BundleEntry`] per tick, ascending.
        entries: Vec<BundleEntry>,
    },
    /// C→S: per-tick state hashes (redundant window), for arbitration (§6.3).
    TickHash {
        /// The client's `conn_id`.
        conn_id: u32,
        /// `(tick, hash)` ascending by tick.
        entries: Vec<(Tick, u64)>,
    },
    /// S→C: arbitration result — only sent on dispute or explicit query (§6.3).
    HashVerdictMsg {
        /// The tick arbitrated.
        tick: Tick,
        /// The verdict for the recipient seat.
        verdict: HashVerdict,
        /// The winning (majority) hash.
        majority_hash: u64,
    },
}

/// A decode failure. Malformed input is an *error value*, never a panic —
/// the fuzz-safety contract of the M8-B brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Buffer shorter than a field required.
    Truncated,
    /// The magic bytes are wrong (not one of our datagrams at all).
    BadMagic,
    /// Header protocol version differs from ours.
    ProtocolMismatch {
        /// The sender's protocol version.
        theirs: u16,
    },
    /// Unknown message type byte.
    UnknownType(u8),
    /// A field held an invalid value (bad enum tag, over-cap count/length).
    BadValue(&'static str),
    /// The payload decoded but bytes were left over.
    TrailingBytes,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Truncated => write!(f, "datagram truncated"),
            WireError::BadMagic => write!(f, "bad magic (not a new-alert datagram)"),
            WireError::ProtocolMismatch { theirs } => write!(
                f,
                "protocol version mismatch (ours {PROTOCOL_VERSION}, theirs {theirs})"
            ),
            WireError::UnknownType(t) => write!(f, "unknown datagram type {t:#04x}"),
            WireError::BadValue(what) => write!(f, "invalid field: {what}"),
            WireError::TrailingBytes => write!(f, "trailing bytes after datagram"),
        }
    }
}

impl std::error::Error for WireError {}

// ---------------------------------------------------------------------------
// Byte-level writer / reader
// ---------------------------------------------------------------------------

// `pub(crate)` so the replay stream format ([`crate::replay`]) reuses the exact
// same byte-level writer/reader and command codec — the "replay reader reuses
// wire decode" contract of SERVER-DESIGN.md §8. Nothing here is part of the
// crate's public API.
pub(crate) struct Writer(pub(crate) Vec<u8>);

impl Writer {
    pub(crate) fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub(crate) fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    pub(crate) fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `u8` length prefix + bytes, truncated to `cap` (encode never fails).
    pub(crate) fn str8(&mut self, s: &str, cap: usize) {
        let bytes = s.as_bytes();
        let n = bytes.len().min(cap).min(255);
        // Truncate on a char boundary so decode's UTF-8 check can't fail.
        let mut n = n;
        while n > 0 && !s.is_char_boundary(n) {
            n -= 1;
        }
        self.u8(n as u8);
        self.0.extend_from_slice(&bytes[..n]);
    }
}

pub(crate) struct Reader<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError::Truncated)?;
        if end > self.buf.len() {
            return Err(WireError::Truncated);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub(crate) fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(crate) fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn str8(&mut self, cap: usize) -> Result<String, WireError> {
        let n = self.u8()? as usize;
        if n > cap {
            return Err(WireError::BadValue("string length over cap"));
        }
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::BadValue("string not UTF-8"))
    }
    pub(crate) fn done(&self) -> Result<(), WireError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }
}

// ---------------------------------------------------------------------------
// Command encode/decode
// ---------------------------------------------------------------------------

const C_MOVE: u8 = 0;
const C_STOP: u8 = 1;
const C_ATTACK: u8 = 2;
const C_DEPLOY: u8 = 3;
const C_START_PRODUCTION: u8 = 4;
const C_PLACE_BUILDING: u8 = 5;
const C_CANCEL_PRODUCTION: u8 = 6;
const C_HOLD_PRODUCTION: u8 = 7;
const C_SELL: u8 = 8;
const C_REPAIR: u8 = 9;
const C_LOAD: u8 = 10;
const C_UNLOAD: u8 = 11;
const C_FIRE_SUPERWEAPON: u8 = 12;

fn put_handle(w: &mut Writer, h: Handle) {
    w.u32(h.index);
    w.u32(h.gen);
}

fn get_handle(r: &mut Reader) -> Result<Handle, WireError> {
    Ok(Handle {
        index: r.u32()?,
        gen: r.u32()?,
    })
}

fn put_cell(w: &mut Writer, c: CellCoord) {
    w.i32(c.x);
    w.i32(c.y);
}

fn get_cell(r: &mut Reader) -> Result<CellCoord, WireError> {
    Ok(CellCoord {
        x: r.i32()?,
        y: r.i32()?,
    })
}

fn put_target(w: &mut Writer, t: Target) {
    match t {
        Target::Unit(h) => {
            w.u8(0);
            put_handle(w, h);
        }
        Target::Building(h) => {
            w.u8(1);
            put_handle(w, h);
        }
        Target::Cell(c) => {
            w.u8(2);
            put_cell(w, c);
        }
    }
}

fn get_target(r: &mut Reader) -> Result<Target, WireError> {
    Ok(match r.u8()? {
        0 => Target::Unit(get_handle(r)?),
        1 => Target::Building(get_handle(r)?),
        2 => Target::Cell(get_cell(r)?),
        _ => return Err(WireError::BadValue("target tag")),
    })
}

fn put_prod_kind(w: &mut Writer, k: ProdKind) {
    w.u8(match k {
        ProdKind::Building => 0,
        ProdKind::Unit => 1,
        ProdKind::Infantry => 2,
    });
}

fn get_prod_kind(r: &mut Reader) -> Result<ProdKind, WireError> {
    Ok(match r.u8()? {
        0 => ProdKind::Building,
        1 => ProdKind::Unit,
        2 => ProdKind::Infantry,
        _ => return Err(WireError::BadValue("prod kind")),
    })
}

fn put_build_item(w: &mut Writer, item: BuildItem) {
    match item {
        BuildItem::Building(id) => {
            w.u8(0);
            w.u32(id);
        }
        BuildItem::Unit(id) => {
            w.u8(1);
            w.u32(id);
        }
    }
}

fn get_build_item(r: &mut Reader) -> Result<BuildItem, WireError> {
    Ok(match r.u8()? {
        0 => BuildItem::Building(r.u32()?),
        1 => BuildItem::Unit(r.u32()?),
        _ => return Err(WireError::BadValue("build item tag")),
    })
}

fn put_super_kind(w: &mut Writer, k: SuperKind) {
    w.u8(match k {
        SuperKind::Nuclear => 0,
        SuperKind::IronCurtain => 1,
        SuperKind::Chronosphere => 2,
    });
}

fn get_super_kind(r: &mut Reader) -> Result<SuperKind, WireError> {
    Ok(match r.u8()? {
        0 => SuperKind::Nuclear,
        1 => SuperKind::IronCurtain,
        2 => SuperKind::Chronosphere,
        _ => return Err(WireError::BadValue("superweapon kind")),
    })
}

pub(crate) fn put_command(w: &mut Writer, c: &Command) {
    match *c {
        Command::Move { unit, dest, house } => {
            w.u8(C_MOVE);
            put_handle(w, unit);
            put_cell(w, dest);
            w.u8(house);
        }
        Command::Stop { unit, house } => {
            w.u8(C_STOP);
            put_handle(w, unit);
            w.u8(house);
        }
        Command::Attack {
            unit,
            target,
            house,
        } => {
            w.u8(C_ATTACK);
            put_handle(w, unit);
            put_target(w, target);
            w.u8(house);
        }
        Command::Deploy { unit, house } => {
            w.u8(C_DEPLOY);
            put_handle(w, unit);
            w.u8(house);
        }
        Command::StartProduction { house, item } => {
            w.u8(C_START_PRODUCTION);
            w.u8(house);
            put_build_item(w, item);
        }
        Command::PlaceBuilding {
            house,
            building,
            cell,
        } => {
            w.u8(C_PLACE_BUILDING);
            w.u8(house);
            w.u32(building);
            put_cell(w, cell);
        }
        Command::CancelProduction { house, kind } => {
            w.u8(C_CANCEL_PRODUCTION);
            w.u8(house);
            put_prod_kind(w, kind);
        }
        Command::HoldProduction { house, kind } => {
            w.u8(C_HOLD_PRODUCTION);
            w.u8(house);
            put_prod_kind(w, kind);
        }
        Command::Sell { house, building } => {
            w.u8(C_SELL);
            w.u8(house);
            put_handle(w, building);
        }
        Command::Repair { house, building } => {
            w.u8(C_REPAIR);
            w.u8(house);
            put_handle(w, building);
        }
        Command::Load {
            passenger,
            transport,
            house,
        } => {
            w.u8(C_LOAD);
            put_handle(w, passenger);
            put_handle(w, transport);
            w.u8(house);
        }
        Command::Unload { transport, house } => {
            w.u8(C_UNLOAD);
            put_handle(w, transport);
            w.u8(house);
        }
        Command::FireSuperWeapon {
            house,
            kind,
            target,
            dest,
        } => {
            w.u8(C_FIRE_SUPERWEAPON);
            w.u8(house);
            put_super_kind(w, kind);
            put_target(w, target);
            match dest {
                Some(c) => {
                    w.u8(1);
                    put_cell(w, c);
                }
                None => w.u8(0),
            }
        }
    }
}

pub(crate) fn get_command(r: &mut Reader) -> Result<Command, WireError> {
    Ok(match r.u8()? {
        C_MOVE => Command::Move {
            unit: get_handle(r)?,
            dest: get_cell(r)?,
            house: r.u8()?,
        },
        C_STOP => Command::Stop {
            unit: get_handle(r)?,
            house: r.u8()?,
        },
        C_ATTACK => Command::Attack {
            unit: get_handle(r)?,
            target: get_target(r)?,
            house: r.u8()?,
        },
        C_DEPLOY => Command::Deploy {
            unit: get_handle(r)?,
            house: r.u8()?,
        },
        C_START_PRODUCTION => Command::StartProduction {
            house: r.u8()?,
            item: get_build_item(r)?,
        },
        C_PLACE_BUILDING => Command::PlaceBuilding {
            house: r.u8()?,
            building: r.u32()?,
            cell: get_cell(r)?,
        },
        C_CANCEL_PRODUCTION => Command::CancelProduction {
            house: r.u8()?,
            kind: get_prod_kind(r)?,
        },
        C_HOLD_PRODUCTION => Command::HoldProduction {
            house: r.u8()?,
            kind: get_prod_kind(r)?,
        },
        C_SELL => Command::Sell {
            house: r.u8()?,
            building: get_handle(r)?,
        },
        C_REPAIR => Command::Repair {
            house: r.u8()?,
            building: get_handle(r)?,
        },
        C_LOAD => Command::Load {
            passenger: get_handle(r)?,
            transport: get_handle(r)?,
            house: r.u8()?,
        },
        C_UNLOAD => Command::Unload {
            transport: get_handle(r)?,
            house: r.u8()?,
        },
        C_FIRE_SUPERWEAPON => Command::FireSuperWeapon {
            house: r.u8()?,
            kind: get_super_kind(r)?,
            target: get_target(r)?,
            dest: match r.u8()? {
                0 => None,
                1 => Some(get_cell(r)?),
                _ => return Err(WireError::BadValue("dest flag")),
            },
        },
        _ => return Err(WireError::BadValue("command tag")),
    })
}

// ---------------------------------------------------------------------------
// Wire v2 opaque command blobs + house accessor (SERVER-DESIGN.md §7)
// ---------------------------------------------------------------------------

/// Encode one `Command` to its opaque wire blob. The relay moves these bytes
/// without decoding them; only the house field is read, via [`command_house`].
pub fn encode_command(c: &Command) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(24));
    put_command(&mut w, c);
    w.0
}

/// Decode one command blob back into a `Command` (client side). Exact
/// consumption — a blob holds exactly one command.
pub fn decode_command(blob: &[u8]) -> Result<Command, WireError> {
    let mut r = Reader { buf: blob, pos: 0 };
    let c = get_command(&mut r)?;
    r.done()?;
    Ok(c)
}

/// The issuing house of an encoded command blob — the seat-house binding check
/// the relay performs per command (§7.3) **without a `ra-sim` dependency at the
/// call site** (the caller gets a `u8`). The per-tag offset knowledge stays here,
/// in one place, per the M9-A brief.
pub fn command_house(blob: &[u8]) -> Result<u8, WireError> {
    let c = decode_command(blob)?;
    Ok(match c {
        Command::Move { house, .. }
        | Command::Stop { house, .. }
        | Command::Attack { house, .. }
        | Command::Deploy { house, .. }
        | Command::StartProduction { house, .. }
        | Command::PlaceBuilding { house, .. }
        | Command::CancelProduction { house, .. }
        | Command::HoldProduction { house, .. }
        | Command::Sell { house, .. }
        | Command::Repair { house, .. }
        | Command::Load { house, .. }
        | Command::Unload { house, .. }
        | Command::FireSuperWeapon { house, .. } => house,
    })
}

/// Write a length-prefixed list of opaque command blobs (`TICK_CMDS`/
/// `TICK_BUNDLE` per-seat payload).
fn put_cmd_blobs(w: &mut Writer, blobs: &[Vec<u8>]) {
    let m = blobs.len().min(MAX_CMDS_PER_TICK);
    w.u16(m as u16);
    for b in blobs.iter().take(MAX_CMDS_PER_TICK) {
        let n = b.len().min(MAX_CMD_BLOB);
        w.u16(n as u16);
        w.0.extend_from_slice(&b[..n]);
    }
}

fn get_cmd_blobs(r: &mut Reader) -> Result<Vec<Vec<u8>>, WireError> {
    let m = r.u16()? as usize;
    if m > MAX_CMDS_PER_TICK {
        return Err(WireError::BadValue("cmd blob count"));
    }
    let mut blobs = Vec::with_capacity(m.min(256));
    for _ in 0..m {
        let n = r.u16()? as usize;
        if n > MAX_CMD_BLOB {
            return Err(WireError::BadValue("cmd blob len over cap"));
        }
        blobs.push(r.take(n)?.to_vec());
    }
    Ok(blobs)
}

/// Write a tick's embedded control records. Every record is length-prefixed so a
/// decoder that does not know a tag can skip it (forward compatibility, §6.2).
fn put_ctrl(w: &mut Writer, ctrl: &[CtrlRecord]) {
    let n = ctrl.len().min(MAX_CTRL_PER_TICK);
    w.u8(n as u8);
    for c in ctrl.iter().take(MAX_CTRL_PER_TICK) {
        match c {
            CtrlRecord::Timing {
                new_delay,
                effective_tick,
            } => {
                w.u8(CTRL_TIMING);
                w.u16(5); // payload: u8 + u32
                w.u8(*new_delay);
                w.u32(*effective_tick);
            }
            CtrlRecord::Unknown { tag, bytes } => {
                w.u8(*tag);
                let n = bytes.len().min(MAX_CTRL_PAYLOAD);
                w.u16(n as u16);
                w.0.extend_from_slice(&bytes[..n]);
            }
        }
    }
}

fn get_ctrl(r: &mut Reader) -> Result<Vec<CtrlRecord>, WireError> {
    let n = r.u8()? as usize;
    if n > MAX_CTRL_PER_TICK {
        return Err(WireError::BadValue("ctrl count over cap"));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r.u8()?;
        let len = r.u16()? as usize;
        if len > MAX_CTRL_PAYLOAD {
            return Err(WireError::BadValue("ctrl payload over cap"));
        }
        let payload = r.take(len)?;
        match tag {
            CTRL_TIMING => {
                let mut pr = Reader {
                    buf: payload,
                    pos: 0,
                };
                let new_delay = pr.u8()?;
                let effective_tick = pr.u32()?;
                pr.done()?;
                out.push(CtrlRecord::Timing {
                    new_delay,
                    effective_tick,
                });
            }
            _ => out.push(CtrlRecord::Unknown {
                tag,
                bytes: payload.to_vec(),
            }),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Datagram encode/decode
// ---------------------------------------------------------------------------

/// Encode with the build's own [`PROTOCOL_VERSION`].
pub fn encode(d: &Datagram) -> Vec<u8> {
    encode_with_protocol(d, PROTOCOL_VERSION)
}

/// Encode with an explicit protocol version — the seam the handshake
/// negative tests use to synthesise a mismatched peer.
pub fn encode_with_protocol(d: &Datagram, protocol: u16) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(64));
    w.u16(WIRE_MAGIC);
    w.u16(protocol);
    match d {
        Datagram::Announce {
            game_version,
            game_port,
            name,
            map,
        } => {
            w.u8(T_ANNOUNCE);
            w.u32(*game_version);
            w.u16(*game_port);
            w.str8(name, MAX_NAME);
            w.str8(map, MAX_MAP_NAME);
        }
        Datagram::Join { game_version, name } => {
            w.u8(T_JOIN);
            w.u32(*game_version);
            w.str8(name, MAX_NAME);
        }
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
            w.u8(T_WELCOME);
            w.u32(*game_version);
            w.u8(*seat);
            w.u8(*host_seat);
            w.u8(*delay);
            w.u32(*seed);
            w.i32(*credits);
            w.str8(map, MAX_MAP_NAME);
            w.str8(host_name, MAX_NAME);
        }
        Datagram::Reject { reason } => {
            w.u8(T_REJECT);
            w.u8(reason.to_byte());
        }
        Datagram::Ready => w.u8(T_READY),
        Datagram::Start => w.u8(T_START),
        Datagram::Leave => w.u8(T_LEAVE),
        Datagram::Bundles { entries } => {
            w.u8(T_BUNDLES);
            w.u8(entries.len().min(MAX_TICK_ENTRIES) as u8);
            for (tick, cmds) in entries.iter().take(MAX_TICK_ENTRIES) {
                w.u32(*tick);
                w.u16(cmds.len().min(MAX_CMDS_PER_TICK) as u16);
                for c in cmds.iter().take(MAX_CMDS_PER_TICK) {
                    put_command(&mut w, c);
                }
            }
        }
        Datagram::Hashes { entries } => {
            w.u8(T_HASHES);
            w.u8(entries.len().min(MAX_TICK_ENTRIES) as u8);
            for (tick, hash) in entries.iter().take(MAX_TICK_ENTRIES) {
                w.u32(*tick);
                w.u64(*hash);
            }
        }
        Datagram::Nack { from } => {
            w.u8(T_NACK);
            w.u32(*from);
        }
        Datagram::KeepAlive { tick } => {
            w.u8(T_KEEPALIVE);
            w.u32(*tick);
        }
        Datagram::Quit => w.u8(T_QUIT),
        Datagram::SnapshotOffer {
            attempt,
            resume_tick,
            declared_hash,
            total_len,
            chunk_size,
        } => {
            w.u8(T_SNAP_OFFER);
            w.u8(*attempt);
            w.u32(*resume_tick);
            w.u64(*declared_hash);
            w.u32(*total_len);
            w.u16(*chunk_size);
        }
        Datagram::SnapshotChunk { attempt, seq, data } => {
            w.u8(T_SNAP_CHUNK);
            w.u8(*attempt);
            w.u32(*seq);
            let n = data.len().min(MAX_SNAP_CHUNK_DATA);
            w.u16(n as u16);
            w.0.extend_from_slice(&data[..n]);
        }
        Datagram::SnapshotAck { attempt, missing } => {
            w.u8(T_SNAP_ACK);
            w.u8(*attempt);
            let n = missing.len().min(MAX_SNAP_MISSING);
            w.u32(n as u32);
            for &seq in missing.iter().take(MAX_SNAP_MISSING) {
                w.u32(seq);
            }
        }
        Datagram::SnapshotDone { attempt, ok } => {
            w.u8(T_SNAP_DONE);
            w.u8(*attempt);
            w.u8(*ok as u8);
        }
        Datagram::SrvHello {
            game_version,
            client_nonce,
        } => {
            w.u8(T_SRV_HELLO);
            w.u32(*game_version);
            w.u32(*client_nonce);
        }
        Datagram::SrvWelcome {
            server_nonce,
            conn_id,
        } => {
            w.u8(T_SRV_WELCOME);
            w.u32(*server_nonce);
            w.u32(*conn_id);
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
            w.u8(T_SESS_CREATE);
            w.u32(*conn_id);
            w.str8(name, MAX_NAME);
            w.str8(map, MAX_MAP_NAME);
            w.u8(*seats);
            w.i32(*credits);
            w.u32(*seed);
            w.u64(*catalog_hash);
        }
        Datagram::SessListReq { conn_id } => {
            w.u8(T_SESS_LIST_REQ);
            w.u32(*conn_id);
        }
        Datagram::SessList { entries } => {
            w.u8(T_SESS_LIST);
            let n = entries.len().min(MAX_SESSIONS_IN_LIST);
            w.u8(n as u8);
            for e in entries.iter().take(MAX_SESSIONS_IN_LIST) {
                w.u32(e.session_id);
                w.str8(&e.name, MAX_NAME);
                w.str8(&e.map, MAX_MAP_NAME);
                w.u8(e.seats_taken);
                w.u8(e.seats);
                w.u8(e.in_progress as u8);
            }
        }
        Datagram::SessJoin {
            conn_id,
            session_id,
            name,
        } => {
            w.u8(T_SESS_JOIN);
            w.u32(*conn_id);
            w.u32(*session_id);
            w.str8(name, MAX_NAME);
        }
        Datagram::SessState {
            session_id,
            phase,
            host_seat,
            delay,
            seed,
            credits,
            map,
            seats,
        } => {
            w.u8(T_SESS_STATE);
            w.u32(*session_id);
            w.u8(phase.to_byte());
            w.u8(*host_seat);
            w.u8(*delay);
            w.u32(*seed);
            w.i32(*credits);
            w.str8(map, MAX_MAP_NAME);
            let n = seats.len().min(MAX_SEATS);
            w.u8(n as u8);
            for s in seats.iter().take(MAX_SEATS) {
                w.u8(s.seat);
                w.str8(&s.name, MAX_NAME);
                w.u8(s.house);
                w.u8(s.ready as u8);
            }
        }
        Datagram::SessReady { conn_id, ready } => {
            w.u8(T_SESS_READY);
            w.u32(*conn_id);
            w.u8(*ready as u8);
        }
        Datagram::SessLeave { conn_id, reason } => {
            w.u8(T_SESS_LEAVE);
            w.u32(*conn_id);
            w.u8(*reason);
        }
        Datagram::SessStart {
            session_id,
            start_tick,
            input_delay,
            seat_map,
        } => {
            w.u8(T_SESS_START);
            w.u32(*session_id);
            w.u32(*start_tick);
            w.u8(*input_delay);
            let n = seat_map.len().min(MAX_SEATS);
            w.u8(n as u8);
            for (seat, house) in seat_map.iter().take(MAX_SEATS) {
                w.u8(*seat);
                w.u8(*house);
            }
        }
        Datagram::TickCmds { conn_id, entries } => {
            w.u8(T_TICK_CMDS);
            w.u32(*conn_id);
            let n = entries.len().min(MAX_TICK_ENTRIES);
            w.u8(n as u8);
            for (tick, blobs) in entries.iter().take(MAX_TICK_ENTRIES) {
                w.u32(*tick);
                put_cmd_blobs(&mut w, blobs);
            }
        }
        Datagram::TickBundle { entries } => {
            w.u8(T_TICK_BUNDLE);
            let n = entries.len().min(MAX_TICK_ENTRIES);
            w.u8(n as u8);
            for e in entries.iter().take(MAX_TICK_ENTRIES) {
                w.u32(e.tick);
                put_ctrl(&mut w, &e.ctrl);
                let ns = e.seats.len().min(MAX_SEATS);
                w.u8(ns as u8);
                for (seat, blobs) in e.seats.iter().take(MAX_SEATS) {
                    w.u8(*seat);
                    put_cmd_blobs(&mut w, blobs);
                }
            }
        }
        Datagram::TickHash { conn_id, entries } => {
            w.u8(T_TICK_HASH);
            w.u32(*conn_id);
            let n = entries.len().min(MAX_TICK_ENTRIES);
            w.u8(n as u8);
            for (tick, hash) in entries.iter().take(MAX_TICK_ENTRIES) {
                w.u32(*tick);
                w.u64(*hash);
            }
        }
        Datagram::HashVerdictMsg {
            tick,
            verdict,
            majority_hash,
        } => {
            w.u8(T_HASH_VERDICT);
            w.u32(*tick);
            w.u8(verdict.to_byte());
            w.u64(*majority_hash);
        }
    }
    w.0
}

/// Decode one datagram. Total: length-checked, tag-validated, cap-enforced,
/// exact-consumption — malformed input yields an error, never a panic.
pub fn decode(buf: &[u8]) -> Result<Datagram, WireError> {
    let mut r = Reader { buf, pos: 0 };
    if r.u16()? != WIRE_MAGIC {
        return Err(WireError::BadMagic);
    }
    let protocol = r.u16()?;
    if protocol != PROTOCOL_VERSION {
        return Err(WireError::ProtocolMismatch { theirs: protocol });
    }
    let d = match r.u8()? {
        T_ANNOUNCE => Datagram::Announce {
            game_version: r.u32()?,
            game_port: r.u16()?,
            name: r.str8(MAX_NAME)?,
            map: r.str8(MAX_MAP_NAME)?,
        },
        T_JOIN => Datagram::Join {
            game_version: r.u32()?,
            name: r.str8(MAX_NAME)?,
        },
        T_WELCOME => Datagram::Welcome {
            game_version: r.u32()?,
            seat: r.u8()?,
            host_seat: r.u8()?,
            delay: r.u8()?,
            seed: r.u32()?,
            credits: r.i32()?,
            map: r.str8(MAX_MAP_NAME)?,
            host_name: r.str8(MAX_NAME)?,
        },
        T_REJECT => Datagram::Reject {
            reason: RejectReason::from_byte(r.u8()?)?,
        },
        T_READY => Datagram::Ready,
        T_START => Datagram::Start,
        T_LEAVE => Datagram::Leave,
        T_BUNDLES => {
            let n = r.u8()? as usize;
            if n > MAX_TICK_ENTRIES {
                return Err(WireError::BadValue("bundle entry count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let tick = r.u32()?;
                let m = r.u16()? as usize;
                if m > MAX_CMDS_PER_TICK {
                    return Err(WireError::BadValue("command count"));
                }
                let mut cmds = Vec::with_capacity(m.min(256));
                for _ in 0..m {
                    cmds.push(get_command(&mut r)?);
                }
                entries.push((tick, cmds));
            }
            Datagram::Bundles { entries }
        }
        T_HASHES => {
            let n = r.u8()? as usize;
            if n > MAX_TICK_ENTRIES {
                return Err(WireError::BadValue("hash entry count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push((r.u32()?, r.u64()?));
            }
            Datagram::Hashes { entries }
        }
        T_NACK => Datagram::Nack { from: r.u32()? },
        T_KEEPALIVE => Datagram::KeepAlive { tick: r.u32()? },
        T_QUIT => Datagram::Quit,
        T_SNAP_OFFER => {
            let attempt = r.u8()?;
            let resume_tick = r.u32()?;
            let declared_hash = r.u64()?;
            let total_len = r.u32()?;
            let chunk_size = r.u16()?;
            if total_len as usize > MAX_SNAPSHOT_LEN {
                return Err(WireError::BadValue("snapshot total_len over cap"));
            }
            if chunk_size == 0 || chunk_size as usize > MAX_SNAP_CHUNK_DATA {
                return Err(WireError::BadValue("snapshot chunk_size"));
            }
            Datagram::SnapshotOffer {
                attempt,
                resume_tick,
                declared_hash,
                total_len,
                chunk_size,
            }
        }
        T_SNAP_CHUNK => {
            let attempt = r.u8()?;
            let seq = r.u32()?;
            let n = r.u16()? as usize;
            if n > MAX_SNAP_CHUNK_DATA {
                return Err(WireError::BadValue("snapshot chunk len over cap"));
            }
            let data = r.take(n)?.to_vec();
            Datagram::SnapshotChunk { attempt, seq, data }
        }
        T_SNAP_ACK => {
            let attempt = r.u8()?;
            let n = r.u32()? as usize;
            if n > MAX_SNAP_MISSING {
                return Err(WireError::BadValue("snapshot missing count over cap"));
            }
            let mut missing = Vec::with_capacity(n.min(256));
            for _ in 0..n {
                missing.push(r.u32()?);
            }
            Datagram::SnapshotAck { attempt, missing }
        }
        T_SNAP_DONE => Datagram::SnapshotDone {
            attempt: r.u8()?,
            ok: match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(WireError::BadValue("snapshot done flag")),
            },
        },
        T_SRV_HELLO => Datagram::SrvHello {
            game_version: r.u32()?,
            client_nonce: r.u32()?,
        },
        T_SRV_WELCOME => Datagram::SrvWelcome {
            server_nonce: r.u32()?,
            conn_id: r.u32()?,
        },
        T_SESS_CREATE => Datagram::SessCreate {
            conn_id: r.u32()?,
            name: r.str8(MAX_NAME)?,
            map: r.str8(MAX_MAP_NAME)?,
            seats: r.u8()?,
            credits: r.i32()?,
            seed: r.u32()?,
            catalog_hash: r.u64()?,
        },
        T_SESS_LIST_REQ => Datagram::SessListReq { conn_id: r.u32()? },
        T_SESS_LIST => {
            let n = r.u8()? as usize;
            if n > MAX_SESSIONS_IN_LIST {
                return Err(WireError::BadValue("session list count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(SessListEntry {
                    session_id: r.u32()?,
                    name: r.str8(MAX_NAME)?,
                    map: r.str8(MAX_MAP_NAME)?,
                    seats_taken: r.u8()?,
                    seats: r.u8()?,
                    in_progress: match r.u8()? {
                        0 => false,
                        1 => true,
                        _ => return Err(WireError::BadValue("in_progress flag")),
                    },
                });
            }
            Datagram::SessList { entries }
        }
        T_SESS_JOIN => Datagram::SessJoin {
            conn_id: r.u32()?,
            session_id: r.u32()?,
            name: r.str8(MAX_NAME)?,
        },
        T_SESS_STATE => {
            let session_id = r.u32()?;
            let phase = SessionPhase::from_byte(r.u8()?)?;
            let host_seat = r.u8()?;
            let delay = r.u8()?;
            let seed = r.u32()?;
            let credits = r.i32()?;
            let map = r.str8(MAX_MAP_NAME)?;
            let n = r.u8()? as usize;
            if n > MAX_SEATS {
                return Err(WireError::BadValue("sess_state seat count"));
            }
            let mut seats = Vec::with_capacity(n);
            for _ in 0..n {
                seats.push(SessSeat {
                    seat: r.u8()?,
                    name: r.str8(MAX_NAME)?,
                    house: r.u8()?,
                    ready: match r.u8()? {
                        0 => false,
                        1 => true,
                        _ => return Err(WireError::BadValue("ready flag")),
                    },
                });
            }
            Datagram::SessState {
                session_id,
                phase,
                host_seat,
                delay,
                seed,
                credits,
                map,
                seats,
            }
        }
        T_SESS_READY => Datagram::SessReady {
            conn_id: r.u32()?,
            ready: match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(WireError::BadValue("ready flag")),
            },
        },
        T_SESS_LEAVE => Datagram::SessLeave {
            conn_id: r.u32()?,
            reason: r.u8()?,
        },
        T_SESS_START => {
            let session_id = r.u32()?;
            let start_tick = r.u32()?;
            let input_delay = r.u8()?;
            let n = r.u8()? as usize;
            if n > MAX_SEATS {
                return Err(WireError::BadValue("sess_start seat count"));
            }
            let mut seat_map = Vec::with_capacity(n);
            for _ in 0..n {
                seat_map.push((r.u8()?, r.u8()?));
            }
            Datagram::SessStart {
                session_id,
                start_tick,
                input_delay,
                seat_map,
            }
        }
        T_TICK_CMDS => {
            let conn_id = r.u32()?;
            let n = r.u8()? as usize;
            if n > MAX_TICK_ENTRIES {
                return Err(WireError::BadValue("tick_cmds entry count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let tick = r.u32()?;
                entries.push((tick, get_cmd_blobs(&mut r)?));
            }
            Datagram::TickCmds { conn_id, entries }
        }
        T_TICK_BUNDLE => {
            let n = r.u8()? as usize;
            if n > MAX_TICK_ENTRIES {
                return Err(WireError::BadValue("tick_bundle entry count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                let tick = r.u32()?;
                let ctrl = get_ctrl(&mut r)?;
                let ns = r.u8()? as usize;
                if ns > MAX_SEATS {
                    return Err(WireError::BadValue("tick_bundle seat count"));
                }
                let mut seats = Vec::with_capacity(ns);
                for _ in 0..ns {
                    let seat = r.u8()?;
                    seats.push((seat, get_cmd_blobs(&mut r)?));
                }
                entries.push(BundleEntry { tick, ctrl, seats });
            }
            Datagram::TickBundle { entries }
        }
        T_TICK_HASH => {
            let conn_id = r.u32()?;
            let n = r.u8()? as usize;
            if n > MAX_TICK_ENTRIES {
                return Err(WireError::BadValue("tick_hash entry count"));
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push((r.u32()?, r.u64()?));
            }
            Datagram::TickHash { conn_id, entries }
        }
        T_HASH_VERDICT => Datagram::HashVerdictMsg {
            tick: r.u32()?,
            verdict: HashVerdict::from_byte(r.u8()?)?,
            majority_hash: r.u64()?,
        },
        t => return Err(WireError::UnknownType(t)),
    };
    r.done()?;
    Ok(d)
}
