//! M8-A proof tests (a, b, d, e): two independent `World`s in lockstep over
//! [`PairTransport`] — hash-chain identity under clean and adversarial
//! delivery, divergence detection, and revert-sensitivity of the input-delay
//! scheduler. (Proof test c — the real-scenario AI-vs-AI lockstep match —
//! needs the asset loaders and lives in
//! `ra-client/tests/net_lockstep_realmap.rs`.)
//!
//! Fixture and non-vacuity style follow `ra-sim/tests/determinism.rs` (M7.19
//! lesson: prove the sim advanced and the commands actually applied before
//! asserting identity).

use ra_net::{
    CommandTransport, DesyncDetected, JitterConfig, PairTransport, PollResult, TickBundle,
    DEFAULT_INPUT_DELAY,
};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, World};

const SEED: u32 = 0xC0FF_EE01;
/// Enough for every scripted move to arrive (speeds are leptons/tick,
/// 256 leptons per cell — a speed-60 unit covers ~0.23 cells/tick).
const TICKS: u32 = 450;

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

/// Two houses, two units each, on an open synthetic map. Both lockstep
/// instances build this identical world from the same seed.
struct Fixture {
    a1: Handle, // house 1
    a2: Handle, // house 1
    b1: Handle, // house 2
    b2: Handle, // house 2
}

fn build_world() -> (World, Fixture) {
    let mut world = World::new(Passability::all_passable(), SEED);
    let a1 = world.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats(60, 10));
    let a2 = world.spawn_unit(1, 1, CellCoord::new(40, 3), Facing(64), 300, stats(30, 3));
    let b1 = world.spawn_unit(2, 2, CellCoord::new(3, 40), Facing(128), 150, stats(60, 20));
    let b2 = world.spawn_unit(3, 2, CellCoord::new(40, 40), Facing(192), 200, stats(60, 8));
    (world, Fixture { a1, a2, b1, b2 })
}

/// The scripted per-seat inputs: seat A (house 1) orders only its own units,
/// seat B (house 2) only its own — different commands per instance, so the
/// run is only green if commands genuinely cross the transport.
fn script_a(f: &Fixture, t: u32) -> Vec<Command> {
    let mut v = Vec::new();
    match t {
        0 => v.push(Command::Move {
            unit: f.a1,
            dest: CellCoord::new(60, 50),
            house: 1,
        }),
        5 => v.push(Command::Move {
            unit: f.a2,
            dest: CellCoord::new(30, 12),
            house: 1,
        }),
        40 => {
            v.push(Command::Stop {
                unit: f.a1,
                house: 1,
            });
            v.push(Command::Move {
                unit: f.a1,
                dest: CellCoord::new(10, 55),
                house: 1,
            });
        }
        _ => {}
    }
    v
}

fn script_b(f: &Fixture, t: u32) -> Vec<Command> {
    let mut v = Vec::new();
    match t {
        2 => v.push(Command::Move {
            unit: f.b1,
            dest: CellCoord::new(45, 8),
            house: 2,
        }),
        7 => v.push(Command::Move {
            unit: f.b2,
            dest: CellCoord::new(5, 5),
            house: 2,
        }),
        60 => v.push(Command::Move {
            unit: f.b1,
            dest: CellCoord::new(55, 55),
            house: 2,
        }),
        _ => {}
    }
    v
}

/// One lockstep game instance: an independent `World` plus its endpoint.
struct Instance {
    world: World,
    tp: PairTransport,
}

enum Outcome {
    /// Both chains, tick-parallel, plus total Waiting stalls across both ends.
    Completed {
        chain_a: Vec<u64>,
        chain_b: Vec<u64>,
        stalls: u64,
    },
    /// A divergence latched: the poll tick it surfaced on and the first
    /// endpoint's report. (The other endpoint can be nudged afterwards.)
    Desync {
        detected_at: u32,
        first: DesyncDetected,
    },
}

/// Poll one endpoint until Ready (caching across a stalled peer is the
/// caller's job; this helper only maps the enum).
fn try_poll(i: &mut Instance, t: u32) -> Result<Option<TickBundle>, DesyncDetected> {
    match i.tp.poll(t) {
        PollResult::Ready(b) => Ok(Some(b)),
        PollResult::Waiting => Ok(None),
        PollResult::Desync(d) => Err(d),
    }
}

