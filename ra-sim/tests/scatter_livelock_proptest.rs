//! M7.12 audit — no-livelock property test for the "ask-the-blocker-to-scatter"
//! mechanic (`ra-sim/tests/scatter_boundary_suite.rs` covers hand-crafted
//! boundary cases; this fuzzes the space instead).
//!
//! Random small maps, a random mix of parked friendly/enemy vehicles, and one
//! mover with a random destination. Over a bounded tick budget:
//! 1. The mover either reaches its destination, or the whole scene settles
//!    into a stable configuration (no unit's cell changes over the tail
//!    window) — never *thrashes* forever.
//! 2. Vehicles never overlap a cell (release-mode check, not `debug_assert`).
//! 3. The scatter RNG is never drawn unboundedly — bounded relative to tick
//!    count and blocker count (see `scatter_boundary_suite.rs`'s
//!    `fully_boxed_blocker_...` for why the bound has headroom: each logical
//!    `range(0,2)` draw can cost more than one raw LCG step on a rejection).
//! 4. The same seed run twice produces an identical position trace and an
//!    identical final RNG state (determinism).

use std::collections::BTreeMap;

use proptest::prelude::*;

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, MoveStats, Passability, World};

const GRID_W: i32 = 16;
const GRID_H: i32 = 16;
/// Generous enough to finish a full corner-to-corner diagonal crossing of the
/// grid: worst case ~15 cells at `stats(24,8)` (24 leptons/tick, 256/cell) is
/// ~160 ticks of pure travel, plus rotation-to-heading and any re-route
/// overhead — 500 leaves ample headroom so "didn't finish" (a test-budget
/// artifact) is never confused with a genuine livelock. An earlier version of
/// this test used 200 ticks and produced a false positive on a full-diagonal
/// scenario that was simply still, correctly, en route (see git history for
/// the trace).
const TICKS: u32 = 500;
/// How many trailing ticks must be perfectly still to count as "settled".
const TAIL: u32 = 40;

fn stats(speed: i32, rot: u8) -> MoveStats {
    MoveStats {
        max_speed: speed,
        rot,
    }
}

fn vehicle_overlap_count(world: &World) -> usize {
    let mut counts: BTreeMap<(i32, i32), i32> = BTreeMap::new();
    for (_, u) in world.units.iter() {
        if !u.is_infantry() {
            let c = u.cell();
            *counts.entry((c.x, c.y)).or_insert(0) += 1;
        }
    }
    counts
        .values()
        .filter(|&&n| n > 1)
        .map(|&n| (n - 1) as usize)
        .sum()
}

/// Independent re-implementation of `RandomLcg`'s step formula, used only to
/// bound-check RNG consumption between two observed seeds (see
/// `scatter_boundary_suite.rs`'s `lcg_steps_between` for the full rationale;
/// duplicated here per this codebase's small-per-file-fixture convention).
fn lcg_steps_between(mut seed: u32, target: u32, max: u32) -> Option<u32> {
    const MULT: u32 = 0x41C6_4E6D;
    const ADD: u32 = 0x0000_3039;
    for n in 0..=max {
        if seed == target {
            return Some(n);
        }
        seed = seed.wrapping_mul(MULT).wrapping_add(ADD);
    }
    None
}

/// `n` distinct cells on the open `GRID_W`x`GRID_H` grid.
///
/// **Must sort after collecting from the `HashSet`.** `std::collections::
/// HashSet`'s iteration order depends on its `RandomState` hasher, which is
/// reseeded per *process*, not per test — so two separate `cargo test`
/// invocations of the exact same shrunk regression seed can assign the same
/// underlying index set to different roles (mover start vs. dest vs. which
/// blocker), silently reproducing a *different* scenario than the one
/// printed. Sorting first makes the index-set -> `Vec<CellCoord>` mapping a
/// pure function of the values alone, restoring proptest's normal "the
/// printed minimal input reliably reproduces" guarantee.
fn distinct_cells(n: usize) -> impl Strategy<Value = Vec<CellCoord>> {
    proptest::collection::hash_set(0u32..(GRID_W * GRID_H) as u32, n).prop_map(|set| {
        let mut idxs: Vec<u32> = set.into_iter().collect();
        idxs.sort_unstable();
        idxs.into_iter()
            .map(|idx| CellCoord::new((idx as i32) % GRID_W, (idx as i32) / GRID_W))
            .collect()
    })
}

