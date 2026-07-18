//! Dynamic **unit** cell occupancy (M7.6) — the reservation layer that keeps the
//! original's cell-ownership invariants:
//!
//! - **One vehicle per cell.** A cell holding a vehicle is not enterable by
//!   another vehicle (`CellClass::Occupier`, `Can_Enter_Cell`, `cell.cpp` /
//!   `unit.cpp:3400`).
//! - **Up to five infantry per cell**, one per sub-cell spot, tracked as a
//!   5-bit occupancy mask exactly like `CellClass::Flag.Occupy` (`cell.h:207`):
//!   bit `i` set = spot `i` taken (bit 0 = centre, 1..4 = the quadrants).
//!
//! This grid is a **cache** fully re-derivable from the units arena (each unit's
//! cell + `sub_cell`, both hashed), so it is *not* itself folded into the state
//! hash — like the building-occupancy layer in [`crate::path::Passability`]. It
//! is rebuilt from current positions at the start of the movement system and
//! maintained through it so in-flight reservations are honoured within a tick.

use crate::arena::Handle;
use crate::coords::{CellCoord, SUBCELL_COUNT};

/// Per-cell unit occupancy: a vehicle occupant handle plus an infantry spot mask.
#[derive(Clone, Debug)]
pub struct UnitGrid {
    width: i32,
    height: i32,
    /// The single vehicle occupying each cell (`None` = no vehicle).
    veh: Vec<Option<Handle>>,
    /// Infantry sub-cell spot occupancy bitmask per cell (bits 0..`SUBCELL_COUNT`).
    spots: Vec<u8>,
}

/// Nearest-neighbour spot search order, verbatim from `_sequence[5][4]`
/// (`cell.cpp:1915`): for each starting spot, the other four spots ordered by
/// increasing distance. Used by [`UnitGrid::closest_free_spot`].
const SEQUENCE: [[u8; 4]; SUBCELL_COUNT] = [
    [1, 2, 3, 4],
    [0, 2, 3, 4],
    [0, 1, 4, 3],
    [0, 1, 4, 2],
    [0, 2, 3, 1],
];

impl UnitGrid {
    /// An empty grid of `width`×`height` cells.
    pub fn new(width: i32, height: i32) -> UnitGrid {
        let n = (width * height) as usize;
        UnitGrid {
            width,
            height,
            veh: vec![None; n],
            spots: vec![0; n],
        }
    }

    /// Clear all occupancy (called before a fresh rebuild each tick).
    pub fn clear(&mut self) {
        for v in &mut self.veh {
            *v = None;
        }
        for s in &mut self.spots {
            *s = 0;
        }
    }

    fn idx(&self, cell: CellCoord) -> Option<usize> {
        if cell.x >= 0 && cell.y >= 0 && cell.x < self.width && cell.y < self.height {
            Some((cell.y * self.width + cell.x) as usize)
        } else {
            None
        }
    }

    /// The vehicle currently occupying `cell`, if any.
    pub fn vehicle_at(&self, cell: CellCoord) -> Option<Handle> {
        self.idx(cell).and_then(|i| self.veh[i])
    }

    /// Whether a vehicle other than `self_handle` occupies `cell` (an off-grid
    /// cell counts as blocked). `self_handle` lets a unit re-enter the cell it
    /// already owns.
    pub fn vehicle_blocked_for(&self, cell: CellCoord, self_handle: Handle) -> bool {
        match self.idx(cell) {
            None => true,
            Some(i) => matches!(self.veh[i], Some(h) if h != self_handle),
        }
    }

    /// Mark `cell` as occupied by vehicle `handle`.
    pub fn claim_vehicle(&mut self, cell: CellCoord, handle: Handle) {
        if let Some(i) = self.idx(cell) {
            self.veh[i] = Some(handle);
        }
    }

    /// Release the vehicle occupancy of `cell` (only if owned by `handle`).
    pub fn release_vehicle(&mut self, cell: CellCoord, handle: Handle) {
        if let Some(i) = self.idx(cell) {
            if self.veh[i] == Some(handle) {
                self.veh[i] = None;
            }
        }
    }

    /// The infantry spot bitmask for `cell` (bit `i` = spot `i` occupied).
    pub fn spot_bits(&self, cell: CellCoord) -> u8 {
        self.idx(cell).map(|i| self.spots[i]).unwrap_or(0x1F)
    }

