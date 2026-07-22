//! Stage 1.5: [`PairTransport`] — two lockstep endpoints joined by in-memory
//! queues; the socket-free rehearsal of the M8-B LAN protocol.
//!
//! Implements the full peer-lockstep protocol of QUEUE.CPP's
//! `Queue_AI_Multiplayer` with the transport medium swapped for deterministic
//! in-process queues:
//!
//! - **Input-delay scheduling** — each endpoint stamps its outgoing commands
//!   `tick + delay` ([`InputScheduler`], queue.cpp:2526) and ships them to the
//!   peer as a per-tick bundle.
//! - **Tick barrier** — an endpoint cannot complete `poll(T)` until it holds
//!   the peer's bundle for `T`; it stalls ([`PollResult::Waiting`]) instead of
//!   free-running, the non-blocking form of "our current frame # must be <
//!   their_frame + Session.MaxAhead" + the CommandCount catch-up rule
//!   (queue.cpp:477-479). Bundles for ticks `0..delay` are defined empty by
//!   the protocol (nothing can be scheduled there — the same reason the
//!   original's frame 0 sends and returns, queue.cpp:795-800).
//! - **Hash exchange & divergence detection** — `report_hash` crosses the
//!   link; a mismatch for the same tick latches [`DesyncDetected`]
//!   (the FRAMEINFO CRC comparison, queue.cpp:3448-3466, minus the fatal
//!   message box of queue.cpp:3298-3307/§3.4).
//! - **Simulated network conditions** — per-message delivery delay in
//!   "delivery steps" (one step elapses per `poll` on either endpoint), drawn
//!   from a seeded LCG ([`JitterConfig`]). No wall clock, no threads: the
//!   whole schedule is a pure function of the poll sequence and the seed.
//!
//! Both endpoints must be driven from one thread (they share the link through
//! `Rc<RefCell>`); that is the point — M8-A proves the protocol with zero
//! nondeterminism, M8-B swaps only the medium.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ra_sim::Command;

use crate::scheduler::InputScheduler;
use crate::transport::{CommandTransport, DesyncDetected, PollResult, SeatId, Tick, TickBundle};

/// Deterministic per-message delivery jitter: each message's delivery is
/// postponed by `lcg() % (max_delay_steps + 1)` delivery steps. Messages with
/// unequal delays genuinely overtake each other (out-of-order delivery within
/// the window); the tick barrier converts any lateness into a stall.
#[derive(Clone, Copy, Debug)]
pub struct JitterConfig {
    /// Seed for the jitter LCG (independent of the sim RNG).
    pub seed: u32,
    /// Maximum extra delivery steps per message (0 = no jitter).
    pub max_delay_steps: u32,
}

/// Minimal LCG for jitter draws — transport-local so the *sim* RNG
/// (`ra_sim::RandomLcg`) is never consumed by the network layer (§4.2's
/// sim/cosmetic RNG separation, applied to the transport).
#[derive(Clone, Copy, Debug)]
struct JitterLcg(u32);

impl JitterLcg {
    fn next(&mut self) -> u32 {
        // Classic glibc-style constants; quality is irrelevant, determinism is not.
        self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.0 >> 16
    }
}

/// What crosses the link. The exec-tick stamp travels with the commands —
/// the scheduling authority is the sender (queue.cpp:2526), never the wire.
#[derive(Clone, Debug)]
enum Payload {
    /// One tick's commands from the sending seat, executing at `exec_tick`.
    Bundle { exec_tick: Tick, cmds: Vec<Command> },
    /// The sender's post-tick state hash for `tick`.
    Hash { tick: Tick, hash: u64 },
}

#[derive(Clone, Debug)]
struct Message {
    /// Link step at (or after) which this message becomes deliverable.
    deliver_at: u64,
    /// Send sequence number — the deterministic tiebreak for equal steps.
    seq: u64,
    payload: Payload,
}

/// The shared in-memory "wire": two directed queues plus the delivery clock.
#[derive(Debug)]
struct Link {
    /// Delivery-step clock; advances by one on every `poll` of either endpoint.
    step: u64,
    /// Global send counter (FIFO tiebreak).
    seq: u64,
    /// Jitter source; `None` = every message delivers on the next step.
    jitter: Option<(JitterLcg, u32)>,
    /// `inbox[side]` holds in-flight messages addressed *to* that side.
    inbox: [Vec<Message>; 2],
}

