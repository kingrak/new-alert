//! Campaign scripting (M7.5) — the RA `TriggerType`/`TeamType` tables and the
//! deterministic engine that evaluates them each tick, so the single-player
//! *missions* (not just skirmish) run.
//!
//! This is a faithful-but-scoped port of the reference scenario-scripting layer
//! (`trigtype.cpp` / `tevent.cpp` / `taction.cpp` / `teamtype.cpp` /
//! `trigger.cpp` / `team.cpp` / `reinf.cpp`). Events and actions are stored as
//! the original's raw `(code, team, trigger, data)` tuples — the `Data` union is
//! a signed int whose meaning depends on the code (waypoint / house / global /
//! count / tenths-of-minute), exactly as `Event_Needs` / `Action_Needs` describe
//! — and interpreted in [`crate::world`]'s `run_campaign`.
//!
//! **Determinism:** the whole thing is sim-side and hashed (globals, per-trigger
//! spring/timer state, mission timer, evac flags). It is folded into the world
//! hash **only when a campaign is present**, so every skirmish world stays
//! byte-identical (like the M6 shroud / M7.10 AI gating).

use crate::combat::WeaponProfile;
use crate::coords::CellCoord;
use crate::hash::Fnv1a;
use crate::unit::MoveStats;

/// `TEventType` codes (`tevent.h:44`). Only the subset the early Allied missions
/// use is *evaluated* (see `run_campaign`); the rest parse and hash but are inert.
pub mod tevent {
    pub const NONE: u8 = 0;
    pub const PLAYER_ENTERED: u8 = 1;
    pub const DISCOVERED: u8 = 4;
    pub const ATTACKED: u8 = 6;
    pub const DESTROYED: u8 = 7;
    pub const ANY: u8 = 8;
    pub const UNITS_DESTROYED: u8 = 9;
    pub const BUILDINGS_DESTROYED: u8 = 10;
    pub const ALL_DESTROYED: u8 = 11;
    pub const TIME: u8 = 13;
    pub const NBUILDINGS_DESTROYED: u8 = 15;
    pub const NUNITS_DESTROYED: u8 = 16;
    pub const EVAC_CIVILIAN: u8 = 18;
    pub const CROSS_HORIZONTAL: u8 = 25;
    pub const CROSS_VERTICAL: u8 = 26;
    pub const GLOBAL_SET: u8 = 27;
    pub const GLOBAL_CLEAR: u8 = 28;
    pub const LOW_POWER: u8 = 30;
    pub const BUILDING_EXISTS: u8 = 32;
}

/// `TActionType` codes (`taction.h:40`).
pub mod taction {
    pub const NONE: u8 = 0;
    pub const WIN: u8 = 1;
    pub const LOSE: u8 = 2;
    pub const BEGIN_PRODUCTION: u8 = 3;
    pub const CREATE_TEAM: u8 = 4;
    pub const DESTROY_TEAM: u8 = 5;
    pub const ALL_HUNT: u8 = 6;
    pub const REINFORCEMENTS: u8 = 7;
    pub const DZ: u8 = 8;
    pub const TEXT_TRIGGER: u8 = 11;
    pub const DESTROY_TRIGGER: u8 = 12;
    pub const ALLOWWIN: u8 = 15;
    pub const REVEAL_ALL: u8 = 16;
    pub const REVEAL_SOME: u8 = 17;
    pub const PLAY_SPEECH: u8 = 21;
    pub const FORCE_TRIGGER: u8 = 22;
    pub const START_TIMER: u8 = 23;
    pub const STOP_TIMER: u8 = 24;
    pub const SET_TIMER: u8 = 27;
    pub const SET_GLOBAL: u8 = 28;
    pub const CLEAR_GLOBAL: u8 = 29;
    pub const DESTROY_OBJECT: u8 = 32;
}

/// `TeamMissionType` codes (`teamtype.h:43`).
pub mod tmission {
    pub const ATTACK: i32 = 0;
    pub const ATT_WAYPT: i32 = 1;
    pub const MOVE: i32 = 3;
    pub const MOVECELL: i32 = 4;
    pub const GUARD: i32 = 5;
    pub const LOOP: i32 = 6;
    pub const PATROL: i32 = 16;
}

/// `MultiStyleType` (`trigtype.h:47`): how event1/event2 combine and how the two
/// events map to the two actions.
pub mod multi {
    pub const ONLY: u8 = 0;
    pub const AND: u8 = 1;
    pub const OR: u8 = 2;
    pub const LINKED: u8 = 3;
}

/// `PersistantType` (`trigtype.h:60`).
pub mod persist {
    pub const VOLATILE: u8 = 0;
    pub const SEMI: u8 = 1;
    pub const PERSISTANT: u8 = 2;
}

