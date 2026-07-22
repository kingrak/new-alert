//! M8-C P0: byte-exact **world snapshots** — a hand-rolled versioned binary
//! serialization of the complete simulation state, used to *resync* a desynced
//! LAN game from an authoritative peer (DESIGN.md §3.6, §4.6) instead of tearing
//! the match down.
//!
//! # Why hand-rolled (design decision)
//!
//! The wire layer ([`ra_net::wire`]) already establishes a discipline this module
//! extends verbatim: little-endian fixed-width integers, length-prefixed
//! variable data, a version byte, and a decoder that **never panics on malformed
//! input** — every read is bounds-checked, every enum tag validated, every count
//! capped against the bytes actually remaining, and the whole buffer must be
//! consumed exactly. `ra-sim` is deliberately dependency-free (its determinism
//! contract, §4.2, is easiest to audit with zero third-party code in the graph);
//! a snapshot format is exactly the kind of adversarially-decoded, size-bounded,
//! version-gated byte stream that discipline was built for, so we keep it rather
//! than pull `serde` + a binary format crate into the pure core. DESIGN §3.6's
//! observation still holds — the problem is *tractable at all* only because
//! `World` holds no pointers, just generational handles (plain numbers) — we
//! simply realise the versioned snapshot it describes with the project's own
//! never-panic codec instead of a derive.
//!
//! # What is and isn't shipped
//!
//! A snapshot carries the entire **dynamic** sim state (both entity arenas incl.
//! their free-lists and generation counters, houses, ore overlay, shroud, RNG,
//! tick, AI controllers, campaign scripting, superweapons — everything a
//! subsequent tick can read). It does **not** ship static shared content: the
//! build [`crate::Catalog`] and the map's static terrain
//! ([`crate::Passability`]) are identical on both peers, so the snapshot only
//! records their **content hashes** and the loader supplies the real values —
//! a mismatch (wrong map / ruleset) is rejected cleanly rather than resumed into
//! a divergent world. The dynamic building-occupancy layer of the passability
//! grid is a cache re-derived from the (shipped) buildings on load.
//!
//! # Envelope
//!
//! ```text
//! offset size field
//! 0      4    magic  0x4E53_4152  ("RASN", LE)
//! 4      2    SNAPSHOT_VERSION
//! 6      4    GAME_BUILD           (same build must produce it)
//! 10     8    catalog content hash
//! 18     8    map (static passability) content hash
//! 26     4    tick (informational; also the authoritative resume tick)
//! 30     ..   payload (the dynamic World state)
//! ```

/// Snapshot magic: little-endian `0x4E53_4152` = the bytes `"RASN"`.
pub const SNAPSHOT_MAGIC: u32 = 0x4E53_4152;

/// Snapshot format version. Bump on ANY change to the byte layout below; a peer
/// that decodes a differently-versioned snapshot rejects it
/// ([`SnapError::Version`]) rather than misreading it.
pub const SNAPSHOT_VERSION: u16 = 1;

/// The game build that produced the snapshot (must match `ra_net::wire`'s
/// `GAME_VERSION`; two builds with the same protocol can still diverge in sim
/// behaviour, so a snapshot only loads into the identical build). Packed
/// `major<<16 | minor<<8 | patch`.
pub const GAME_BUILD: u32 = 0x0000_0100; // 0.1.0

/// Hard cap on a decoded snapshot, so a malformed length can never trigger an
/// unbounded allocation. Generous for a real mid-game world (~tens of KB) while
/// bounding worst-case memory. Chunked transfer stays well under this.
pub const MAX_SNAPSHOT: usize = 16 * 1024 * 1024;

