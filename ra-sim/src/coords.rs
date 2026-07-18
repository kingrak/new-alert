//! Fixed-point world geometry — the vocabulary the whole sim speaks in.
//!
//! Red Alert measures the world in **leptons**: 256 leptons to a cell, 128×128
//! cells to a map. Positions are integers, never floats (see the crate-level
//! `deny(clippy::float_arithmetic)`), and directions are 8-bit **binary
//! angles** where wraparound is exact and free. We keep the original units —
//! they are part of how the game feels — but wrap them in newtypes so a cell
//! count can never be silently used where a lepton offset is meant.
//!
//! Facing math (`desired_facing256`, `Facing::rotate_toward`) is ported from
//! the original: `common/face.cpp` (`Desired_Facing256`) and
//! `redalert/facing.cpp` (`FacingClass::Rotation_Adjust`).

/// Leptons per cell edge — the original's fundamental sub-cell resolution.
pub const LEPTONS_PER_CELL: i32 = 256;
/// Map width in cells (fixed in RA).
pub const MAP_CELL_W: i32 = 128;
/// Map height in cells (fixed in RA).
pub const MAP_CELL_H: i32 = 128;

/// A distance or coordinate measured in leptons (1/256 of a cell).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Lepton(pub i32);

impl Lepton {
    /// The raw lepton count.
    pub fn raw(self) -> i32 {
        self.0
    }
}

/// A cell coordinate on the 128×128 grid. Stored signed so pathfinding
/// neighbour math (which transiently steps off the edge) needs no casts; the
/// grid bounds-checks before indexing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct CellCoord {
    /// Cell column, 0..128 on-map.
    pub x: i32,
    /// Cell row, 0..128 on-map.
    pub y: i32,
}

impl CellCoord {
    /// Build a cell coordinate.
    pub fn new(x: i32, y: i32) -> CellCoord {
        CellCoord { x, y }
    }

    /// Decode a linear RA cell number (`y * 128 + x`) as scenario INIs store it.
    pub fn from_index(cell: u32) -> CellCoord {
        CellCoord {
            x: (cell & (MAP_CELL_W as u32 - 1)) as i32,
            y: (cell >> 7) as i32,
        }
    }

    /// The linear RA cell number, or `None` if off the 128×128 map.
    pub fn to_index(self) -> Option<u32> {
        if self.on_map() {
            Some((self.y as u32) * (MAP_CELL_W as u32) + self.x as u32)
        } else {
            None
        }
    }

    /// Whether this cell is within the 128×128 map.
    pub fn on_map(self) -> bool {
        self.x >= 0 && self.x < MAP_CELL_W && self.y >= 0 && self.y < MAP_CELL_H
    }

    /// The world coordinate at the centre of this cell (offset 128,128).
    pub fn center(self) -> WorldCoord {
        WorldCoord {
            x: Lepton(self.x * LEPTONS_PER_CELL + LEPTONS_PER_CELL / 2),
            y: Lepton(self.y * LEPTONS_PER_CELL + LEPTONS_PER_CELL / 2),
        }
    }
}

/// An absolute position on the map, in leptons on each axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct WorldCoord {
    /// X in leptons from the map's left edge.
    pub x: Lepton,
    /// Y in leptons from the map's top edge.
    pub y: Lepton,
}

impl WorldCoord {
    /// Build a world coordinate from raw lepton values.
    pub fn new(x: i32, y: i32) -> WorldCoord {
        WorldCoord {
            x: Lepton(x),
            y: Lepton(y),
        }
    }

    /// The cell this position falls in.
    pub fn cell(self) -> CellCoord {
        CellCoord {
            x: self.x.0.div_euclid(LEPTONS_PER_CELL),
            y: self.y.0.div_euclid(LEPTONS_PER_CELL),
        }
    }
}