/// Drive both instances tick by tick through the full protocol:
/// submit → poll-until-Ready (stall loop) → apply bundle → report hash.
/// `mutate` is the test's hook to corrupt a world mid-run (test d).
fn drive(
    a: &mut Instance,
    b: &mut Instance,
    ticks: u32,
    mut submit: impl FnMut(u32, &mut Instance, &mut Instance),
    mut mutate: impl FnMut(u32, &mut Instance, &mut Instance),
) -> Outcome {
    let mut chain_a = Vec::new();
    let mut chain_b = Vec::new();
    for t in 0..ticks {
        submit(t, a, b);
        mutate(t, a, b);
        let mut bundle_a: Option<TickBundle> = None;
        let mut bundle_b: Option<TickBundle> = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            if bundle_a.is_none() {
                match try_poll(a, t) {
                    Ok(x) => bundle_a = x,
                    Err(d) => {
                        return Outcome::Desync {
                            detected_at: t,
                            first: d,
                        }
                    }
                }
            }
            if bundle_b.is_none() {
                match try_poll(b, t) {
                    Ok(x) => bundle_b = x,
                    Err(d) => {
                        return Outcome::Desync {
                            detected_at: t,
                            first: d,
                        }
                    }
                }
            }
            spins += 1;
            assert!(spins < 100_000, "lockstep deadlock at tick {t}");
        }
        // Both endpoints must assemble the identical canonical bundle.
        assert_eq!(bundle_a, bundle_b, "bundle mismatch at tick {t}");
        let ha = a.world.tick(&bundle_a.unwrap().flatten());
        let hb = b.world.tick(&bundle_b.unwrap().flatten());
        a.tp.report_hash(t, ha);
        b.tp.report_hash(t, hb);
        chain_a.push(ha);
        chain_b.push(hb);
    }
    let stalls = a.tp.stall_count() + b.tp.stall_count();
    Outcome::Completed {
        chain_a,
        chain_b,
        stalls,
    }
}

fn make_pair(delay: u32, jitter: Option<JitterConfig>) -> (Instance, Instance, Fixture) {
    // Seats are the two player houses (1 and 2), per the canonical-house-order
    // contract.
    let (ta, tb) = PairTransport::pair(1, 2, delay, jitter);
    let (wa, f) = build_world();
    let (wb, _) = build_world();
    assert_eq!(
        wa.state_hash(),
        wb.state_hash(),
        "instances must start from identical worlds"
    );
    (
        Instance { world: wa, tp: ta },
        Instance { world: wb, tp: tb },
        f,
    )
}

/// Run the standard scripted game and return its outcome plus both instances.
fn scripted_run(
    delay: u32,
    jitter: Option<JitterConfig>,
    ticks: u32,
) -> (Outcome, Instance, Instance, Fixture) {
    let (mut a, mut b, f) = make_pair(delay, jitter);
    let out = drive(
        &mut a,
        &mut b,
        ticks,
        |t, ia, ib| {
            for c in script_a(&f, t) {
                ia.tp.submit(c);
            }
            for c in script_b(&f, t) {
                ib.tp.submit(c);
            }
        },
        |_, _, _| {},
    );
    (out, a, b, f)
}

/// Non-vacuity checks shared by tests a and b (M7.19 lesson): the sim really
/// advanced, both scripts' commands really applied, and they applied *through
/// the transport* (each world moved the OTHER instance's units too).
fn assert_non_vacuous(chain: &[u64], a: &Instance, b: &Instance, f: &Fixture) {
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(
        distinct.len() > 50,
        "hash chain suspiciously static ({} distinct of {})",
        distinct.len(),
        chain.len()
    );
    // Seat A's a1 was rerouted to (10,55) at t=40; seat B's b2 was sent to
    // (5,5) at t=7. Both worlds must agree — including about the *peer's*
    // units, which is what proves the commands crossed the transport.
    for (name, w) in [("A", &a.world), ("B", &b.world)] {
        let a1 = w.units.get(f.a1).expect("a1 alive");
        assert_eq!(
            a1.cell(),
            CellCoord::new(10, 55),
            "instance {name}: house-1 unit did not reach its scripted dest"
        );
        let b2 = w.units.get(f.b2).expect("b2 alive");
        assert_eq!(
            b2.cell(),
            CellCoord::new(5, 5),
            "instance {name}: house-2 unit did not reach its scripted dest"
        );
    }
}

