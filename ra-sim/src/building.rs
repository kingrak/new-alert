//! The `Building` entity (DESIGN.md §4.9 M5) — a second entity arena alongside
//! units (§5 "Entity arenas: per-kind"). A building is a plain component struct:
//! a footprint anchored at a top-left cell, health, owning house, a net power
//! value, and a couple of role flags (refinery / construction yard / war
//! factory) the systems match on at their boundary (§3.1: closed-enum match, not
//! scattered `What_Am_I()` casts).

use crate::coords::{CellCoord, Facing};
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
    /// Armor class index (`Armor=`, `techno.cpp:7059`). Selects the column of an
    /// attacker's warhead `Verses` matrix — buildings take damage through the
    /// same `modify_damage` path as units (`object.cpp:1661`).
    pub armor: u8,
    /// Sight range in cells (`Sight=`) — reveals the shroud on placement (M6).
    pub sight: u8,
    /// Build cost in credits (`Cost=`) — for the sell refund (M6).
    pub cost: i32,
    /// Net power (positive output, negative drain) — mirrored into the owning
    /// house's power totals when placed/removed.
    pub power: i32,
    /// Refinery (PROC): a harvester dock.
    pub is_refinery: bool,
    /// Construction yard (CONST/FACT): builds structures.
    pub is_construction_yard: bool,
    /// War factory (WEAP): builds vehicles.
    pub is_war_factory: bool,
    /// Barracks (TENT/BARR): builds infantry (M7.6).
    pub is_barracks: bool,

    // --- Defense combat (M7.7 Chunk B) ---
    /// Defensive weapon, or `None` for a non-combat structure.
    pub weapon: Option<crate::combat::WeaponProfile>,
    /// Whether it aims an independently-rotating turret (GUN).
    pub has_turret: bool,
    /// Whether the weapon charges up before firing (tesla coil).
    pub charges: bool,
    /// Turret/emplacement facing (binary angle) — the direction it last aimed.
    pub turret_facing: Facing,
    /// Rearm countdown in ticks (`Arm`): 0 = ready to fire.
    pub arm: u16,
    /// Charge-up countdown in ticks (tesla): counts up while charging; fires the
    /// bolt when it reaches the charge time. 0 = not charging.
    pub charge: u16,
    /// Current auto-acquired attack target (`TarCom`), if any.
    pub target: Option<crate::combat::Target>,
    /// Wall segment (SBAG/CYCL/BRIK) — blocks movement, attackable, not a base
    /// structure (see QUIRKS Q9).
    pub is_wall: bool,
    /// Credit storage this structure adds to its house's cap (`Storage=`).
    pub storage: i32,
    /// Player has toggled repair on this building (`BuildingClass::IsRepairing`,
    /// `building.cpp:1669`). While set, `run_building_repair` heals it on the
    /// global repair cadence, draining credits per step; it clears itself at full
    /// health or when the house can't pay. Defaults `false` (M7.9 P1). Hashed only
    /// when `true`, so no non-repairing building perturbs an existing golden.
    pub is_repairing: bool,
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

    /// Whether this is an armed defense building (fires through the combat path).
    pub fn is_combat(&self) -> bool {
        self.weapon.is_some()
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
        // `armor`, `sight`, and `cost` are constants derived from `type_id` (which
        // is already hashed above), so they are not folded again. Likewise
        // `weapon`/`has_turret`/`charges`/`is_wall` are type constants.
        //
        // Defense combat *state* (turret facing, rearm, charge, target) changes
        // over time and is folded in ONLY for armed buildings — appending no bytes
        // for ordinary structures, so every pre-Chunk-B golden (no armed building)
        // hashes byte-identically.
        // Repair toggle (M7.9 P1): folded in ONLY while actively repairing, so a
        // building that has never been ordered to repair (every pre-M7.9 golden)
        // appends no byte and hashes identically.
        if self.is_repairing {
            h.write_u8(0x5A);
        }
        if self.is_combat() {
            h.write_u8(0xDE);
            h.write_u8(self.turret_facing.0);
            h.write_u16(self.arm);
            h.write_u16(self.charge);
            match self.target {
                None => h.write_u8(0),
                Some(crate::combat::Target::Unit(t)) => {
                    h.write_u8(1);
                    h.write_u32(t.index);
                    h.write_u32(t.gen);
                }
                Some(crate::combat::Target::Building(t)) => {
                    h.write_u8(2);
                    h.write_u32(t.index);
                    h.write_u32(t.gen);
                }
                Some(crate::combat::Target::Cell(c)) => {
                    h.write_u8(3);
                    h.write_i32(c.x);
                    h.write_i32(c.y);
                }
            }
        }
    }
}
