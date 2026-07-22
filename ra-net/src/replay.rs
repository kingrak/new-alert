//! M7.23 P0: the on-disk **replay** format and a [`ReplayTransport`] that plays
//! a recorded command stream back through the [`CommandTransport`] seam.
//!
//! DESIGN.md's day-one claim ("a replay is just the command log + initial
//! seed", §4.4) and SERVER-DESIGN.md §8 both specify this: the replay stream
//! *is* the wire encoding — one length-prefixed record after another — so the
//! reader reuses [`crate::wire`]'s never-panic decode discipline and a replay
//! can drive a [`ReplayTransport`] exactly like a live peer.
//!
//! # Why player commands + seed reproduce the whole game (the load-bearing bit)
//!
//! In single player the AI's commands do **not** cross the transport: `run_ai`
//! is *System 0* inside `ra_sim::apply` (world.rs), issuing its `Command`s from
//! the same seeded `World`-owned RNG the rest of the sim draws from, in a fixed
//! per-house order. Because the AI is a pure function of `World` state + that
//! RNG — and both evolve identically when the *same* player command stream is
//! re-applied to the *same* initial world (`seed` in the header) — replaying
//! the recorded **player** bundles re-derives every AI decision bit-for-bit.
//! The interleaved hash records (every [`HASH_INTERVAL`] ticks) are the proof:
//! `replay-verify` re-simulates and asserts each one.
//!
//! # File layout
//!
//! ```text
//! "RARP"                       4 bytes magic
//! replay_version               u16   (this build: REPLAY_VERSION)
//! header_len                   u32   length of the header body that follows
//! header body                  header_len bytes (see encode_header)
//! record*                      zero or more length-prefixed records:
//!   rec_len                    u32   length of the record body that follows
//!   rec body                   rec_len bytes, tag u8 + payload
//! ```
//!
//! Every multi-byte field is little-endian. Decoding is length-checked,
//! tag-validated, cap-enforced, and exact-consumption per record — a malformed
//! or truncated file yields a [`ReplayError`], never a panic (the same
//! fuzz-safety contract as `wire`).

use std::collections::BTreeMap;

use ra_sim::Command;

use crate::transport::{CommandTransport, PollResult, SeatId, Tick, TickBundle};
use crate::wire::{
    get_command, put_command, Reader, WireError, Writer, MAX_CMDS_PER_TICK, MAX_MAP_NAME,
    MAX_TICK_ENTRIES,
};

/// File magic: the ASCII bytes `RARP` (Red Alert RePlay).
pub const REPLAY_MAGIC: [u8; 4] = *b"RARP";

/// Replay container version. Bump on ANY change to the header or record layout;
/// a reader refuses a file whose version it does not understand.
pub const REPLAY_VERSION: u16 = 1;

/// A state-hash record is written every this-many ticks (SERVER-DESIGN.md §8:
/// "the winning `TICK_HASH` chain"). Sparse enough to keep files small, dense
/// enough to localise a divergence to a 1-second window at 15 Hz.
pub const HASH_INTERVAL: u32 = 15;

/// Cap on seats named in a replay header (the original tops out at 8 MP houses;
/// a malformed count must never over-allocate).
pub const MAX_REPLAY_SEATS: usize = 16;

// Record type tags.
const R_TICK: u8 = 1;
const R_HASH: u8 = 2;
const R_END: u8 = 3;

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

/// Per-seat cosmetic/setup identity carried in the header (house + colour), so a
/// replay renders with the same colours the live game used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaySeat {
    /// The seat id (== house index — the transport's [`SeatId`]).
    pub seat: SeatId,
    /// The house this seat played.
    pub house: u8,
    /// The colour-remap row painting this seat's units.
    pub color: u8,
}

