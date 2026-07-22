//! Stage 1: [`LocalTransport`] — the zero-delay loopback (DESIGN.md §4.6).
//!
//! Single player runs the full command pipeline through the same trait the
//! LAN transport will use — the original's own unification in reverse: RA ran
//! single player through the same `Queue_AI` path as multiplayer
//! (`Queue_AI_Normal`, queue.cpp:403: OutList → DoList → `Execute_DoList` with
//! one house, queue.cpp:409-430). Zero delay: a command submitted during tick
//! `T` executes at `T`, preserving pre-M8 single-player behavior
//! byte-identically.

use ra_sim::Command;

use crate::scheduler::InputScheduler;
use crate::transport::{CommandTransport, PollResult, SeatId, Tick, TickBundle};

/// Zero-delay loopback transport: one seat, no peers, always
/// [`PollResult::Ready`].
#[derive(Clone, Debug)]
pub struct LocalTransport {
    /// The single local seat (nominal — ordering is trivial with one seat).
    seat: SeatId,
    /// The shared scheduler, run at delay 0 so stamp and take happen in the
    /// same poll: submissions since the previous poll execute this tick, in
    /// issue order — exactly the pre-M8 drain-and-apply behavior.
    sched: InputScheduler,
    /// The most recently reported `(tick, hash)`, kept so the shell/tests can
    /// observe that the hash chain is being fed (single-player has no peer to
    /// compare against; replays assert the chain in CI, §4.6 stage 1).
    last_hash: Option<(Tick, u64)>,
}

impl LocalTransport {
    /// A loopback transport for the local player (seat 0).
    pub fn new() -> LocalTransport {
        LocalTransport {
            seat: 0,
            sched: InputScheduler::new(0),
            last_hash: None,
        }
    }

    /// The most recently reported `(tick, hash)`, if any.
    pub fn last_hash(&self) -> Option<(Tick, u64)> {
        self.last_hash
    }
}

impl Default for LocalTransport {
    fn default() -> LocalTransport {
        LocalTransport::new()
    }
}

impl CommandTransport for LocalTransport {
    fn submit(&mut self, cmd: Command) {
        self.sched.submit(cmd);
    }

    fn poll(&mut self, tick: Tick) -> PollResult {
        // Zero delay: stamp for `tick + 0`, then take what is due — the
        // commands just stamped plus nothing else.
        let _ = self.sched.stamp(tick);
        let cmds = self.sched.take_due(tick);
        PollResult::Ready(TickBundle {
            tick,
            seats: vec![(self.seat, cmds)],
        })
    }

    fn report_hash(&mut self, tick: Tick, hash: u64) {
        self.last_hash = Some((tick, hash));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_sim::ProdKind;

    fn cmd(house: u8) -> Command {
        Command::CancelProduction {
            house,
            kind: ProdKind::Building,
        }
    }

    /// Generic driver bound only by [`CommandTransport`] — the "trait
    /// conformance" check: any implementor must satisfy this exact
    /// submit/poll/report_hash sequence, so writing it against the trait
    /// (not `LocalTransport` directly) proves `LocalTransport` is usable
    /// wherever the trait is required, e.g. `Box<dyn CommandTransport>`.
    fn drive_one_command<T: CommandTransport>(tp: &mut T, tick: Tick, c: Command) -> TickBundle {
        tp.submit(c);
        match tp.poll(tick) {
            PollResult::Ready(b) => b,
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    /// Zero delay: a command submitted for tick T is Ready at T, in the same
    /// poll — no barrier, no stall, ever (there is no peer to wait on).
    #[test]
    fn submits_and_executes_in_the_same_tick() {
        let mut tp = LocalTransport::new();
        let bundle = drive_one_command(&mut tp, 0, cmd(1));
        assert_eq!(
            bundle,
            TickBundle {
                tick: 0,
                seats: vec![(0, vec![cmd(1)])],
            }
        );
    }

    /// A tick with nothing submitted is still `Ready` with an empty command
    /// list for the single seat — `poll` must never return `Waiting` (no
    /// peer exists to wait on) or `Desync` (nothing detects divergence
    /// locally).
    #[test]
    fn empty_tick_is_ready_with_no_commands() {
        let mut tp = LocalTransport::new();
        match tp.poll(0) {
            PollResult::Ready(b) => assert_eq!(
                b,
                TickBundle {
                    tick: 0,
                    seats: vec![(0, vec![])]
                }
            ),
            other => panic!("LocalTransport must never stall or desync, got {other:?}"),
        }
    }

    /// Sequential ticks each pick up exactly their own submissions — proves
    /// the scheduler is properly drained per poll, not accumulating stale
    /// commands across ticks.
    #[test]
    fn sequential_ticks_isolate_their_own_submissions() {
        let mut tp = LocalTransport::new();
        let b0 = drive_one_command(&mut tp, 0, cmd(1));
        assert_eq!(b0.flatten(), vec![cmd(1)]);
        let b1 = drive_one_command(&mut tp, 1, cmd(2));
        assert_eq!(
            b1.flatten(),
            vec![cmd(2)],
            "tick 1 must not replay tick 0's command"
        );
    }

    /// `report_hash` observably updates `last_hash` — the seam
    /// single-player replay/CI hash-chain assertions read.
    #[test]
    fn report_hash_is_observable() {
        let mut tp = LocalTransport::new();
        assert_eq!(tp.last_hash(), None);
        tp.report_hash(0, 0xABCD);
        assert_eq!(tp.last_hash(), Some((0, 0xABCD)));
        tp.report_hash(1, 0xEF01);
        assert_eq!(
            tp.last_hash(),
            Some((1, 0xEF01)),
            "must reflect the most recent report, not the first"
        );
    }
}
