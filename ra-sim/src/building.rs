//! The `Building` entity (DESIGN.md §4.9 M5) — a second entity arena alongside
//! units (§5 "Entity arenas: per-kind"). A building is a plain component struct:
//! a footprint anchored at a top-left cell, health, owning house, a net power
//! value, and a couple of role flags (refinery / construction yard / war
//! factory) the systems match on at their boundary (§3.1: closed-enum match, not
//! scattered `What_Am_I()` casts).

use crate::coords::CellCoord;
use crate::hash::Fnv1a;

/// A placed building.
#[derive(Clone, Debug)]
pub struct Building {
    /// Building type id (index into [`crate::catalog::Catalog::buildings`]);
    /// the client maps it to a SHP. Opaque to most of the sim.
    pub type_id: u32,
    /// Owning house index.
    pub house: u8,
    /// Top-left footprint cell.
    pub cell: CellCoord,
    /// Footprint width in cells.
    pub foot_w: u8,
    /// Footprint height in cells.
    pub foot_h: u8,
    /// Current strength.
    pub health: u16,
    /// Max strength.
    pub max_health: u16,
    /// Net power (positive output, negative drain) — mirrored into the owning
    /// house's power totals when placed/removed.
    pub power: i32,
    /// Refinery (PROC): a harvester dock.
    pub is_refinery: bool,
    /// Construction yard (CONST/FACT): builds structures.
    pub is_construction_yard: bool,
    /// War factory (WEAP): builds vehicles.
    pub is_war_factory: bool,
}

impl Building {
    /// The building's centre cell (footprint midpoint, integer-floored).
    pub fn center_cell(&self) -> CellCoord {
        CellCoord::new(
            self.cell.x + self.foot_w as i32 / 2,
            self.cell.y + self.foot_h as i32 / 2,
        )
    }

    /// Iterate the footprint cells (top-left origin, row-major).
    pub fn footprint(&self) -> impl Iterator<Item = CellCoord> + '_ {
        let (x0, y0) = (self.cell.x, self.cell.y);
        let (w, h) = (self.foot_w as i32, self.foot_h as i32);
        (0..h).flat_map(move |dy| (0..w).map(move |dx| CellCoord::new(x0 + dx, y0 + dy)))
    }

    /// Whether `cell` lies within this building's footprint.
    pub fn covers(&self, cell: CellCoord) -> bool {
        cell.x >= self.cell.x
            && cell.y >= self.cell.y
            && cell.x < self.cell.x + self.foot_w as i32
            && cell.y < self.cell.y + self.foot_h as i32
    }

    /// Whether `cell` is orthogonally/diagonally adjacent to (but outside) the
    /// footprint — the ring of cells a mover can dock or exit from.
    pub fn adjacent(&self, cell: CellCoord) -> bool {
        if self.covers(cell) {
            return false;
        }
        cell.x >= self.cell.x - 1
            && cell.y >= self.cell.y - 1
            && cell.x <= self.cell.x + self.foot_w as i32
            && cell.y <= self.cell.y + self.foot_h as i32
    }

    /// Whether the building is alive.
    pub fn is_alive(&self) -> bool {
        self.health > 0
    }

    /// Health as integer permille (0..=1000) of max — for the client's bar.
    pub fn health_permille(&self) -> i32 {
        (self.health as i32 * 1000 / self.max_health.max(1) as i32).clamp(0, 1000)
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u32(self.type_id);
        h.write_u8(self.house);
        h.write_i32(self.cell.x);
        h.write_i32(self.cell.y);
        h.write_u8(self.foot_w);
        h.write_u8(self.foot_h);
        h.write_u16(self.health);
        h.write_u16(self.max_health);
        h.write_i32(self.power);
    }
}
