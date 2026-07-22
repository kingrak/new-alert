//! Ore/Tiberium overlay as a harvestable resource grid (DESIGN.md §4.9 M5).
//!
//! The scenario's `[OverlayPack]` already decodes to per-cell overlay bytes
//! (`ra_data::scenario`). The gold overlay ids (`OVERLAY_GOLD1..GOLD4` = 5..8)
//! and gem ids (`OVERLAY_GEMS1..GEMS4` = 9..12, `redalert/defines.h:1508`) are
//! interpreted here as ore cells carrying a number of harvestable **bails**.
//!
//! **Growth stage → bail count.** The original stores a per-cell density
//! (`CellClass::OverlayData`, 0..11) derived from the count of adjacent ore
//! cells via the `_adj[9] = {0,1,3,4,6,7,8,10,11}` table (gems use
//! `_adjgem[9]` clamped to 2), `cell.cpp:2160-2179`. A harvester lifts one bail
//! (one "level") per harvest step (`Harvesting`, `unit.cpp:2412` `reducer = 1`),
//! and each gold bail books `Gold++`, each gem bail `Gems++` — cashed on unload
//! as `Gold*GoldValue + Gems*GemValue` (`Credit_Load`, `unit.cpp:5003`).
//!
//! We reproduce that density init exactly, so a cell yields `density+1` bails,
//! and tag each ore cell gold vs gem for the unload value split.
//!
//! **Deferred to M6 (cited).** Ore *growth* and *spread* (`OreClass`/
//! `CellClass::Grow_Tiberium` driven from `LogicClass::AI`) consume the sync
//! RNG (`Random_Pick`), so per the determinism contract they are out of M5
//! scope: M5 ore is **static** (no growth). This module therefore draws no RNG.

use crate::coords::CellCoord;
use crate::hash::Fnv1a;

/// First gold overlay id (`OVERLAY_GOLD1`).
pub const OVERLAY_GOLD_FIRST: u8 = 5;
/// Last gold overlay id (`OVERLAY_GOLD4`).
pub const OVERLAY_GOLD_LAST: u8 = 8;
/// First gem overlay id (`OVERLAY_GEMS1`).
pub const OVERLAY_GEM_FIRST: u8 = 9;
/// Last gem overlay id (`OVERLAY_GEMS4`).
pub const OVERLAY_GEM_LAST: u8 = 12;

/// Whether an overlay byte is a gold ore id.
pub fn is_gold(overlay: u8) -> bool {
    (OVERLAY_GOLD_FIRST..=OVERLAY_GOLD_LAST).contains(&overlay)
}
/// Whether an overlay byte is a gem ore id.
pub fn is_gem(overlay: u8) -> bool {
    (OVERLAY_GEM_FIRST..=OVERLAY_GEM_LAST).contains(&overlay)
}
/// Whether an overlay byte is any ore.
pub fn is_ore(overlay: u8) -> bool {
    is_gold(overlay) || is_gem(overlay)
}

/// The adjacency→density tables ported from `cell.cpp:2141` (`_adj`, `_adjgem`).
const ADJ: [u16; 9] = [0, 1, 3, 4, 6, 7, 8, 10, 11];
const ADJ_GEM: [u16; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];

/// A harvestable ore cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OreCell {
    /// Bails of ore remaining (0 = no ore here).
    pub bails: u16,
    /// Whether this cell is gems (else gold) — sets the per-bail unload value.
    pub gem: bool,
}

/// The map's ore overlay, one [`OreCell`] per map cell (row-major).
#[derive(Clone, Debug)]
pub struct OreField {
    width: i32,
    height: i32,
    cells: Vec<OreCell>,
}

impl OreField {
    /// An empty ore field of the given size (no ore anywhere).
    pub fn empty(width: i32, height: i32) -> OreField {
        OreField {
            width,
            height,
            cells: vec![OreCell::default(); (width * height) as usize],
        }
    }

