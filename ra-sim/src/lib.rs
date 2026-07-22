//! `ra-sim` — the deterministic simulation core. Owns the `World` state, the
//! systems, and command application. NO floating point, NO rendering, NO
//! wall-clock, NO I/O, NO OS randomness.
//!
//! See `docs/DESIGN.md` §4.1 (crate layout), §4.2 (determinism contract), and
//! §4.3/§4.4 (entity model + command pipeline). Stub crate: populated starting
//! at milestone M3.
//!
//! The determinism contract is load-bearing and is asserted from the very first
//! commit: fixed-point arithmetic only — floating point is a compile error in
//! this crate (see the crate-level attribute below). Keep it here permanently.
//!
//! Modules:
//! - [`coords`] — leptons, cells, world coordinates, binary-angle facings.
//! - [`fixed`]  — a 16.16 fixed-point number for the few fractional needs.
//! - [`rng`]    — the seeded LCG ported from the original `RandomClass`.
//! - [`arena`]  — a generational arena addressed by [`arena::Handle`].
//! - [`hash`]   — the hand-rolled FNV-1a per-tick state hasher.
//! - [`path`]   — deterministic grid A* over a passability grid.
//! - [`combat`] — weapon/warhead profiles and the ported `Modify_Damage` (M4).
//! - [`bullet`] — projectiles in flight (M4).
//! - [`unit`]   — the movable [`unit::Unit`] entity, its movement + combat stats.
//! - [`world`]  — [`World`], the command pipeline, and the fixed system order.
#![deny(clippy::float_arithmetic)]

pub mod ai;
pub mod arena;
pub mod building;
pub mod bullet;
pub mod campaign;
pub mod catalog;
pub mod combat;
pub mod coords;
pub mod fixed;
pub mod hash;
pub mod house;
pub mod occupancy;
pub mod ore;
pub mod path;
pub mod rng;
pub mod shroud;
pub mod snapshot;
pub mod superweapon;
pub mod unit;
pub mod world;

pub use ai::{AiPlayer, AiProfile, Difficulty};
pub use arena::{Arena, Handle};
pub use building::Building;
pub use bullet::Bullet;
pub use campaign::{
    Campaign, EnemyActivation, SpawnProto, TActionDef, TEventDef, TeamClass, TeamMission, TeamType,
    TriggerType,
};
pub use catalog::{AiRules, BuildingProto, Catalog, EconRules, IqRules, UnitProto};
pub use combat::{modify_damage, Target, WarheadProfile, WeaponProfile, ARMOR_COUNT};
pub use coords::{
    spot_index, CellCoord, Facing, Lepton, Locomotor, WorldCoord, LEPTONS_PER_CELL, SPOT_OFFSET,
    SUBCELL_COUNT,
};
pub use house::{BuildItem, Handicap, House, ProdKind, Production};
pub use occupancy::UnitGrid;
pub use ore::{OreCell, OreField};
pub use path::Passability;
pub use rng::RandomLcg;
pub use shroud::Shroud;
pub use snapshot::{SnapError, SnapReader, SnapWriter, GAME_BUILD, SNAPSHOT_VERSION};
pub use superweapon::{NukeStrike, SuperKind, SuperWeapon};
pub use unit::{
    AirState, HarvStatus, HarvestState, Mission, MoveStats, Passenger, Unit, UnitKind, FLIGHT_LEVEL,
};
pub use world::{apply, Command, GameOver, World, LOCO_AIR_INDEX, LOCO_WATER_INDEX};