/// One trigger event: raw `(code, team, data)` (`TEventClass`, `tevent.cpp:528`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TEventDef {
    /// `TEventType` code (see [`tevent`]).
    pub code: u8,
    /// Raw TeamType index for `NEED_TEAM` events (`LEAVES_MAP`), else `-1`.
    pub team: i32,
    /// The `Data` union value — house / waypoint / global index / count /
    /// tenths-of-minute depending on `code`.
    pub data: i32,
}

/// One trigger action: raw `(code, team, trigger, data)` (`TActionClass`,
/// `taction.cpp:245`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TActionDef {
    /// `TActionType` code (see [`taction`]).
    pub code: u8,
    /// Raw TeamType index (CREATE_TEAM / REINFORCEMENTS / DESTROY_TEAM), else `-1`.
    pub team: i32,
    /// Raw TriggerType index (FORCE_TRIGGER / DESTROY_TRIGGER), else `-1`.
    pub trigger: i32,
    /// The `Data` union value.
    pub data: i32,
}

/// A parsed trigger type (`TriggerTypeClass`), the static half of a trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerType {
    /// The trigger's INI name (also its identity for cell/object attachment).
    pub name: String,
    /// `PersistantType` (see [`persist`]).
    pub persist: u8,
    /// Owning `HousesType` index (the house `TEVENT_*`/`TACTION_*` scope to).
    pub house: i32,
    /// How event1/event2 combine ([`multi`]).
    pub event_ctrl: u8,
    /// How the events map to actions ([`multi`]).
    pub action_ctrl: u8,
    /// Event 1.
    pub e1: TEventDef,
    /// Event 2.
    pub e2: TEventDef,
    /// Action 1.
    pub a1: TActionDef,
    /// Action 2.
    pub a2: TActionDef,
}

/// A resolved spawn prototype for one TeamType member (the client lifts the SHP
/// index + rules.ini stats; `None` for a class we cannot spawn — naval/air,
/// deferred). Mirrors what [`crate::World::spawn_unit`] + `set_unit_*` need.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnProto {
    /// Sprite/type index (opaque to the sim; the client maps it to a SHP).
    pub type_id: u32,
    /// Max strength.
    pub max_health: u16,
    /// Movement stats.
    pub stats: MoveStats,
    /// Armor class.
    pub armor: u8,
    /// Primary weapon.
    pub weapon: Option<WeaponProfile>,
    /// Secondary weapon.
    pub secondary: Option<WeaponProfile>,
    /// Independently-rotating turret.
    pub has_turret: bool,
    /// Sight in cells.
    pub sight: u8,
    /// Infantry (sub-cell) vs vehicle.
    pub is_infantry: bool,
    /// Harvester capability.
    pub is_harvester: bool,
    /// Evacuable civilian VIP (Einstein/…).
    pub is_civ_evac: bool,
}

/// One `class:count` entry of a team (`TeamTypeClass::Members`).
#[derive(Clone, Debug, PartialEq)]
pub struct TeamClass {
    /// Resolved spawn prototype, or `None` if unspawnable (naval/air — deferred).
    pub proto: Option<SpawnProto>,
    /// How many of this class the team contains.
    pub count: u16,
}

/// One team mission (`TeamMissionClass`): `(code, arg)` (`teamtype.h:78`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeamMission {
    /// `TeamMissionType` (see [`tmission`]).
    pub code: i32,
    /// The mission argument (waypoint index / cell / quarry / duration).
    pub arg: i32,
}

/// A parsed team type (`TeamTypeClass`).
#[derive(Clone, Debug, PartialEq)]
pub struct TeamType {
    /// INI name.
    pub name: String,
    /// Owning `HousesType` index.
    pub house: i32,
    /// Packed flag bits (RoundAbout/Suicide/Autocreate/Prebuilt/Reinforcable).
    pub flags: u32,
    /// Recruit priority (0..15).
    pub recruit: i32,
    /// Initial number to create.
    pub init_num: i32,
    /// Max simultaneously allowed.
    pub max_allowed: i32,
    /// Origin waypoint index (spawn/reinforcement origin), or `-1`.
    pub origin: i32,
    /// Trigger assigned to members (raw TriggerType index), or `-1`.
    pub trigger: i32,
    /// The class list.
    pub classes: Vec<TeamClass>,
    /// The ordered mission list.
    pub missions: Vec<TeamMission>,
}

