//! Deriving a coarse passable/impassable grid from a scenario's terrain — the
//! M3 stand-in for full per-icon land types (DESIGN.md §3.7: "a simple
//! passable/impassable from M2 terrain data is fine for M3").
//!
//! The original derives movement cost per *icon* from each tileset's control
//! map at load time; we defer that. For now a cell is impassable only if it is
//! open water (`W1`/`W2`) — enough to keep ground units off the sea while
//! leaving shores, rivers, and roads drivable. Shore/river-body tiles are
//! intentionally left passable at M3 (documented deviation), since marking
//! whole shore templates impassable would wall off coastlines a real unit can
//! partly traverse.

use crate::scenario::{Scenario, MAP_CELL_H, MAP_CELL_W};
use crate::templates;

/// Template id `W1` (open water).
const TEMPLATE_WATER1: u16 = 1;
/// Template id `W2` (open water).
const TEMPLATE_WATER2: u16 = 2;

/// Whether a template id is open water a ground unit cannot enter.
pub fn is_water(template: u16) -> bool {
    template == TEMPLATE_WATER1 || template == TEMPLATE_WATER2
}

/// Build a row-major `128*128` passability mask for `scenario`: `true` where a
/// ground unit may move.
pub fn build(scenario: &Scenario) -> Vec<bool> {
    let w = MAP_CELL_W;
    let h = MAP_CELL_H;
    let mut mask = vec![true; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let cell = scenario.cell(x, y);
            // Clear/"no template" sentinels are always passable ground.
            let passable = if cell.template == templates::TEMPLATE_NONE
                || cell.template == 255
                || cell.template == templates::TEMPLATE_CLEAR1
            {
                true
            } else {
                !is_water(cell.template)
            };
            mask[(y * w + x) as usize] = passable;
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{MapCell, Theater};

    fn scenario_with(cells: Vec<MapCell>) -> Scenario {
        Scenario {
            theater: Theater::Snow,
            map_x: 0,
            map_y: 0,
            map_width: 4,
            map_height: 4,
            cells,
            overlay: Vec::new(),
        }
    }

    #[test]
    fn water_is_impassable_clear_is_not() {
        let total = (MAP_CELL_W * MAP_CELL_H) as usize;
        let mut cells = vec![
            MapCell {
                template: 0xFFFF,
                icon: 0
            };
            total
        ];
        cells[0] = MapCell {
            template: TEMPLATE_WATER1,
            icon: 0,
        };
        let mask = build(&scenario_with(cells));
        assert!(!mask[0], "water cell should be impassable");
        assert!(mask[1], "clear cell should be passable");
    }
}
