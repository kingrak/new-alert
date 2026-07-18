//! Per-house shroud (DESIGN.md §4.9 M6, item 1) — the "explored" fog model.
//!
//! Red Alert 1's shroud is a per-cell **explored** state that, once set, stays
//! set: terrain a house has seen remains revealed after its units leave. RA1
//! has **no** re-shrouding fog of war by default (unlike Tiberian Sun); the
//! optional "shadow regrow" game option is forced off in single-player
//! (`scenario.cpp:2187`), so we model only the sticky explored bit. Port of the
//! per-house `IsMappedByPlayerMask` bitmap (`cell.h:192`, `cell.cpp:3258`).
//!
//! The model lives in the sim (not the client) because both the player and the
//! skirmish AI share it, and — since the AI keys its targeting off what it has
//! explored — it is hash-relevant determinism state (§4.2): it is folded into
//! the per-tick [`crate::World::state_hash`].
//!
//! **Reveal shape.** `MapClass::Sight_From` (`map.cpp:576`) reveals every cell
//! within `Distance(cell, center) <= sight * CELL_LEPTON_W` of the sighting
//! object — an octagonal disc under the engine's `Distance` metric (our
//! [`crate::coords::leptons_distance`]), *not* a square. Sight range is a cell
//! radius capped at 10 (`map.cpp:588`).

use crate::coords::{leptons_distance, CellCoord, LEPTONS_PER_CELL};
use crate::hash::Fnv1a;

/// The eight country houses the shroud tracks (matches `ra_data::house::HOUSE_COUNT`).
pub const NUM_HOUSES: usize = 8;

/// A per-house "explored" bitmap over the map grid. Cell `(x,y)` for house `h`
/// is explored iff `explored[h * w * h_ + y * w + x]` is set (packed as bytes).
#[derive(Clone, Debug)]
pub struct Shroud {
    width: i32,
    height: i32,
    /// Whether the shroud is active at all. When `false`, every cell reads as
    /// explored (the pre-M6 "no shroud" behaviour test/campaign worlds want).
    enabled: bool,
    /// `NUM_HOUSES · width · height` explored bits, packed one bool per entry
    /// (kept as `bool` for clarity; hashing packs them into bytes).
    explored: Vec<bool>,
}

impl Shroud {
    /// A shroud sized to the map, **disabled** by default (everything reads as
    /// explored) so movement/combat/economy worlds are unaffected until a
    /// skirmish explicitly [`Shroud::enable`]s it.
    pub fn new(width: i32, height: i32) -> Shroud {
        Shroud {
            width,
            height,
            enabled: false,
            explored: vec![false; NUM_HOUSES * (width * height).max(0) as usize],
        }
    }

    /// Turn the shroud on: from now on cells start unexplored until revealed.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Whether the shroud is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Grid width in cells.
    pub fn width(&self) -> i32 {
        self.width
    }
    /// Grid height in cells.
    pub fn height(&self) -> i32 {
        self.height
    }

    fn index(&self, house: u8, cell: CellCoord) -> Option<usize> {
        if (house as usize) >= NUM_HOUSES
            || cell.x < 0
            || cell.y < 0
            || cell.x >= self.width
            || cell.y >= self.height
        {
            return None;
        }
        let plane = (self.width * self.height) as usize;
        Some(house as usize * plane + (cell.y * self.width + cell.x) as usize)
    }

    /// Whether `house` has explored `cell`. A disabled shroud reports every
    /// on-grid cell as explored.
    pub fn is_explored(&self, house: u8, cell: CellCoord) -> bool {
        if !self.enabled {
            return cell.x >= 0 && cell.y >= 0 && cell.x < self.width && cell.y < self.height;
        }
        self.index(house, cell)
            .map(|i| self.explored[i])
            .unwrap_or(false)
    }

    /// Reveal the octagonal sight disc of radius `sight` cells around `center`
    /// for `house` (sticky). No-op if the shroud is disabled, `sight == 0`, or
    /// the house is out of range. Port of `Sight_From` (`map.cpp:576-636`): a
    /// cell is revealed when `leptons_distance(center, cell) <= sight·256`.
    pub fn reveal(&mut self, house: u8, center: CellCoord, sight: u8) {
        if !self.enabled || sight == 0 || (house as usize) >= NUM_HOUSES {
            return;
        }
        let r = (sight.min(10)) as i32;
        let reach = r * LEPTONS_PER_CELL;
        let cc = center.center();
        for dy in -r..=r {
            for dx in -r..=r {
                let c = CellCoord::new(center.x + dx, center.y + dy);
                if leptons_distance(cc, c.center()) > reach {
                    continue;
                }
                if let Some(i) = self.index(house, c) {
                    self.explored[i] = true;
                }
            }
        }
    }