impl Link {
    fn send(&mut self, to: usize, payload: Payload) {
        let delay = match &mut self.jitter {
            Some((lcg, max)) if *max > 0 => u64::from(lcg.next() % (*max + 1)),
            _ => 0,
        };
        self.seq += 1;
        self.inbox[to].push(Message {
            deliver_at: self.step + delay,
            seq: self.seq,
            payload,
        });
    }

    /// Advance the delivery clock one step and drain every message now due
    /// for `side`, in deterministic `(deliver_at, seq)` order.
    fn pump(&mut self, side: usize) -> Vec<Message> {
        self.step += 1;
        let step = self.step;
        let inbox = &mut self.inbox[side];
        let mut due: Vec<Message> = Vec::new();
        let mut rest: Vec<Message> = Vec::new();
        for m in inbox.drain(..) {
            if m.deliver_at <= step {
                due.push(m);
            } else {
                rest.push(m);
            }
        }
        *inbox = rest;
        due.sort_by_key(|m| (m.deliver_at, m.seq));
        due
    }
}

/// One endpoint of an in-process lockstep pair. See the module docs for the
/// protocol; see [`PairTransport::pair`] to build one.
#[derive(Debug)]
pub struct PairTransport {
    /// This endpoint's seat (its player's house id).
    seat: SeatId,
    /// The peer's seat.
    peer: SeatId,
    /// Which side of the link we read (0 = first endpoint, 1 = second).
    side: usize,
    /// Shared input delay in ticks (protocol constant for the session; the
    /// original retunes MaxAhead at runtime via TIMING events,
    /// queue.cpp:1440-1461 — deferred to M8-B).
    delay: u32,
    sched: InputScheduler,
    link: Rc<RefCell<Link>>,
    /// The tick currently being assembled (polls must be sequential).
    current: Tick,
    /// Whether `current` has been stamped/sent already (Waiting re-polls must
    /// not re-stamp: one bundle per exec tick, ever).
    stamped: bool,
    /// Local commands due at `current`, held across Waiting re-polls.
    local_due: Option<Vec<Command>>,
    /// Delivered peer bundles, keyed by execution tick.
    remote: BTreeMap<Tick, Vec<Command>>,
    /// Own reported hashes awaiting the peer's report for the same tick.
    my_hashes: BTreeMap<Tick, u64>,
    /// Peer hashes awaiting our own report for the same tick.
    peer_hashes: BTreeMap<Tick, u64>,
    /// Latched divergence state (sticky).
    desync: Option<DesyncDetected>,
    /// Number of polls that returned [`PollResult::Waiting`] — the barrier's
    /// observable, for tests ("stalls, never diverges").
    stalls: u64,
}

impl PairTransport {
    /// Build a connected endpoint pair. `seat_a`/`seat_b` are the two players'
    /// house ids (must differ — they key canonical bundle ordering); `delay`
    /// is the shared input delay in ticks
    /// ([`DEFAULT_INPUT_DELAY`](crate::DEFAULT_INPUT_DELAY) for the §4.6
    /// LAN default); `jitter` simulates network conditions deterministically.
    pub fn pair(
        seat_a: SeatId,
        seat_b: SeatId,
        delay: u32,
        jitter: Option<JitterConfig>,
    ) -> (PairTransport, PairTransport) {
        assert_ne!(seat_a, seat_b, "lockstep seats must be distinct");
        let link = Rc::new(RefCell::new(Link {
            step: 0,
            seq: 0,
            jitter: jitter.map(|j| (JitterLcg(j.seed), j.max_delay_steps)),
            inbox: [Vec::new(), Vec::new()],
        }));
        let make = |seat: SeatId, peer: SeatId, side: usize| PairTransport {
            seat,
            peer,
            side,
            delay,
            sched: InputScheduler::new(delay),
            link: Rc::clone(&link),
            current: 0,
            stamped: false,
            local_due: None,
            remote: BTreeMap::new(),
            my_hashes: BTreeMap::new(),
            peer_hashes: BTreeMap::new(),
            desync: None,
            stalls: 0,
        };
        (make(seat_a, seat_b, 0), make(seat_b, seat_a, 1))
    }