/// Integer square root (floor). Deterministic, no floating point. Used for
/// straight-line distance between world points during movement.
pub fn isqrt(n: i64) -> i64 {
    if n <= 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Lepton distance between two world points, using the original's fast
/// **"Dragon Strike"** octagonal metric — *not* Euclidean. Port of `Distance`
/// (`redalert/coord.cpp:119`): `max(|dx|,|dy|) + min(|dx|,|dy|)/2`. This is the
/// exact metric the engine uses for weapon-range and scatter-distance checks,
/// so combat must use it (movement's straight-line stepping keeps [`isqrt`]).
pub fn leptons_distance(a: WorldCoord, b: WorldCoord) -> i32 {
    let dy = (a.y.0 - b.y.0).abs();
    let dx = (a.x.0 - b.x.0).abs();
    if dy > dx {
        dy + ((dx as u32) / 2) as i32
    } else {
        dx + ((dy as u32) / 2) as i32
    }
}

/// Move a world coordinate `distance` leptons along binary-angle `dir`. Port of
/// `Coord_Move` / `Move_Point` (`redalert/coord.cpp:364`, `:432`) using the
/// original's 256-entry cosine/sine tables and `calcx`/`calcy`
/// (`common/misc.cpp`): `x += (cos*d)>>7`, `y += -((sin*d)>>7)` (screen-Y grows
/// downward, hence the negated sine). Deterministic integer math; used for
/// projectile-scatter displacement in combat.
pub fn coord_move(start: WorldCoord, dir: Facing, distance: i32) -> WorldCoord {
    let d = dir.0 as usize;
    // calcx/calcy: (param * distance) >> 7, truncated to u16 then re-widened —
    // faithful to `(unsigned short)(tmp >> 7)`. Distances here are tiny so the
    // truncation is a no-op, but we mirror it exactly.
    let cos = COS_TABLE[d] as i32; // signed char
    let sin = SIN_TABLE[d] as i32;
    let dx = ((((cos * distance) >> 7) as u16) as i16) as i32;
    let dy = -(((((sin * distance) >> 7) as u16) as i16) as i32);
    WorldCoord::new(start.x.0 + dx, start.y.0 + dy)
}

/// Cosine lookup, 256 binary-angle steps, values are signed `char` in ±0x7f.
/// Verbatim from `Move_Point` (`redalert/coord.cpp`).
static COS_TABLE: [i8; 256] = cos_sin_tables().0;
/// Sine lookup, 256 binary-angle steps. Verbatim from `Move_Point`.
static SIN_TABLE: [i8; 256] = cos_sin_tables().1;

/// The original's cosine/sine byte tables. `SIN[i] == COS[(i+64) mod 256]`
/// shifted per the source layout; we transcribe both directly to stay exact.
const fn cos_sin_tables() -> ([i8; 256], [i8; 256]) {
    // Transcribed byte-for-byte from `Move_Point`'s CosTable/SinTable
    // (`redalert/coord.cpp`). `as i8` reinterprets the >0x7f bytes as negative,
    // exactly as `(char)` does in the original.
    const C: [u8; 256] = [
        0x00, 0x03, 0x06, 0x09, 0x0c, 0x0f, 0x12, 0x15, 0x18, 0x1b, 0x1e, 0x21, 0x24, 0x27, 0x2a,
        0x2d, 0x30, 0x33, 0x36, 0x39, 0x3b, 0x3e, 0x41, 0x43, 0x46, 0x49, 0x4b, 0x4e, 0x50, 0x52,
        0x55, 0x57, 0x59, 0x5b, 0x5e, 0x60, 0x62, 0x64, 0x65, 0x67, 0x69, 0x6b, 0x6c, 0x6e, 0x6f,
        0x71, 0x72, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7b, 0x7c, 0x7d, 0x7d, 0x7e,
        0x7e, 0x7e, 0x7e, 0x7e, 0x7f, 0x7e, 0x7e, 0x7e, 0x7e, 0x7e, 0x7d, 0x7d, 0x7c, 0x7b, 0x7b,
        0x7a, 0x79, 0x78, 0x77, 0x76, 0x75, 0x74, 0x72, 0x71, 0x70, 0x6e, 0x6c, 0x6b, 0x69, 0x67,
        0x66, 0x64, 0x62, 0x60, 0x5e, 0x5b, 0x59, 0x57, 0x55, 0x52, 0x50, 0x4e, 0x4b, 0x49, 0x46,
        0x43, 0x41, 0x3e, 0x3b, 0x39, 0x36, 0x33, 0x30, 0x2d, 0x2a, 0x27, 0x24, 0x21, 0x1e, 0x1b,
        0x18, 0x15, 0x12, 0x0f, 0x0c, 0x09, 0x06, 0x03, 0x00, 0xfd, 0xfa, 0xf7, 0xf4, 0xf1, 0xee,
        0xeb, 0xe8, 0xe5, 0xe2, 0xdf, 0xdc, 0xd9, 0xd6, 0xd3, 0xd0, 0xcd, 0xca, 0xc7, 0xc5, 0xc2,
        0xbf, 0xbd, 0xba, 0xb7, 0xb5, 0xb2, 0xb0, 0xae, 0xab, 0xa9, 0xa7, 0xa5, 0xa2, 0xa0, 0x9e,
        0x9c, 0x9a, 0x99, 0x97, 0x95, 0x94, 0x92, 0x91, 0x8f, 0x8e, 0x8c, 0x8b, 0x8a, 0x89, 0x88,
        0x87, 0x86, 0x85, 0x85, 0x84, 0x83, 0x83, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82, 0x82,
        0x82, 0x82, 0x82, 0x83, 0x83, 0x84, 0x85, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c,
        0x8e, 0x8f, 0x90, 0x92, 0x94, 0x95, 0x97, 0x99, 0x9a, 0x9c, 0x9e, 0xa0, 0xa2, 0xa5, 0xa7,
        0xa9, 0xab, 0xae, 0xb0, 0xb2, 0xb5, 0xb7, 0xba, 0xbd, 0xbf, 0xc2, 0xc5, 0xc7, 0xca, 0xcd,
        0xd0, 0xd3, 0xd6, 0xd9, 0xdc, 0xdf, 0xe2, 0xe5, 0xe8, 0xeb, 0xee, 0xf1, 0xf4, 0xf7, 0xfa,
        0xfd,
    ];
    // The original's SinTable equals CosTable advanced a quarter turn:
    // SIN[i] == COS[(i + 64) & 255] (verified against the verbatim SinTable in
    // `Move_Point`: SinTable[0]=0x7f=CosTable[64], etc.).
    let mut cos = [0i8; 256];
    let mut sin = [0i8; 256];
    let mut i = 0;
    while i < 256 {
        cos[i] = C[i] as i8;
        sin[i] = C[(i + 64) & 255] as i8;
        i += 1;
    }
    (cos, sin)
}

/// A binary-angle facing: 0 = north, 64 = east, 128 = south, 192 = west,
/// increasing clockwise. Wraparound is exact `u8` arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Facing(pub u8);

impl Facing {
    /// The facing pointing from `from` toward `to`. Returns `None` when the two
    /// points coincide (no meaningful direction). Ported from
    /// `Desired_Facing256` (`common/face.cpp`).
    pub fn toward(from: WorldCoord, to: WorldCoord) -> Option<Facing> {
        if from == to {
            return None;
        }
        Some(Facing(desired_facing256(
            from.x.0, from.y.0, to.x.0, to.y.0,
        )))
    }

    /// Shortest signed rotation from `self` to `other`, in the range
    /// `-128..=127` (positive = clockwise). Mirrors `FacingClass::Difference`.
    pub fn difference(self, other: Facing) -> i32 {
        (other.0 as i8).wrapping_sub(self.0 as i8) as i32
    }

    /// Rotate toward `desired` by at most `rate` binary-angle units, snapping
    /// when within `rate`. Ported from `FacingClass::Rotation_Adjust`
    /// (`redalert/facing.cpp`); the sim passes `Class->ROT + 1` as `rate`.
    pub fn rotate_toward(self, desired: Facing, rate: u8) -> Facing {
        if self == desired {
            return self;
        }
        let rate = rate.min(127) as i32;
        let diff = self.difference(desired);
        if diff.abs() < rate {
            desired
        } else if diff < 0 {
            Facing(self.0.wrapping_sub(rate as u8))
        } else {
            Facing(self.0.wrapping_add(rate as u8))
        }
    }
}

/// Port of `Desired_Facing256` (`common/face.cpp`). Operates on lepton
/// coordinates; screen-Y grows downward, so north is `y2 < y1`.
fn desired_facing256(x1: i32, y1: i32, x2: i32, y2: i32) -> u8 {
    let mut unk1: i8 = 0;

    let mut x_diff = x2 - x1;
    if x_diff < 0 {
        x_diff = -x_diff;
        unk1 = -64;
    }

    let mut y_diff = y1 - y2;
    if y_diff < 0 {
        unk1 ^= 64;
        y_diff = -y_diff;
    }

    if x_diff != 0 || y_diff != 0 {
        let (s_diff, l_diff) = if x_diff >= y_diff {
            (y_diff, x_diff)
        } else {
            (x_diff, y_diff)
        };

        let mut unk2 = 32 * s_diff / l_diff;
        let mut ranged_dir = (unk1 as i32) & 64;
        if x_diff > y_diff {
            ranged_dir ^= 64;
        }
        if ranged_dir != 0 {
            unk2 = ranged_dir - unk2 - 1;
        }
        return ((unk2 + unk1 as i32) & 255) as u8;
    }
    255
}

/// Convert an 8-bit facing to the 0..31 sprite-rotation index the vehicle SHP
/// frame table is keyed by. Port of `Dir_To_32` / `Facing32` (`const.cpp`):
/// `((dir + 4) >> 3) & 31`.
pub fn dir_to_32(facing: Facing) -> u8 {
    (((facing.0 as u16 + 4) >> 3) & 31) as u8
}

/// A unit's ground-movement class (`SpeedType`, `defines.h`): which column of the
/// land-type cost table (`Ground[land].Cost[speed]`, `rules.cpp` `Land_Types`)
/// governs whether it may enter a cell. Infantry are `Foot`; tracked vehicles
/// (tanks) are `Track`; wheeled vehicles (jeep/APC/harvester) are `Wheel`. The
/// three differ in their per-land passability (rock/water block all; rivers block
/// ground; infantry cross terrain a vehicle cannot), which is exactly the
/// distinction infantry movement needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Locomotor {
    /// Infantry (`SPEED_FOOT`).
    Foot,
    /// Tracked vehicle (`SPEED_TRACK`) — tanks.
    #[default]
    Track,
    /// Wheeled vehicle (`SPEED_WHEEL`) — jeep, APC, harvester.
    Wheel,
}