/// A snapshot decode/verify failure. Decoding is total: any malformed,
/// truncated, or mismatched input produces one of these — never a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapError {
    /// Ran off the end of the buffer (a length claimed more bytes than exist).
    Truncated,
    /// Wrong magic — not a snapshot at all.
    BadMagic,
    /// Snapshot format version this build does not understand.
    Version,
    /// A different game build produced it.
    Build,
    /// An enum discriminant / tag byte outside its valid set.
    BadTag(&'static str),
    /// A length/count exceeded its cap (would over-allocate).
    TooLong(&'static str),
    /// The loader's catalog does not match the one the snapshot was made against.
    CatalogMismatch,
    /// The loader's map (static passability) does not match the snapshot's.
    MapMismatch,
    /// Bytes remained after a complete decode (exact-consumption violation).
    TrailingBytes,
}

/// A little-endian byte sink mirroring `ra_net::wire`'s encoder discipline.
#[derive(Default)]
pub struct SnapWriter {
    buf: Vec<u8>,
}

impl SnapWriter {
    /// A fresh empty writer.
    pub fn new() -> SnapWriter {
        SnapWriter { buf: Vec::new() }
    }

    /// Consume the writer, yielding the encoded bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    /// Current length (used by tests / chunking).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Append one byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    /// Append a `bool` as one byte (`0`/`1`).
    pub fn boolean(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    /// Append a little-endian `u16`.
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Append a little-endian `u32`.
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Append a little-endian `i32`.
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Append a little-endian `u64`.
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// Append a length-prefixed (`u32`) run of raw bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    /// Append a length-prefixed (`u32`) UTF-8 string.
    pub fn string(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    /// Write a `u32` length prefix, then `f` for each item.
    pub fn seq<T, F: FnMut(&mut SnapWriter, &T)>(&mut self, items: &[T], mut f: F) {
        self.u32(items.len() as u32);
        for it in items {
            f(self, it);
        }
    }
    /// Write an `Option` as a presence byte then (if present) `f`.
    pub fn option<T, F: FnMut(&mut SnapWriter, &T)>(&mut self, o: &Option<T>, mut f: F) {
        match o {
            Some(v) => {
                self.u8(1);
                f(self, v);
            }
            None => self.u8(0),
        }
    }
}

/// A bounds-checked little-endian cursor. Every accessor returns `Result`, so a
/// decoder built on it cannot panic on adversarial input.
pub struct SnapReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> SnapReader<'a> {
    /// Wrap a byte slice.
    pub fn new(buf: &'a [u8]) -> SnapReader<'a> {
        SnapReader { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Require the buffer to be fully consumed (exact-consumption invariant).
    pub fn finish(self) -> Result<(), SnapError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(SnapError::TrailingBytes)
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], SnapError> {
        let end = self.pos.checked_add(n).ok_or(SnapError::Truncated)?;
        if end > self.buf.len() {
            return Err(SnapError::Truncated);
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read one byte.
    pub fn u8(&mut self) -> Result<u8, SnapError> {
        Ok(self.take(1)?[0])
    }
    /// Read a `bool` (any non-zero is `true`; strict `0`/`1` not required — but
    /// callers that care can validate).
    pub fn boolean(&mut self) -> Result<bool, SnapError> {
        Ok(self.u8()? != 0)
    }
    /// Read a little-endian `u16`.
    pub fn u16(&mut self) -> Result<u16, SnapError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    /// Read a little-endian `u32`.
    pub fn u32(&mut self) -> Result<u32, SnapError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// Read a little-endian `i32`.
    pub fn i32(&mut self) -> Result<i32, SnapError> {
        Ok(self.u32()? as i32)
    }
    /// Read a little-endian `u64`.
    pub fn u64(&mut self) -> Result<u64, SnapError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a `u32`-prefixed count, validating it against the bytes remaining
    /// (each element is at least one byte, so a count larger than `remaining`
    /// cannot be honest) so a corrupt length can never over-allocate. `what`
    /// names the field for diagnostics.
    pub fn count(&mut self, what: &'static str) -> Result<usize, SnapError> {
        let n = self.u32()? as usize;
        if n > self.remaining() {
            return Err(SnapError::TooLong(what));
        }
        Ok(n)
    }

    /// Read a length-prefixed byte run.
    pub fn bytes(&mut self, what: &'static str) -> Result<&'a [u8], SnapError> {
        let n = self.count(what)?;
        self.take(n)
    }

    /// Read a length-prefixed UTF-8 string (invalid UTF-8 → [`SnapError::BadTag`]).
    pub fn string(&mut self, what: &'static str) -> Result<String, SnapError> {
        let b = self.bytes(what)?;
        core::str::from_utf8(b)
            .map(|s| s.to_owned())
            .map_err(|_| SnapError::BadTag(what))
    }

    /// Read a `u32`-prefixed sequence, decoding each element with `f`. Capacity
    /// is reserved conservatively (`count` already bounded the length against the
    /// buffer).
    pub fn seq<T, F: FnMut(&mut SnapReader<'a>) -> Result<T, SnapError>>(
        &mut self,
        what: &'static str,
        mut f: F,
    ) -> Result<Vec<T>, SnapError> {
        let n = self.count(what)?;
        let mut out = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }

    /// Read an `Option` (presence byte then, if `1`, `f`).
    pub fn option<T, F: FnMut(&mut SnapReader<'a>) -> Result<T, SnapError>>(
        &mut self,
        what: &'static str,
        mut f: F,
    ) -> Result<Option<T>, SnapError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            _ => Err(SnapError::BadTag(what)),
        }
    }
}

// -------------------------------------------------------------------------
// Leaf / pub-field type serializers.
//
// These types expose their fields, so their (de)serialization lives here next
// to the codec. Types with private fields (Arena, World, Shroud, OreField,
// AiPlayer) carry their own `snap_write`/`snap_read` in their own module.
// -------------------------------------------------------------------------

use crate::arena::Handle;
use crate::building::Building;
use crate::bullet::Bullet;
use crate::campaign::{
    SpawnProto, TActionDef, TEventDef, TeamClass, TeamMission, TeamType, TriggerState, TriggerType,
};
use crate::combat::{Target, WarheadProfile, WeaponProfile, ARMOR_COUNT};
use crate::coords::{CellCoord, Facing, Lepton, Locomotor, WorldCoord};
use crate::house::{BuildItem, Handicap, House, Production};
use crate::superweapon::{NukeStrike, SuperKind, SuperWeapon};
use crate::unit::{
    AirState, HarvStatus, HarvestState, Mission, MoveStats, Passenger, Unit, UnitKind,
};

impl Handle {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.u32(self.index);
        w.u32(self.gen);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Handle, SnapError> {
        Ok(Handle {
            index: r.u32()?,
            gen: r.u32()?,
        })
    }
}

impl CellCoord {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.x);
        w.i32(self.y);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<CellCoord, SnapError> {
        Ok(CellCoord {
            x: r.i32()?,
            y: r.i32()?,
        })
    }
}

impl WorldCoord {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.x.0);
        w.i32(self.y.0);
    }
    fn snap_read(r: &mut SnapReader) -> Result<WorldCoord, SnapError> {
        Ok(WorldCoord {
            x: Lepton(r.i32()?),
            y: Lepton(r.i32()?),
        })
    }
}

impl Locomotor {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u8(match self {
            Locomotor::Foot => 0,
            Locomotor::Track => 1,
            Locomotor::Wheel => 2,
            Locomotor::Air => 3,
            Locomotor::Water => 4,
        });
    }
    fn snap_read(r: &mut SnapReader) -> Result<Locomotor, SnapError> {
        Ok(match r.u8()? {
            0 => Locomotor::Foot,
            1 => Locomotor::Track,
            2 => Locomotor::Wheel,
            3 => Locomotor::Air,
            4 => Locomotor::Water,
            _ => return Err(SnapError::BadTag("Locomotor")),
        })
    }
}

impl MoveStats {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.max_speed);
        w.u8(self.rot);
    }
    fn snap_read(r: &mut SnapReader) -> Result<MoveStats, SnapError> {
        Ok(MoveStats {
            max_speed: r.i32()?,
            rot: r.u8()?,
        })
    }
}

