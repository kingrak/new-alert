//! Building types from rules.ini plus the **code-defined** footprint table
//! (DESIGN.md §3.8, §4.9 M5). Building stats (`Cost`, `Power`, `Strength`,
//! `Armor`, `Prerequisite`, `TechLevel`) are data in rules.ini, but a building's
//! footprint (`BSIZE`) lives in the original's C++ `BuildingTypeClass` table
//! (`redalert/bdata.cpp`), not the INI — so, exactly like the turret capability
//! (`ra_data::combat::turret_equipped`), it is ported here as a small named
//! table rather than read from the INI.
//!
//! **Footprint table** (ported from `bdata.cpp` `SIZE` fields crossed with
//! `BuildingTypeClass::Width`/`Height`, `bdata.cpp:3431/3451`, which map the
//! `BSIZE_WH` enum to `width[BSIZE]`/`height[BSIZE]`):
//! `width  = {1,2,1,2,2,3,3,4,5}`, `height = {1,1,2,2,3,2,3,2,5}` indexed by
//! `BSIZE_{11,21,12,22,23,32,33,42,55}`. The starter buildings resolve to:
//! FACT `BSIZE_33`→3×3, PROC `BSIZE_33`→3×3, POWR `BSIZE_22`→2×2,
//! WEAP `BSIZE_32`→3×2. We use the full W×H rectangle as the occupied
//! footprint (deviation: the original's `Occupy_List` can leave a few interior
//! cells unoccupied; the full rectangle is a faithful-enough M5 simplification).

use ra_formats::ini::Ini;

use crate::combat::armor_index;

/// The building's resolved static stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildingStats {
    /// Footprint width in cells.
    pub foot_w: u8,
    /// Footprint height in cells.
    pub foot_h: u8,
    /// Max strength / hit points (`Strength=`).
    pub strength: u16,
    /// Armor class index (`Armor=`).
    pub armor: u8,
    /// Net power: `+` output, `-` drain (`Power=`).
    pub power: i32,
    /// Build cost in credits (`Cost=`).
    pub cost: i32,
    /// Tech level (`TechLevel=`; `-1` = never appears in the build menu).
    pub tech_level: i32,
    /// Prerequisite building short-names (lowercased), from `Prerequisite=`.
    pub prereq: Vec<String>,
    /// `Sight=` range in cells — how far the structure reveals the shroud on
    /// placement (M6). Defaults to 4 when absent.
    pub sight: u8,
    /// Credit storage (`Storage=`) — refineries/silos; 0 otherwise (M7.7 Chunk C).
    pub storage: i32,
}

/// The code-defined footprint (width, height) in cells for a building short
/// name, or `None` for a building this milestone does not model. Ported from
/// `bdata.cpp` (see module docs).
pub fn footprint(name: &str) -> Option<(u8, u8)> {
    let (w, h) = match name.trim().to_ascii_uppercase().as_str() {
        "FACT" => (3, 3),          // construction yard, BSIZE_33 (bdata.cpp:658)
        "PROC" => (3, 3),          // ore refinery,        BSIZE_33 (bdata.cpp:748)
        "POWR" => (2, 2),          // power plant,         BSIZE_22 (bdata.cpp:983)
        "APWR" => (3, 3),          // advanced power,      BSIZE_33
        "ATEK" => (2, 3),          // allied tech centre,  BSIZE_23
        "STEK" => (3, 3),          // soviet tech centre,  BSIZE_33
        "WEAP" => (3, 2),          // war factory,         BSIZE_32 (bdata.cpp:394)
        "FIX" => (3, 2),           // service depot,       BSIZE_32
        "SILO" => (1, 1),          // ore silo,            BSIZE_11
        "BARR" | "TENT" => (2, 2), // barracks,   BSIZE_22
        "DOME" => (2, 2),          // radar dome,          BSIZE_22
        // --- Defenses (M7.7 Chunk B) ---
        "PBOX" => (1, 1), // pillbox,             BSIZE_11 (bdata.cpp)
        "HBOX" => (1, 1), // camo pillbox,        BSIZE_11
        "GUN" => (1, 1),  // gun turret,          BSIZE_11
        "FTUR" => (1, 1), // flame turret,        BSIZE_11
        "TSLA" => (1, 2), // tesla coil,          BSIZE_12
        // --- Walls (M7.7 Chunk B) — 1×1 buildable segments ---
        "SBAG" | "CYCL" | "BRIK" => (1, 1),
        _ => return None,
    };
    Some((w, h))
}

