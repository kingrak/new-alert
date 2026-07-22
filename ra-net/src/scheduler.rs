//! The lockstep input-delay scheduler — the MaxAhead mechanism of QUEUE.CPP
//! with the timing made explicit and deterministic.
//!
//! **What the original does.** Player input becomes `EventClass` records
//! queued into `OutList`; when the queue layer runs for frame `F` it stamps
//! each outgoing event's execution frame as `OutList.First().Frame = Frame +
//! frame_delay` with `frame_delay = Session.MaxAhead`
//! (`Add_Uncompressed_Events`, queue.cpp:2526; caller passes
//! `Session.MaxAhead`, queue.cpp:789-790; the compressed protocol additionally
//! rounds up to a `FrameSendRate` multiple, queue.cpp:2754-2759). Every system
//! — including the sender — then executes the event only when `Frame >=
//! DoList[j].Frame` (`Execute_DoList`, queue.cpp:3321). MaxAhead defaults to 5
//! frames (session.cpp:167) and is retuned at runtime via TIMING events
//! (queue.cpp:1440-1461) — runtime retuning is out of scope until M8-B.
//!
//! **Our rule.** A command submitted during tick `T` (i.e. before the
//! scheduler's stamp for `T`) executes at `T + delay` on every peer. The stamp
//! travels with the bundle, so the execution tick is fixed by the *sender's*
//! clock alone — arrival timing cannot move it (the determinism contract in
//! the crate docs).

use std::collections::BTreeMap;

use ra_sim::Command;

use crate::transport::Tick;

/// Default input delay in ticks for peer lockstep, per DESIGN.md §4.6: "LAN's
/// low, stable latency makes a small fixed input delay (2–3 ticks at 15 Hz)
/// imperceptible". (The original's MaxAhead default is 5 frames,
/// session.cpp:167, sized for IPX-era latency and its every-`n`-frames send
/// cadence; we send every tick.)
pub const DEFAULT_INPUT_DELAY: u32 = 3;

/// Sender-side command scheduler: stage → stamp(`T`) → due at `T + delay`.
///
/// [`LocalTransport`](crate::LocalTransport) runs one with `delay = 0` (the
/// zero-delay loopback — stamp and take in the same poll);
/// [`PairTransport`](crate::PairTransport) runs one per endpoint with the
/// session's shared input delay.
#[derive(Clone, Debug, Default)]
pub struct InputScheduler {
    /// Ticks of input delay: a command stamped at tick `T` executes at `T + delay`.
    delay: u32,
    /// Commands submitted since the last stamp, in issue order.
    staged: Vec<Command>,
    /// Stamped commands keyed by execution tick.
    scheduled: BTreeMap<Tick, Vec<Command>>,
}

impl InputScheduler {
    /// A scheduler with the given input delay in ticks.
    pub fn new(delay: u32) -> InputScheduler {
        InputScheduler {
            delay,
            staged: Vec::new(),
            scheduled: BTreeMap::new(),
        }
    }

    /// The configured input delay in ticks.
    pub fn delay(&self) -> u32 {
        self.delay
    }

    /// Stage a command (issue order is preserved through to execution).
    pub fn submit(&mut self, cmd: Command) {
        self.staged.push(cmd);
    }

    /// Stamp everything staged since the last stamp for execution at
    /// `tick + delay` (queue.cpp:2526), scheduling it locally and returning
    /// `(execution_tick, the stamped commands)` — the copy a networked
    /// transport puts on the wire. Call exactly once per tick, at the first
    /// poll of that tick.
    pub fn stamp(&mut self, tick: Tick) -> (Tick, Vec<Command>) {
        let exec = tick + self.delay;
        let cmds = std::mem::take(&mut self.staged);
        let wire = cmds.clone();
        if !cmds.is_empty() {
            self.scheduled.entry(exec).or_default().extend(cmds);
        }
        (exec, wire)
    }

    /// Remove and return the commands due at exactly `tick` (empty if none) —
    /// the `Frame >= DoList[j].Frame` execution gate (queue.cpp:3321), exact
    /// because the barrier guarantees we never skip a tick.
    pub fn take_due(&mut self, tick: Tick) -> Vec<Command> {
        self.scheduled.remove(&tick).unwrap_or_default()
    }
}