impl WarheadProfile {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.spread);
        for v in &self.verses {
            w.i32(*v);
        }
    }
    fn snap_read(r: &mut SnapReader) -> Result<WarheadProfile, SnapError> {
        let spread = r.i32()?;
        let mut verses = [0i32; ARMOR_COUNT];
        for v in verses.iter_mut() {
            *v = r.i32()?;
        }
        Ok(WarheadProfile { spread, verses })
    }
}

impl WeaponProfile {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.damage);
        w.u16(self.rof);
        w.i32(self.range);
        w.i32(self.proj_speed);
        w.u8(self.proj_rot);
        w.boolean(self.invisible);
        w.boolean(self.instant);
        self.warhead.snap_write(w);
        w.boolean(self.warhead_ap);
        w.boolean(self.arcing);
        w.i32(self.ballistic_scatter);
        w.i32(self.homing_scatter);
        w.i32(self.min_damage);
        w.i32(self.max_damage);
    }
    fn snap_read(r: &mut SnapReader) -> Result<WeaponProfile, SnapError> {
        Ok(WeaponProfile {
            damage: r.i32()?,
            rof: r.u16()?,
            range: r.i32()?,
            proj_speed: r.i32()?,
            proj_rot: r.u8()?,
            invisible: r.boolean()?,
            instant: r.boolean()?,
            warhead: WarheadProfile::snap_read(r)?,
            warhead_ap: r.boolean()?,
            arcing: r.boolean()?,
            ballistic_scatter: r.i32()?,
            homing_scatter: r.i32()?,
            min_damage: r.i32()?,
            max_damage: r.i32()?,
        })
    }
}

impl Target {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        match self {
            Target::Unit(h) => {
                w.u8(0);
                h.snap_write(w);
            }
            Target::Building(h) => {
                w.u8(1);
                h.snap_write(w);
            }
            Target::Cell(c) => {
                w.u8(2);
                c.snap_write(w);
            }
        }
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Target, SnapError> {
        Ok(match r.u8()? {
            0 => Target::Unit(Handle::snap_read(r)?),
            1 => Target::Building(Handle::snap_read(r)?),
            2 => Target::Cell(CellCoord::snap_read(r)?),
            _ => return Err(SnapError::BadTag("Target")),
        })
    }
}

impl HarvestState {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u8(match self.status {
            HarvStatus::Looking => 0,
            HarvStatus::Harvesting => 1,
            HarvStatus::FindHome => 2,
            HarvStatus::HeadingHome => 3,
            HarvStatus::Unloading => 4,
            HarvStatus::Idle => 5,
        });
        w.u16(self.cargo);
        w.u16(self.gold);
        w.u16(self.gems);
        w.u16(self.timer);
        w.option(&self.home, |w, h| h.snap_write(w));
        w.u8(self.retarget);
    }
    fn snap_read(r: &mut SnapReader) -> Result<HarvestState, SnapError> {
        Ok(HarvestState {
            status: match r.u8()? {
                0 => HarvStatus::Looking,
                1 => HarvStatus::Harvesting,
                2 => HarvStatus::FindHome,
                3 => HarvStatus::HeadingHome,
                4 => HarvStatus::Unloading,
                5 => HarvStatus::Idle,
                _ => return Err(SnapError::BadTag("HarvStatus")),
            },
            cargo: r.u16()?,
            gold: r.u16()?,
            gems: r.u16()?,
            timer: r.u16()?,
            home: r.option("harvest.home", Handle::snap_read)?,
            retarget: r.u8()?,
        })
    }
}

fn write_mission(w: &mut SnapWriter, m: Mission) {
    w.u8(match m {
        Mission::Guard => 0,
        Mission::AreaGuard => 1,
        Mission::Hunt => 2,
        Mission::Sleep => 3,
        Mission::Sticky => 4,
        Mission::Harvest => 5,
    });
}
fn read_mission(r: &mut SnapReader) -> Result<Mission, SnapError> {
    Ok(match r.u8()? {
        0 => Mission::Guard,
        1 => Mission::AreaGuard,
        2 => Mission::Hunt,
        3 => Mission::Sleep,
        4 => Mission::Sticky,
        5 => Mission::Harvest,
        _ => return Err(SnapError::BadTag("Mission")),
    })
}

impl Passenger {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u32(self.type_id);
        w.u8(self.house);
        w.u16(self.health);
        w.u16(self.max_health);
        self.stats.snap_write(w);
        w.u8(self.armor);
        w.option(&self.weapon, |w, p| p.snap_write(w));
        w.option(&self.secondary, |w, p| p.snap_write(w));
        w.boolean(self.has_turret);
        w.u8(self.sight);
        w.boolean(self.is_infantry);
        write_mission(w, self.mission);
    }
    fn snap_read(r: &mut SnapReader) -> Result<Passenger, SnapError> {
        Ok(Passenger {
            type_id: r.u32()?,
            house: r.u8()?,
            health: r.u16()?,
            max_health: r.u16()?,
            stats: MoveStats::snap_read(r)?,
            armor: r.u8()?,
            weapon: r.option("passenger.weapon", WeaponProfile::snap_read)?,
            secondary: r.option("passenger.secondary", WeaponProfile::snap_read)?,
            has_turret: r.boolean()?,
            sight: r.u8()?,
            is_infantry: r.boolean()?,
            mission: read_mission(r)?,
        })
    }
}

