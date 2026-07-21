//! The `Unit` entity and its movement stats. Following DESIGN.md §4.3, a unit
//! is a plain struct of components composed together, not a node in an
//! inheritance tree; systems (in [`crate::world`]) are free functions over the
//! arena. All state is fixed-point / integer so the whole struct hashes
//! bit-identically (§4.2).

use crate::arena::Handle;
use crate::combat::{Target, WeaponProfile};
use crate::coords::{CellCoord, Facing, Locomotor, WorldCoord};
use crate::hash::Fnv1a;

/// Which broad kind of movable object this is (DESIGN.md §4.3 — a discriminant on
/// the shared `Units` arena rather than a separate per-kind arena, so movement,
/// combat, targeting, retaliation, bullets, and selection treat infantry as
/// first-class without duplicating every system). The distinction the sim acts
/// on is cell occupancy: a [`Vehicle`](UnitKind::Vehicle) owns a whole cell,
/// while [`Infantry`](UnitKind::Infantry) occupy one of five sub-cell spots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnitKind {
    /// A whole-cell ground vehicle (tank, jeep, harvester, MCV).
    #[default]
    Vehicle,
    /// A foot soldier occupying a sub-cell spot (E1/E2/E3).
    Infantry,
    /// An aircraft (helicopter/fixed-wing). Flies at [`Unit::altitude`] over any
    /// terrain, occupies **no** ground cell (`AircraftClass`, `aircraft.cpp`), and
    /// is driven by its own FSM (`crate::world::run_aircraft`) rather than the
    /// ground movement/combat systems. Only weapons with an anti-air projectile can
    /// hit it while airborne (`Height > 0`).
    Aircraft,
}

/// An aircraft's flight-mission FSM state — a simplified port of the
/// `AircraftClass` mission handlers (`aircraft.cpp`). Meaningful only when
/// [`Unit::kind`] is [`UnitKind::Aircraft`]; hashed only for aircraft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AirState {
    /// No target, ammo available: fly home and settle on the helipad (or hover in
    /// place if it has no home pad). `Enter_Idle_Mode` → `MISSION_GUARD`
    /// (`aircraft.cpp:2134`).
    #[default]
    Idle,
    /// Has a target and ammo: fly to a firing position and strafe it
    /// (`Mission_Attack`, `aircraft.cpp:2527`).
    Attack,
    /// Out of ammo (or ordered home): fly to the home helipad to rearm
    /// (`MISSION_ENTER` after `Ammo == 0`, `aircraft.cpp:3869`).
    Returning,
    /// Docked on the helipad (altitude 0): the pad reloads one round per
    /// `RELOAD` cadence until full, then the craft takes off again
    /// (`BuildingClass::Mission_Repair` `RADIO_RELOAD`, `building.cpp:4433`).
    Rearming,
}

