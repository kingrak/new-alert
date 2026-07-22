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
//! frames (session.cpp:175) and is retuned at runtime via TIMING events
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
/// session.cpp:175, sized for IPX-era latency and its every-`n`-frames send
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

#[cfg(test)]
mod tests {
    use super::*;
    use ra_sim::ProdKind;

    /// A cheap, primitive-only `Command` — no `World`/`Handle` needed, so
    /// these tests exercise `InputScheduler` in complete isolation from the
    /// sim, per its own doc contract ("no sim dependency beyond `ra_sim`
    /// types").
    fn cmd(house: u8) -> Command {
        Command::CancelProduction {
            house,
            kind: ProdKind::Building,
        }
    }

    /// Edge: `stamp` called with nothing staged must not panic, must not
    /// schedule a phantom empty entry, and must still report the correct
    /// exec tick (a networked transport ships `(exec_tick, wire)` on every
    /// poll, including empty ticks — the wire payload just carries nothing).
    #[test]
    fn stamp_with_nothing_staged_is_empty_and_schedules_nothing() {
        let mut s = InputScheduler::new(3);
        let (exec, wire) = s.stamp(10);
        assert_eq!(exec, 13, "exec tick must still be tick + delay");
        assert!(wire.is_empty(), "wire payload must be empty");
        // No phantom entry: taking due at the exec tick returns empty too,
        // and does so via the same `unwrap_or_default` path as a tick that
        // was never stamped at all (both must behave identically).
        assert!(s.take_due(13).is_empty());
    }

    /// Edge: `take_due` on a tick nothing was ever scheduled for is empty,
    /// not a panic — the barrier's execution gate (queue.cpp:3321) must
    /// degrade gracefully when a tick has genuinely nothing due.
    #[test]
    fn take_due_on_an_untouched_tick_is_empty() {
        let mut s = InputScheduler::new(0);
        assert!(s.take_due(0).is_empty());
        assert!(s.take_due(9_999).is_empty());
    }

    /// Edge ("stamp-while-waiting"): a command submitted *after* tick T has
    /// already been stamped, but *before* T+1 is stamped, must NOT retroactively
    /// join T's bundle — `stamp` drains `staged` at the moment it is called, so
    /// late submissions belong to the next stamp cycle and execute one tick
    /// later than a command that made the T stamp. This is the scenario a
    /// stalled lockstep peer hits: local input keeps arriving while `poll`
    /// re-spins on `Waiting`, but the transport must only call `stamp` once
    /// per tick (PairTransport's `stamped` guard) — this test pins the
    /// scheduler-level contract that guard depends on.
    #[test]
    fn command_submitted_after_stamp_executes_one_tick_later() {
        let mut s = InputScheduler::new(2);
        s.submit(cmd(1)); // staged before tick 5's stamp
        let (exec5, wire5) = s.stamp(5);
        assert_eq!(exec5, 7);
        assert_eq!(wire5, vec![cmd(1)]);

        // Submitted *after* tick 5 was stamped, before tick 6 is stamped —
        // must not appear in what's due at tick 7 (tick 5's exec tick).
        s.submit(cmd(2));
        assert_eq!(
            s.take_due(7),
            vec![cmd(1)],
            "late submission must not retroactively join the already-stamped tick"
        );

        // It surfaces one full cycle later: stamped at 6, due at 8.
        let (exec6, wire6) = s.stamp(6);
        assert_eq!(exec6, 8);
        assert_eq!(wire6, vec![cmd(2)]);
        assert_eq!(s.take_due(8), vec![cmd(2)]);
    }

    /// Issue order is preserved end-to-end: several commands staged before
    /// one stamp come out of `take_due` in the same order (the original's
    /// in-order DoList scan per house, queue.cpp:3312-3321, depends on this
    /// for a single house's own event ordering).
    #[test]
    fn issue_order_is_preserved_through_a_single_stamp_cycle() {
        let mut s = InputScheduler::new(1);
        for h in [3u8, 1, 4, 1, 5] {
            s.submit(cmd(h));
        }
        let (exec, wire) = s.stamp(0);
        let expected: Vec<Command> = [3u8, 1, 4, 1, 5].into_iter().map(cmd).collect();
        assert_eq!(wire, expected, "wire payload must preserve issue order");
        assert_eq!(
            s.take_due(exec),
            expected,
            "scheduled/take_due must preserve issue order too"
        );
    }

    /// Commands staged across *two different* source ticks but landing on
    /// the *same* exec tick (delay chosen so `T1 + delay == T2 + delay - 1`
    /// is not possible without unequal delays — instead we drive two stamp
    /// calls whose targets collide by construction) must still concatenate
    /// in stamp-call order, never drop or duplicate. Exercises the
    /// `entry(...).or_default().extend(cmds)` accumulation path.
    #[test]
    fn two_stamps_landing_on_the_same_exec_tick_accumulate_in_order() {
        let mut s = InputScheduler::new(0);
        // delay 0: stamp(5) and a manually-collided second batch both target
        // exec tick 5 by calling stamp again for the same tick number (a
        // transport would never do this in practice — PairTransport's
        // `stamped` flag exists precisely to prevent it — but the scheduler
        // itself must not corrupt state if it happens, since that flag lives
        // one layer up).
        s.submit(cmd(9));
        let (exec_a, _) = s.stamp(5);
        s.submit(cmd(8));
        let (exec_b, _) = s.stamp(5);
        assert_eq!((exec_a, exec_b), (5, 5));
        assert_eq!(
            s.take_due(5),
            vec![cmd(9), cmd(8)],
            "repeated stamps for the same tick must accumulate, not clobber"
        );
    }
}