impl Unit {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.u32(self.type_id);
        w.u8(self.house);
        self.coord.snap_write(w);
        w.u8(self.facing.0);
        w.u16(self.health);
        w.u16(self.max_health);
        self.stats.snap_write(w);
        w.seq(&self.path, |w, c| c.snap_write(w));
        w.option(&self.dest, |w, c| c.snap_write(w));
        w.u8(self.armor);
        w.option(&self.weapon, |w, p| p.snap_write(w));
        w.option(&self.secondary, |w, p| p.snap_write(w));
        w.boolean(self.has_turret);
        w.u8(self.turret_facing.0);
        w.option(&self.target, |w, t| t.snap_write(w));
        w.u16(self.arm);
        w.boolean(self.is_harvester);
        self.harvest.snap_write(w);
        w.u8(self.sight);
        w.u8(match self.kind {
            UnitKind::Vehicle => 0,
            UnitKind::Infantry => 1,
            UnitKind::Aircraft => 2,
        });
        self.locomotor.snap_write(w);
        w.u8(self.sub_cell);
        w.option(&self.trigger, |w, t| w.u16(*t));
        w.boolean(self.is_civ_evac);
        w.boolean(self.hunt);
        write_mission(w, self.mission);
        w.option(&self.guard_post, |w, c| c.snap_write(w));
        w.boolean(self.guard_target);
        w.u8(self.capacity);
        w.seq(&self.cargo, |w, p| p.snap_write(w));
        w.option(&self.board_target, |w, h| h.snap_write(w));
        w.option(&self.unload_at, |w, c| c.snap_write(w));
        w.i32(self.altitude);
        w.u16(self.ammo);
        w.u16(self.max_ammo);
        w.u8(match self.air_state {
            AirState::Idle => 0,
            AirState::Attack => 1,
            AirState::Returning => 2,
            AirState::Rearming => 3,
        });
        w.option(&self.home, |w, h| h.snap_write(w));
        w.u16(self.rearm_timer);
        w.boolean(self.is_submarine);
        w.boolean(self.is_detector);
        w.boolean(self.submerged);
        w.u16(self.recloak);
        w.boolean(self.spy);
        w.boolean(self.thief);
        w.boolean(self.bomber);
        w.boolean(self.is_canine);
        w.boolean(self.disguised);
        w.u16(self.iron_curtain);
        w.u16(self.reroute_delay);
        w.u8(self.reroute_fails);
    }

    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Unit, SnapError> {
        Ok(Unit {
            type_id: r.u32()?,
            house: r.u8()?,
            coord: WorldCoord::snap_read(r)?,
            facing: Facing(r.u8()?),
            health: r.u16()?,
            max_health: r.u16()?,
            stats: MoveStats::snap_read(r)?,
            path: r.seq("unit.path", CellCoord::snap_read)?,
            dest: r.option("unit.dest", CellCoord::snap_read)?,
            armor: r.u8()?,
            weapon: r.option("unit.weapon", WeaponProfile::snap_read)?,
            secondary: r.option("unit.secondary", WeaponProfile::snap_read)?,
            has_turret: r.boolean()?,
            turret_facing: Facing(r.u8()?),
            target: r.option("unit.target", Target::snap_read)?,
            arm: r.u16()?,
            is_harvester: r.boolean()?,
            harvest: HarvestState::snap_read(r)?,
            sight: r.u8()?,
            kind: match r.u8()? {
                0 => UnitKind::Vehicle,
                1 => UnitKind::Infantry,
                2 => UnitKind::Aircraft,
                _ => return Err(SnapError::BadTag("UnitKind")),
            },
            locomotor: Locomotor::snap_read(r)?,
            sub_cell: r.u8()?,
            trigger: r.option("unit.trigger", |r| r.u16())?,
            is_civ_evac: r.boolean()?,
            hunt: r.boolean()?,
            mission: read_mission(r)?,
            guard_post: r.option("unit.guard_post", CellCoord::snap_read)?,
            guard_target: r.boolean()?,
            capacity: r.u8()?,
            cargo: r.seq("unit.cargo", Passenger::snap_read)?,
            board_target: r.option("unit.board_target", Handle::snap_read)?,
            unload_at: r.option("unit.unload_at", CellCoord::snap_read)?,
            altitude: r.i32()?,
            ammo: r.u16()?,
            max_ammo: r.u16()?,
            air_state: match r.u8()? {
                0 => AirState::Idle,
                1 => AirState::Attack,
                2 => AirState::Returning,
                3 => AirState::Rearming,
                _ => return Err(SnapError::BadTag("AirState")),
            },
            home: r.option("unit.home", Handle::snap_read)?,
            rearm_timer: r.u16()?,
            is_submarine: r.boolean()?,
            is_detector: r.boolean()?,
            submerged: r.boolean()?,
            recloak: r.u16()?,
            spy: r.boolean()?,
            thief: r.boolean()?,
            bomber: r.boolean()?,
            is_canine: r.boolean()?,
            disguised: r.boolean()?,
            iron_curtain: r.u16()?,
            reroute_delay: r.u16()?,
            reroute_fails: r.u8()?,
        })
    }
}