/// Parse a `Prerequisite=` list into lowercased short-names. `"none"` (and the
/// absence of the key) yield an empty list.
fn parse_prereq(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty() && t != "none")
        .collect()
}

/// Resolve a building type's stats from `rules` by its short name (e.g.
/// `"POWR"`). Returns `None` if the section is absent, has no `Cost=`, or the
/// building has no modelled footprint.
pub fn building_stats(rules: &Ini, name: &str) -> Option<BuildingStats> {
    if !rules.has_section(name) {
        return None;
    }
    let (foot_w, foot_h) = footprint(name)?;
    let cost = rules.get_int(name, "Cost")? as i32;
    let strength = rules
        .get_int(name, "Strength")
        .unwrap_or(1)
        .clamp(1, u16::MAX as i64) as u16;
    let armor = armor_index(rules.get(name, "Armor").unwrap_or("none"));
    let power = rules.get_int(name, "Power").unwrap_or(0) as i32;
    let tech_level = rules.get_int(name, "TechLevel").unwrap_or(-1) as i32;
    let prereq = rules
        .get(name, "Prerequisite")
        .map(parse_prereq)
        .unwrap_or_default();
    let sight = rules.get_int(name, "Sight").unwrap_or(4).clamp(0, 10) as u8;
    let storage = rules.get_int(name, "Storage").unwrap_or(0) as i32;

    Some(BuildingStats {
        foot_w,
        foot_h,
        strength,
        armor,
        power,
        cost,
        tech_level,
        prereq,
        sight,
        storage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Ini {
        // Trimmed real rules.ini values for the starter structures.
        Ini::parse(
            "[FACT]\nStrength=1000\nArmor=heavy\nTechLevel=-1\nCost=2500\nPower=0\n\
             [POWR]\nPrerequisite=fact\nStrength=400\nArmor=wood\nTechLevel=1\nCost=300\nPower=100\n\
             [PROC]\nPrerequisite=powr\nStrength=900\nArmor=wood\nTechLevel=1\nCost=2000\nPower=-30\n\
             [WEAP]\nPrerequisite=proc\nStrength=1000\nArmor=light\nTechLevel=3\nCost=2000\nPower=-30\n",
        )
    }

    #[test]
    fn footprints_match_bdata() {
        assert_eq!(footprint("FACT"), Some((3, 3)));
        assert_eq!(footprint("PROC"), Some((3, 3)));
        assert_eq!(footprint("POWR"), Some((2, 2)));
        assert_eq!(footprint("WEAP"), Some((3, 2)));
        assert_eq!(footprint("weap"), Some((3, 2))); // case-insensitive
        assert_eq!(footprint("NOPE"), None);
    }

    #[test]
    fn resolves_powr() {
        let s = building_stats(&rules(), "POWR").unwrap();
        assert_eq!((s.foot_w, s.foot_h), (2, 2));
        assert_eq!(s.cost, 300);
        assert_eq!(s.power, 100); // output
        assert_eq!(s.armor, 1); // wood
        assert_eq!(s.prereq, vec!["fact".to_string()]);
    }

    #[test]
    fn resolves_proc_drain() {
        let s = building_stats(&rules(), "PROC").unwrap();
        assert_eq!(s.power, -30); // drain
        assert_eq!(s.cost, 2000);
        assert_eq!(s.prereq, vec!["powr".to_string()]);
    }

    #[test]
    fn construction_yard_has_no_prereq() {
        let s = building_stats(&rules(), "FACT").unwrap();
        assert!(s.prereq.is_empty());
        assert_eq!((s.foot_w, s.foot_h), (3, 3));
    }
}