/// Proof test (a): two-instance lockstep, clean delivery. Same scenario/seed,
/// different commands per instance, hundreds of ticks: per-tick hash chains
/// must be byte-identical the whole way.
#[test]
fn two_instance_lockstep_hash_chains_identical() {
    let (out, a, b, f) = scripted_run(DEFAULT_INPUT_DELAY, None, TICKS);
    let Outcome::Completed {
        chain_a, chain_b, ..
    } = out
    else {
        panic!("unexpected desync in clean run");
    };
    assert_eq!(chain_a.len(), TICKS as usize);
    assert_eq!(chain_a, chain_b, "hash chains diverged");
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
    assert_non_vacuous(&chain_a, &a, &b, &f);
}

/// Input-delay scheduling is visible and exact: a command submitted during
/// tick T first perturbs the sim at T + delay (the queue.cpp:2526 stamp),
/// proven by diffing the hash chain against a command-less control run.
#[test]
fn input_delay_defers_execution_by_exactly_delay_ticks() {
    let delay = DEFAULT_INPUT_DELAY;
    const SUBMIT_AT: u32 = 10;
    const N: u32 = 30;

    let run = |with_command: bool| -> Vec<u64> {
        let (mut a, mut b, f) = make_pair(delay, None);
        let out = drive(
            &mut a,
            &mut b,
            N,
            |t, ia, _| {
                if with_command && t == SUBMIT_AT {
                    ia.tp.submit(Command::Move {
                        unit: f.a1,
                        dest: CellCoord::new(30, 3),
                        house: 1,
                    });
                }
            },
            |_, _, _| {},
        );
        match out {
            Outcome::Completed { chain_a, .. } => chain_a,
            Outcome::Desync { .. } => panic!("clean run desynced"),
        }
    };

    let with_cmd = run(true);
    let control = run(false);
    let first_diff = with_cmd
        .iter()
        .zip(&control)
        .position(|(x, y)| x != y)
        .expect("command had no effect at all (vacuous)");
    assert_eq!(
        first_diff as u32,
        SUBMIT_AT + delay,
        "command perturbed the sim at the wrong tick"
    );
}

/// Proof test (b): adversarial deterministic jitter — seeded per-message
/// delays with genuine out-of-order delivery. Hashes stay identical, the tick
/// barrier holds (stalls observed, no divergence), and the chain equals the
/// clean run's chain exactly (arrival timing must not leak into the sim).
#[test]
fn jittered_lockstep_identical_and_barrier_stalls() {
    let (clean, _, _, _) = scripted_run(DEFAULT_INPUT_DELAY, None, TICKS);
    let Outcome::Completed {
        chain_a: clean_chain,
        stalls: clean_stalls,
        ..
    } = clean
    else {
        panic!("clean run desynced");
    };

    let jitter = JitterConfig {
        seed: 0xDEAD_BEEF,
        max_delay_steps: 7,
    };
    let (out, a, b, f) = scripted_run(DEFAULT_INPUT_DELAY, Some(jitter), TICKS);
    let Outcome::Completed {
        chain_a,
        chain_b,
        stalls,
    } = out
    else {
        panic!("jittered run desynced");
    };
    assert_eq!(chain_a, chain_b, "jittered chains diverged");
    assert_eq!(
        chain_a, clean_chain,
        "arrival timing leaked into the sim state"
    );
    assert!(
        stalls > clean_stalls,
        "jitter produced no extra stalls (jittered {stalls} vs clean {clean_stalls}) — barrier not exercised"
    );
    assert_non_vacuous(&chain_a, &a, &b, &f);
}