impl Building {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.u32(self.type_id);
        w.u8(self.house);
        self.cell.snap_write(w);
        w.u8(self.foot_w);
        w.u8(self.foot_h);
        w.u16(self.health);
        w.u16(self.max_health);
        w.u8(self.armor);
        w.u8(self.sight);
        w.i32(self.cost);
        w.i32(self.power);
        w.boolean(self.is_refinery);
        w.boolean(self.is_construction_yard);
        w.boolean(self.is_war_factory);
        w.boolean(self.is_barracks);
        w.option(&self.weapon, |w, p| p.snap_write(w));
        w.boolean(self.has_turret);
        w.boolean(self.charges);
        w.u8(self.turret_facing.0);
        w.u16(self.arm);
        w.u16(self.charge);
        w.option(&self.target, |w, t| t.snap_write(w));
        w.boolean(self.is_wall);
        w.i32(self.storage);
        w.boolean(self.is_repairing);
        w.option(&self.trigger, |w, t| w.u16(*t));
        w.u16(self.c4_fuse);
        w.u8(self.c4_by);
        w.u16(self.iron_curtain);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Building, SnapError> {
        Ok(Building {
            type_id: r.u32()?,
            house: r.u8()?,
            cell: CellCoord::snap_read(r)?,
            foot_w: r.u8()?,
            foot_h: r.u8()?,
            health: r.u16()?,
            max_health: r.u16()?,
            armor: r.u8()?,
            sight: r.u8()?,
            cost: r.i32()?,
            power: r.i32()?,
            is_refinery: r.boolean()?,
            is_construction_yard: r.boolean()?,
            is_war_factory: r.boolean()?,
            is_barracks: r.boolean()?,
            weapon: r.option("building.weapon", WeaponProfile::snap_read)?,
            has_turret: r.boolean()?,
            charges: r.boolean()?,
            turret_facing: Facing(r.u8()?),
            arm: r.u16()?,
            charge: r.u16()?,
            target: r.option("building.target", Target::snap_read)?,
            is_wall: r.boolean()?,
            storage: r.i32()?,
            is_repairing: r.boolean()?,
            trigger: r.option("building.trigger", |r| r.u16())?,
            c4_fuse: r.u16()?,
            c4_by: r.u8()?,
            iron_curtain: r.u16()?,
        })
    }
}

impl Bullet {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        self.pos.snap_write(w);
        self.impact.snap_write(w);
        self.target.snap_write(w);
        w.i32(self.speed);
        w.u8(self.facing.0);
        w.i32(self.damage);
        self.warhead.snap_write(w);
        w.i32(self.min_damage);
        w.i32(self.max_damage);
        w.u8(self.source_house);
        self.source_unit.snap_write(w);
        w.boolean(self.instant);
        w.boolean(self.invisible);
        w.boolean(self.arcing);
        w.i32(self.height);
        w.i32(self.riser);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Bullet, SnapError> {
        Ok(Bullet {
            pos: WorldCoord::snap_read(r)?,
            impact: WorldCoord::snap_read(r)?,
            target: Target::snap_read(r)?,
            speed: r.i32()?,
            facing: Facing(r.u8()?),
            damage: r.i32()?,
            warhead: WarheadProfile::snap_read(r)?,
            min_damage: r.i32()?,
            max_damage: r.i32()?,
            source_house: r.u8()?,
            source_unit: Handle::snap_read(r)?,
            instant: r.boolean()?,
            invisible: r.boolean()?,
            arcing: r.boolean()?,
            height: r.i32()?,
            riser: r.i32()?,
        })
    }
}

impl Handicap {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.firepower);
        w.i32(self.armor);
        w.i32(self.rof);
        w.i32(self.groundspeed);
        w.i32(self.cost);
        w.i32(self.build_time);
    }
    fn snap_read(r: &mut SnapReader) -> Result<Handicap, SnapError> {
        Ok(Handicap {
            firepower: r.i32()?,
            armor: r.i32()?,
            rof: r.i32()?,
            groundspeed: r.i32()?,
            cost: r.i32()?,
            build_time: r.i32()?,
        })
    }
}