/// A scenario: mover start/dest, 0..=4 parked friendlies, 0..=3 parked
/// enemies — all on distinct cells (mover start, mover dest, and every
/// blocker each get their own cell so the fixture itself starts overlap-free
/// and route-free-of-a-baked-in-immediate-block, though the mover's route may
/// well run straight through one or more blockers, which is the point).
#[derive(Debug, Clone)]
struct Scenario {
    mover_start: CellCoord,
    mover_dest: CellCoord,
    friendlies: Vec<CellCoord>,
    enemies: Vec<CellCoord>,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    (0usize..=4, 0usize..=3).prop_flat_map(|(nf, ne)| {
        let total = nf + ne + 2; // + mover start + mover dest
        distinct_cells(total).prop_map(move |cells| {
            let mover_start = cells[0];
            let mover_dest = cells[1];
            let friendlies = cells[2..2 + nf].to_vec();
            let enemies = cells[2 + nf..2 + nf + ne].to_vec();
            Scenario {
                mover_start,
                mover_dest,
                friendlies,
                enemies,
            }
        })
    })
}

/// Per-tick trace of every unit's cell (mover first), the final RNG seed,
/// whether the mover reached its destination, and the max vehicle-overlap
/// count observed at any point during the run.
struct RunResult {
    trace: Vec<Vec<(i32, i32)>>,
    final_seed: u32,
    reached: bool,
    max_overlap: usize,
}

/// Build the world and run it for `TICKS` ticks.
fn run(seed: u32, s: &Scenario) -> RunResult {
    let mut world = World::new(Passability::all_passable(), seed);
    let mover = world.spawn_unit(0, 1, s.mover_start, Facing(64), 400, stats(24, 8));
    let mut handles = vec![mover];
    for &c in &s.friendlies {
        handles.push(world.spawn_unit(0, 1, c, Facing(0), 400, stats(24, 8)));
    }
    for &c in &s.enemies {
        handles.push(world.spawn_unit(0, 2, c, Facing(0), 400, stats(24, 8)));
    }

    world.tick(&[Command::Move {
        unit: mover,
        dest: s.mover_dest,
        house: 1,
    }]);

    let mut trace = Vec::with_capacity(TICKS as usize);
    let mut max_overlap = 0usize;
    for _ in 0..TICKS {
        world.tick(&[]);
        max_overlap = max_overlap.max(vehicle_overlap_count(&world));
        trace.push(
            handles
                .iter()
                .map(|&h| {
                    let c = world.units.get(h).unwrap().cell();
                    (c.x, c.y)
                })
                .collect::<Vec<_>>(),
        );
    }
    let reached = world.units.get(mover).unwrap().cell() == s.mover_dest;
    RunResult {
        trace,
        final_seed: world.rng_seed(),
        reached,
        max_overlap,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Core no-livelock property: reach the destination, or settle into a
    /// perfectly still tail. Never overlap. Never draw RNG unboundedly.
    #[test]
    fn mover_reaches_dest_or_settles_never_overlaps_never_draws_unbounded_rng(
        seed in any::<u32>(),
        s in scenario(),
    ) {
        let result = run(seed, &s);
        prop_assert_eq!(result.max_overlap, 0, "a vehicle cell was shared at some point during the run");

        if !result.reached {
            // Tail window must be perfectly still — every unit's cell
            // identical across the last TAIL ticks. An oscillating /
            // thrashing scene (never settling, never arriving) would fail
            // this.
            let tail_start = (TICKS - TAIL) as usize;
            let anchor = &result.trace[tail_start];
            for (t, cells) in result.trace.iter().enumerate().skip(tail_start) {
                prop_assert_eq!(
                    cells, anchor,
                    "scene did not settle: tick {} differs from the tail anchor (tick {})",
                    t, tail_start
                );
            }
        }

        // RNG bound: total blockers in the scene upper-bounds how many
        // distinct units could ever be legitimately asked to scatter in a
        // single tick; over TICKS ticks, with the generous rejection-sampling
        // headroom established in scatter_boundary_suite.rs, this must never
        // blow past a loose multiple.
        let blocker_count = (s.friendlies.len() + s.enemies.len()).max(1) as u32;
        let bound = TICKS * blocker_count * 8 + 64;
        let steps = lcg_steps_between(seed, result.final_seed, bound);
        prop_assert!(
            steps.is_some(),
            "RNG draw count exceeded the generous bound ({bound}) — looks unbounded/runaway"
        );
    }

    /// Determinism: the same seed and the same scenario run twice must
    /// produce an identical position trace and an identical final RNG state.
    #[test]
    fn same_seed_twice_gives_identical_traces(
        seed in any::<u32>(),
        s in scenario(),
    ) {
        let a = run(seed, &s);
        let b = run(seed, &s);
        prop_assert_eq!(a.trace, b.trace, "same seed+script twice diverged in position trace");
        prop_assert_eq!(
            a.final_seed,
            b.final_seed,
            "same seed+script twice diverged in final RNG state"
        );
    }
}