/// Everything needed to rebuild the identical initial `World` and drive a
/// replay: the versions to reject a drifted build, the scenario + seed + shared
/// settings the loader consumes, a catalog content-hash to flag asset drift,
/// and the wall-clock start time (**passed in by the caller** — `ra-net` core
/// takes no wall-clock, §4.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayHeader {
    /// The [`REPLAY_VERSION`] the file was written with.
    pub replay_version: u16,
    /// The [`wire::GAME_VERSION`] of the recording build.
    pub game_version: u32,
    /// The [`wire::PROTOCOL_VERSION`] of the recording build.
    pub protocol_version: u16,
    /// Scenario / map filename both the recording and the replay load.
    pub scenario: String,
    /// The `World` RNG seed at game start.
    pub seed: u32,
    /// AI difficulty (0 = Easy, 1 = Normal, 2 = Hard).
    pub difficulty: u8,
    /// Starting credits for the houses.
    pub credits: i32,
    /// `Catalog::content_hash()` at record time — a replay loaded against
    /// drifted stats will diverge, and this lets a tool say so up front.
    pub catalog_hash: u64,
    /// Recording start time, Unix epoch milliseconds. Supplied by the shell
    /// layer; the sim/net core never reads a clock.
    pub start_millis: u64,
    /// Per-seat house/colour setup.
    pub seats: Vec<ReplaySeat>,
}

impl ReplayHeader {
    /// Encode the header body (everything after `header_len`). Used by
    /// [`encode_header`]; exposed for symmetry with the decoder.
    fn write_body(&self, w: &mut Writer) {
        w.u32(self.game_version);
        w.u16(self.protocol_version);
        w.u32(self.seed);
        w.u8(self.difficulty);
        w.i32(self.credits);
        w.u64(self.catalog_hash);
        w.u64(self.start_millis);
        w.str8(&self.scenario, MAX_MAP_NAME);
        let n = self.seats.len().min(MAX_REPLAY_SEATS);
        w.u8(n as u8);
        for s in self.seats.iter().take(MAX_REPLAY_SEATS) {
            w.u8(s.seat);
            w.u8(s.house);
            w.u8(s.color);
        }
    }

    fn read_body(r: &mut Reader, replay_version: u16) -> Result<ReplayHeader, ReplayError> {
        let game_version = r.u32()?;
        let protocol_version = r.u16()?;
        let seed = r.u32()?;
        let difficulty = r.u8()?;
        let credits = r.i32()?;
        let catalog_hash = r.u64()?;
        let start_millis = r.u64()?;
        let scenario = r.str8(MAX_MAP_NAME)?;
        let n = r.u8()? as usize;
        if n > MAX_REPLAY_SEATS {
            return Err(ReplayError::Wire(WireError::BadValue("replay seat count")));
        }
        let mut seats = Vec::with_capacity(n);
        for _ in 0..n {
            seats.push(ReplaySeat {
                seat: r.u8()?,
                house: r.u8()?,
                color: r.u8()?,
            });
        }
        Ok(ReplayHeader {
            replay_version,
            game_version,
            protocol_version,
            seed,
            difficulty,
            credits,
            catalog_hash,
            start_millis,
            scenario,
            seats,
        })
    }
}

/// One decoded stream record: a non-empty tick's [`TickBundle`], a periodic
/// state hash, or the terminating end marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayRecord {
    /// A tick that carried at least one command (empty ticks are omitted).
    Tick {
        /// The execution tick.
        tick: Tick,
        /// Every seat's commands for it, in canonical order.
        bundle: TickBundle,
    },
    /// A post-tick state hash (written every [`HASH_INTERVAL`] ticks).
    Hash {
        /// The tick the hash is for.
        tick: Tick,
        /// The `World::state_hash()` after that tick's `apply`.
        hash: u64,
    },
    /// Terminates the stream: why the game ended and the final tick reached.
    End {
        /// The end reason.
        reason: EndReason,
        /// The last tick simulated.
        final_tick: Tick,
    },
}