/// A unit's standing mission — the INI `[UNITS]`/`[INFANTRY]` order (Guard, Area
/// Guard, Hunt, Sleep, Sticky, Harvest…) or the default a produced/skirmish unit
/// spawns with. Drives *autonomous* target acquisition and the guard "leash" in
/// [`crate::world`]. Ported from the `MissionType` handlers (`foot.cpp`
/// `Mission_Guard`/`Mission_Guard_Area`/`Mission_Hunt`, `mission.cpp`
/// `Mission_Sleep`). Player Move/Attack orders do **not** change this field (they
/// set a target directly and clear `guard_target`); when the order finishes the
/// unit reverts to acquiring under its standing mission, exactly like the
/// original's `Enter_Idle_Mode` (`unit.cpp:1343`, default `MISSION_GUARD`).
///
/// [`Mission::Guard`] is the default; it is hashed **only when non-default**, so a
/// vehicle-only skirmish world whose units are all default-guard appends no bytes
/// for this field (see [`Unit::hash_into`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mission {
    /// `MISSION_GUARD` (`foot.cpp:594`): sit at post and acquire any enemy that
    /// enters weapon range (`Target_Something_Nearby(THREAT_RANGE)`), then engage.
    /// **Leash:** the acquired target is dropped the instant it leaves weapon range
    /// (`In_Range` → `Assign_Target(TARGET_NONE)`) — plain Guard never chases. The
    /// spawn default (`Enter_Idle_Mode`, `unit.cpp:1343`).
    #[default]
    Guard,
    /// `MISSION_GUARD_AREA` (`foot.cpp:1001`): acquire within *twice* weapon range
    /// of the guard post (`THREAT_AREA`), chase the target, but race back to the
    /// post the moment it strays more than weapon range from it
    /// (`Distance(ArchiveTarget) > Threat_Range(1)/2`).
    AreaGuard,
    /// `MISSION_HUNT` (`foot.cpp:670`): seek the nearest enemy anywhere and attack
    /// (mirrors the separate [`Unit::hunt`] flag used by teams / `ALL_HUNT`).
    Hunt,
    /// `MISSION_SLEEP` (`mission.cpp:93`): fully inert — never auto-acquires and,
    /// per the handler never touching TarCom, never retaliates.
    Sleep,
    /// `MISSION_STICKY`: holds position; like Sleep, never auto-acquires/retaliates.
    Sticky,
    /// `MISSION_HARVEST`: behaviour owned by the harvest FSM ([`Unit::is_harvester`]).
    Harvest,
}

impl Mission {
    /// Map a scenario INI mission string (`[UNITS]`/`[INFANTRY]` final field, e.g.
    /// `"Guard"`, `"Area Guard"`, `"Sleep"`) to a [`Mission`]. Names follow the
    /// original's `Missions[]` table (`const.cpp:71`); an unknown or empty order
    /// falls back to Guard (the engine's own idle default).
    pub fn from_ini_name(name: &str) -> Mission {
        match name.trim().to_ascii_lowercase().as_str() {
            "area guard" | "areaguard" => Mission::AreaGuard,
            "hunt" => Mission::Hunt,
            "sleep" | "harmless" => Mission::Sleep,
            "sticky" | "ambush" => Mission::Sticky,
            "harvest" => Mission::Harvest,
            // "guard", "return", "none", "" and anything unrecognised → Guard.
            _ => Mission::Guard,
        }
    }

