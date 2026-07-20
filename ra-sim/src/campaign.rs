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

/// Number of `HousesType` slots a campaign world allocates (`Spain`..`Multi8`,
/// `hdata.cpp`). Mirrors the loader's `CAMPAIGN_HOUSE_COUNT`; kept here so the sim
/// can bounds-check trigger-action house indices without depending on `ra-data`.
pub const CAMPAIGN_HOUSE_SLOTS: usize = 20;

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
    /// Alert the target house so it forms autocreate teams (`taction.h:58`).
    pub const AUTOCREATE: u8 = 13;
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
    /// Unload the transport at the team's current location (`TMISSION_UNLOAD`).
    pub const UNLOAD: i32 = 8;
    /// `TMISSION_DO` (`teamtype.h:57`): the team members adopt the `MissionType`
    /// named by the mission's `arg` and it "sticks" (guard / sticky / area-guard /
    /// hunt). The common autocreate-team script is `DO:MISSION_HUNT` (arg 14).
    pub const DO: i32 = 11;
    /// Load the team's foot members onto its transport member (`TMISSION_LOAD`).
    pub const LOAD: i32 = 14;
    pub const PATROL: i32 = 16;

    /// `MissionType::MISSION_HUNT` (`mission.h`, index 14) — the `arg` of the
    /// `DO:14` autocreate-team script that makes the team hunt the player.
    pub const MISSION_HUNT_ARG: i32 = 14;
}

/// `TeamTypeClass` packed-flag bits, as (de)serialised in `teamtype.cpp:1787`.
pub mod team_flags {
    /// The computer may create this team automatically on the alerted/autocreate
    /// cadence (`TeamTypeClass::IsAutocreate`, `teamtype.h:219`). Bit `0x4`.
    pub const AUTOCREATE: u32 = 0x0004;
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
    /// Passenger capacity (`Passengers=`) — non-zero for a transport (APC).
    pub passengers: u8,
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

/// Campaign **enemy-activation** state (M7.5-C): the runtime latches and static
/// data that let a *computer* house form autocreate teams and produce/rebuild —
/// the scripted-enemy behaviour gated behind `TACTION_AUTOCREATE` /
/// `TACTION_BEGIN_PRODUCTION`. Kept in a small side-struct (rather than folded
/// into [`Campaign`]) so it can be added, hashed only when it is actually doing
/// something, and left `None` for a skirmish — with no churn to the ~19 existing
/// `Campaign { … }` literals.
///
/// Ported from `HouseClass::IsAlerted`/`IsStarted` + `AlertTime` (house.cpp:1042,
/// house.h:781) and `BaseClass` (base.cpp:432 `[Base]`, base.cpp:377
/// `Next_Buildable`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnemyActivation {
    /// Per-house `IsAlerted` latch (`TACTION_AUTOCREATE` sets it, taction.cpp:645).
    /// When set, the house forms autocreate-flagged teams on the `AlertTime`
    /// cadence (house.cpp:1042). Grown to house count by the loader.
    pub alerted: Vec<bool>,
    /// Per-house autocreate countdown (`AlertTime`, house.cpp:1056). `0` fires a
    /// wave this tick then re-arms; only meaningful while the house is alerted.
    pub alert_timer: Vec<i32>,
    /// Per-house `IsStarted` latch (`TACTION_BEGIN_PRODUCTION` sets it,
    /// house.h:781). When set, the house produces from its live factories and (if
    /// it owns the `[Base]` list) rebuilds destroyed base buildings.
    pub production: Vec<bool>,
    /// The `[Base]` owner (`BaseClass::House`, base.cpp:443) — the one computer
    /// house whose destroyed base buildings are rebuilt.
    pub base_house: u8,
    /// The `[Base]` rebuild list (`BaseClass::Nodes`, base.cpp:432): ordered
    /// `(building-proto id, footprint top-left cell)`; **list order is the rebuild
    /// priority** (`Next_Buildable`, base.cpp:377). Static — not hashed.
    pub base_nodes: Vec<(u32, CellCoord)>,
    /// Scenario `[Basic] TechLevel`, for the autocreate wave-count formula
    /// (`Random_Pick(2, (TechLevel-1)/3+1)`, house.cpp:1047). Static.
    pub tech_level: i32,
}

impl EnemyActivation {
    /// Whether any house is currently alerted or has begun production — the gate
    /// for both running the system and folding it into the hash (so a campaign
    /// that never fires either trigger, e.g. Allied mission 1, is byte-identical).
    pub fn is_active(&self) -> bool {
        self.alerted.iter().any(|&a| a) || self.production.iter().any(|&p| p)
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(0xEA);
        for &a in &self.alerted {
            h.write_u8(a as u8);
        }
        // Fold the AlertTime only for alerted houses (an inactive house's timer is
        // inert), so the appended bytes track exactly what drives future RNG draws.
        for (i, &t) in self.alert_timer.iter().enumerate() {
            if self.alerted.get(i).copied().unwrap_or(false) {
                h.write_i32(t);
            }
        }
        for &p in &self.production {
            h.write_u8(p as u8);
        }
    }
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