/// Why a recorded game ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    /// The recording player won.
    Victory,
    /// The recording player lost.
    Defeat,
    /// The player quit / closed the game (or the process exited cleanly).
    Quit,
    /// A LAN game diverged (hash mismatch) and could not resync.
    Desync,
}

impl EndReason {
    fn to_byte(self) -> u8 {
        match self {
            EndReason::Victory => 0,
            EndReason::Defeat => 1,
            EndReason::Quit => 2,
            EndReason::Desync => 3,
        }
    }
    fn from_byte(b: u8) -> Result<EndReason, ReplayError> {
        Ok(match b {
            0 => EndReason::Victory,
            1 => EndReason::Defeat,
            2 => EndReason::Quit,
            3 => EndReason::Desync,
            _ => return Err(ReplayError::Wire(WireError::BadValue("end reason"))),
        })
    }
}

/// A replay decode failure. Like [`WireError`], malformed input is an error
/// *value*, never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// The first four bytes are not `RARP`.
    BadMagic,
    /// The file's [`REPLAY_VERSION`] is not one this build understands.
    UnsupportedVersion {
        /// The version found in the file.
        found: u16,
    },
    /// An unknown record tag byte.
    UnknownRecord(u8),
    /// A field / length check failed inside a record or the header (reuses the
    /// `wire` reader's vocabulary).
    Wire(WireError),
}

impl From<WireError> for ReplayError {
    fn from(e: WireError) -> ReplayError {
        ReplayError::Wire(e)
    }
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::BadMagic => write!(f, "not a replay file (bad magic)"),
            ReplayError::UnsupportedVersion { found } => write!(
                f,
                "unsupported replay version {found} (this build reads {REPLAY_VERSION})"
            ),
            ReplayError::UnknownRecord(t) => write!(f, "unknown replay record tag {t:#04x}"),
            ReplayError::Wire(e) => write!(f, "malformed replay: {e}"),
        }
    }
}

impl std::error::Error for ReplayError {}

// ---------------------------------------------------------------------------
// Encoding (append-oriented: the client writes these bytes straight to a file)
// ---------------------------------------------------------------------------

/// Encode the file prefix: magic + version + length-prefixed header body.
pub fn encode_header(h: &ReplayHeader) -> Vec<u8> {
    let mut body = Writer(Vec::with_capacity(64));
    h.write_body(&mut body);
    let body = body.0;

    let mut w = Writer(Vec::with_capacity(body.len() + 10));
    w.0.extend_from_slice(&REPLAY_MAGIC);
    w.u16(REPLAY_VERSION);
    w.u32(body.len() as u32);
    w.0.extend_from_slice(&body);
    w.0
}

/// Frame one record body (`tag + payload`) with its `u32` length prefix.
fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(body.len() + 4));
    w.u32(body.len() as u32);
    w.0.extend_from_slice(&body);
    w.0
}

/// Encode a non-empty tick's bundle as a framed record. Empty bundles are the
/// caller's responsibility to skip (only non-empty ticks are recorded).
pub fn encode_tick(tick: Tick, bundle: &TickBundle) -> Vec<u8> {
    let mut b = Writer(Vec::with_capacity(32));
    b.u8(R_TICK);
    b.u32(tick);
    let nseats = bundle.seats.len().min(MAX_TICK_ENTRIES);
    b.u8(nseats as u8);
    for (seat, cmds) in bundle.seats.iter().take(MAX_TICK_ENTRIES) {
        b.u8(*seat);
        let m = cmds.len().min(MAX_CMDS_PER_TICK);
        b.u16(m as u16);
        for c in cmds.iter().take(MAX_CMDS_PER_TICK) {
            put_command(&mut b, c);
        }
    }
    frame(b.0)
}

/// Encode a periodic hash record.
pub fn encode_hash(tick: Tick, hash: u64) -> Vec<u8> {
    let mut b = Writer(Vec::with_capacity(16));
    b.u8(R_HASH);
    b.u32(tick);
    b.u64(hash);
    frame(b.0)
}

