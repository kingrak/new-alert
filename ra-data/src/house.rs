//! Houses (the eight RA countries) and the house-colour remap tables.
//!
//! House order is the original's `HousesType` enum (`redalert/defines.h`):
//! Spain, Greece, USSR, England, Ukraine, Germany, France, Turkey. Each country
//! maps to a `PlayerColorType` used to pick a row out of `PALETTE.CPS` — and
//! for these eight the house index and the player-colour index coincide (Spain
//! = Gold = 0, Greece = LtBlue = 1, USSR = Red = 2, England = Green = 3,
//! Ukraine = Orange = 4, Germany = Grey = 5, France = Blue = 6, Turkey = Brown =
//! 7), so the house index doubles as the CPS row.
//!
//! The remap tables are built exactly as `Init_Color_Remaps` (`init.cpp`): row
//! 0 of `PALETTE.CPS` holds the 16 "unity" indices sprites are drawn in; row
//! `p` holds that player colour's 16 replacements. A house's remap LUT is the
//! identity with those 16 source indices redirected — applied to a sprite's
//! palette indices before RGBA expansion (DESIGN.md §3.9).

use ra_formats::cps::Cps;

/// Number of country houses M3 handles (Spain..Turkey).
pub const HOUSE_COUNT: usize = 8;
/// Number of remapped "unity" indices per colour scheme.
const REMAP_BAND: usize = 16;

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

/// Build the eight house remap LUTs from a decoded `PALETTE.CPS` image.
///
/// Port of `Init_Color_Remaps`: for house/player colour `p`, start from the
/// identity and set `table[row0[i]] = row_p[i]` for `i` in `0..16`.
pub fn build_house_remaps(cps: &Cps) -> [RemapTable; HOUSE_COUNT] {
    let mut source = [0u8; REMAP_BAND];
    for (i, s) in source.iter_mut().enumerate() {
        *s = cps.pixel(i, 0);
    }

    let mut tables = [[0u8; 256]; HOUSE_COUNT];
    for (p, table) in tables.iter_mut().enumerate() {
        for (i, entry) in table.iter_mut().enumerate() {
            *entry = i as u8; // identity
        }
        for (i, &src) in source.iter().enumerate() {
            table[src as usize] = cps.pixel(i, p);
        }
    }
    tables
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