/// Mutable per-trigger runtime state (`TriggerClass`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriggerState {
    /// Already sprung (a VOLATILE/SEMI trigger fires once). PERSISTANT resets.
    pub sprung: bool,
    /// Destroyed by `TACTION_DESTROY_TRIGGER` (`taction.cpp:568-578` deletes the
    /// `TriggerClass` instance outright). Unlike `sprung`, this suppresses **all**
    /// future evaluation regardless of persistence — a destroyed PERSISTANT
    /// trigger stops firing.
    pub destroyed: bool,
    /// Countdown timer for a `TEVENT_TIME` event1 (ticks), `-1` if not a timer.
    pub e1_timer: i32,
    /// Countdown timer for a `TEVENT_TIME` event2.
    pub e2_timer: i32,
    /// Live object carriers of this trigger remaining (for `DESTROYED`).
    pub carriers: i32,
    /// Initial carrier count (so SEMI knows "all gone").
    pub carriers_init: i32,
    /// At least one carrier has died since load (VOLATILE `DESTROYED` latch).
    pub any_destroyed: bool,
    /// At least one carrier has been attacked (VOLATILE `ATTACKED` latch).
    pub any_attacked: bool,
}

/// The complete campaign scenario-scripting state attached to a [`crate::World`].
#[derive(Clone, Debug, PartialEq)]
pub struct Campaign {
    /// The trigger types, in INI order (indices are the raw ids triggers/teams
    /// reference).
    pub triggers: Vec<TriggerType>,
    /// The team types, in INI order.
    pub teamtypes: Vec<TeamType>,
    /// Waypoint cells, indexed by waypoint number (`-1` = unset). Sized 101
    /// (`WAYPT_COUNT`); indices 98/99/100 are HOME/REINF/SPECIAL.
    pub waypoints: Vec<i32>,
    /// Global flags (`Scen.GlobalFlags`).
    pub globals: Vec<bool>,
    /// Cell triggers: `(cell number, trigger index)` (`[CellTriggers]`). A unit of
    /// the event's house standing on one of these cells satisfies a
    /// `PLAYER_ENTERED`/`CROSS_*` event on that trigger.
    pub cell_triggers: Vec<(u32, u16)>,
    /// Per-trigger runtime state, parallel to `triggers`.
    pub state: Vec<TriggerState>,
    /// Whether the per-trigger timers/carriers have been initialised (first tick).
    pub started: bool,
    /// Whether a mission timer is running, and its remaining ticks. Hashed.
    pub mission_timer: Option<i32>,
    /// Cells where a friendly civilian VIP standing on it counts as evacuated
    /// (dropped by `TACTION_DZ`). Our simplified evac point (see Q for the
    /// aircraft-leaves-map deviation).
    pub evac_cells: Vec<CellCoord>,
    /// Per-house "a civilian has been evacuated" latch
    /// (`HouseClass::IsCivEvacuated`). Drives `TEVENT_EVAC_CIVILIAN`.
    pub civ_evacuated: Vec<bool>,
    /// `TACTION_REVEAL_ALL` fired (client reveals the whole map). Cosmetic.
    pub reveal_all: bool,
    /// Cells the client should reveal (from `REVEAL_SOME` waypoints). Cosmetic —
    /// drained by the client, not hashed.
    pub reveal_cells: Vec<u32>,
    /// Text-message ids queued for the client (`TutorialText`). Cosmetic.
    pub pending_texts: Vec<i32>,
    /// EVA speech ids queued for the client. Cosmetic.
    pub pending_speech: Vec<i32>,
}

impl Campaign {
    /// A cell number resolved from a waypoint index, or `None` if unset/out of
    /// range.
    pub fn waypoint_cell(&self, wp: i32) -> Option<CellCoord> {
        if wp < 0 {
            return None;
        }
        let cell = *self.waypoints.get(wp as usize)?;
        if cell < 0 {
            None
        } else {
            Some(CellCoord::from_index(cell as u32))
        }
    }

    /// Whether house `h` has had a civilian evacuated.
    pub fn is_civ_evacuated(&self, h: u8) -> bool {
        self.civ_evacuated.get(h as usize).copied().unwrap_or(false)
    }

    /// Fold the sim-relevant campaign state into the world hash. Cosmetic outputs
    /// (`reveal_cells`/`pending_texts`/`pending_speech`) are excluded — they never
    /// feed the sim, only the client.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(0xCA);
        h.write_u8(self.started as u8);
        for &g in &self.globals {
            h.write_u8(g as u8);
        }
        for s in &self.state {
            h.write_u8(s.sprung as u8);
            h.write_u8(s.destroyed as u8);
            h.write_i32(s.e1_timer);
            h.write_i32(s.e2_timer);
            h.write_i32(s.carriers);
            h.write_u8(s.any_destroyed as u8);
            h.write_u8(s.any_attacked as u8);
        }
        match self.mission_timer {
            Some(t) => {
                h.write_u8(1);
                h.write_i32(t);
            }
            None => h.write_u8(0),
        }
        h.write_u32(self.evac_cells.len() as u32);
        for c in &self.evac_cells {
            h.write_i32(c.x);
            h.write_i32(c.y);
        }
        for &e in &self.civ_evacuated {
            h.write_u8(e as u8);
        }
        h.write_u8(self.reveal_all as u8);
    }
}