/// Encode the terminating end record.
pub fn encode_end(reason: EndReason, final_tick: Tick) -> Vec<u8> {
    let mut b = Writer(Vec::with_capacity(8));
    b.u8(R_END);
    b.u8(reason.to_byte());
    b.u32(final_tick);
    frame(b.0)
}

// ---------------------------------------------------------------------------
// Decoding: parse the header, then iterate records
// ---------------------------------------------------------------------------

/// Reader over a fully-loaded replay file. [`ReplayReader::open`] parses the
/// header and positions at the first record; the value then iterates records
/// (`Iterator<Item = Result<ReplayRecord, ReplayError>>`) — the "reader is an
/// iterator over records" shape the brief calls for.
pub struct ReplayReader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Set once a record failed to decode: iteration stops (fused) so a caller
    /// looping to `None` cannot spin on the same bad bytes.
    errored: bool,
}

impl<'a> ReplayReader<'a> {
    /// Parse the header and return it alongside a reader poised at the first
    /// record. Fails (never panics) on bad magic, an unknown version, or a
    /// truncated header.
    pub fn open(buf: &'a [u8]) -> Result<(ReplayHeader, ReplayReader<'a>), ReplayError> {
        let mut r = Reader { buf, pos: 0 };
        let magic = r.take(4)?;
        if magic != REPLAY_MAGIC {
            return Err(ReplayError::BadMagic);
        }
        let replay_version = r.u16()?;
        if replay_version != REPLAY_VERSION {
            return Err(ReplayError::UnsupportedVersion {
                found: replay_version,
            });
        }
        let header_len = r.u32()? as usize;
        let body = r.take(header_len)?;
        // Decode the header body in its own exact-consumption sub-reader, so a
        // header claiming more fields than it holds is caught here.
        let mut hr = Reader { buf: body, pos: 0 };
        let header = ReplayHeader::read_body(&mut hr, replay_version)?;
        hr.done()?;
        Ok((
            header,
            ReplayReader {
                buf,
                pos: r.pos,
                errored: false,
            },
        ))
    }

    /// Decode all records into a `Vec`, stopping at the first error (which is
    /// returned). Convenience for tools that want the whole stream in hand.
    pub fn collect_records(self) -> Result<Vec<ReplayRecord>, ReplayError> {
        let mut out = Vec::new();
        for rec in self {
            out.push(rec?);
        }
        Ok(out)
    }

    /// Decode one framed record body.
    fn decode_body(body: &[u8]) -> Result<ReplayRecord, ReplayError> {
        let mut r = Reader { buf: body, pos: 0 };
        let rec = match r.u8()? {
            R_TICK => {
                let tick = r.u32()?;
                let nseats = r.u8()? as usize;
                if nseats > MAX_TICK_ENTRIES {
                    return Err(ReplayError::Wire(WireError::BadValue(
                        "replay seat entries",
                    )));
                }
                let mut seats: Vec<(SeatId, Vec<Command>)> = Vec::with_capacity(nseats);
                for _ in 0..nseats {
                    let seat = r.u8()?;
                    let m = r.u16()? as usize;
                    if m > MAX_CMDS_PER_TICK {
                        return Err(ReplayError::Wire(WireError::BadValue(
                            "replay command count",
                        )));
                    }
                    let mut cmds = Vec::with_capacity(m.min(256));
                    for _ in 0..m {
                        cmds.push(get_command(&mut r)?);
                    }
                    seats.push((seat, cmds));
                }
                ReplayRecord::Tick {
                    tick,
                    bundle: TickBundle { tick, seats },
                }
            }
            R_HASH => ReplayRecord::Hash {
                tick: r.u32()?,
                hash: r.u64()?,
            },
            R_END => ReplayRecord::End {
                reason: EndReason::from_byte(r.u8()?)?,
                final_tick: r.u32()?,
            },
            t => return Err(ReplayError::UnknownRecord(t)),
        };
        r.done()?;
        Ok(rec)
    }
}

