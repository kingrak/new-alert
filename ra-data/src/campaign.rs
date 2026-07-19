//! Campaign scenario-INI parsing (M7.5): the sections a single-player mission
//! declares beyond terrain + `[UNITS]` — `[INFANTRY]`, `[STRUCTURES]`,
//! `[TERRAIN]`, `[Trigs]`, `[TeamTypes]`, `[Waypoints]`, `[CellTriggers]`,
//! `[Briefing]`, and the per-house `[Basic]`/country sections (Player=, Allies=,
//! Credits=, Edge=).
//!
//! This module is the format layer only: it produces **raw typed defs** (plain
//! structs of ints/strings). The client lifts them into the sim's
//! `ra_sim::campaign` types (resolving names → indices / spawn prototypes),
//! exactly as it lifts rules.ini stats into the sim catalog (DESIGN §4.1).
//!
//! Field orders follow the reference `Read_INI` routines:
//! `InfantryClass::Read_INI`, `BuildingClass::Read_INI`, `TerrainClass::Read_INI`,
//! `TriggerTypeClass::Read_INI` (`trigtype.cpp:2160`), `TeamTypeClass::Read_INI`
//! (`teamtype.cpp:1772`), `DisplayClass::Read_INI` (waypoints / cell triggers).

use ra_formats::ini::Ini;

/// Resolve a house name to its `HousesType` index using the **full** RA table
/// (`HouseTypeClass::From_Name`, `hdata.cpp:380`) — including GoodGuy(8),
/// BadGuy(9), Neutral(10), Special(11), Multi1..8(12..19). Unlike
/// [`crate::house::house_from_name`] (which collapses the campaign aliases onto
/// their colour twin for skirmish colour picking), this preserves the true
/// index the trigger/teamtype integer house fields reference. Case-insensitive.
pub fn campaign_house_index(name: &str) -> Option<u8> {
    let idx = match name.trim().to_ascii_uppercase().as_str() {
        "SPAIN" => 0,
        "GREECE" => 1,
        "USSR" => 2,
        "ENGLAND" => 3,
        "UKRAINE" => 4,
        "GERMANY" => 5,
        "FRANCE" => 6,
        "TURKEY" => 7,
        "GOODGUY" => 8,
        "BADGUY" => 9,
        "NEUTRAL" => 10,
        "SPECIAL" => 11,
        "MULTI1" => 12,
        "MULTI2" => 13,
        "MULTI3" => 14,
        "MULTI4" => 15,
        "MULTI5" => 16,
        "MULTI6" => 17,
        "MULTI7" => 18,
        "MULTI8" => 19,
        _ => return None,
    };
    Some(idx)
}

/// The number of house slots a campaign world allocates (`Spain`..`Multi8`).
pub const CAMPAIGN_HOUSE_COUNT: usize = 20;

/// One `[INFANTRY]` placement: `house,type,strength,cell,subcell,mission,facing,
/// trigger` (`InfantryClass::Read_INI`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfantryPlacement {
    /// Owning house (`HousesType` index).
    pub house: u8,
    /// Infantry type name (`E1`/`DOG`/`EINSTEIN`/…).
    pub unit_type: String,
    /// Strength as a 0..=256 fraction of max.
    pub strength: u16,
    /// Linear cell number.
    pub cell: u32,
    /// Sub-cell spot 0..5 (centre + 4 quadrants).
    pub sub_cell: u8,
    /// Initial mission name.
    pub mission: String,
    /// Body facing (binary angle).
    pub facing: u8,
    /// Attached trigger name (`"None"` = none).
    pub trigger: String,
}

/// One `[STRUCTURES]` placement: `house,type,strength,cell,facing,trigger,
/// sellable,rebuild` (`BuildingClass::Read_INI`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructurePlacement {
    /// Owning house.
    pub house: u8,
    /// Building type name (`POWR`/`TSLA`/`SBAG`/…).
    pub building_type: String,
    /// Strength as a 0..=256 fraction of max.
    pub strength: u16,
    /// Linear cell number of the footprint top-left.
    pub cell: u32,
    /// Facing (binary angle; buildings ignore it except turrets).
    pub facing: u8,
    /// Attached trigger name (`"None"` = none).
    pub trigger: String,
}

