//! `ra-net` — command transport behind a trait: local loopback (single player),
//! then LAN peer lockstep, then server relay. The sim is network-shaped from day
//! one; only this crate grows per networking stage.
//!
//! See `docs/DESIGN.md` §4.6 (networking evolution) and §4.1. Stub crate:
//! populated starting at milestone M8.