    /// Whether spot `spot` of `cell` is free (`Is_Spot_Free`, `cell.h:304`).
    pub fn is_spot_free(&self, cell: CellCoord, spot: u8) -> bool {
        (self.spot_bits(cell) & (1 << spot)) == 0
    }

    /// Whether `cell` has at least one free infantry spot.
    pub fn has_free_spot(&self, cell: CellCoord) -> bool {
        (self.spot_bits(cell) & 0x1F) != 0x1F
    }

    /// Claim infantry `spot` of `cell`.
    pub fn claim_spot(&mut self, cell: CellCoord, spot: u8) {
        if let Some(i) = self.idx(cell) {
            self.spots[i] |= 1 << spot;
        }
    }

    /// Release infantry `spot` of `cell`.
    pub fn release_spot(&mut self, cell: CellCoord, spot: u8) {
        if let Some(i) = self.idx(cell) {
            self.spots[i] &= !(1 << spot);
        }
    }

    /// The closest free spot to `desired` in `cell` (`Closest_Free_Spot`,
    /// `cell.cpp:1897`): the desired spot if free, otherwise the first free spot
    /// in the precomputed nearest-neighbour order; `None` if the cell is full.
    ///
    /// Deviation: the original mixes up an occupied *centre* request with a
    /// `Random_Pick`ed `_alternate` row (`cell.cpp:1948`); we deterministically
    /// use the fixed `_sequence[0]` order instead, avoiding a new sim-RNG draw in
    /// the movement path (documented in QUIRKS).
    pub fn closest_free_spot(&self, cell: CellCoord, desired: u8) -> Option<u8> {
        closest_free_spot_bits(self.spot_bits(cell), desired)
    }
}

/// The closest free spot to `desired` given a spot occupancy `bits` mask
/// (`Closest_Free_Spot` over a bare bitmask, `cell.cpp:1897`). Shared by
/// [`UnitGrid::closest_free_spot`] and the spawn-time spot assignment.
pub fn closest_free_spot_bits(bits: u8, desired: u8) -> Option<u8> {
    let desired = desired.min(SUBCELL_COUNT as u8 - 1);
    if bits & (1 << desired) == 0 {
        return Some(desired);
    }
    SEQUENCE[desired as usize]
        .iter()
        .copied()
        .find(|&s| bits & (1 << s) == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(i: u32) -> Handle {
        Handle { index: i, gen: 0 }
    }

    #[test]
    fn one_vehicle_per_cell() {
        let mut g = UnitGrid::new(8, 8);
        let c = CellCoord::new(2, 3);
        assert!(!g.vehicle_blocked_for(c, h(0)));
        g.claim_vehicle(c, h(0));
        // Same unit not blocked; a different unit is.
        assert!(!g.vehicle_blocked_for(c, h(0)));
        assert!(g.vehicle_blocked_for(c, h(1)));
        g.release_vehicle(c, h(0));
        assert!(!g.vehicle_blocked_for(c, h(1)));
    }

    #[test]
    fn five_spots_then_full() {
        let mut g = UnitGrid::new(8, 8);
        let c = CellCoord::new(1, 1);
        let mut claimed = Vec::new();
        for _ in 0..SUBCELL_COUNT {
            let s = g.closest_free_spot(c, 0).expect("a spot should be free");
            assert!(!claimed.contains(&s));
            g.claim_spot(c, s);
            claimed.push(s);
        }
        assert!(!g.has_free_spot(c));
        assert_eq!(g.closest_free_spot(c, 0), None);
    }

    #[test]
    fn closest_free_prefers_desired_then_sequence() {
        let mut g = UnitGrid::new(8, 8);
        let c = CellCoord::new(4, 4);
        assert_eq!(g.closest_free_spot(c, 2), Some(2)); // desired free
        g.claim_spot(c, 2);
        // Desired taken -> first of _sequence[2] = 0.
        assert_eq!(g.closest_free_spot(c, 2), Some(0));
    }

    #[test]
    fn off_grid_is_blocked() {
        let g = UnitGrid::new(4, 4);
        assert!(g.vehicle_blocked_for(CellCoord::new(-1, 0), h(0)));
        assert!(!g.has_free_spot(CellCoord::new(-1, 0)));
    }
}