/// Number of infantry sub-cell spots per cell (`center + 4 quadrants`).
pub const SUBCELL_COUNT: usize = 5;

/// Lepton offsets of each sub-cell spot from a cell's **top-left** corner,
/// transcribed verbatim from `StoppingCoordAbs[5]` (`const.cpp:282`): index
/// 0=center, 1=NW, 2=NE, 3=SW, 4=SE. The array index doubles as the cell
/// occupancy bit position (`CellClass::Flag.Occupy`, `cell.h:207`).
pub const SPOT_OFFSET: [(i32, i32); SUBCELL_COUNT] = [
    (128, 128), // 0 center
    (64, 64),   // 1 upper-left  (NW)
    (192, 64),  // 2 upper-right (NE)
    (64, 192),  // 3 lower-left  (SW)
    (192, 192), // 4 lower-right (SE)
];

impl CellCoord {
    /// The world coordinate of sub-cell `spot` (0..[`SUBCELL_COUNT`]) within this
    /// cell — the cell's top-left plus [`SPOT_OFFSET`]. `spot` is clamped so an
    /// out-of-range index resolves to the centre.
    pub fn spot_center(self, spot: u8) -> WorldCoord {
        let (ox, oy) = SPOT_OFFSET[(spot as usize).min(SUBCELL_COUNT - 1)];
        WorldCoord {
            x: Lepton(self.x * LEPTONS_PER_CELL + ox),
            y: Lepton(self.y * LEPTONS_PER_CELL + oy),
        }
    }
}

