//! Turn a parsed scenario plus its theater templates into a single indexed-color
//! terrain raster the [`crate::compositor`] can palette-map into any viewport.
//!
//! This is where the RA-specific cell → icon resolution lives (clear-terrain
//! scrambling, the `TType ∈ {0, 255, 0xFFFF}` clear test), following
//! `redalert/cell.cpp` (`Draw_It`, `Clear_Icon`).

use std::collections::HashMap;

use ra_data::scenario::{Scenario, MAP_CELL_H, MAP_CELL_W};
use ra_data::templates;
use ra_formats::tmpl::{Template, ICON_HEIGHT, ICON_WIDTH};

use crate::compositor::IndexedImage;

/// The clear-terrain template id (`CLEAR1`).
const CLEAR_TEMPLATE: u16 = templates::TEMPLATE_CLEAR1;

/// A resolved set of theater templates, keyed by template id.
#[derive(Debug, Default)]
pub struct TileSet {
    templates: HashMap<u16, Template>,
}

impl TileSet {
    /// Build an empty tile set.
    pub fn new() -> TileSet {
        TileSet {
            templates: HashMap::new(),
        }
    }

    /// Insert a parsed template under its id.
    pub fn insert(&mut self, id: u16, template: Template) {
        self.templates.insert(id, template);
    }

    /// Whether a template id is loaded.
    pub fn contains(&self, id: u16) -> bool {
        self.templates.contains_key(&id)
    }

    /// Number of loaded templates.
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    /// Whether no templates are loaded.
    pub fn is_empty(&self) -> bool {
        self.templates.is_empty()
    }

    /// Resolve a cell's drawable icon: the 24×24 indexed pixels and whether
    /// index 0 is transparent. `None` when nothing should be drawn.
    fn cell_icon(&self, template: u16, icon: u8) -> Option<(&[u8], bool)> {
        let t = self.templates.get(&template)?;
        let ic = t.icon(icon as usize)?;
        Some((ic.pixels, ic.transparent))
    }
}

/// Is this template id a "clear" cell (rendered from CLEAR1 with a scrambled
/// icon)? Matches `cell.cpp`: `TType == TEMPLATE_NONE || TEMPLATE_CLEAR1 || 255`.
fn is_clear(template: u16) -> bool {
    template == CLEAR_TEMPLATE || template == 0xFFFF || template == 255
}

/// The scrambled clear-terrain icon for cell (x, y): `(x&3) | ((y&3)<<2)`
/// (`CellClass::Clear_Icon`).
fn clear_icon(x: u32, y: u32) -> u8 {
    ((x & 0x03) | ((y & 0x03) << 2)) as u8
}

/// Rasterize the whole 128×128 map into one indexed-color image
/// (3072×3072 px). Cells whose template is missing or clear fall back to the
/// CLEAR1 template; if even that is absent the cell is left as index 0.
pub fn rasterize(scenario: &Scenario, tiles: &TileSet) -> IndexedImage {
    let w = MAP_CELL_W * ICON_WIDTH as u32;
    let h = MAP_CELL_H * ICON_HEIGHT as u32;
    let mut img = IndexedImage::filled(w, h, 0);

    for cy in 0..MAP_CELL_H {
        for cx in 0..MAP_CELL_W {
            let cell = scenario.cell(cx, cy);
            let (template, icon) = if is_clear(cell.template) {
                (CLEAR_TEMPLATE, clear_icon(cx, cy))
            } else {
                (cell.template, cell.icon)
            };

            let resolved = tiles
                .cell_icon(template, icon)
                // Fall back to clear terrain if the named template is missing.
                .or_else(|| tiles.cell_icon(CLEAR_TEMPLATE, clear_icon(cx, cy)));

            if let Some((pixels, transparent)) = resolved {
                img.blit_tile(
                    (cx * ICON_WIDTH as u32) as i64,
                    (cy * ICON_HEIGHT as u32) as i64,
                    pixels,
                    ICON_WIDTH as u32,
                    ICON_HEIGHT as u32,
                    transparent,
                );
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_icon_scramble() {
        assert_eq!(clear_icon(0, 0), 0);
        assert_eq!(clear_icon(3, 0), 3);
        assert_eq!(clear_icon(0, 3), 12);
        assert_eq!(clear_icon(5, 6), (5 & 3) | ((6 & 3) << 2)); // wraps low bits
    }

    #[test]
    fn is_clear_sentinels() {
        assert!(is_clear(0));
        assert!(is_clear(255));
        assert!(is_clear(0xFFFF));
        assert!(!is_clear(3));
    }

    #[test]
    fn empty_tileset_produces_full_size_blank() {
        // A scenario with no MapPack can't be built here without assets, so just
        // exercise rasterize's sizing with an empty tileset over a synthesized
        // all-clear scenario.
        let scen = Scenario {
            theater: ra_data::scenario::Theater::Snow,
            map_x: 1,
            map_y: 1,
            map_width: 4,
            map_height: 4,
            cells: vec![
                ra_data::scenario::MapCell {
                    template: 0xFFFF,
                    icon: 0
                };
                (MAP_CELL_W * MAP_CELL_H) as usize
            ],
            overlay: Vec::new(),
        };
        let tiles = TileSet::new();
        let img = rasterize(&scen, &tiles);
        assert_eq!(img.width, MAP_CELL_W * 24);
        assert_eq!(img.height, MAP_CELL_H * 24);
        assert!(img.pixels.iter().all(|&p| p == 0)); // nothing drawn
    }
}