    /// The latched divergence state, if any.
    pub fn desync(&self) -> Option<DesyncDetected> {
        self.desync
    }

    /// How many polls have stalled at the tick barrier so far.
    pub fn stall_count(&self) -> u64 {
        self.stalls
    }

    /// This endpoint's seat id.
    pub fn seat(&self) -> SeatId {
        self.seat
    }

    /// The tick this endpoint is currently assembling (i.e. the next tick to
    /// pass to [`CommandTransport::poll`]).
    pub fn current_tick(&self) -> Tick {
        self.current
    }

    fn flag_desync(&mut self, d: DesyncDetected) {
        // Keep the earliest mismatching tick (with jitter, reports can arrive
        // out of order).
        match self.desync {
            Some(e) if e.tick <= d.tick => {}
            _ => self.desync = Some(d),
        }
    }

    fn receive(&mut self, payload: Payload) {
        match payload {
            Payload::Bundle { exec_tick, cmds } => {
                // Exactly one bundle is ever sent per exec tick; `entry` keeps
                // this robust rather than load-bearing.
                self.remote.entry(exec_tick).or_default().extend(cmds);
            }
            Payload::Hash { tick, hash } => match self.my_hashes.get(&tick) {
                Some(&mine) if mine != hash => {
                    let peer = self.peer;
                    self.flag_desync(DesyncDetected {
                        tick,
                        local_hash: mine,
                        remote_hash: hash,
                        peer,
                    });
                }
                Some(_) => {
                    // Confirmed in sync for `tick`; prune (bounded memory).
                    self.my_hashes.remove(&tick);
                }
                None => {
                    self.peer_hashes.insert(tick, hash);
                }
            },
        }
    }
}

impl CommandTransport for PairTransport {
    fn submit(&mut self, cmd: Command) {
        if self.desync.is_some() {
            return; // session is dead pending resync (M8-B); drop input
        }
        self.sched.submit(cmd);
    }

    fn poll(&mut self, tick: Tick) -> PollResult {
        if let Some(d) = self.desync {
            return PollResult::Desync(d);
        }
        assert_eq!(
            tick, self.current,
            "lockstep ticks must be polled sequentially (expected {}, got {})",
            self.current, tick
        );

        // First poll of this tick: stamp staged input for `tick + delay`,
        // ship it, and set aside our own commands due *this* tick.
        if !self.stamped {
            let (exec_tick, cmds) = self.sched.stamp(tick);
            self.link
                .borrow_mut()
                .send(1 - self.side, Payload::Bundle { exec_tick, cmds });
            self.local_due = Some(self.sched.take_due(tick));
            self.stamped = true;
        }

        // Advance the delivery clock one step and file whatever arrived.
        let due = self.link.borrow_mut().pump(self.side);
        for m in due {
            self.receive(m.payload);
        }
        if let Some(d) = self.desync {
            return PollResult::Desync(d);
        }

        // Tick barrier: we need the peer's bundle for `tick`. Ticks below the
        // input delay are empty by protocol definition (no tick exists whose
        // stamp could land there).
        let remote_cmds = if tick < self.delay {
            Some(Vec::new())
        } else {
            self.remote.remove(&tick)
        };
        let Some(remote_cmds) = remote_cmds else {
            self.stalls += 1;
            return PollResult::Waiting;
        };

        let local_cmds = self.local_due.take().unwrap_or_default();
        let mut seats = vec![(self.seat, local_cmds), (self.peer, remote_cmds)];
        seats.sort_by_key(|&(s, _)| s); // canonical house order, queue.cpp:3286-3290
        self.current += 1;
        self.stamped = false;
        PollResult::Ready(TickBundle { tick, seats })
    }

    fn report_hash(&mut self, tick: Tick, hash: u64) {
        if self.desync.is_some() {
            return;
        }
        self.link
            .borrow_mut()
            .send(1 - self.side, Payload::Hash { tick, hash });
        match self.peer_hashes.remove(&tick) {
            Some(theirs) if theirs != hash => {
                let peer = self.peer;
                self.flag_desync(DesyncDetected {
                    tick,
                    local_hash: hash,
                    remote_hash: theirs,
                    peer,
                });
            }
            Some(_) => {} // confirmed in sync; both sides pruned
            None => {
                self.my_hashes.insert(tick, hash);
            }
        }
    }
}