    /// Build an ore field from a row-major overlay-byte plane (as decoded by
    /// `ra_data::scenario`). Each ore cell's bail count is `density + 1`, where
    /// `density` is the ported adjacency table applied to the count of adjacent
    /// ore cells (`cell.cpp:2160-2179`). Non-ore cells are left empty.
    pub fn from_overlay(width: i32, height: i32, overlay: &[u8]) -> OreField {
        let mut field = OreField::empty(width, height);
        if overlay.len() < (width * height) as usize {
            return field;
        }
        let at = |x: i32, y: i32| -> u8 {
            if x < 0 || y < 0 || x >= width || y >= height {
                return 0xFF;
            }
            overlay[(y * width + x) as usize]
        };
        for y in 0..height {
            for x in 0..width {
                let ov = at(x, y);
                if !is_ore(ov) {
                    continue;
                }
                let gem = is_gem(ov);
                // Count the 8 adjacent cells that also hold ore.
                let mut count = 0usize;
                for (dx, dy) in [
                    (0, -1),
                    (1, -1),
                    (1, 0),
                    (1, 1),
                    (0, 1),
                    (-1, 1),
                    (-1, 0),
                    (-1, -1),
                ] {
                    if is_ore(at(x + dx, y + dy)) {
                        count += 1;
                    }
                }
                let density = if gem {
                    ADJ_GEM[count.min(8)]
                } else {
                    ADJ[count.min(8)]
                };
                field.cells[(y * width + x) as usize] = OreCell {
                    bails: density + 1,
                    gem,
                };
            }
        }
        field
    }

    /// Grid width in cells.
    pub fn width(&self) -> i32 {
        self.width
    }
    /// Grid height in cells.
    pub fn height(&self) -> i32 {
        self.height
    }

    fn index(&self, cell: CellCoord) -> Option<usize> {
        if cell.x < 0 || cell.y < 0 || cell.x >= self.width || cell.y >= self.height {
            return None;
        }
        Some((cell.y * self.width + cell.x) as usize)
    }

    /// The ore cell at `cell` (empty if off-grid).
    pub fn at(&self, cell: CellCoord) -> OreCell {
        self.index(cell).map(|i| self.cells[i]).unwrap_or_default()
    }

    /// Whether `cell` currently holds any ore.
    pub fn has_ore(&self, cell: CellCoord) -> bool {
        self.at(cell).bails > 0
    }

    /// Lift up to `want` bails from `cell`. Returns the [`OreCell`] describing
    /// what was lifted (`bails` = amount actually taken, `gem` = its kind).
    /// Empties/clears the cell when it runs out.
    pub fn harvest(&mut self, cell: CellCoord, want: u16) -> OreCell {
        if let Some(i) = self.index(cell) {
            let c = self.cells[i];
            if c.bails == 0 {
                return OreCell::default();
            }
            let taken = want.min(c.bails);
            self.cells[i].bails = c.bails - taken;
            OreCell {
                bails: taken,
                gem: c.gem,
            }
        } else {
            OreCell::default()
        }
    }

    /// Total bails of ore remaining on the whole map (for reporting/tests).
    pub fn total_bails(&self) -> u64 {
        self.cells.iter().map(|c| c.bails as u64).sum()
    }

    /// Whether `cell` may grow denser (M6, `CellClass::Can_Tiberium_Grow`,
    /// `cell.cpp:3075`): gold ore below the density cap. Density is `bails - 1`,
    /// so the `OverlayData < 11` gate is `bails <= 11` (density ≤ 10).
    pub fn can_grow(&self, cell: CellCoord) -> bool {
        let c = self.at(cell);
        c.bails > 0 && !c.gem && c.bails <= 11
    }

    /// Increase `cell`'s density by one level (`CellClass::Grow_Tiberium`,
    /// `cell.cpp:3150`), capped at density 11 (12 bails). No-op if ineligible.
    pub fn grow(&mut self, cell: CellCoord) {
        if self.can_grow(cell) {
            if let Some(i) = self.index(cell) {
                self.cells[i].bails += 1;
            }
        }
    }

