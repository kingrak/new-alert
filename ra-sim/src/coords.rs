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
