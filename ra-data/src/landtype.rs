//! Per-cell land types and per-locomotor passability (DESIGN.md §3.7 — real land
//! types, replacing the M3 water-only stand-in). Ported from the original's
//! `LandType` pipeline:
//!
//! - A cell's land type is **data-driven per icon** from the theater tileset's
//!   ColorMap control byte, mapped through a fixed 16-entry table
//!   (`TemplateTypeClass::Land_Type`, `cdata.cpp:1011-1041`). There is no
//!   per-template land field.
//! - Whether a locomotor may enter a cell is `Ground[land].Cost[speed] != 0`
//!   (`unit.cpp:3429`, `infantry.cpp:1568`): a `0%` cost means impassable. The
//!   costs come from rules.ini `[Clear]/[Road]/[Water]/[Rock]/[Wall]/[Ore]/
//!   [Beach]/[Rough]/[River]` `Foot=`/`Track=`/`Wheel=` (`rules.cpp:831-852`).
//!
//! Speed *modifiers* per land class (the `<100%` costs) are not modelled this
//! pass — only impassability (`cost == 0`), which is the movement-correctness
//! must-have; the fractional speeds are a documented deferral.

use ra_formats::ini::Ini;

/// Land types in `LandType` enum order (`defines.h:2919-2934`). Index 5
/// (`LAND_TIBERIUM`) is the rules.ini section `[Ore]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandType {
    Clear = 0,
    Road = 1,
    Water = 2,
    Rock = 3,
    Wall = 4,
    Ore = 5,
    Beach = 6,
    Rough = 7,
    River = 8,
}

/// Number of land types (`LAND_COUNT`).
pub const LAND_COUNT: usize = 9;

/// Movement locomotor column index into a land's cost row (`SpeedType`,
/// `defines.h:3132`): Foot=0, Track=1, Wheel=2.
pub const LOCO_FOOT: usize = 0;
pub const LOCO_TRACK: usize = 1;
pub const LOCO_WHEEL: usize = 2;

/// Map a tileset ColorMap control byte (0..15) to a [`LandType`] — verbatim from
/// the `_land[16]` table in `TemplateTypeClass::Land_Type` (`cdata.cpp:1015-1035`).
/// Only bytes 6/8/9/10/11/14 are non-clear; everything else is clear terrain.
pub fn land_from_control(byte: u8) -> LandType {
    match byte & 0x0F {
        6 => LandType::Beach,
        8 => LandType::Rock,
        9 => LandType::Road,
        10 => LandType::Water,
        11 => LandType::River,
        14 => LandType::Rough,
        _ => LandType::Clear,
    }
}

/// Per-land, per-locomotor passability resolved from rules.ini. `passable[land]
/// [loco]` is `true` when `Ground[land].Cost[loco] != 0` (drivable), `false` for
/// an impassable `0%` cost.
#[derive(Clone, Debug)]
pub struct LandCosts {
    passable: [[bool; 3]; LAND_COUNT],
}

impl LandCosts {
    /// The rules.ini section name for each land type, in enum order
    /// (`rules.cpp:837`: index 5 is `[Ore]`, index 4 is `[Wall]`).
    const SECTIONS: [&'static str; LAND_COUNT] = [
        "Clear", "Road", "Water", "Rock", "Wall", "Ore", "Beach", "Rough", "River",
    ];

    /// Parse the land-cost sections from `rules`. A land section that is present
    /// sets Foot/Track/Wheel from its keys (a missing key defaults to 100% =
    /// passable, matching `Get_Fixed(..., 1)`). A land section that is **absent**
    /// falls back to the retail-stock impassability for the hard-blocking
    /// terrains (Water/Rock/Wall/River impassable to all ground locomotors) and
    /// passable otherwise — so movement is correct even against a trimmed
    /// rules.ini, while a real rules.ini drives the exact values
    /// (`rules.cpp:831-852`; zero-init `Ground[]` means an absent section is
    /// impassable, but the shipped rules.ini always defines all nine).
    pub fn from_rules(rules: &Ini) -> LandCosts {
        // Stock fallbacks (used only when a section is absent): true = passable.
        // Water/Rock/Wall/River block every ground locomotor; the rest are
        // passable (speed reductions on Beach/Rough are deferred).
        let stock: [[bool; 3]; LAND_COUNT] = [
            [true, true, true],    // Clear
            [true, true, true],    // Road
            [false, false, false], // Water
            [false, false, false], // Rock
            [false, false, false], // Wall
            [true, true, true],    // Ore
            [true, true, true],    // Beach
            [true, true, true],    // Rough
            [false, false, false], // River
        ];
        let mut passable = stock;
        for (land, sec) in Self::SECTIONS.iter().enumerate() {
            if !rules.has_section(sec) {
                continue;
            }
            // Present: read each locomotor; missing key => 100% (passable).
            let read = |key: &str| -> bool { land_cost_passable(rules, sec, key) };
            passable[land] = [read("Foot"), read("Track"), read("Wheel")];
        }
        LandCosts { passable }
    }

