//! The [`CommandTransport`] trait and its wire vocabulary: [`Tick`],
//! [`TickBundle`], [`PollResult`], [`DesyncDetected`] (DESIGN.md §4.6).

use ra_sim::Command;

/// A simulation tick number — the same counter `World::tick_count` advances.
pub type Tick = u32;

/// A transport seat: the stable per-player identity that owns a command
/// stream. In practice this is the player's **house** id, so that bundle
/// ordering below is literally the original's canonical house order; it is
/// the analogue of `EventClass::ID` (`PlayerPtr->ID`, queue.cpp:2531).
/// Ownership validation does *not* rely on it — every [`Command`] carries its
/// issuing house explicitly (§4.6 trust boundaries).
pub type SeatId = u8;

/// Every seat's commands for one tick, in canonical order.
///
/// `seats` is sorted ascending by [`SeatId`] and contains one entry per seat
/// in the session (empty command list when a seat issued nothing). Applying
/// `flatten()` on every peer therefore executes the same commands in the same
/// order — the original executes its DoList "in the order of the HouseClass
/// array" precisely because "events must be executed in the same order on all
/// systems" (`Execute_DoList`, queue.cpp:3281-3290), with each house's own
/// events kept in issue order (the in-order DoList scan, queue.cpp:3312-3321).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickBundle {
    /// The tick these commands execute on.
    pub tick: Tick,
    /// Per-seat command lists, ascending by seat id.
    pub seats: Vec<(SeatId, Vec<Command>)>,
}

impl TickBundle {
    /// The commands of every seat concatenated in canonical (seat-ascending,
    /// then issue) order — exactly what `apply(world, tick, &cmds)` consumes.
    pub fn flatten(&self) -> Vec<Command> {
        let mut out = Vec::with_capacity(self.command_count());
        for (_, cmds) in &self.seats {
            out.extend_from_slice(cmds);
        }
        out
    }

