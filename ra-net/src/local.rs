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
