//! Static build data — the sim's `TypeClass` layer expressed as plain data
//! (DESIGN.md §3.8, §4.9 M5). This is the immutable catalog of what a house can
//! build: building and unit *prototypes* with their cost, footprint, power,
//! prerequisites, and the runtime stats a completed object is spawned with.
//!
//! Like [`crate::unit::MoveStats`] and [`crate::combat::WeaponProfile`], these
//! prototypes are **lifted from `ra-data`** (rules.ini + the code-defined
//! footprint table) by the client at load time and handed to the sim, so
//! `ra-sim` stays off the INI layer (§4.1). The catalog is immutable, so it is
//! not folded into the per-tick state hash.

use crate::combat::WeaponProfile;
use crate::unit::MoveStats;

/// Economy constants, from rules.ini `[General]` (defaults are the RA stock
/// values, `redalert/rules.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EconRules {
    /// Credits per gold bail (`GoldValue`, rules.cpp:231, default 35).
    pub gold_value: i32,
    /// Credits per gem bail (`GemValue`, rules.cpp:232, default 110).
    pub gem_value: i32,
    /// Harvester capacity in bails (`BailCount`, rules.cpp:230, default 28).
    pub bail_count: u16,
    /// Ticks between harvest/dump steps (`OreDumpRate`, rules.cpp:175, default 2).
    pub ore_dump_rate: u16,
    /// Ticks per game minute (`TICKS_PER_MINUTE`, defines.h:3122 = 15*60 = 900).
    pub ticks_per_minute: i32,
    /// Production installment steps (`STEP_COUNT`, factory.h:118 = 54).
    pub step_count: i32,
    /// Long ore-scan radius in cells (`TiberiumLongScan/CELL`, rules.cpp:267 =
    /// 0x2000 leptons = 32 cells).
    pub long_scan_cells: i32,
    /// Short ore-scan radius in cells (`TiberiumShortScan/CELL`, rules.cpp:266 =
    /// 0x0600 leptons = 6 cells).
    pub short_scan_cells: i32,
}

impl Default for EconRules {
    fn default() -> EconRules {
        EconRules {
            gold_value: 35,
            gem_value: 110,
            bail_count: 28,
            ore_dump_rate: 2,
            ticks_per_minute: 900,
            step_count: 54,
            long_scan_cells: 32,
            short_scan_cells: 6,
        }
    }
}

/// A buildable building type (footprint + stats + prerequisites).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildingProto {
    /// Human-facing short name (rules.ini section), e.g. `"POWR"`.
    pub name: String,
    /// Footprint width in cells (from the code-defined BSIZE table).
    pub foot_w: u8,
    /// Footprint height in cells.
    pub foot_h: u8,
    /// Max strength / hit points (`Strength=`).
    pub max_health: u16,
    /// Armor class index (`Armor=`).
    pub armor: u8,
    /// Net power: positive = output, negative = drain (`Power=`).
    pub power: i32,
    /// Build cost in credits (`Cost=`).
    pub cost: i32,
    /// Prerequisite building type ids (indices into
    /// [`Catalog::buildings`]) that the house must already own.
    pub prereq: Vec<u32>,
    /// This is a Tiberium refinery (PROC): a harvester dock, and it spawns a
    /// free harvester when built (`building.cpp:2640`).
    pub is_refinery: bool,
    /// This is a construction yard (CONST/FACT): produces buildings, and is what
    /// an MCV deploys into.
    pub is_construction_yard: bool,
    /// This is a war factory (WEAP): produces vehicles.
    pub is_war_factory: bool,
    /// The unit-proto index of the free harvester a refinery spawns (if
    /// `is_refinery`).
    pub free_harvester_unit: Option<u32>,
    /// Client sprite index for this building (opaque to the sim).
    pub sprite_id: u32,
}

/// A buildable unit type (vehicle) with the runtime stats it spawns with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitProto {
    /// Human-facing short name (rules.ini section), e.g. `"2TNK"`.
    pub name: String,
    /// Client sprite index (becomes `Unit::type_id`).
    pub sprite_id: u32,
    /// Max strength (`Strength=`).
    pub max_health: u16,
    /// Movement stats.
    pub stats: MoveStats,
    /// Armor class index.
    pub armor: u8,
    /// Primary weapon (None = unarmed, e.g. HARV/MCV).
    pub weapon: Option<WeaponProfile>,
    /// Whether it aims an independent turret.
    pub has_turret: bool,
    /// Whether this unit is a harvester (drives the harvest FSM).
    pub is_harvester: bool,
    /// The building-proto index this unit deploys into (MCV → CONST), if any.
    pub deploys_to: Option<u32>,
    /// Build cost in credits (`Cost=`).
    pub cost: i32,
    /// Prerequisite building type ids the house must own to build it.
    pub prereq: Vec<u32>,
}

/// The immutable catalog handed to [`crate::World`] at construction.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// Building prototypes, indexed by building type id.
    pub buildings: Vec<BuildingProto>,
    /// Unit prototypes, indexed by unit-proto id.
    pub units: Vec<UnitProto>,
    /// Economy constants.
    pub econ: EconRules,
}

impl Catalog {
    /// An empty catalog with default economy rules (movement/combat-only worlds).
    pub fn new() -> Catalog {
        Catalog {
            buildings: Vec::new(),
            units: Vec::new(),
            econ: EconRules::default(),
        }
    }

    /// Borrow a building prototype by id.
    pub fn building(&self, id: u32) -> Option<&BuildingProto> {
        self.buildings.get(id as usize)
    }

    /// Borrow a unit prototype by id.
    pub fn unit(&self, id: u32) -> Option<&UnitProto> {
        self.units.get(id as usize)
    }

    /// Time to build an item costing `cost` credits, in ticks. Port of
    /// `TechnoTypeClass::Time_To_Build` (`techno.cpp:6777`) with the stock
    /// `BuildSpeedBias = 1`: `time = Cost * TICKS_PER_MINUTE / 1000`, floored to
    /// at least 1 tick.
    pub fn time_to_build(&self, cost: i32) -> i32 {
        ((cost as i64 * self.econ.ticks_per_minute as i64) / 1000).max(1) as i32
    }
}