fn write_build_item(w: &mut SnapWriter, it: BuildItem) {
    match it {
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
fn read_build_item(r: &mut SnapReader) -> Result<BuildItem, SnapError> {
    Ok(match r.u8()? {
        0 => BuildItem::Building(r.u32()?),
        1 => BuildItem::Unit(r.u32()?),
        _ => return Err(SnapError::BadTag("BuildItem")),
    })
}

impl Production {
    fn snap_write(&self, w: &mut SnapWriter) {
        write_build_item(w, self.item);
        w.i32(self.cost);
        w.i32(self.total_ticks);
        w.i32(self.progress);
        w.i32(self.spent);
        w.boolean(self.done);
        w.boolean(self.paused);
    }
    fn snap_read(r: &mut SnapReader) -> Result<Production, SnapError> {
        Ok(Production {
            item: read_build_item(r)?,
            cost: r.i32()?,
            total_ticks: r.i32()?,
            progress: r.i32()?,
            spent: r.i32()?,
            done: r.boolean()?,
            paused: r.boolean()?,
        })
    }
}

impl House {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.credits);
        w.i32(self.tiberium);
        w.i32(self.power_output);
        w.i32(self.power_drain);
        w.seq(&self.building_counts, |w, c| w.u16(*c));
        w.option(&self.building_prod, |w, p| p.snap_write(w));
        w.option(&self.unit_prod, |w, p| p.snap_write(w));
        w.option(&self.infantry_prod, |w, p| p.snap_write(w));
        w.option(&self.ready_building, |w, id| w.u32(*id));
        self.handicap.snap_write(w);
        w.seq(&self.units_killed_by, |w, v| w.u32(*v));
        w.seq(&self.buildings_killed_by, |w, v| w.u32(*v));
        w.option(&self.last_attacker, |w, h| w.u8(*h));
        w.i32(self.iq);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<House, SnapError> {
        Ok(House {
            credits: r.i32()?,
            tiberium: r.i32()?,
            power_output: r.i32()?,
            power_drain: r.i32()?,
            building_counts: r.seq("house.building_counts", |r| r.u16())?,
            building_prod: r.option("house.building_prod", Production::snap_read)?,
            unit_prod: r.option("house.unit_prod", Production::snap_read)?,
            infantry_prod: r.option("house.infantry_prod", Production::snap_read)?,
            ready_building: r.option("house.ready_building", |r| r.u32())?,
            handicap: Handicap::snap_read(r)?,
            units_killed_by: r.seq("house.units_killed_by", |r| r.u32())?,
            buildings_killed_by: r.seq("house.buildings_killed_by", |r| r.u32())?,
            last_attacker: r.option("house.last_attacker", |r| r.u8())?,
            iq: r.i32()?,
        })
    }
}

impl SuperWeapon {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.u8(self.house);
        w.u8(match self.kind {
            SuperKind::Nuclear => 0,
            SuperKind::IronCurtain => 1,
            SuperKind::Chronosphere => 2,
        });
        w.i32(self.recharge);
        w.i32(self.control);
        w.boolean(self.ready);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<SuperWeapon, SnapError> {
        Ok(SuperWeapon {
            house: r.u8()?,
            kind: match r.u8()? {
                0 => SuperKind::Nuclear,
                1 => SuperKind::IronCurtain,
                2 => SuperKind::Chronosphere,
                _ => return Err(SnapError::BadTag("SuperKind")),
            },
            recharge: r.i32()?,
            control: r.i32()?,
            ready: r.boolean()?,
        })
    }
}

impl NukeStrike {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        self.cell.snap_write(w);
        w.u8(self.house);
        w.u16(self.timer);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<NukeStrike, SnapError> {
        Ok(NukeStrike {
            cell: CellCoord::snap_read(r)?,
            house: r.u8()?,
            timer: r.u16()?,
        })
    }
}

// ------------------------- campaign types -------------------------

impl TEventDef {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u8(self.code);
        w.i32(self.team);
        w.i32(self.data);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TEventDef, SnapError> {
        Ok(TEventDef {
            code: r.u8()?,
            team: r.i32()?,
            data: r.i32()?,
        })
    }
}

impl TActionDef {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u8(self.code);
        w.i32(self.team);
        w.i32(self.trigger);
        w.i32(self.data);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TActionDef, SnapError> {
        Ok(TActionDef {
            code: r.u8()?,
            team: r.i32()?,
            trigger: r.i32()?,
            data: r.i32()?,
        })
    }
}

impl TriggerType {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.string(&self.name);
        w.u8(self.persist);
        w.i32(self.house);
        w.u8(self.event_ctrl);
        w.u8(self.action_ctrl);
        self.e1.snap_write(w);
        self.e2.snap_write(w);
        self.a1.snap_write(w);
        self.a2.snap_write(w);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TriggerType, SnapError> {
        Ok(TriggerType {
            name: r.string("trigger.name")?,
            persist: r.u8()?,
            house: r.i32()?,
            event_ctrl: r.u8()?,
            action_ctrl: r.u8()?,
            e1: TEventDef::snap_read(r)?,
            e2: TEventDef::snap_read(r)?,
            a1: TActionDef::snap_read(r)?,
            a2: TActionDef::snap_read(r)?,
        })
    }
}

impl SpawnProto {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.u32(self.type_id);
        w.u16(self.max_health);
        self.stats.snap_write(w);
        w.u8(self.armor);
        w.option(&self.weapon, |w, p| p.snap_write(w));
        w.option(&self.secondary, |w, p| p.snap_write(w));
        w.boolean(self.has_turret);
        w.u8(self.sight);
        w.boolean(self.is_infantry);
        w.boolean(self.is_harvester);
        w.boolean(self.is_civ_evac);
        w.u8(self.passengers);
    }
    fn snap_read(r: &mut SnapReader) -> Result<SpawnProto, SnapError> {
        Ok(SpawnProto {
            type_id: r.u32()?,
            max_health: r.u16()?,
            stats: MoveStats::snap_read(r)?,
            armor: r.u8()?,
            weapon: r.option("spawnproto.weapon", WeaponProfile::snap_read)?,
            secondary: r.option("spawnproto.secondary", WeaponProfile::snap_read)?,
            has_turret: r.boolean()?,
            sight: r.u8()?,
            is_infantry: r.boolean()?,
            is_harvester: r.boolean()?,
            is_civ_evac: r.boolean()?,
            passengers: r.u8()?,
        })
    }
}

impl TeamClass {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.option(&self.proto, |w, p| p.snap_write(w));
        w.u16(self.count);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TeamClass, SnapError> {
        Ok(TeamClass {
            proto: r.option("teamclass.proto", SpawnProto::snap_read)?,
            count: r.u16()?,
        })
    }
}

impl TeamMission {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.i32(self.code);
        w.i32(self.arg);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TeamMission, SnapError> {
        Ok(TeamMission {
            code: r.i32()?,
            arg: r.i32()?,
        })
    }
}