    /// Whether locomotor column `loco` (0=Foot,1=Track,2=Wheel) may enter a cell
    /// of land type `land`.
    pub fn passable(&self, land: LandType, loco: usize) -> bool {
        self.passable[land as usize][loco.min(2)]
    }
}

/// Read a land-cost percentage key and return whether it is non-zero (passable).
/// A `0`/`0%` value (or an explicit `no`) is impassable; a missing key defaults
/// to passable (100%). Parses the leading integer of a `NN%`/`NN` value.
fn land_cost_passable(rules: &Ini, section: &str, key: &str) -> bool {
    match rules.get(section, key) {
        None => true,
        Some(v) => {
            let s = v.trim();
            let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            match digits.parse::<i64>() {
                Ok(n) => n != 0,
                // Non-numeric (e.g. blank): treat as passable default.
                Err(_) => true,
            }
        }
    }
}

/// The movement locomotor for a unit type — a small named table (the original
/// reads `Tracked=` per unit in `udata.cpp:1301`, defaulting tanks to tracked and
/// everything else wheeled; infantry are always foot in `idata.cpp:1081`).
/// Returns `LOCO_TRACK`/`LOCO_WHEEL`; infantry callers pass [`LOCO_FOOT`]
/// directly. Unlisted vehicles default to wheeled (the ctor default,
/// `udata.cpp:839`).
pub fn vehicle_locomotor(name: &str) -> usize {
    match name.trim().to_ascii_uppercase().as_str() {
        // Tracked vehicles (Tracked=yes): the tanks + tracked specials.
        "1TNK" | "2TNK" | "3TNK" | "4TNK" | "HTNK" | "MTNK" | "TTNK" | "FTNK" | "STNK" | "ARTY"
        | "MCV" | "HARV" | "MNLY" | "MRJ" | "MGG" => LOCO_TRACK,
        _ => LOCO_WHEEL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_bytes_map_to_land() {
        assert_eq!(land_from_control(0), LandType::Clear);
        assert_eq!(land_from_control(8), LandType::Rock);
        assert_eq!(land_from_control(10), LandType::Water);
        assert_eq!(land_from_control(11), LandType::River);
        assert_eq!(land_from_control(14), LandType::Rough);
        assert_eq!(land_from_control(6), LandType::Beach);
        assert_eq!(land_from_control(9), LandType::Road);
    }

    #[test]
    fn absent_sections_block_hard_terrain() {
        let costs = LandCosts::from_rules(&Ini::parse(""));
        assert!(costs.passable(LandType::Clear, LOCO_FOOT));
        assert!(!costs.passable(LandType::Rock, LOCO_TRACK));
        assert!(!costs.passable(LandType::Water, LOCO_WHEEL));
        assert!(!costs.passable(LandType::River, LOCO_FOOT));
        assert!(costs.passable(LandType::Rough, LOCO_FOOT));
    }

    #[test]
    fn rules_zero_cost_is_impassable() {
        // A [Rough] present with Wheel=0 blocks wheels but not foot.
        let ini = Ini::parse("[Rough]\nFoot=90%\nTrack=50%\nWheel=0%\n");
        let costs = LandCosts::from_rules(&ini);
        assert!(costs.passable(LandType::Rough, LOCO_FOOT));
        assert!(costs.passable(LandType::Rough, LOCO_TRACK));
        assert!(!costs.passable(LandType::Rough, LOCO_WHEEL));
    }

    #[test]
    fn vehicle_locomotors() {
        assert_eq!(vehicle_locomotor("2TNK"), LOCO_TRACK);
        assert_eq!(vehicle_locomotor("JEEP"), LOCO_WHEEL);
        assert_eq!(vehicle_locomotor("APC"), LOCO_WHEEL);
    }
}
