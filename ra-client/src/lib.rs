//! `ra-client` — the application layer: decoding assets to textures, camera,
//! input → commands, and the UI shell. The only crate that observes the
//! platform; it observes the sim but never reaches into it (DESIGN.md §4.1,
//! §4.5, §4.7, §4.8).
//!
//! Architecture (DESIGN.md §4.8): all behavior lives in the windowless
//! [`AppCore`]; the macroquad shell is a thin adapter. Everything below is
//! reachable headless:
//!
//! - [`compositor`] — pure indexed → RGBA compositing core.
//! - [`terrain`]    — scenario + templates → indexed terrain raster.
//! - [`appcore`]    — windowless core: camera state, `handle`/`update`/`compose`.
//! - [`input`]      — our own [`InputEvent`] vocabulary (no macroquad types leak).
//! - [`assets`]     — load a scenario's terrain from the real archives.
//! - [`png`]        — dependency-free PNG writer for `--dump`.
//! - [`platform`]   — the sole home of platform-specific code (§4.7).

pub mod appcore;
pub mod assets;
pub mod compositor;
pub mod font;
pub mod input;
pub mod menu;
pub mod platform;
pub mod png;
pub mod terrain;
pub mod unit_render;

#[cfg(feature = "window")]
pub mod shell;

pub use appcore::{AppCore, Command, Frame, NetEnd, SoundEvent};
pub use input::{InputEvent, Key, MouseButton, Rect};
