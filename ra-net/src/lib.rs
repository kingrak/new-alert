//! `ra-net` — command transport behind one trait: local loopback (single
//! player), then LAN peer lockstep, then server relay. The sim is
//! network-shaped from day one; only this crate grows per networking stage
//! (DESIGN.md §4.6 — three transports, one sim).
//!
//! # Contracts (DESIGN.md §4.6, §4.7)
//!
//! - **std-only.** This crate may talk `std::net` and nothing below it — no
//!   async, no threads. As of M8-B the [`LanTransport`]/lobby layer uses
//!   non-blocking UDP sockets and transport-local wall-clock (keepalives,
//!   timeouts — never command scheduling); the in-process transports remain
//!   socket- and clock-free. Anything OS-conditional hides in
//!   [`platform`]; a `#[cfg(target_os)]` anywhere else in this crate fails
//!   review (§4.7).
//! - **No sim dependency beyond `ra-sim` types.** The transport moves
//!   [`ra_sim::Command`] values, tick numbers, and 64-bit state hashes. It
//!   never constructs, reads, or mutates a `World`; the sim only ever consumes
//!   `(tick, ordered commands)` and emits state hashes (§4.6).
//! - **Deterministic scheduling.** The tick at which a submitted command
//!   executes is a pure function of the tick during which it was submitted —
//!   `T_submit + input_delay` — never of arrival timing. This is the
//!   original's MaxAhead scheme: the *sender* stamps each outgoing event with
//!   `Frame + Session.MaxAhead` (QUEUE.CPP `Add_Uncompressed_Events`,
//!   queue.cpp:2526) and every peer executes strictly by that stamp, in
//!   canonical house order (`Execute_DoList`, queue.cpp:3286-3321). Arrival
//!   timing may only *stall* a peer at the tick barrier
//!   ([`PollResult::Waiting`]); it can never reorder or reschedule.
//! - **Divergence is a state, not a panic.** Peers exchange per-tick state
//!   hashes ([`CommandTransport::report_hash`], the original's FRAMEINFO CRC
//!   ring, queue.cpp:3448-3466); a mismatch surfaces as
//!   [`PollResult::Desync`] with the first mismatching tick attributed, so a
//!   later stage can resync from a snapshot (§3.6) instead of ending the
//!   match (§3.4: the original tears the game down here).
//!
//! # Stage map
//!
//! - Stage 1 (this crate, M8-A): [`LocalTransport`] — zero-delay loopback,
//!   preserving single-player behavior exactly, plus [`PairTransport`] — two
//!   in-process endpoints over deterministic in-memory queues, the socket-free
//!   rehearsal of the full LAN lockstep protocol.
//! - Stage 2 (M8-B, this cycle): [`LanTransport`] — UDP peer-to-peer over
//!   the [`wire`] datagram format, same scheduler/barrier/hash semantics,
//!   plus broadcast discovery and a host-authoritative lobby ([`lobby`]).
//! - Stage 3: `RelayTransport` — server-sequenced internet play.
//!
//! One deliberate deviation from the §4.6 trait sketch: `poll` returns
//! [`PollResult`], not a bare `TickBundle`. The sketch's shape cannot express
//! the tick barrier ("cannot execute tick T until every peer's bundle for T is
//! held") without blocking, and blocking would require threads or wall-clock
//! waits that §4.2/§4.7 forbid in this layer; the enum also carries the desync
//! state the same non-blocking way.

pub mod lan;
pub mod lobby;
pub mod local;
pub mod pair;
pub mod platform;
pub mod scheduler;
pub mod transport;
pub mod wire;

pub use lan::{LanTransport, PEER_TIMEOUT, REDUNDANT_TICKS};
pub use lobby::{
    DiscoveredSession, DiscoveryConfig, HostLobby, JoinLobby, SessionBrowser, SessionSettings,
    WelcomeInfo,
};
pub use local::LocalTransport;
pub use pair::{JitterConfig, PairTransport};
pub use scheduler::{InputScheduler, DEFAULT_INPUT_DELAY};
pub use transport::{
    CommandTransport, ConnectionLost, DesyncDetected, LostReason, PollResult, SeatId, Tick,
    TickBundle,
};