    /// Whether this mission autonomously scans for and engages enemies at its post
    /// (Guard / Area Guard). Hunt uses the separate [`Unit::hunt`] path.
    pub fn is_guarding(self) -> bool {
        matches!(self, Mission::Guard | Mission::AreaGuard)
    }
}

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
    /// Resolved `Secondary=` weapon (e.g. the mammoth's MammothTusk missiles), or
    /// `None`. When present, `run_combat` picks primary vs. secondary per the
    /// target's armor (`What_Weapon_Should_I_Use`, `techno.cpp:360`). A type
    /// constant, so — like `locomotor`/`sight` — it is **not** hashed; its effect
    /// flows through the (hashed) `arm`/`health`/bullet state it produces.
    pub secondary: Option<WeaponProfile>,
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

    // --- Kind + occupancy (M7.6) ---
    /// Whether this is a vehicle (whole-cell) or infantry (sub-cell spot). A
    /// discriminant on the shared arena rather than a separate arena (§4.3), so
    /// every existing system treats infantry as first-class.
    pub kind: UnitKind,
    /// Ground-movement locomotor (`SPEED_FOOT`/`TRACK`/`WHEEL`) — selects the
    /// per-land passability column for pathfinding. Constant per unit type, so —
    /// like `sight`/`armor` derivation — it is **not** hashed (its effect is
    /// captured through the unit's `coord`, which is hashed).
    pub locomotor: Locomotor,
    /// The infantry sub-cell spot (0..[`crate::coords::SUBCELL_COUNT`]) this unit
    /// currently occupies within its cell (center + 4 quadrants,
    /// `StoppingCoordAbs`, `const.cpp:282`). Meaningful only when
    /// `kind == Infantry`; 0 for vehicles. Hashed for infantry (it changes as
    /// they repack), gated so vehicle-only worlds hash byte-identically.
    pub sub_cell: u8,

    // --- Campaign scripting (M7.5) ---
    /// Campaign trigger attached to this object (index into
    /// [`crate::campaign::Campaign::triggers`]), or `None`. Set only by the
    /// campaign loader / reinforcement spawns; a `TEVENT_DESTROYED`/`ATTACKED`
    /// on this trigger latches when this unit dies / is hit. Hashed **only when
    /// `Some`**, so every non-campaign world is byte-identical.
    pub trigger: Option<u16>,
    /// Whether this unit counts as an evacuable civilian VIP (Einstein/Delphi/…)
    /// for `TEVENT_EVAC_CIVILIAN` (`_Counts_As_Civ_Evac`, `aircraft.cpp:112`).
    /// Set by the campaign loader. Hashed only when `true`.
    pub is_civ_evac: bool,
    /// Auto-hunt: when idle (no target, no path) this unit acquires the nearest
    /// enemy and attacks it (`MISSION_HUNT`). Set by `TACTION_ALL_HUNT` and by
    /// campaign attack-teams (M7.5). Hashed only when `true`.
    pub hunt: bool,

    // --- Per-unit mission layer (M7.5-B) ---
    /// This unit's standing mission (Guard/Area Guard/Hunt/Sleep/Sticky/Harvest).
    /// Drives autonomous acquisition + the guard leash in [`crate::world`]. Set
    /// from the scenario INI order for placed units, and left at the spawn default
    /// [`Mission::Guard`] for produced/skirmish units (matching the original's
    /// `Enter_Idle_Mode`, `unit.cpp:1343`). Hashed **only when non-default**, so a
    /// default-guard skirmish world appends no bytes for it.
    pub mission: Mission,
    /// The guard post (`ArchiveTarget`, `foot.cpp:1023`) an Area-Guard unit returns
    /// to when it strays too far while chasing. Set to the spawn cell for
    /// Area-Guard units. Hashed only when `Some`.
    pub guard_post: Option<CellCoord>,
    /// Whether the current [`Self::target`] was **auto-acquired** by a guard
    /// mission (vs. a player Move/Attack order or retaliation). Only auto-acquired
    /// guard targets are leashed (dropped when out of range / when the unit strays
    /// from its post); a player order always chases. Hashed only when `true`.
    pub guard_target: bool,

    // --- Transport / passengers (M7.5-B P1) ---
    /// Passenger capacity (`Passengers=` from rules.ini, `udata.cpp`). 0 for a
    /// non-transport. Constant per unit type (like `sight`/`locomotor`), so — its
    /// effect flowing through the hashed `passengers` list — it is **not** hashed.
    pub capacity: u8,
    /// Loaded passengers (`FootClass::Passenger`/cargo hold). A passenger is
    /// removed from the map when it boards and stored here; it re-enters the arena
    /// on unload, and **dies with the transport** (`DriveClass`/`foot` cargo kill).
    /// Hashed only when non-empty, so a world with no loaded transport is
    /// byte-identical.
    pub cargo: Vec<Passenger>,
    /// The transport this (infantry) unit is currently trying to board — set by a
    /// `Load` order; it walks adjacent and boards when it arrives (`MISSION_ENTER`
    /// radio approach, simplified). Cleared once aboard or if the transport is
    /// gone/full. Hashed only when `Some`.
    pub board_target: Option<Handle>,
    /// A cell this transport should auto-unload its cargo at on arrival — set by a
    /// scripted team `UNLOAD` mission so a campaign assault disgorges at the
    /// objective. Cleared once it unloads. Hashed only when `Some`.
    pub unload_at: Option<CellCoord>,

    // --- Aircraft / flight (P0 aircraft arc) ---
    /// Height above the ground in **leptons**, `0..=`[`FLIGHT_LEVEL`]
    /// (`AbstractClass::Height`, `abstract.h:69`). Meaningful only when
    /// `kind == Aircraft`: `FLIGHT_LEVEL` = full flight, `0` = landed/docked. An
    /// aircraft is a valid target for a **ground** weapon only at `altitude == 0`
    /// (`Can_Fire`, `techno.cpp:2895`), and takes half damage while `altitude > 0`
    /// (`AircraftClass::Take_Damage`, `aircraft.cpp:1685`). Hashed only for aircraft.
    pub altitude: i32,
    /// Rounds of ammunition remaining (`TechnoClass::Ammo`). An aircraft that hits
    /// `0` flies home to rearm; a value of `max_ammo` is full. Hashed only for
    /// aircraft (non-aircraft leave it `0`).
    pub ammo: u16,
    /// Ammunition capacity (`Class->MaxAmmo`, rules.ini `Ammo=`). A type constant
    /// (like `sight`/`capacity`) — **not** hashed; its effect flows through the
    /// hashed `ammo`.
    pub max_ammo: u16,
    /// Flight-mission FSM state. Hashed only for aircraft.
    pub air_state: AirState,
    /// The home helipad this aircraft rearms/lands at (`Find_Docking_Bay`), if any.
    /// Hashed only for aircraft.
    pub home: Option<Handle>,
    /// Rearm cadence countdown while docked (ticks until the next `+1` ammo). The
    /// pad reloads one round each time this reaches 0 (`Rule.ReloadRate`,
    /// `building.cpp:4438`). Hashed only for aircraft.
    pub rearm_timer: u16,

    // --- Naval / submarine (naval arc P0) ---
    /// This vessel is a **submarine** (SS/MSUB) — it cruises submerged
    /// (`IsCloakable`, `vessel.cpp:113`), invisible to non-detector enemies, and
    /// surfaces to fire. A type constant (like `locomotor`), **not** hashed; its
    /// effect flows through the hashed [`Self::submerged`] state.
    pub is_submarine: bool,
    /// This unit can **detect** submerged submarines (a destroyer, DD) — a nearby
    /// enemy sub is revealed to it and its allies. A type constant, **not** hashed.
    pub is_detector: bool,
    /// Whether a submarine is currently **submerged** (cloaked). `true` = hidden
    /// from non-detector enemies; a sub surfaces (`false`) while it has a target
    /// and for a recloak grace period after (`Is_Allowed_To_Recloak`,
    /// `vessel.cpp:2044`). Hashed **only for submarines**, so every non-naval world
    /// is byte-identical.
    pub submerged: bool,
    /// Recloak grace countdown (ticks a surfaced sub stays visible after losing its
    /// target before re-submerging — the original's `PulseCountDown`). Hashed only
    /// for submarines.
    pub recloak: u16,
}