    /// Whether `cell` is dense enough to spread (`Can_Tiberium_Spread`,
    /// `cell.cpp:3114`): gold with `OverlayData > 6`, i.e. `bails >= 8`.
    pub fn can_spread(&self, cell: CellCoord) -> bool {
        let c = self.at(cell);
        c.bails > 0 && !c.gem && c.bails >= 8
    }

    /// Germinate fresh gold ore at an empty `cell` (`Spread_Tiberium`,
    /// `cell.cpp:3187`: a new `OVERLAY_GOLD*` at `OverlayData = 0` → 1 bail).
    pub fn germinate(&mut self, cell: CellCoord) {
        if let Some(i) = self.index(cell) {
            self.cells[i] = OreCell {
                bails: 1,
                gem: false,
            };
        }
    }

    /// Fold the ore field into the world hash: only non-empty cells, in
    /// row-major (fixed) order, so a divergent harvest is caught.
    /// Byte-exact snapshot (M8-C): dims plus every cell's bails+gem flag.
    pub(crate) fn snap_write(&self, w: &mut crate::snapshot::SnapWriter) {
        w.i32(self.width);
        w.i32(self.height);
        w.seq(&self.cells, |w, c| {
            w.u16(c.bails);
            w.boolean(c.gem);
        });
    }
    /// Inverse of [`OreField::snap_write`].
    pub(crate) fn snap_read(
        r: &mut crate::snapshot::SnapReader,
    ) -> Result<OreField, crate::snapshot::SnapError> {
        Ok(OreField {
            width: r.i32()?,
            height: r.i32()?,
            cells: r.seq("ore.cells", |r| {
                Ok(OreCell {
                    bails: r.u16()?,
                    gem: r.boolean()?,
                })
            })?,
        })
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        let mut live = 0u32;
        for c in &self.cells {
            if c.bails > 0 {
                live += 1;
            }
        }
        h.write_u32(live);
        for (i, c) in self.cells.iter().enumerate() {
            if c.bails > 0 {
                h.write_u32(i as u32);
                h.write_u16(c.bails);
                h.write_u8(c.gem as u8);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_ids_classify() {
        assert!(is_gold(5) && is_gold(8));
        assert!(is_gem(9) && is_gem(12));
        assert!(!is_ore(4) && !is_ore(13) && !is_ore(0xFF));
    }

    #[test]
    fn isolated_gold_cell_yields_one_bail() {
        // A single gold cell with no ore neighbours: count 0 -> density _adj[0]=0
        // -> 1 bail.
        let w = 8;
        let h = 8;
        let mut ov = vec![0xFFu8; (w * h) as usize];
        ov[(3 * w + 3) as usize] = OVERLAY_GOLD_FIRST;
        let field = OreField::from_overlay(w, h, &ov);
        assert_eq!(field.at(CellCoord::new(3, 3)).bails, 1);
        assert!(!field.at(CellCoord::new(3, 3)).gem);
    }

    #[test]
    fn dense_gold_cell_yields_more() {
        // A gold cell fully surrounded by gold: count 8 -> _adj[8]=11 -> 12 bails.
        let w = 8;
        let h = 8;
        let mut ov = vec![0xFFu8; (w * h) as usize];
        for dy in -1..=1 {
            for dx in -1..=1 {
                ov[((4 + dy) * w + (4 + dx)) as usize] = OVERLAY_GOLD_FIRST;
            }
        }
        let field = OreField::from_overlay(w, h, &ov);
        assert_eq!(field.at(CellCoord::new(4, 4)).bails, 12);
    }

    #[test]
    fn harvest_depletes_cell() {
        let w = 4;
        let h = 4;
        let mut ov = vec![0xFFu8; (w * h) as usize];
        ov[0] = OVERLAY_GOLD_FIRST;
        let mut field = OreField::from_overlay(w, h, &ov);
        let c = CellCoord::new(0, 0);
        assert_eq!(field.at(c).bails, 1);
        let got = field.harvest(c, 5);
        assert_eq!(got.bails, 1); // only 1 was available
        assert!(!field.has_ore(c));
        assert_eq!(field.harvest(c, 1).bails, 0); // exhausted
    }
}
