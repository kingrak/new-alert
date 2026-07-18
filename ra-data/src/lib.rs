//! `ra-data` — rules.ini + scenario INI parsed into typed static game data
//! (the original engine's `*TypeClass` stat layer, expressed as data).
//!
//! See `docs/DESIGN.md` §4.1 (crate layout) and §3.8 (rules.ini as the single
//! source of stats).
//!
//! Modules:
//! - [`templates`] — the terrain template catalog (id → filename + theaters).
//! - [`scenario`]  — scenario INI: theater, map rectangle, and the decoded
//!   `[MapPack]` / `[OverlayPack]` terrain cells.

pub mod scenario;
pub mod templates;