/// One `[TERRAIN]` placement: `cell=TypeName` (`TerrainClass::Read_INI`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainPlacement {
    /// Linear cell number.
    pub cell: u32,
    /// Terrain type name (`T01`..`T17`, `TC01`..`TC05`, …).
    pub terrain_type: String,
}

/// A raw parsed trigger (`[Trigs]`): the 18 fields as typed ints + name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerRaw {
    /// Trigger name.
    pub name: String,
    /// Persistence (0/1/2).
    pub persist: u8,
    /// House index the trigger scopes to.
    pub house: i32,
    /// Event-combine style (0..3).
    pub event_ctrl: u8,
    /// Action-combine style (0..3).
    pub action_ctrl: u8,
    /// Event 1 `(code, teamRaw, data)`.
    pub e1: (u8, i32, i32),
    /// Event 2.
    pub e2: (u8, i32, i32),
    /// Action 1 `(code, teamRaw, triggerRaw, data)`.
    pub a1: (u8, i32, i32, i32),
    /// Action 2.
    pub a2: (u8, i32, i32, i32),
}

/// A raw parsed team type (`[TeamTypes]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamTypeRaw {
    /// Team name.
    pub name: String,
    /// House index.
    pub house: i32,
    /// Packed flag bits.
    pub flags: u32,
    /// Recruit priority.
    pub recruit: i32,
    /// Initial number.
    pub init_num: i32,
    /// Max allowed.
    pub max_allowed: i32,
    /// Origin waypoint index (`-1` = none).
    pub origin: i32,
    /// Assigned trigger raw index (`-1` = none).
    pub trigger: i32,
    /// Class list `(typename, count)`.
    pub classes: Vec<(String, u16)>,
    /// Mission list `(code, arg)`.
    pub missions: Vec<(i32, i32)>,
}

/// A per-house scenario definition (from `[Greece]`/`[USSR]`/… sections).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseDef {
    /// House index.
    pub index: u8,
    /// Section name (`"Greece"`).
    pub name: String,
    /// Starting credits (already ×100 as the engine does).
    pub credits: i32,
    /// Allied house names (from `Allies=`).
    pub allies: Vec<String>,
    /// Map edge units reinforce from (`Edge=`), or empty.
    pub edge: String,
}

/// Parse the `[INFANTRY]` section.
pub fn parse_infantry(ini: &Ini) -> Vec<InfantryPlacement> {
    let Some(entries) = ini.section_entries("INFANTRY") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, value) in entries {
        let f: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
        if f.len() < 6 {
            continue;
        }
        let Some(house) = campaign_house_index(f[0]) else {
            continue;
        };
        let Ok(cell) = f[3].parse::<u32>() else {
            continue;
        };
        out.push(InfantryPlacement {
            house,
            unit_type: f[1].to_string(),
            strength: f[2].parse().unwrap_or(256).min(256),
            cell,
            sub_cell: f[4].parse::<u32>().unwrap_or(0).min(4) as u8,
            mission: f[5].to_string(),
            facing: (f.get(6).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) & 0xFF) as u8,
            trigger: f
                .get(7)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "None".into()),
        });
    }
    out
}

/// Parse the `[STRUCTURES]` section.
pub fn parse_structures(ini: &Ini) -> Vec<StructurePlacement> {
    let Some(entries) = ini.section_entries("STRUCTURES") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, value) in entries {
        let f: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
        if f.len() < 5 {
            continue;
        }
        let Some(house) = campaign_house_index(f[0]) else {
            continue;
        };
        let Ok(cell) = f[3].parse::<u32>() else {
            continue;
        };
        out.push(StructurePlacement {
            house,
            building_type: f[1].to_string(),
            strength: f[2].parse().unwrap_or(256).min(256),
            cell,
            facing: (f[4].parse::<i64>().unwrap_or(0) & 0xFF) as u8,
            trigger: f
                .get(5)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "None".into()),
        });
    }
    out
}

