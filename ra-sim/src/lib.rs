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
#![deny(clippy::float_arithmetic)]