    /// Total number of commands across all seats.
    pub fn command_count(&self) -> usize {
        self.seats.iter().map(|(_, c)| c.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_sim::{ProdKind, RandomLcg};

    fn cmd(tag: u8) -> Command {
        Command::CancelProduction {
            house: tag,
            kind: ProdKind::Building,
        }
    }

    /// `flatten()` on a bundle already in canonical (seat-ascending) order
    /// concatenates each seat's commands in that order, preserving each
    /// seat's own issue order — the literal contract this type exists to
    /// guarantee (queue.cpp:3281-3290 house-array order + queue.cpp:3312-3321
    /// per-house issue order).
    #[test]
    fn flatten_concatenates_in_seat_ascending_then_issue_order() {
        let bundle = TickBundle {
            tick: 0,
            seats: vec![
                (1, vec![cmd(11), cmd(12)]),
                (3, vec![cmd(31)]),
                (7, vec![]),
                (9, vec![cmd(91), cmd(92), cmd(93)]),
            ],
        };
        assert_eq!(
            bundle.flatten(),
            vec![cmd(11), cmd(12), cmd(31), cmd(91), cmd(92), cmd(93)]
        );
        assert_eq!(bundle.command_count(), 6);
    }

    /// Property, swept over many seeded random seat-command layouts: for a
    /// bundle whose `seats` are sorted ascending by `SeatId` (the contract
    /// every [`CommandTransport`] impl must uphold — see `PairTransport`'s
    /// `seats.sort_by_key` at the end of `poll`), `flatten()` must equal a
    /// reference built by manually walking the seats in ascending id order.
    /// This is deliberately proven *independently* of `TickBundle::flatten`'s
    /// own implementation (the reference walks `seats` with a hand-rolled
    /// loop over a `BTreeMap`, not by calling `flatten` on a re-sorted copy),
    /// so a bug in `flatten` itself cannot cancel out against the same bug in
    /// the reference.
    #[test]
    fn flatten_ordering_property_holds_across_many_random_layouts() {
        let mut rng = RandomLcg::new(0x5EA7_0001);
        for case in 0..200u32 {
            // Random distinct seat ids (0..16), random command counts (0..4).
            let num_seats = 1 + (rng.range(0, 7) as usize);
            let mut seat_ids: Vec<u8> = Vec::new();
            while seat_ids.len() < num_seats {
                let candidate = rng.range(0, 15) as u8;
                if !seat_ids.contains(&candidate) {
                    seat_ids.push(candidate);
                }
            }
            let mut seats: Vec<(SeatId, Vec<Command>)> = seat_ids
                .iter()
                .map(|&s| {
                    let n = rng.range(0, 3) as usize;
                    let cmds = (0..n).map(|i| cmd(s.wrapping_add(i as u8))).collect();
                    (s, cmds)
                })
                .collect();
            // Canonical order per the contract: ascending by seat id.
            seats.sort_by_key(|&(s, _)| s);

            // Independent reference: a BTreeMap walk (ascending key order by
            // construction), not a call to `flatten`.
            let mut reference_map: std::collections::BTreeMap<SeatId, Vec<Command>> =
                std::collections::BTreeMap::new();
            for (s, cmds) in &seats {
                reference_map.insert(*s, cmds.clone());
            }
            let mut reference = Vec::new();
            for cmds in reference_map.values() {
                reference.extend_from_slice(cmds);
            }

            let bundle = TickBundle { tick: case, seats };
            assert_eq!(
                bundle.flatten(),
                reference,
                "case {case}: flatten() diverged from the independent seat-ascending reference"
            );
            assert_eq!(bundle.command_count(), reference.len());
        }
    }

    /// Revert-sensitivity anchor for the property above: if `seats` is
    /// *not* sorted ascending (a hypothetical broken `CommandTransport` impl
    /// that forgot to sort), `flatten()` faithfully reproduces that
    /// (non-canonical) order rather than silently re-sorting — proving the
    /// ordering guarantee lives in the *producer* contract, not inside
    /// `flatten` itself, exactly as the doc comment states ("Applying
    /// `flatten()` on every peer therefore executes the same commands in the
    /// same order" — same order as `seats`, not a re-derived canonical one).
    #[test]
    fn flatten_does_not_itself_enforce_canonical_order() {
        let bundle = TickBundle {
            tick: 0,
            // Deliberately descending — flatten must not silently fix this.
            seats: vec![(9, vec![cmd(9)]), (1, vec![cmd(1)])],
        };
        assert_eq!(
            bundle.flatten(),
            vec![cmd(9), cmd(1)],
            "flatten must reproduce seats' actual order, not re-sort it"
        );
    }
}

/// A detected lockstep divergence: two peers reported different state hashes
/// for the same tick. This is a *state* the session enters (the M8-B/C resync
/// hook), not a panic — unlike the original, where a FRAMEINFO CRC mismatch
/// puts up "Out of sync" and tears down the connections (queue.cpp:3298-3307
/// per DESIGN.md §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesyncDetected {
    /// The first tick (as detected) whose hashes disagreed.
    pub tick: Tick,
    /// This endpoint's hash for that tick.
    pub local_hash: u64,
    /// The peer's hash for that tick.
    pub remote_hash: u64,
    /// The seat that reported the disagreeing hash.
    pub peer: SeatId,
}

/// Outcome of [`CommandTransport::poll`]. Non-blocking by design: a transport
/// can never wait on a thread or the wall clock (§4.2), so "not yet" and
/// "diverged" are values, not conditions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollResult {
    /// Every seat's commands for the requested tick are in hand; the caller
    /// must now apply exactly this bundle to its `World`.
    Ready(TickBundle),
    /// Tick barrier: some peer's bundle for this tick has not arrived yet.
    /// The sim must *stall* (poll again later, same tick) — never advance
    /// without the bundle. This is the original's frame-sync rule "our
    /// current frame # must be < their_frame + Session.MaxAhead"
    /// (queue.cpp:477-478); because we stall instead of free-running, the
    /// original's fatal "packet received too late" case (queue.cpp:3328-3343)
    /// is structurally impossible here.
    Waiting,
    /// The session has diverged (hash mismatch). Sticky: every subsequent
    /// poll returns the same state.
    Desync(DesyncDetected),
}

/// One trait in `ra-net` behind which everything network-shaped hides
/// (DESIGN.md §4.6). The game loop's contract, per tick `T`:
///
/// 1. [`submit`](Self::submit) any local player commands issued during `T`;
/// 2. [`poll`](Self::poll)`(T)` until [`PollResult::Ready`] (stall on
///    [`PollResult::Waiting`] without ticking the sim);
/// 3. apply the bundle via `apply(world, T, &bundle.flatten())`;
/// 4. [`report_hash`](Self::report_hash)`(T, world.state_hash())`.
///
/// Commands submitted between the first poll of `T` and the first poll of
/// `T + 1` are scheduled for tick `T + 1 + input_delay` — the sender-side
/// stamp of queue.cpp:2526, applied at the first moment the transport sees
/// them.
pub trait CommandTransport {
    /// Queue a local player command. It will execute — on every peer, at the
    /// same tick — `input_delay` ticks after the tick in which the transport
    /// next stamps it (see the trait docs; zero delay for [`crate::LocalTransport`]).
    fn submit(&mut self, cmd: Command);

    /// Try to assemble every seat's commands for `tick`. Ticks must be polled
    /// in order, starting at 0; re-polling the same tick after
    /// [`PollResult::Waiting`] is how a stalled peer catches up.
    fn poll(&mut self, tick: Tick) -> PollResult;

    /// Report this endpoint's post-tick state hash for `tick`, to be compared
    /// against every peer's hash for the same tick (the FRAMEINFO CRC
    /// exchange, queue.cpp:3448-3466).
    fn report_hash(&mut self, tick: Tick, hash: u64);
}
