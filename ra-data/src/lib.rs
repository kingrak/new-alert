//! `ra-data` — rules.ini + scenario INI parsed into typed static game data
//! (the original engine's `*TypeClass` stat layer, expressed as data).
//!
//! See `docs/DESIGN.md` §4.1 (crate layout) and §3.8 (rules.ini as the single
//! source of stats).
//!
//! Modules:
//! - [`templates`]    — the terrain template catalog (id → filename + theaters).
//! - [`scenario`]     — scenario INI: theater, map rectangle, the decoded
//!   `[MapPack]` / `[OverlayPack]` terrain cells, and `[UNITS]` placements.
//! - [`rules`]        — unit stats (Speed/ROT/Strength) from `rules.ini` (§3.8).
//! - [`combat`]       — weapon/warhead/projectile rules + resolved unit combat.
//! - [`house`]        — the eight countries and their colour-remap tables.
//! - [`passability`]  — a coarse passable/impassable grid from terrain (§3.7).

pub mod buildings;
pub mod combat;
pub mod house;
pub mod passability;
pub mod rules;
pub mod scenario;
pub mod templates;
