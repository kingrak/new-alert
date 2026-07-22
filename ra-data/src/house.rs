//! Houses (the eight RA countries) and the house-colour remap tables.
//!
//! House order is the original's `HousesType` enum (`redalert/defines.h`):
//! Spain, Greece, USSR, England, Ukraine, Germany, France, Turkey. Each country
//! maps to a `PlayerColorType` (the CPS row that paints it) via the `RemapColor`
//! field of its `HouseTypeClass` (`HDATA.CPP:46-124`):
//!
//! | house idx | country | `RemapColor`          | CPS row |
//! |-----------|---------|-----------------------|---------|
//! | 0         | Spain   | `PCOLOR_GOLD`         | 0       |
//! | 1         | Greece  | `PCOLOR_LTBLUE`       | 1       |
//! | 2         | USSR    | `PCOLOR_RED`          | 2       |
//! | 3         | England | `PCOLOR_GREEN`        | 3       |
//! | 4         | Ukraine | `PCOLOR_ORANGE`       | 4       |
//! | 5         | Germany | `PCOLOR_GREY`         | **6**   |
//! | 6         | France  | `PCOLOR_BLUE`         | **5**   |
//! | 7         | Turkey  | `PCOLOR_BROWN`        | 7       |
//!
//! The house index and the CPS row coincide for six of the eight, but **not**
//! for Germany and France: the `PlayerColorType` enum orders BLUE (5) before
//! GREY (6) (`DEFINES.H:1226-1235`) while the house enum orders Germany (5)
//! before France (6). Assuming `row == house index` therefore paints Germany
//! blue and France grey — the sibling of the reported "I selected blue, I got
//! green" colour bug. [`HOUSE_PCOLOR`] carries the true `RemapColor` per house.
//!
//! The remap tables are built exactly as `Init_Color_Remaps` (`INIT.CPP:2639`):
//! row 0 of `PALETTE.CPS` holds the 16 "unity" indices sprites are drawn in; row
//! `p` holds `PlayerColorType` `p`'s 16 replacements. A colour's remap LUT is the
//! identity with those 16 source indices redirected — applied to a sprite's
//! palette indices before RGBA expansion (DESIGN.md §3.9).

use ra_formats::cps::Cps;

/// Number of country houses M3 handles (Spain..Turkey).
pub const HOUSE_COUNT: usize = 8;
/// Number of selectable player colours (`PlayerColorType` GOLD..BROWN).
pub const COLOR_COUNT: usize = 8;
/// Number of remapped "unity" indices per colour scheme.
const REMAP_BAND: usize = 16;

/// Each house's `PlayerColorType` — the `PALETTE.CPS` row that paints it — in
/// house-index order (Spain..Turkey). Ported from the `RemapColor` argument of
/// each `HouseTypeClass` (`HDATA.CPP:46-124`). Note Germany→6 (grey) and
/// France→5 (blue) do **not** equal their house index.
pub const HOUSE_PCOLOR: [u8; HOUSE_COUNT] = [0, 1, 2, 3, 4, 6, 5, 7];

/// Resolve a scenario `[UNITS]` house name to its house index (0..8), matching
/// `HouseTypeClass::From_Name`. Case-insensitive. `None` for unknown names.
pub fn house_from_name(name: &str) -> Option<u8> {
    let idx = match name.trim().to_ascii_uppercase().as_str() {
        "SPAIN" => 0,
        "GREECE" => 1,
        "USSR" => 2,
        "ENGLAND" => 3,
        "UKRAINE" => 4,
        "GERMANY" => 5,
        "FRANCE" => 6,
        "TURKEY" => 7,
        // Campaign/multi aliases the missions use, mapped to their colour twin.
        "GOODGUY" => 1, // GDI-ish -> LtBlue
        "BADGUY" => 2,  // -> Red
        "NEUTRAL" | "SPECIAL" => 0,
        _ => return None,
    };
    Some(idx)
}

/// A 256-entry palette-index remap LUT for one house.
pub type RemapTable = [u8; 256];

/// Build one remap LUT for `PlayerColorType` (`PALETTE.CPS` row) `pcolor`.
///
/// Port of the inner loop of `Init_Color_Remaps` (`INIT.CPP:2643-2650`): start
/// from the identity and set `table[row0[i]] = row_pcolor[i]` for `i` in `0..16`
/// (`ptr[Get_Pixel(i,0)] = Get_Pixel(i,pcolor)`).
pub fn build_color_remap(cps: &Cps, pcolor: u8) -> RemapTable {
    let mut table = [0u8; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        *entry = i as u8; // identity
    }
    for i in 0..REMAP_BAND {
        let src = cps.pixel(i, 0);
        table[src as usize] = cps.pixel(i, pcolor as usize);
    }
    table
}

/// Build the eight **player-colour** remap LUTs, indexed by `PlayerColorType`
/// (row 0 = GOLD … row 7 = BROWN). Use this when the caller has a raw colour
/// choice (e.g. the skirmish colour picker) rather than a house.
pub fn build_color_remaps(cps: &Cps) -> [RemapTable; COLOR_COUNT] {
    core::array::from_fn(|p| build_color_remap(cps, p as u8))
}

/// Build the eight **house** remap LUTs from a decoded `PALETTE.CPS` image,
/// indexed by house (Spain..Turkey). Each house's table uses its true
/// `RemapColor` row ([`HOUSE_PCOLOR`]) — so Germany wears grey (row 6) and
/// France wears blue (row 5), not the blue/grey that a naive `row == house`
/// mapping would give.
pub fn build_house_remaps(cps: &Cps) -> [RemapTable; HOUSE_COUNT] {
    core::array::from_fn(|h| build_color_remap(cps, HOUSE_PCOLOR[h]))
}

/// The identity remap (Spain/Gold is the unmodified art, but this is a safe
/// fallback for any out-of-range house index too).
pub fn identity_remap() -> RemapTable {
    let mut t = [0u8; 256];
    for (i, e) in t.iter_mut().enumerate() {
        *e = i as u8;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve() {
        assert_eq!(house_from_name("USSR"), Some(2));
        assert_eq!(house_from_name("greece"), Some(1));
        assert_eq!(house_from_name("England"), Some(3));
        assert_eq!(house_from_name("Nowhere"), None);
    }

    #[test]
    fn identity_is_identity() {
        let t = identity_remap();
        assert!(t.iter().enumerate().all(|(i, &v)| v as usize == i));
    }
}