impl TeamType {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.string(&self.name);
        w.i32(self.house);
        w.u32(self.flags);
        w.i32(self.recruit);
        w.i32(self.init_num);
        w.i32(self.max_allowed);
        w.i32(self.origin);
        w.i32(self.trigger);
        w.seq(&self.classes, |w, c| c.snap_write(w));
        w.seq(&self.missions, |w, m| m.snap_write(w));
    }
    fn snap_read(r: &mut SnapReader) -> Result<TeamType, SnapError> {
        Ok(TeamType {
            name: r.string("teamtype.name")?,
            house: r.i32()?,
            flags: r.u32()?,
            recruit: r.i32()?,
            init_num: r.i32()?,
            max_allowed: r.i32()?,
            origin: r.i32()?,
            trigger: r.i32()?,
            classes: r.seq("teamtype.classes", TeamClass::snap_read)?,
            missions: r.seq("teamtype.missions", TeamMission::snap_read)?,
        })
    }
}

impl TriggerState {
    fn snap_write(&self, w: &mut SnapWriter) {
        w.boolean(self.sprung);
        w.boolean(self.destroyed);
        w.i32(self.e1_timer);
        w.i32(self.e2_timer);
        w.i32(self.carriers);
        w.i32(self.carriers_init);
        w.boolean(self.any_destroyed);
        w.boolean(self.any_attacked);
    }
    fn snap_read(r: &mut SnapReader) -> Result<TriggerState, SnapError> {
        Ok(TriggerState {
            sprung: r.boolean()?,
            destroyed: r.boolean()?,
            e1_timer: r.i32()?,
            e2_timer: r.i32()?,
            carriers: r.i32()?,
            carriers_init: r.i32()?,
            any_destroyed: r.boolean()?,
            any_attacked: r.boolean()?,
        })
    }
}

use crate::campaign::{Campaign, EnemyActivation};

impl Campaign {
    /// Full-fidelity serialization — including the cosmetic client-drained
    /// outputs (`reveal_cells`/`pending_texts`/`pending_speech`), so a resumed
    /// world is byte-identical for the client too, not merely hash-identical.
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.seq(&self.triggers, |w, t| t.snap_write(w));
        w.seq(&self.teamtypes, |w, t| t.snap_write(w));
        w.seq(&self.waypoints, |w, v| w.i32(*v));
        w.seq(&self.globals, |w, g| w.boolean(*g));
        w.seq(&self.cell_triggers, |w, (c, t)| {
            w.u32(*c);
            w.u16(*t);
        });
        w.seq(&self.state, |w, s| s.snap_write(w));
        w.boolean(self.started);
        w.option(&self.mission_timer, |w, t| w.i32(*t));
        w.seq(&self.evac_cells, |w, c| c.snap_write(w));
        w.seq(&self.civ_evacuated, |w, e| w.boolean(*e));
        w.boolean(self.reveal_all);
        w.seq(&self.reveal_cells, |w, c| w.u32(*c));
        w.seq(&self.pending_texts, |w, t| w.i32(*t));
        w.seq(&self.pending_speech, |w, s| w.i32(*s));
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<Campaign, SnapError> {
        Ok(Campaign {
            triggers: r.seq("campaign.triggers", TriggerType::snap_read)?,
            teamtypes: r.seq("campaign.teamtypes", TeamType::snap_read)?,
            waypoints: r.seq("campaign.waypoints", |r| r.i32())?,
            globals: r.seq("campaign.globals", |r| r.boolean())?,
            cell_triggers: r.seq("campaign.cell_triggers", |r| Ok((r.u32()?, r.u16()?)))?,
            state: r.seq("campaign.state", TriggerState::snap_read)?,
            started: r.boolean()?,
            mission_timer: r.option("campaign.mission_timer", |r| r.i32())?,
            evac_cells: r.seq("campaign.evac_cells", CellCoord::snap_read)?,
            civ_evacuated: r.seq("campaign.civ_evacuated", |r| r.boolean())?,
            reveal_all: r.boolean()?,
            reveal_cells: r.seq("campaign.reveal_cells", |r| r.u32())?,
            pending_texts: r.seq("campaign.pending_texts", |r| r.i32())?,
            pending_speech: r.seq("campaign.pending_speech", |r| r.i32())?,
        })
    }
}

