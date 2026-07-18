//! The `Unit` entity and its movement stats. Following DESIGN.md §4.3, a unit
//! is a plain struct of components composed together, not a node in an
//! inheritance tree; systems (in [`crate::world`]) are free functions over the
//! arena. All state is fixed-point / integer so the whole struct hashes
//! bit-identically (§4.2).

use crate::arena::Handle;
use crate::combat::{Target, WeaponProfile};
use crate::coords::{CellCoord, Facing, WorldCoord};
use crate::hash::Fnv1a;

/// The 5-state harvester mission FSM, ported from `UnitClass::Mission_Harvest`
/// (`unit.cpp:2898`): scan for ore, drive to it, harvest until full, find a
/// refinery, dock, and unload. `Idle` is the guard state the original drops into
/// when there is no refinery or no ore (`unit.cpp:2922`, `:2975`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarvStatus {
    /// LOOKING: seek the nearest ore field and drive toward it.
    Looking,
    /// HARVESTING: lift bails from the current ore cell until full/exhausted.
    Harvesting,
    /// FINDHOME: pick the nearest owned refinery and route to its dock cell.
    FindHome,
    /// HEADINGHOME: driving to the refinery dock.
    HeadingHome,
    /// UNLOADING: at the dock, cashing the cargo in.
    Unloading,
    /// Guard/idle (no refinery, or no reachable ore).
    Idle,
}

/// The harvester's working state (only meaningful when [`Unit::is_harvester`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HarvestState {
    /// Current FSM state.
    pub status: HarvStatus,
    /// Bails currently carried (0..=`bail_count`).
    pub cargo: u16,
    /// Gold bails carried (books at `GoldValue` on unload).
    pub gold: u16,
    /// Gem bails carried (books at `GemValue` on unload).
    pub gems: u16,
    /// Countdown between harvest/unload steps (`OreDumpRate` cadence).
    pub timer: u16,
    /// The refinery being docked at, if any.
    pub home: Option<Handle>,
}

impl Default for HarvestState {
    fn default() -> HarvestState {
        HarvestState {
            status: HarvStatus::Looking,
            cargo: 0,
            gold: 0,
            gems: 0,
            timer: 0,
            home: None,
        }
    }
}

impl HarvestState {
    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(self.status as u8);
        h.write_u16(self.cargo);
        h.write_u16(self.gold);
        h.write_u16(self.gems);
        h.write_u16(self.timer);
        match self.home {
            Some(handle) => {
                h.write_u8(1);
                h.write_u32(handle.index);
                h.write_u32(handle.gen);
            }
            None => h.write_u8(0),
        }
    }
}

/// The immutable movement stats a unit carries, resolved from rules.ini at
/// spawn time by `ra-data` (never hardcoded — DESIGN.md §3.8). Kept on the unit
/// so the sim needs no back-reference to a type table during a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveStats {
    /// Top speed in leptons per tick (256 = one whole cell per tick). Derived
    /// from rules.ini `Speed=` via `speed * 256 / 100` (see `ra_data::rules`).
    pub max_speed: i32,
    /// Rate of turn: binary-angle units the facing may change per tick. The
    /// sim applies `rot + 1` (matching `Rotation_Adjust(Class->ROT + 1)`).
    pub rot: u8,
}

/// A movable ground unit (vehicle). Buildings/infantry/etc. get their own
/// arenas in later milestones (§5 "Entity arenas: per-kind").
#[derive(Clone, Debug)]
pub struct Unit {
    /// Data-defined unit type index (see `ra_data::rules::UnitCatalog`); the
    /// client maps this to the SHP + remap. Opaque to the sim.
    pub type_id: u32,
    /// Owning house index (see §4.6: commands carry the issuing house, so
    /// ownership is validated, not implied).
    pub house: u8,
    /// Current sub-cell position, in leptons.
    pub coord: WorldCoord,
    /// Current body facing (binary angle).
    pub facing: Facing,
    /// Current strength/health (0..=max_health).
    pub health: u16,
    /// Maximum strength (`Strength=`). Constant per unit; used for the health
    /// fraction the client renders. Defaults to the spawn health (full).
    pub max_health: u16,
    /// Movement stats resolved from rules.ini.
    pub stats: MoveStats,
    /// Remaining path waypoints (cell centres to visit, in order). Empty = idle.
    pub path: Vec<CellCoord>,
    /// Final ordered destination, if any (kept for re-issue/debug).
    pub dest: Option<CellCoord>,

    // --- Combat (M4) ---
    /// Armor class index (0=none … 3=steel/"heavy" … 4=concrete), from `Armor=`.
    /// Selects the column of an attacker's warhead `Verses` matrix.
    pub armor: u8,
    /// Resolved primary weapon, or `None` for unarmed units (e.g. HARV).
    pub weapon: Option<WeaponProfile>,
    /// Whether the unit aims an independently-rotating turret (1TNK/2TNK/JEEP)
    /// versus rotating its whole body to aim (turretless — e.g. HARV, if armed).
    pub has_turret: bool,
    /// Turret facing (binary angle). Equals `facing` for turretless units.
    pub turret_facing: Facing,
    /// Current attack target (unit handle or force-fire cell), if any. This is
    /// the TarCom equivalent (`techno.h`).
    pub target: Option<Target>,
    /// Rearm countdown in ticks (`Arm`): 0 = ready to fire, else counting down.
    pub arm: u16,