impl Iterator for ReplayReader<'_> {
    type Item = Result<ReplayRecord, ReplayError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.errored || self.pos >= self.buf.len() {
            return None;
        }
        let mut r = Reader {
            buf: self.buf,
            pos: self.pos,
        };
        let len = match r.u32() {
            Ok(n) => n as usize,
            Err(e) => {
                self.errored = true;
                return Some(Err(e.into()));
            }
        };
        let body = match r.take(len) {
            Ok(b) => b,
            Err(e) => {
                self.errored = true;
                return Some(Err(e.into()));
            }
        };
        self.pos = r.pos;
        match ReplayReader::decode_body(body) {
            Ok(rec) => Some(Ok(rec)),
            Err(e) => {
                self.errored = true;
                Some(Err(e))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ReplayTransport: play a recorded stream back through the CommandTransport seam
// ---------------------------------------------------------------------------

/// A [`CommandTransport`] that replays a recorded command stream: `poll(tick)`
/// yields the recorded bundle for that tick (or an empty bundle for the ticks
/// that carried no commands), so the sim re-executes the exact history. Local
/// input is ignored (`submit` is a no-op) and hashes are not compared here —
/// the watchable playback path (M7.23 P3) just needs bundles on schedule;
/// verification re-simulates and checks the hash chain itself.
#[derive(Clone, Debug)]
pub struct ReplayTransport {
    /// Recorded non-empty ticks, keyed by tick (ascending iteration is free).
    bundles: BTreeMap<Tick, TickBundle>,
    /// The seats present in the game (for synthesising empty-tick bundles).
    seats: Vec<SeatId>,
    /// The final recorded tick — playback is complete once past it.
    final_tick: Tick,
    /// The end reason recorded in the stream.
    end_reason: EndReason,
}

impl ReplayTransport {
    /// Build a transport from a parsed header + record stream. Errors are
    /// propagated (a truncated stream yields the decode error).
    pub fn from_reader(
        header: &ReplayHeader,
        reader: ReplayReader,
    ) -> Result<ReplayTransport, ReplayError> {
        let mut bundles = BTreeMap::new();
        let mut final_tick = 0;
        let mut end_reason = EndReason::Quit;
        for rec in reader {
            match rec? {
                ReplayRecord::Tick { tick, bundle } => {
                    final_tick = final_tick.max(tick);
                    bundles.insert(tick, bundle);
                }
                ReplayRecord::Hash { tick, .. } => final_tick = final_tick.max(tick),
                ReplayRecord::End {
                    reason,
                    final_tick: ft,
                } => {
                    end_reason = reason;
                    final_tick = final_tick.max(ft);
                }
            }
        }
        let seats: Vec<SeatId> = if header.seats.is_empty() {
            vec![0]
        } else {
            let mut s: Vec<SeatId> = header.seats.iter().map(|s| s.seat).collect();
            s.sort_unstable();
            s.dedup();
            s
        };
        Ok(ReplayTransport {
            bundles,
            seats,
            final_tick,
            end_reason,
        })
    }

    /// The last tick the recording reached — the shell stops playback here.
    pub fn final_tick(&self) -> Tick {
        self.final_tick
    }

    /// Why the recorded game ended.
    pub fn end_reason(&self) -> EndReason {
        self.end_reason
    }

    /// Whether `tick` is at or before the final recorded tick.
    pub fn has_more(&self, tick: Tick) -> bool {
        tick <= self.final_tick
    }

    /// An empty per-seat bundle for a tick that recorded no commands.
    fn empty_bundle(&self, tick: Tick) -> TickBundle {
        TickBundle {
            tick,
            seats: self.seats.iter().map(|s| (*s, Vec::new())).collect(),
        }
    }
}

impl CommandTransport for ReplayTransport {
    fn submit(&mut self, _cmd: Command) {
        // Playback ignores live input entirely (the brief: input ignored except
        // the shell's quit/pause/speed keys, which never reach the transport).
    }

    fn poll(&mut self, tick: Tick) -> PollResult {
        let bundle = self
            .bundles
            .get(&tick)
            .cloned()
            .unwrap_or_else(|| self.empty_bundle(tick));
        PollResult::Ready(bundle)
    }

    fn report_hash(&mut self, _tick: Tick, _hash: u64) {
        // No peer to compare against during playback.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{GAME_VERSION, PROTOCOL_VERSION};
    use ra_sim::{coords::CellCoord, ProdKind};

    fn sample_header() -> ReplayHeader {
        ReplayHeader {
            replay_version: REPLAY_VERSION,
            game_version: GAME_VERSION,
            protocol_version: PROTOCOL_VERSION,
            scenario: "scm01ea.ini".to_string(),
            seed: 0x1234_5678,
            difficulty: 1,
            credits: 8000,
            catalog_hash: 0xDEAD_BEEF_CAFE_F00D,
            start_millis: 1_700_000_000_000,
            seats: vec![
                ReplaySeat {
                    seat: 1,
                    house: 1,
                    color: 3,
                },
                ReplaySeat {
                    seat: 2,
                    house: 2,
                    color: 5,
                },
            ],
        }
    }

    fn mv(house: u8, x: i32, y: i32) -> Command {
        Command::Move {
            unit: ra_sim::Handle { index: 7, gen: 1 },
            dest: CellCoord::new(x, y),
            house,
        }
    }

    fn cancel(house: u8) -> Command {
        Command::CancelProduction {
            house,
            kind: ProdKind::Building,
        }
    }

    /// Header + a mixed record stream survive a full encode → decode round-trip
    /// byte-for-byte.
    #[test]
    fn full_roundtrip() {
        let header = sample_header();
        let mut file = encode_header(&header);

        let b1 = TickBundle {
            tick: 4,
            seats: vec![(1, vec![mv(1, 10, 12), cancel(1)]), (2, vec![])],
        };
        file.extend_from_slice(&encode_tick(4, &b1));
        file.extend_from_slice(&encode_hash(15, 0xAAAA_BBBB_CCCC_DDDD));
        let b2 = TickBundle {
            tick: 20,
            seats: vec![(1, vec![cancel(1)]), (2, vec![mv(2, 3, 4)])],
        };
        file.extend_from_slice(&encode_tick(20, &b2));
        file.extend_from_slice(&encode_hash(30, 0x0102_0304_0506_0708));
        file.extend_from_slice(&encode_end(EndReason::Victory, 42));

        let (h, reader) = ReplayReader::open(&file).expect("header");
        assert_eq!(h, header);
        let recs = reader.collect_records().expect("records");
        assert_eq!(
            recs,
            vec![
                ReplayRecord::Tick {
                    tick: 4,
                    bundle: b1
                },
                ReplayRecord::Hash {
                    tick: 15,
                    hash: 0xAAAA_BBBB_CCCC_DDDD
                },
                ReplayRecord::Tick {
                    tick: 20,
                    bundle: b2
                },
                ReplayRecord::Hash {
                    tick: 30,
                    hash: 0x0102_0304_0506_0708
                },
                ReplayRecord::End {
                    reason: EndReason::Victory,
                    final_tick: 42
                },
            ]
        );
    }

    /// Bad magic, bad version, and a truncated header are all *errors*, not
    /// panics.
    #[test]
    fn header_rejections() {
        assert_eq!(
            ReplayReader::open(b"nope").err(),
            Some(ReplayError::BadMagic)
        );

        let mut bad_ver = encode_header(&sample_header());
        bad_ver[4] = 0xFF; // clobber the version u16 low byte
        bad_ver[5] = 0xFF;
        assert!(matches!(
            ReplayReader::open(&bad_ver),
            Err(ReplayError::UnsupportedVersion { .. })
        ));

        let full = encode_header(&sample_header());
        for cut in 0..full.len() {
            // Truncated headers must never panic.
            let _ = ReplayReader::open(&full[..cut]);
        }
    }

    /// A truncated record mid-stream yields an error from the iterator and then
    /// fuses to `None`, never panicking or looping.
    #[test]
    fn truncated_record_is_a_fused_error() {
        let header = sample_header();
        let mut file = encode_header(&header);
        let b1 = TickBundle {
            tick: 1,
            seats: vec![(1, vec![mv(1, 1, 1)])],
        };
        file.extend_from_slice(&encode_tick(1, &b1));
        let good_len = file.len();
        file.extend_from_slice(&encode_tick(2, &b1));
        // Chop the last record in half.
        file.truncate(good_len + 3);

        let (_h, mut reader) = ReplayReader::open(&file).unwrap();
        assert!(matches!(reader.next(), Some(Ok(ReplayRecord::Tick { .. }))));
        assert!(matches!(reader.next(), Some(Err(_))));
        assert!(reader.next().is_none(), "iterator must fuse after an error");
    }

    /// Deterministic byte-sweep fuzz: no prefix of a valid file, and no
    /// single-byte mutation of it, may panic the reader.
    #[test]
    fn never_panics_on_corruption() {
        let header = sample_header();
        let mut file = encode_header(&header);
        let b = TickBundle {
            tick: 3,
            seats: vec![(1, vec![mv(1, 2, 3), cancel(1)]), (2, vec![mv(2, 9, 9)])],
        };
        file.extend_from_slice(&encode_tick(3, &b));
        file.extend_from_slice(&encode_hash(15, 0x1234));
        file.extend_from_slice(&encode_end(EndReason::Defeat, 3));

        // Every prefix.
        for cut in 0..=file.len() {
            if let Ok((_h, reader)) = ReplayReader::open(&file[..cut]) {
                let _ = reader.collect_records();
            }
        }
        // Every single-byte flip at a spread of offsets.
        for i in (0..file.len()).step_by(1) {
            let mut m = file.clone();
            m[i] ^= 0xFF;
            if let Ok((_h, reader)) = ReplayReader::open(&m) {
                let _ = reader.collect_records();
            }
        }
    }

    /// The [`ReplayTransport`] yields recorded bundles for recorded ticks and
    /// empty (per-seat) bundles for the gaps, and reports the final tick / end
    /// reason.
    #[test]
    fn transport_plays_recorded_and_gap_ticks() {
        let header = sample_header();
        let mut file = encode_header(&header);
        let b = TickBundle {
            tick: 5,
            seats: vec![(1, vec![mv(1, 4, 4)]), (2, vec![])],
        };
        file.extend_from_slice(&encode_tick(5, &b));
        file.extend_from_slice(&encode_end(EndReason::Quit, 9));

        let (h, reader) = ReplayReader::open(&file).unwrap();
        let mut tp = ReplayTransport::from_reader(&h, reader).unwrap();
        assert_eq!(tp.final_tick(), 9);
        assert_eq!(tp.end_reason(), EndReason::Quit);

        // A recorded tick replays its exact bundle.
        match tp.poll(5) {
            PollResult::Ready(got) => assert_eq!(got, b),
            other => panic!("expected Ready, got {other:?}"),
        }
        // A gap tick is Ready with empty per-seat lists (seats from the header).
        match tp.poll(6) {
            PollResult::Ready(got) => {
                assert_eq!(got.command_count(), 0);
                assert_eq!(
                    got.seats.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
                    vec![1, 2]
                );
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        // submit is ignored; poll still returns the recorded/gap bundle.
        tp.submit(cancel(1));
        assert!(matches!(tp.poll(5), PollResult::Ready(_)));
    }
}