/// `FLIGHT_LEVEL` — full flight altitude in leptons (`ObjectClass` enum,
/// `object.h:299`): one cell (256 leptons) of altitude.
pub const FLIGHT_LEVEL: i32 = 256;

/// A boarded passenger — the minimal state needed to re-materialise a unit on
/// unload (`InfantryClass`/`FootClass` limbo state). Stored on the transport's
/// [`Unit::cargo`]; the passenger is out of the map arena while aboard.
#[derive(Clone, Debug, PartialEq)]
pub struct Passenger {
    /// Sprite/type index (`Unit::type_id`).
    pub type_id: u32,
    /// Owning house.
    pub house: u8,
    /// Current health (preserved across the ride).
    pub health: u16,
    /// Max strength.
    pub max_health: u16,
    /// Movement stats (to re-spawn a drivable unit).
    pub stats: MoveStats,
    /// Armor class.
    pub armor: u8,
    /// Primary weapon.
    pub weapon: Option<WeaponProfile>,
    /// Secondary weapon.
    pub secondary: Option<WeaponProfile>,
    /// Turret.
    pub has_turret: bool,
    /// Sight in cells.
    pub sight: u8,
    /// Infantry (sub-cell) vs vehicle.
    pub is_infantry: bool,
    /// The mission the passenger resumes on unload.
    pub mission: Mission,
}