impl EnemyActivation {
    pub(crate) fn snap_write(&self, w: &mut SnapWriter) {
        w.seq(&self.alerted, |w, a| w.boolean(*a));
        w.seq(&self.alert_timer, |w, t| w.i32(*t));
        w.seq(&self.production, |w, p| w.boolean(*p));
        w.u8(self.base_house);
        w.seq(&self.base_nodes, |w, (id, c)| {
            w.u32(*id);
            c.snap_write(w);
        });
        w.i32(self.tech_level);
    }
    pub(crate) fn snap_read(r: &mut SnapReader) -> Result<EnemyActivation, SnapError> {
        Ok(EnemyActivation {
            alerted: r.seq("ea.alerted", |r| r.boolean())?,
            alert_timer: r.seq("ea.alert_timer", |r| r.i32())?,
            production: r.seq("ea.production", |r| r.boolean())?,
            base_house: r.u8()?,
            base_nodes: r.seq("ea.base_nodes", |r| {
                Ok((r.u32()?, CellCoord::snap_read(r)?))
            })?,
            tech_level: r.i32()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::Facing;
    use crate::unit::MoveStats;
    use crate::world::World;
    use crate::{CellCoord, Passability};
    use proptest::prelude::*;

    /// 2TNK-ish AP cannon so armed units actually fire (draws sim RNG on the AP
    /// scatter path, spawning bullets and killing units — exercising the bullet
    /// arena, kill tallies, and free-list reuse).
    fn cannon() -> WeaponProfile {
        WeaponProfile {
            damage: 40,
            rof: 20,
            range: 1216,
            proj_speed: 102,
            proj_rot: 0,
            invisible: false,
            instant: false,
            warhead: WarheadProfile {
                spread: 3,
                verses: [65536; ARMOR_COUNT],
            },
            warhead_ap: true,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// A rich synthetic mid-game world: three mutually-hostile houses whose armed
    /// units are interleaved so they auto-acquire and fight, plus shroud and ore
    /// growth (both draw the sync RNG). Combat depends on the *non-hashed* `weapon`
    /// constant, so a serializer that dropped any such field would diverge the
    /// hash within a few ticks — making the round-trip property non-vacuous.
    fn synth_world(seed: u32) -> World {
        let mut w = World::new(Passability::all_passable(), seed);
        w.init_houses(3, 5000);
        w.enable_shroud();
        w.set_ore_growth(true, true);
        let stats = MoveStats {
            max_speed: 48,
            rot: 8,
        };
        for i in 0..36u32 {
            let cell = CellCoord::new(30 + (i % 12) as i32, 30 + (i / 12) as i32);
            let house = (i % 3) as u8;
            let h = w.spawn_unit(
                i % 4,
                house,
                cell,
                Facing((i.wrapping_mul(23) & 0xFF) as u8),
                180,
                stats,
            );
            if i % 3 != 0 {
                w.set_unit_combat(h, (i % 5) as u8, Some(cannon()), i % 2 == 0);
            }
            w.reveal_shroud(house, cell, 4);
        }
        w
    }

    /// P2a — snapshot round-trip: for a seeded world stepped to a random tick,
    /// save→load reproduces the byte-identical state-hash chain for 200 more ticks.
    fn roundtrip(seed: u32, warmup: u32) {
        let mut orig = synth_world(seed);
        for _ in 0..warmup {
            orig.tick(&[]);
        }
        let bytes = orig.save_snapshot();
        let mut loaded =
            World::load_snapshot(&bytes, orig.catalog().clone(), orig.passability().clone())
                .expect("load must succeed");
        // Immediate identity: every hashed field survived the round-trip.
        assert_eq!(
            orig.state_hash(),
            loaded.state_hash(),
            "hash mismatch at load"
        );
        // Chain identity: non-hashed determinism-relevant state (weapons, etc.)
        // survived too — otherwise combat/movement would diverge.
        for t in 0..200 {
            let ho = orig.tick(&[]);
            let hl = loaded.tick(&[]);
            assert_eq!(ho, hl, "hash chain diverged at step {t}");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn roundtrip_hash_chain_identity(seed in any::<u32>(), warmup in 0u32..80) {
            roundtrip(seed, warmup);
        }

        /// P2b — malformed fuzz: an arbitrary byte string never panics the decoder;
        /// it is either rejected or (vanishingly rarely) a valid-looking world.
        #[test]
        fn malformed_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let cat = crate::catalog::Catalog::new();
            let pass = Passability::all_passable();
            // Must not panic. Result is ignored: any Ok/Err is acceptable, no hang.
            let _ = World::load_snapshot(&bytes, cat, pass);
        }

        /// Every truncation of a real snapshot is rejected cleanly (never panics,
        /// never a spurious success from a short buffer).
        #[test]
        fn truncations_rejected(seed in any::<u32>(), cut in 0usize..2000) {
            let w = synth_world(seed);
            let bytes = w.save_snapshot();
            let n = cut.min(bytes.len().saturating_sub(1));
            let res = World::load_snapshot(&bytes[..n], w.catalog().clone(), w.passability().clone());
            prop_assert!(res.is_err(), "a truncated snapshot must not load");
        }
    }

    #[test]
    fn version_magic_and_content_mismatch_rejected() {
        let w = synth_world(7);
        let good = w.save_snapshot();

        // Bad magic.
        let mut bad = good.clone();
        bad[0] ^= 0xFF;
        assert_eq!(
            World::load_snapshot(&bad, w.catalog().clone(), w.passability().clone()).unwrap_err(),
            SnapError::BadMagic
        );

        // Bad version (bytes 4..6).
        let mut badv = good.clone();
        badv[4] = badv[4].wrapping_add(1);
        assert_eq!(
            World::load_snapshot(&badv, w.catalog().clone(), w.passability().clone()).unwrap_err(),
            SnapError::Version
        );

        // Catalog mismatch: a catalog with different content.
        let mut other_cat = crate::catalog::Catalog::new();
        other_cat.buildings.push(crate::catalog::BuildingProto {
            name: "X".into(),
            foot_w: 1,
            foot_h: 1,
            max_health: 1,
            armor: 0,
            power: 0,
            cost: 1,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 0,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        });
        assert_eq!(
            World::load_snapshot(&good, other_cat, w.passability().clone()).unwrap_err(),
            SnapError::CatalogMismatch
        );

        // Map mismatch: a differently-sized passability grid.
        let other_map = Passability::new(8, 8, vec![true; 64]);
        assert_eq!(
            World::load_snapshot(&good, w.catalog().clone(), other_map).unwrap_err(),
            SnapError::MapMismatch
        );
    }

    #[test]
    fn ai_alliances_roundtrip() {
        use crate::ai::{AiPlayer, Difficulty};
        let mut w = synth_world(11);
        w.set_player_house(0);
        w.set_alliances(vec![0b001, 0b010, 0b100]);
        w.set_ai(vec![
            AiPlayer::new(1, Difficulty::Hard),
            AiPlayer::new(2, Difficulty::Easy),
        ]);
        let bytes = w.save_snapshot();
        let loaded = World::load_snapshot(&bytes, w.catalog().clone(), w.passability().clone())
            .expect("load");
        assert_eq!(w.state_hash(), loaded.state_hash());
    }
}