    // --- Harvester capability (M5) ---
    /// Whether this unit runs the harvest FSM (a named capability component,
    /// DESIGN.md §3.8 — the sim never infers it from `type_id`).
    pub is_harvester: bool,
    /// Harvester working state (only meaningful when `is_harvester`).
    pub harvest: HarvestState,

    // --- Shroud (M6) ---
    /// Sight range in cells (`Sight=`, `techno.cpp:7062`), used to reveal the
    /// shroud around this unit as it moves. Capped at 10 like the original.
    pub sight: u8,
}

impl Unit {
    /// Spawn a unit at a cell centre, facing `facing`, idle.
    pub fn new(
        type_id: u32,
        house: u8,
        cell: CellCoord,
        facing: Facing,
        health: u16,
        stats: MoveStats,
    ) -> Unit {
        Unit {
            type_id,
            house,
            coord: cell.center(),
            facing,
            health,
            max_health: health.max(1),
            stats,
            path: Vec::new(),
            dest: None,
            armor: 0,
            weapon: None,
            has_turret: false,
            turret_facing: facing,
            target: None,
            arm: 0,
            is_harvester: false,
            harvest: HarvestState::default(),
            sight: 0,
        }
    }

    /// Set the unit's sight range in cells (from its type's `Sight=`, capped at
    /// 10 as the original does). Called by the loader right after spawning.
    pub fn set_sight(&mut self, sight: u8) {
        self.sight = sight.min(10);
    }

    /// Mark this unit as a harvester (drives the harvest FSM). Called by the
    /// loader right after spawning, from the unit's [`crate::catalog::UnitProto`].
    pub fn set_harvester(&mut self, is_harvester: bool) {
        self.is_harvester = is_harvester;
    }

    /// Attach combat stats to a freshly-spawned unit (resolved from rules.ini by
    /// `ra-data`). Turretless units keep the turret facing locked to the body.
    pub fn set_combat(&mut self, armor: u8, weapon: Option<WeaponProfile>, has_turret: bool) {
        self.armor = armor;
        self.weapon = weapon;
        self.has_turret = has_turret;
        if !has_turret {
            self.turret_facing = self.facing;
        }
    }

    /// Set the maximum strength (called by the loader when a unit spawns at a
    /// scenario health percentage below full).
    pub fn set_max_health(&mut self, max_health: u16) {
        self.max_health = max_health.max(1);
    }

    /// Whether the unit is alive (health above zero).
    pub fn is_alive(&self) -> bool {
        self.health > 0
    }

    /// Health as integer permille (0..=1000) of max — for the client's bar.
    pub fn health_permille(&self) -> i32 {
        (self.health as i32 * 1000 / self.max_health.max(1) as i32).clamp(0, 1000)
    }

    /// Whether the unit currently has an attack target.
    pub fn has_target(&self) -> bool {
        self.target.is_some()
    }

    /// Whether the unit currently has somewhere to go.
    pub fn is_moving(&self) -> bool {
        !self.path.is_empty()
    }

    /// The cell the unit currently occupies (its position rounded to a cell).
    pub fn cell(&self) -> CellCoord {
        self.coord.cell()
    }

    /// Fold this unit's mutable state into the world hash, in a fixed field
    /// order. Path waypoints are included so a divergent route is caught.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u32(self.type_id);
        h.write_u8(self.house);
        h.write_i32(self.coord.x.0);
        h.write_i32(self.coord.y.0);
        h.write_u8(self.facing.0);
        h.write_u16(self.health);
        h.write_u16(self.max_health);
        h.write_i32(self.stats.max_speed);
        h.write_u8(self.stats.rot);
        h.write_u32(self.path.len() as u32);
        for cell in &self.path {
            h.write_i32(cell.x);
            h.write_i32(cell.y);
        }
        match self.dest {
            Some(c) => {
                h.write_u8(1);
                h.write_i32(c.x);
                h.write_i32(c.y);
            }
            None => h.write_u8(0),
        }

        // Combat state.
        h.write_u8(self.armor);
        h.write_u8(self.has_turret as u8);
        h.write_u8(self.turret_facing.0);
        h.write_u16(self.arm);
        match &self.weapon {
            Some(w) => {
                h.write_u8(1);
                w.hash_into(h);
            }
            None => h.write_u8(0),
        }
        match self.target {
            None => h.write_u8(0),
            Some(Target::Unit(handle)) => {
                h.write_u8(1);
                h.write_u32(handle.index);
                h.write_u32(handle.gen);
            }
            Some(Target::Cell(c)) => {
                h.write_u8(2);
                h.write_i32(c.x);
                h.write_i32(c.y);
            }
            Some(Target::Building(handle)) => {
                h.write_u8(3);
                h.write_u32(handle.index);
                h.write_u32(handle.gen);
            }
        }

        // Harvester state.
        h.write_u8(self.is_harvester as u8);
        if self.is_harvester {
            self.harvest.hash_into(h);
        }
        // `sight` is a constant derived from the unit type (it never changes), so
        // it is deliberately NOT folded into the hash — doing so would only churn
        // the M5 golden pins with no determinism benefit.
    }
}