/// Parse the `[TERRAIN]` section (`cell=TypeName`).
pub fn parse_terrain(ini: &Ini) -> Vec<TerrainPlacement> {
    let Some(entries) = ini.section_entries("TERRAIN") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in entries {
        if let Ok(cell) = key.trim().parse::<u32>() {
            out.push(TerrainPlacement {
                cell,
                terrain_type: value.trim().to_string(),
            });
        }
    }
    out
}

/// Parse `[Waypoints]` into a 101-slot vector (`-1` = unset).
pub fn parse_waypoints(ini: &Ini) -> Vec<i32> {
    let mut wp = vec![-1i32; 101];
    if let Some(entries) = ini.section_entries("Waypoints") {
        for (key, value) in entries {
            if let (Ok(idx), Ok(cell)) = (key.trim().parse::<usize>(), value.trim().parse::<i32>())
            {
                if idx < wp.len() {
                    wp[idx] = cell;
                }
            }
        }
    }
    wp
}

/// Parse `[CellTriggers]` into `(cell, trigger name)` pairs.
pub fn parse_cell_triggers(ini: &Ini) -> Vec<(u32, String)> {
    let Some(entries) = ini.section_entries("CellTriggers") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in entries {
        if let Ok(cell) = key.trim().parse::<u32>() {
            out.push((cell, value.trim().to_string()));
        }
    }
    out
}

/// Parse the `[Trigs]` section (18 comma-separated fields per line).
pub fn parse_triggers(ini: &Ini) -> Vec<TriggerRaw> {
    let Some(entries) = ini.section_entries("Trigs") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, value) in entries {
        let f: Vec<i32> = value
            .split(',')
            .map(|s| s.trim().parse::<i32>().unwrap_or(0))
            .collect();
        if f.len() < 18 {
            continue;
        }
        out.push(TriggerRaw {
            name: name.to_string(),
            persist: f[0] as u8,
            house: f[1],
            event_ctrl: f[2] as u8,
            action_ctrl: f[3] as u8,
            e1: (f[4] as u8, f[5], f[6]),
            e2: (f[7] as u8, f[8], f[9]),
            a1: (f[10] as u8, f[11], f[12], f[13]),
            a2: (f[14] as u8, f[15], f[16], f[17]),
        });
    }
    out
}

/// Parse the `[TeamTypes]` section (NewINIFormat ≥ 2: packed-flags form).
pub fn parse_teamtypes(ini: &Ini) -> Vec<TeamTypeRaw> {
    let Some(entries) = ini.section_entries("TeamTypes") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, value) in entries {
        // Tokens are split on both ',' and ':' (class/mission pairs use ':').
        let toks: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
        if toks.len() < 8 {
            continue;
        }
        let geti =
            |i: usize| -> i32 { toks.get(i).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0) };
        let house = geti(0);
        let flags = geti(1) as u32;
        let recruit = geti(2);
        let init_num = geti(3);
        let max_allowed = geti(4);
        let origin = geti(5);
        let trigger = geti(6);
        let class_count = geti(7).max(0) as usize;
        let mut idx = 8usize;
        let mut classes = Vec::new();
        for _ in 0..class_count {
            let Some(pair) = toks.get(idx) else { break };
            idx += 1;
            if let Some((cname, cnum)) = pair.split_once(':') {
                classes.push((
                    cname.trim().to_string(),
                    cnum.trim().parse::<u16>().unwrap_or(1),
                ));
            }
        }
        let mission_count = toks
            .get(idx)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(0)
            .max(0) as usize;
        idx += 1;
        let mut missions = Vec::new();
        for _ in 0..mission_count {
            let Some(pair) = toks.get(idx) else { break };
            idx += 1;
            if let Some((code, arg)) = pair.split_once(':') {
                missions.push((
                    code.trim().parse::<i32>().unwrap_or(-1),
                    arg.trim().parse::<i32>().unwrap_or(0),
                ));
            }
        }
        out.push(TeamTypeRaw {
            name: name.to_string(),
            house,
            flags,
            recruit,
            init_num,
            max_allowed,
            origin,
            trigger,
            classes,
            missions,
        });
    }
    out
}

