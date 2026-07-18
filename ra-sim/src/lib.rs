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
//! Modules (M3):
//! - [`coords`] — leptons, cells, world coordinates, binary-angle facings.
//! - [`fixed`]  — a 16.16 fixed-point number for the few fractional needs.
//! - [`rng`]    — the seeded LCG ported from the original `RandomClass`.
//! - [`arena`]  — a generational arena addressed by [`arena::Handle`].
//! - [`hash`]   — the hand-rolled FNV-1a per-tick state hasher.
//! - [`path`]   — deterministic grid A* over a passability grid.
//! - [`unit`]   — the movable [`unit::Unit`] entity and its movement stats.
//! - [`world`]  — [`World`], the command pipeline, and the fixed system order.
#![deny(clippy::float_arithmetic)]

pub mod arena;
pub mod coords;
pub mod fixed;
pub mod hash;
pub mod path;
pub mod rng;
pub mod unit;
pub mod world;

pub use arena::{Arena, Handle};
pub use coords::{CellCoord, Facing, Lepton, WorldCoord, LEPTONS_PER_CELL};
pub use path::Passability;
pub use rng::RandomLcg;
pub use unit::{MoveStats, Unit};
pub use world::{apply, Command, World};
