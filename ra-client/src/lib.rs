//! `ra-client` — the macroquad application layer: decoding assets to textures,
//! camera, input → commands, tick interpolation, audio, and the UI shell. This
//! is the only crate that observes the platform; it observes the sim but never
//! reaches into it.
//!
//! See `docs/DESIGN.md` §4.1 and §4.5 (rendering). Stub crate: populated
//! starting at milestone M2.