/// Parse the `[Briefing]` section into a single wrapped string (numbered lines
/// joined with spaces, `DisplayClass` briefing text).
pub fn parse_briefing(ini: &Ini) -> String {
    let Some(entries) = ini.section_entries("Briefing") else {
        return String::new();
    };
    let mut parts: Vec<(usize, String)> = Vec::new();
    for (key, value) in entries {
        let idx = key.trim().parse::<usize>().unwrap_or(usize::MAX);
        parts.push((idx, value.trim().to_string()));
    }
    parts.sort_by_key(|(i, _)| *i);
    parts
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the player house index from `[Basic] Player=`.
pub fn parse_player_house(ini: &Ini) -> Option<u8> {
    ini.get("Basic", "Player").and_then(campaign_house_index)
}

/// Parse per-house definitions from the country sections that appear in the INI
/// (credits ×100, allies, edge). Only sections that name a known house are read.
pub fn parse_house_defs(ini: &Ini) -> Vec<HouseDef> {
    let mut out = Vec::new();
    for name in [
        "Spain", "Greece", "USSR", "England", "Ukraine", "Germany", "France", "Turkey", "GoodGuy",
        "BadGuy", "Neutral", "Special", "Multi1", "Multi2", "Multi3", "Multi4", "Multi5", "Multi6",
        "Multi7", "Multi8",
    ] {
        if !ini.has_section(name) {
            continue;
        }
        let Some(index) = campaign_house_index(name) else {
            continue;
        };
        let credits = ini.get_int(name, "Credits").unwrap_or(0) as i32 * 100;
        let allies = ini
            .get(name, "Allies")
            .map(|s| {
                s.split(',')
                    .map(|a| a.trim().to_string())
                    .filter(|a| !a.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let edge = ini.get(name, "Edge").unwrap_or("").to_string();
        out.push(HouseDef {
            index,
            name: name.to_string(),
            credits,
            allies,
            edge,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn house_indices_full_table() {
        assert_eq!(campaign_house_index("Greece"), Some(1));
        assert_eq!(campaign_house_index("USSR"), Some(2));
        assert_eq!(campaign_house_index("GoodGuy"), Some(8));
        assert_eq!(campaign_house_index("Special"), Some(11));
        assert_eq!(campaign_house_index("Nobody"), None);
    }

    #[test]
    fn parses_scg01_style_trigger() {
        // The real `win` line: Greece wins on EVAC_CIVILIAN.
        let ini = Ini::parse("[Trigs]\nwin=0,1,0,0,18,-1,0,0,-1,0,1,-1,-1,-255,0,-1,-1,-1\n");
        let t = parse_triggers(&ini);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "win");
        assert_eq!(t[0].house, 1);
        assert_eq!(t[0].e1.0, 18); // EVAC_CIVILIAN
        assert_eq!(t[0].a1.0, 1); // WIN
    }

    #[test]
    fn parses_teamtype_with_classes_and_missions() {
        // einst=1,0,10,0,0,7,0,1,EINSTEIN:1,1,10:0
        let ini = Ini::parse("[TeamTypes]\neinst=1,0,10,0,0,7,0,1,EINSTEIN:1,1,10:0\n");
        let t = parse_teamtypes(&ini);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].house, 1);
        assert_eq!(t[0].origin, 7);
        assert_eq!(t[0].trigger, 0);
        assert_eq!(t[0].classes, vec![("EINSTEIN".to_string(), 1)]);
        assert_eq!(t[0].missions, vec![(10, 0)]);
    }

    #[test]
    fn parses_infantry_placement() {
        let ini = Ini::parse("[INFANTRY]\n0=USSR,DOG,256,7615,2,Hunt,0,dwig\n");
        let inf = parse_infantry(&ini);
        assert_eq!(inf.len(), 1);
        assert_eq!(inf[0].house, 2);
        assert_eq!(inf[0].unit_type, "DOG");
        assert_eq!(inf[0].cell, 7615);
        assert_eq!(inf[0].sub_cell, 2);
        assert_eq!(inf[0].trigger, "dwig");
    }

    #[test]
    fn house_credits_scaled_by_100() {
        let ini = Ini::parse("[USSR]\nCredits=25\nAllies=Greece,GoodGuy\n");
        let defs = parse_house_defs(&ini);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].credits, 2500);
        assert_eq!(defs[0].allies, vec!["Greece", "GoodGuy"]);
    }
}