/// Proof test (d): divergence drill. Corrupt instance B's world mid-run
/// (spawning a unit outside the command pipeline); the next hash exchange must
/// latch DesyncDetected on both endpoints, attributed to the corruption tick,
/// with the two hashes recorded unequal — a state, not a panic.
#[test]
fn corrupted_world_latches_desync_at_the_right_tick() {
    const CORRUPT_AT: u32 = 150;
    let (mut a, mut b, f) = make_pair(DEFAULT_INPUT_DELAY, None);
    let out = drive(
        &mut a,
        &mut b,
        300,
        |t, ia, ib| {
            for c in script_a(&f, t) {
                ia.tp.submit(c);
            }
            for c in script_b(&f, t) {
                ib.tp.submit(c);
            }
        },
        |t, _, ib| {
            if t == CORRUPT_AT {
                // Out-of-band world mutation: the exact class of bug the hash
                // exchange exists to catch.
                ib.world
                    .spawn_unit(0, 2, CellCoord::new(20, 20), Facing(0), 100, stats(10, 5));
            }
        },
    );
    let Outcome::Desync { detected_at, first } = out else {
        panic!("corruption went undetected");
    };
    // Hashes for CORRUPT_AT are exchanged during tick CORRUPT_AT+1's polls.
    assert_eq!(
        detected_at,
        CORRUPT_AT + 1,
        "desync surfaced at an unexpected poll tick"
    );
    assert_eq!(first.tick, CORRUPT_AT, "wrong tick attributed");
    assert_ne!(first.local_hash, first.remote_hash);

    // Both endpoints latch (nudge whichever had not yet pumped the peer's
    // hash when the drive stopped), and they agree on the tick.
    if a.tp.desync().is_none() {
        let t = a.tp.current_tick();
        let _ = a.tp.poll(t);
    }
    if b.tp.desync().is_none() {
        let t = b.tp.current_tick();
        let _ = b.tp.poll(t);
    }
    for (name, tp) in [("A", &a.tp), ("B", &b.tp)] {
        let d = tp
            .desync()
            .unwrap_or_else(|| panic!("endpoint {name} did not latch the desync"));
        assert_eq!(d.tick, CORRUPT_AT, "endpoint {name}: wrong tick attributed");
        assert_ne!(
            d.local_hash, d.remote_hash,
            "endpoint {name}: recorded hashes should differ"
        );
    }
}

/// Jitter-seed sweep: proof test (b) pinned one seed; a scheduler bug that
/// only misbehaves for specific delivery orderings could hide behind a
/// single lucky seed. Sweep several independent seeds (still deterministic —
/// no wall clock, no thread nondeterminism) and require every one to stay
/// hash-identical to the clean run AND to produce at least one genuine stall
/// (proving the barrier was actually exercised, not just present).
#[test]
fn jittered_lockstep_identical_across_many_seeds() {
    let (clean, _, _, _) = scripted_run(DEFAULT_INPUT_DELAY, None, TICKS);
    let Outcome::Completed {
        chain_a: clean_chain,
        ..
    } = clean
    else {
        panic!("clean run desynced");
    };

    for seed in [
        0x0000_0001u32,
        0xDEAD_BEEF,
        0x1234_5678,
        0xFFFF_FFFF,
        0x5EA7_C0DE,
        0x0BAD_F00D,
    ] {
        for max_delay_steps in [1u32, 4, 9] {
            let jitter = JitterConfig {
                seed,
                max_delay_steps,
            };
            let (out, a, b, f) = scripted_run(DEFAULT_INPUT_DELAY, Some(jitter), TICKS);
            let Outcome::Completed {
                chain_a, chain_b, ..
            } = out
            else {
                panic!("jittered run desynced (seed={seed:#x}, max_delay={max_delay_steps})");
            };
            assert_eq!(
                chain_a, chain_b,
                "endpoints diverged (seed={seed:#x}, max_delay={max_delay_steps})"
            );
            assert_eq!(
                chain_a, clean_chain,
                "arrival timing leaked into the sim state (seed={seed:#x}, max_delay={max_delay_steps})"
            );
            assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
            assert_non_vacuous(&chain_a, &a, &b, &f);
        }
    }
}

/// Negative drill: corrupt BOTH worlds *identically* mid-run — the same
/// out-of-band mutation applied to both instances at the same tick. This must
/// NOT trip the desync detector: hash equality is the only oracle the hash
/// exchange has, and if it fired here it would mean the detector is
/// trigger-happy (comparing something other than genuine state divergence,
/// e.g. an unstable field like a wall-clock timestamp or an address). A
/// detector that can't tell "identical corruption" from "divergence" would
/// false-positive constantly on legitimate, income-neutral non-determinism
/// sources that happen to be symmetric.
#[test]
fn identically_corrupting_both_worlds_does_not_desync() {
    const CORRUPT_AT: u32 = 150;
    let (mut a, mut b, f) = make_pair(DEFAULT_INPUT_DELAY, None);
    let out = drive(
        &mut a,
        &mut b,
        300,
        |t, ia, ib| {
            for c in script_a(&f, t) {
                ia.tp.submit(c);
            }
            for c in script_b(&f, t) {
                ib.tp.submit(c);
            }
        },
        |t, ia, ib| {
            if t == CORRUPT_AT {
                // Identical out-of-band mutation on both sides.
                ia.world
                    .spawn_unit(0, 2, CellCoord::new(20, 20), Facing(0), 100, stats(10, 5));
                ib.world
                    .spawn_unit(0, 2, CellCoord::new(20, 20), Facing(0), 100, stats(10, 5));
            }
        },
    );
    let Outcome::Completed {
        chain_a, chain_b, ..
    } = out
    else {
        panic!(
            "identical corruption on both sides was flagged as a desync — the detector is \
             comparing something other than genuine divergence"
        );
    };
    assert_eq!(
        chain_a, chain_b,
        "identically-corrupted worlds must still produce identical hash chains"
    );
    assert!(a.tp.desync().is_none() && b.tp.desync().is_none());
    // Non-vacuity: the corruption must have actually landed (both worlds
    // gained the extra unit), and the run must have covered the corrupt
    // tick and kept going well past it.
    assert_eq!(chain_a.len(), 300);
    for (name, w) in [("A", &a.world), ("B", &b.world)] {
        let extra = w
            .units
            .iter()
            .filter(|(_, u)| u.cell() == CellCoord::new(20, 20))
            .count();
        assert_eq!(
            extra, 1,
            "instance {name}: corruption did not land as expected"
        );
    }
}