    /// Count of explored cells for a house (for reporting / tests).
    pub fn explored_count(&self, house: u8) -> u32 {
        if (house as usize) >= NUM_HOUSES {
            return 0;
        }
        let plane = (self.width * self.height) as usize;
        let base = house as usize * plane;
        self.explored[base..base + plane]
            .iter()
            .filter(|&&e| e)
            .count() as u32
    }

    /// Fold the shroud into the world hash. A disabled shroud contributes **no**
    /// bytes (so a non-skirmish world's hash is byte-identical to M5); an enabled
    /// one writes a marker byte followed by the packed explored bits.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        if !self.enabled {
            return;
        }
        h.write_u8(1);
        // Pack 8 explored bits per byte, in fixed order.
        let mut byte = 0u8;
        let mut nbits = 0u8;
        for &e in &self.explored {
            byte = (byte << 1) | (e as u8);
            nbits += 1;
            if nbits == 8 {
                h.write_u8(byte);
                byte = 0;
                nbits = 0;
            }
        }
        if nbits > 0 {
            h.write_u8(byte << (8 - nbits));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_reads_all_explored() {
        let s = Shroud::new(16, 16);
        assert!(s.is_explored(1, CellCoord::new(5, 5)));
        assert!(!s.is_explored(1, CellCoord::new(-1, 0))); // off-grid still false
    }

    #[test]
    fn reveal_marks_a_disc() {
        let mut s = Shroud::new(32, 32);
        s.enable();
        assert!(!s.is_explored(1, CellCoord::new(10, 10)));
        s.reveal(1, CellCoord::new(10, 10), 3);
        // Centre and near cells explored; a cell well outside the radius is not.
        assert!(s.is_explored(1, CellCoord::new(10, 10)));
        assert!(s.is_explored(1, CellCoord::new(12, 10)));
        assert!(!s.is_explored(1, CellCoord::new(20, 20)));
        // Only house 1 was revealed.
        assert!(!s.is_explored(2, CellCoord::new(10, 10)));
    }

    #[test]
    fn reveal_is_sticky() {
        let mut s = Shroud::new(16, 16);
        s.enable();
        s.reveal(0, CellCoord::new(8, 8), 2);
        let before = s.explored_count(0);
        // Revealing elsewhere never clears prior exploration.
        s.reveal(0, CellCoord::new(2, 2), 1);
        assert!(s.explored_count(0) > before);
        assert!(s.is_explored(0, CellCoord::new(8, 8)));
    }

    /// Direct proof of the `hash_into` doc claim ("a disabled shroud
    /// contributes **no** bytes"): folding a disabled shroud into a hasher
    /// must leave it byte-identical to never having folded anything in at
    /// all, even after cells have been "revealed" while enabled and the
    /// shroud is later... well, `Shroud` has no `disable()`, so this checks
    /// the only reachable disabled state (never enabled) plus a populated
    /// `explored` buffer sized like an enabled one, to rule out the early
    /// return silently depending on the buffer being empty.
    #[test]
    fn disabled_shroud_hash_into_writes_no_bytes() {
        let s = Shroud::new(16, 16); // disabled by default
        let mut h_folded = Fnv1a::new();
        s.hash_into(&mut h_folded);
        let h_untouched = Fnv1a::new();
        assert_eq!(
            h_folded.finish(),
            h_untouched.finish(),
            "a disabled shroud must fold zero bytes into the hash"
        );

        // Same check, but with a large-ish grid (nonzero-length `explored`
        // buffer) to rule out an accidental early-return-only-when-empty bug.
        let big_disabled = Shroud::new(128, 128);
        let mut h_big = Fnv1a::new();
        big_disabled.hash_into(&mut h_big);
        assert_eq!(
            h_big.finish(),
            h_untouched.finish(),
            "a disabled shroud with a nonzero-size grid must still fold zero bytes"
        );
    }
}