/// The sub-cell spot index (0..[`SUBCELL_COUNT`]) a world coordinate falls in —
/// port of `CellClass::Spot_Index` (`cell.cpp:1845`): within 60 leptons of the
/// cell centre → spot 0; otherwise the quadrant chosen by whether the sub-cell
/// fraction exceeds 128 on each axis (`+1` for the right column, `+2` for the
/// bottom row, then `+1` to skip the centre slot).
pub fn spot_index(coord: WorldCoord) -> u8 {
    let fx = coord.x.0.rem_euclid(LEPTONS_PER_CELL);
    let fy = coord.y.0.rem_euclid(LEPTONS_PER_CELL);
    // Octagonal `Distance` from the cell centre (128,128) < 60 → centre spot.
    let dx = (fx - 128).abs();
    let dy = (fy - 128).abs();
    let dist = if dx > dy { dx + dy / 2 } else { dy + dx / 2 };
    if dist < 60 {
        return 0;
    }
    let mut index = 0u8;
    if fx > 128 {
        index |= 0x01;
    }
    if fy > 128 {
        index |= 0x02;
    }
    index + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_index_roundtrips() {
        // scg01ea JEEP at cell 6463 -> x=63, y=50.
        let c = CellCoord::from_index(6463);
        assert_eq!((c.x, c.y), (63, 50));
        assert_eq!(c.to_index(), Some(6463));
    }

    #[test]
    fn cell_center_is_offset_128() {
        let c = CellCoord::new(1, 2).center();
        assert_eq!((c.x.0, c.y.0), (256 + 128, 512 + 128));
        assert_eq!(c.cell(), CellCoord::new(1, 2));
    }

    #[test]
    fn facing_cardinals() {
        // These are the exact outputs of the original integer `Desired_Facing256`
        // routine, which is faithful but asymmetric: it lands dead-on for N and
        // W and one binary-angle unit short (a `-1` bias) for E and S. We keep
        // the quirk rather than "correct" it, so the facing math is bit-identical
        // to the original engine (1/256 of a turn is imperceptible anyway).
        let o = WorldCoord::new(1000, 1000);
        assert_eq!(Facing::toward(o, WorldCoord::new(1000, 0)), Some(Facing(0))); // N
        assert_eq!(
            Facing::toward(o, WorldCoord::new(2000, 1000)),
            Some(Facing(63)) // E (64 nominal, 63 from the routine)
        );
        assert_eq!(
            Facing::toward(o, WorldCoord::new(1000, 2000)),
            Some(Facing(127)) // S (128 nominal, 127 from the routine)
        );
        assert_eq!(
            Facing::toward(o, WorldCoord::new(0, 1000)),
            Some(Facing(192)) // W (exact)
        );
        assert_eq!(Facing::toward(o, o), None);
    }

    #[test]
    fn rotate_snaps_and_wraps() {
        // Turning from north toward east at rate 6 steps clockwise.
        let f = Facing(0).rotate_toward(Facing(64), 6);
        assert_eq!(f, Facing(6));
        // Within rate -> snap.
        assert_eq!(Facing(60).rotate_toward(Facing(64), 6), Facing(64));
        // Shortest path wraps the other way.
        let f = Facing(2).rotate_toward(Facing(250), 6);
        assert_eq!(f, Facing(252)); // counter-clockwise, wrapped
    }

    #[test]
    fn dir_to_32_zones() {
        assert_eq!(dir_to_32(Facing(0)), 0);
        assert_eq!(dir_to_32(Facing(64)), 8); // east
        assert_eq!(dir_to_32(Facing(128)), 16); // south
        assert_eq!(dir_to_32(Facing(252)), 0); // wraps
    }

    #[test]
    fn isqrt_floor() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(15), 3);
        assert_eq!(isqrt(16), 4);
        assert_eq!(isqrt(1_000_000), 1000);
    }

    #[test]
    fn dragon_strike_distance() {
        let o = WorldCoord::new(0, 0);
        // Pure axis: distance == the axis delta.
        assert_eq!(leptons_distance(o, WorldCoord::new(256, 0)), 256);
        assert_eq!(leptons_distance(o, WorldCoord::new(0, 256)), 256);
        // Octagonal metric: max + min/2. (300, 100) -> 300 + 50 = 350.
        assert_eq!(leptons_distance(o, WorldCoord::new(300, 100)), 350);
        assert_eq!(leptons_distance(o, WorldCoord::new(-100, 300)), 350);
        assert_eq!(leptons_distance(o, o), 0);
    }

    #[test]
    fn coord_move_cardinals() {
        let o = WorldCoord::new(1000, 1000);
        // North (screen-Y up): y decreases by ~dist, x ~unchanged.
        let n = coord_move(o, Facing(0), 100);
        assert!(n.y.0 < o.y.0 && (n.x.0 - o.x.0).abs() <= 1);
        // East: x increases, y ~unchanged.
        let e = coord_move(o, Facing(64), 100);
        assert!(e.x.0 > o.x.0 && (e.y.0 - o.y.0).abs() <= 1);
        // South: y increases.
        let s = coord_move(o, Facing(128), 100);
        assert!(s.y.0 > o.y.0);
        // West: x decreases.
        let w = coord_move(o, Facing(192), 100);
        assert!(w.x.0 < o.x.0);
        // Zero distance is a no-op.
        assert_eq!(coord_move(o, Facing(37), 0), o);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// Independent reference port of `FacingClass::Rotation_Adjust`
    /// (`redalert/facing.cpp:135`) plus `FacingClass::Difference`
    /// (`redalert/facing.h:85`, `(signed char)(desired - current)`),
    /// transcribed directly from the C++ rather than calling
    /// [`Facing::rotate_toward`] — so agreement between the two is a real
    /// cross-check of the port, not a tautology. Mirrors the original
    /// bit-for-bit: `rate = min(rate, 127)`; snap if `abs(diff) < rate`;
    /// otherwise step `rate` toward the shorter side, with wrapping `u8`
    /// (`DirType`) arithmetic throughout.
    fn reference_rotation_adjust(current: u8, desired: u8, rate: u8) -> u8 {
        if current == desired {
            return current;
        }
        let rate = rate.min(127) as i32;
        let diff = (desired as i8).wrapping_sub(current as i8) as i32; // Difference()
        if diff.abs() < rate {
            desired
        } else if diff < 0 {
            current.wrapping_sub(rate as u8)
        } else {
            current.wrapping_add(rate as u8)
        }
    }

    proptest! {
        /// [`Facing::rotate_toward`] must agree with the independent
        /// reference transcription above for every `(current, desired,
        /// rate)` triple, not just the handful of examples above.
        #[test]
        fn rotate_toward_matches_reference(current: u8, desired: u8, rate: u8) {
            let got = Facing(current).rotate_toward(Facing(desired), rate);
            let want = reference_rotation_adjust(current, desired, rate);
            prop_assert_eq!(got.0, want);
        }

        /// Repeatedly applying `rotate_toward` at a fixed nonzero rate must
        /// reach `desired` in a bounded number of steps and, once reached,
        /// stay there (idempotent fixed point) — the "eventually converges,
        /// never overshoots past and oscillates forever" property the
        /// original's per-tick `Class->ROT + 1` calling convention relies on.
        #[test]
        fn rotate_toward_converges_and_is_a_fixed_point(
            current: u8, desired: u8, rate in 1u8..=127
        ) {
            let mut f = Facing(current);
            let target = Facing(desired);
            // Worst case is a near-180 turn at rate 1 (diff up to 128): give
            // a generous margin above that floor.
            let mut steps = 0;
            while f != target && steps < 300 {
                f = f.rotate_toward(target, rate);
                steps += 1;
            }
            prop_assert_eq!(f, target, "did not converge within 300 steps");
            // Fixed point: rotating further leaves it unchanged.
            prop_assert_eq!(f.rotate_toward(target, rate), target);
        }

        /// Integer square root: floor(sqrt(n)) for all non-negative `n` a
        /// squared lepton distance could plausibly produce (`i32::MAX` as
        /// `i64` squared headroom -> bound the input so `x*x` stays in
        /// range). `isqrt(n)^2 <= n < (isqrt(n)+1)^2` is the defining
        /// algebraic identity of a floor square root.
        #[test]
        fn isqrt_is_floor_sqrt(n in 0i64..=(1i64 << 40)) {
            let r = isqrt(n);
            prop_assert!(r * r <= n);
            prop_assert!((r + 1) * (r + 1) > n);
        }

        /// `dir_to_32` never panics and always stays in the documented
        /// 0..32 sprite-rotation range, for every possible facing byte.
        #[test]
        fn dir_to_32_always_in_range(f: u8) {
            prop_assert!(dir_to_32(Facing(f)) < 32);
        }
    }
}