/// Proof test (e): revert-sensitivity — the input-delay scheduler is
/// load-bearing. Simulate the classic naive-lockstep bug: instance A applies
/// its own input immediately at the submit tick (skipping the T+delay stamp)
/// while B applies it on schedule. The chains must diverge at exactly the
/// submit tick, and the hash exchange must latch DesyncDetected there.
#[test]
fn skipping_input_delay_diverges_and_is_detected() {
    const SUBMIT_AT: u32 = 20;
    let delay = DEFAULT_INPUT_DELAY;
    let (mut a, mut b, f) = make_pair(delay, None);
    let cmd = Command::Move {
        unit: f.a1,
        dest: CellCoord::new(50, 40),
        house: 1,
    };

    let mut first_divergence: Option<u32> = None;
    let mut desync: Option<(u32, DesyncDetected)> = None;
    'game: for t in 0..60u32 {
        if t == SUBMIT_AT {
            a.tp.submit(cmd);
        }
        let mut bundle_a: Option<TickBundle> = None;
        let mut bundle_b: Option<TickBundle> = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            if bundle_a.is_none() {
                match try_poll(&mut a, t) {
                    Ok(x) => bundle_a = x,
                    Err(d) => {
                        desync = Some((t, d));
                        break 'game;
                    }
                }
            }
            if bundle_b.is_none() {
                match try_poll(&mut b, t) {
                    Ok(x) => bundle_b = x,
                    Err(d) => {
                        desync = Some((t, d));
                        break 'game;
                    }
                }
            }
            spins += 1;
            assert!(spins < 100_000, "deadlock at tick {t}");
        }
        // THE BUG UNDER TEST: A executes its own commands the moment they were
        // submitted (tick T), stripping its seat from the scheduled bundle —
        // "input delay off" for the local player only.
        let mut cmds_a: Vec<Command> = Vec::new();
        if t == SUBMIT_AT {
            cmds_a.push(cmd);
        }
        for (seat, cmds) in &bundle_a.unwrap().seats {
            if *seat != a.tp.seat() {
                cmds_a.extend_from_slice(cmds);
            }
        }
        let ha = a.world.tick(&cmds_a);
        let hb = b.world.tick(&bundle_b.unwrap().flatten());
        if ha != hb && first_divergence.is_none() {
            first_divergence = Some(t);
        }
        a.tp.report_hash(t, ha);
        b.tp.report_hash(t, hb);
    }

    assert_eq!(
        first_divergence,
        Some(SUBMIT_AT),
        "executing at the submit tick instead of T+delay must diverge at T"
    );
    let (detected_at, d) = desync.expect("divergence went undetected");
    assert_eq!(d.tick, SUBMIT_AT, "desync attributed to the wrong tick");
    assert_eq!(detected_at, SUBMIT_AT + 1);

    // Control: the identical script through the *correct* scheduler stays
    // hash-identical (pins the contrast within this test's own scenario).
    let (mut a2, mut b2, f2) = make_pair(delay, None);
    let cmd2 = Command::Move {
        unit: f2.a1,
        dest: CellCoord::new(50, 40),
        house: 1,
    };
    let out = drive(
        &mut a2,
        &mut b2,
        60,
        |t, ia, _| {
            if t == SUBMIT_AT {
                ia.tp.submit(cmd2);
            }
        },
        |_, _, _| {},
    );
    let Outcome::Completed {
        chain_a, chain_b, ..
    } = out
    else {
        panic!("control run desynced");
    };
    assert_eq!(chain_a, chain_b);
}