impl Passenger {
    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u32(self.type_id);
        h.write_u8(self.house);
        h.write_u16(self.health);
        h.write_u16(self.max_health);
        h.write_u8(self.is_infantry as u8);
        h.write_u8(self.mission as u8);
    }
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
            secondary: None,
            has_turret: false,
            turret_facing: facing,
            target: None,
            arm: 0,
            is_harvester: false,
            harvest: HarvestState::default(),
            sight: 0,
            kind: UnitKind::Vehicle,
            locomotor: Locomotor::Track,
            sub_cell: 0,
            trigger: None,
            is_civ_evac: false,
            hunt: false,
            mission: Mission::Guard,
            guard_post: None,
            guard_target: false,
            capacity: 0,
            cargo: Vec::new(),
            board_target: None,
            unload_at: None,
            altitude: 0,
            ammo: 0,
            max_ammo: 0,
            air_state: AirState::Idle,
            home: None,
            rearm_timer: 0,
            is_submarine: false,
            is_detector: false,
            submerged: false,
            recloak: 0,
        }
    }

    /// Turn this unit into a **naval vessel**: the `Water` locomotor and its
    /// submarine/detector capability flags. A submarine spawns already submerged
    /// (`VesselClass` ctor + `IsCloaked`, `vessel.cpp:113`). Called by the
    /// loader/production right after spawning a DD/CA/SS. Vessels otherwise reuse
    /// the ground vehicle systems (movement/combat) over water.
    pub fn make_vessel(&mut self, is_submarine: bool, is_detector: bool) {
        self.locomotor = Locomotor::Water;
        self.is_submarine = is_submarine;
        self.is_detector = is_detector;
        self.submerged = is_submarine;
    }

    /// Whether this unit is a naval vessel (floats, paths over water).
    pub fn is_vessel(&self) -> bool {
        self.locomotor == Locomotor::Water
    }

    /// Turn this unit into an **aircraft** (helicopter/fixed-wing): the `Air`
    /// locomotor, spawned at [`FLIGHT_LEVEL`] with a full magazine, occupying no
    /// ground cell. Called by the loader/production right after spawning a HELI/
    /// HIND/TRAN/… (`AircraftClass` ctor: `Height = FLIGHT_LEVEL`, `Ammo =
    /// Class->MaxAmmo`, `aircraft.cpp:254`).
    pub fn make_aircraft(&mut self, max_ammo: u16) {
        self.kind = UnitKind::Aircraft;
        self.locomotor = Locomotor::Air;
        self.max_ammo = max_ammo;
        self.ammo = max_ammo;
        self.altitude = FLIGHT_LEVEL;
        self.air_state = AirState::Idle;
    }

    /// Whether this unit is an aircraft (flies at altitude, own FSM).
    pub fn is_aircraft(&self) -> bool {
        self.kind == UnitKind::Aircraft
    }

    /// Whether this unit is an aircraft currently **airborne** (`Height > 0`), i.e.
    /// only an anti-air weapon can hit it (`Can_Fire`, `techno.cpp:2895`). A landed/
    /// docked aircraft (`altitude == 0`) is a ground target like any vehicle.
    pub fn is_airborne(&self) -> bool {
        self.kind == UnitKind::Aircraft && self.altitude > 0
    }

    /// Set this unit's ground locomotor (from its type — tanks Track, jeep/harv
    /// Wheel, infantry Foot). Called by the loader right after spawning.
    pub fn set_locomotor(&mut self, locomotor: Locomotor) {
        self.locomotor = locomotor;
    }

    /// Turn this unit into infantry occupying sub-cell spot `sub_cell`, with the
    /// `Foot` locomotor and its coord snapped to that spot's centre. Called by the
    /// loader / production right after spawning an E1/E2/E3.
    pub fn make_infantry(&mut self, sub_cell: u8) {
        self.kind = UnitKind::Infantry;
        self.locomotor = Locomotor::Foot;
        self.sub_cell = sub_cell;
        self.coord = self.cell().spot_center(sub_cell);
    }

    /// Whether this unit is infantry (occupies a sub-cell spot).
    pub fn is_infantry(&self) -> bool {
        self.kind == UnitKind::Infantry
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

    /// Attach a `Secondary=` weapon (mammoth-tank style dual armament). Call
    /// after [`Self::set_combat`]; harmless (no-op behavior) for units the sim
    /// never selects a secondary for.
    pub fn set_secondary(&mut self, secondary: Option<WeaponProfile>) {
        self.secondary = secondary;
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

        // Infantry sub-cell spot (M7.6). Folded ONLY for infantry, appending no
        // bytes for vehicles — so a vehicle-only world (every M3/M4/M5/M6/M7
        // golden) hashes byte-identically to before this milestone. `locomotor`
        // and `kind` are constants derived from the unit type (like `sight`) and
        // their movement effect is already captured through `coord`, so they are
        // not hashed; `sub_cell` changes as infantry repack a cell, so it is.
        if self.kind == UnitKind::Infantry {
            h.write_u8(0x1F);
            h.write_u8(self.sub_cell);
        }

        // Campaign attachment (M7.5). Folded ONLY when set, appending no bytes
        // otherwise, so every non-campaign world (all prior goldens) is identical.
        if let Some(t) = self.trigger {
            h.write_u8(0x2A);
            h.write_u16(t);
        }
        if self.is_civ_evac {
            h.write_u8(0x2B);
        }
        if self.hunt {
            h.write_u8(0x2C);
        }

        // Per-unit mission layer (M7.5-B). Each field is folded ONLY when it
        // departs from the inert spawn default (mission Guard / no post / not a
        // guard target / empty cargo), appending no bytes otherwise — so every
        // default-guard vehicle-only world (all prior skirmish/synthetic goldens)
        // hashes byte-identically. `capacity` is a type constant (like `sight`),
        // captured through the `cargo` list, so it is not hashed.
        if self.mission != Mission::Guard {
            h.write_u8(0x2D);
            h.write_u8(self.mission as u8);
        }
        if let Some(p) = self.guard_post {
            h.write_u8(0x2E);
            h.write_i32(p.x);
            h.write_i32(p.y);
        }
        if self.guard_target {
            h.write_u8(0x2F);
        }
        if !self.cargo.is_empty() {
            h.write_u8(0x30);
            h.write_u32(self.cargo.len() as u32);
            for p in &self.cargo {
                p.hash_into(h);
            }
        }
        if let Some(t) = self.board_target {
            h.write_u8(0x31);
            h.write_u32(t.index);
            h.write_u32(t.gen);
        }
        if let Some(c) = self.unload_at {
            h.write_u8(0x32);
            h.write_i32(c.x);
            h.write_i32(c.y);
        }

        // Aircraft / flight state (P0 aircraft arc). Folded ONLY for aircraft,
        // appending no bytes for vehicles/infantry — so every pre-aircraft world
        // (all prior goldens) hashes byte-identically. `max_ammo` is a type
        // constant (like `sight`), captured through `ammo`, so it is not hashed.
        if self.kind == UnitKind::Aircraft {
            h.write_u8(0x33);
            h.write_i32(self.altitude);
            h.write_u16(self.ammo);
            h.write_u8(self.air_state as u8);
            h.write_u16(self.rearm_timer);
            match self.home {
                Some(handle) => {
                    h.write_u8(1);
                    h.write_u32(handle.index);
                    h.write_u32(handle.gen);
                }
                None => h.write_u8(0),
            }
        }

        // Submarine stealth state (naval arc). Folded ONLY for submarines, so every
        // non-submarine world (all prior goldens, and surface vessels) is
        // byte-identical. `is_submarine`/`is_detector` are type constants (like
        // `locomotor`) captured through this gated `submerged`/`recloak`.
        if self.is_submarine {
            h.write_u8(0x34);
            h.write_u8(self.submerged as u8);
            h.write_u16(self.recloak);
        }
    }
}
