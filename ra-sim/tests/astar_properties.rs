//! Property tests for grid A* (`ra_sim::path`, DESIGN.md §3.7/§4.2): random
//! passability grids, checking the documented invariants hold for *every*
//! generated case, not just the handful of hand-built grids in `path.rs`'s
//! own unit tests — plus one determinism check over a real scenario map.
//!
//! Each property is checked against `find_path`'s own documented contract
//! (module docs on `ra_sim::path`): adjacent 8-directional steps, no
//! diagonal corner-cutting, every stepped-through cell passable, the path
//! ends exactly at the goal, unreachable goals return `None` without a
//! panic, and the same inputs always produce the same output.

use proptest::prelude::*;

use ra_sim::coords::CellCoord;
use ra_sim::path::{find_path, Passability};

/// The 8 king-move neighbour offsets, independently listed here (not
/// imported from `ra_sim::path`) so the adjacency oracle below doesn't share
/// a typo with the code under test.
const KING_MOVES: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// A random small passability grid plus a start/goal pair, both guaranteed
/// on-grid (may or may not be passable — `find_path`'s off-grid/impassable
/// handling is exercised by construction whenever the strategy happens to
/// pick an impassable one). Kept small (4..16 per axis) so proptest can
/// afford hundreds of cases; `find_path`'s determinism/correctness has no
/// dependency on map size.
fn grid_and_endpoints() -> impl Strategy<Value = (Passability, CellCoord, CellCoord)> {
    (4i32..16, 4i32..16).prop_flat_map(|(w, h)| {
        let n = (w * h) as usize;
        // 75% passable: dense enough that most start/goal pairs are
        // reachable (so the "valid path" properties get exercised often),
        // sparse enough that unreachable cases still show up regularly.
        let cells = proptest::collection::vec(proptest::bool::weighted(0.75), n);
        let coord = (0i32..w, 0i32..h).prop_map(|(x, y)| CellCoord::new(x, y));
        (cells, Just((w, h)), coord.clone(), coord).prop_map(move |(cells, (w, h), start, goal)| {
            (Passability::new(w, h, cells), start, goal)
        })
    })
}

/// Independent reachability oracle: BFS (unweighted, so no claim about
/// *shortest*, only *reachable*) using the same 8-direction-plus-no-corner-
/// cutting adjacency rule `find_path` documents, but written from scratch
/// against `Passability::is_passable` rather than calling into `path.rs`, so
/// agreement with `find_path`'s `Some`/`None` is a real cross-check of
/// completeness (does A* find a path whenever one exists?), not a tautology.
fn bfs_reachable(grid: &Passability, start: CellCoord, goal: CellCoord) -> bool {
    // Mirror `find_path`'s own check order exactly (post-fix): endpoint
    // validity is checked *before* the `start == goal` short-circuit, so an
    // off-grid or impassable cell asked to path to itself is `None`, not an
    // empty path (see `off_grid_equal_start_and_goal_returns_none`).
    if !grid.is_passable(start) || !grid.is_passable(goal) {
        return false;
    }
    if start == goal {
        return true;
    }
    use std::collections::VecDeque;
    let mut seen = std::collections::HashSet::new();
    let mut q = VecDeque::new();
    seen.insert((start.x, start.y));
    q.push_back(start);
    while let Some(cur) = q.pop_front() {
        for (dx, dy) in KING_MOVES {
            let next = CellCoord::new(cur.x + dx, cur.y + dy);
            if !grid.is_passable(next) {
                continue;
            }
            if dx != 0 && dy != 0 {
                let side_a = CellCoord::new(cur.x + dx, cur.y);
                let side_b = CellCoord::new(cur.x, cur.y + dy);
                if !grid.is_passable(side_a) || !grid.is_passable(side_b) {
                    continue;
                }
            }
            if next == goal {
                return true;
            }
            if seen.insert((next.x, next.y)) {
                q.push_back(next);
            }
        }
    }
    false
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every path `find_path` returns is internally valid: each step
    /// (including start -> first waypoint) is a single king-move, no
    /// diagonal step cuts a corner, every stepped-through cell is passable,
    /// and the path ends exactly at `goal`.
    #[test]
    fn returned_path_is_valid((grid, start, goal) in grid_and_endpoints()) {
        if let Some(path) = find_path(&grid, start, goal) {
            if start == goal {
                // `start == goal` short-circuits to `Some(empty)` before any
                // passability check (see `find_path`'s doc comment: "An
                // empty vec means start == goal") — a unit already standing
                // somewhere doesn't need that cell to be re-validated as
                // passable to "arrive" there.
                prop_assert!(path.is_empty());
            } else {
                prop_assert!(grid.is_passable(start));
                prop_assert!(grid.is_passable(goal));
                prop_assert_eq!(path.last().copied(), Some(goal));
                let mut prev = start;
                for &step in &path {
                    let dx = step.x - prev.x;
                    let dy = step.y - prev.y;
                    prop_assert!(
                        KING_MOVES.contains(&(dx, dy)),
                        "non-adjacent step {prev:?} -> {step:?}"
                    );
                    prop_assert!(grid.is_passable(step), "path steps onto impassable {step:?}");
                    if dx != 0 && dy != 0 {
                        let side_a = CellCoord::new(prev.x + dx, prev.y);
                        let side_b = CellCoord::new(prev.x, prev.y + dy);
                        prop_assert!(
                            grid.is_passable(side_a) && grid.is_passable(side_b),
                            "path cuts a corner at {prev:?} -> {step:?}"
                        );
                    }
                    prev = step;
                }
            }
        }
    }

    /// `find_path` is `Some` exactly when the independent BFS oracle (same
    /// adjacency + no-corner-cutting rule, separately implemented) says the
    /// goal is reachable from the start — i.e. A* never wrongly reports
    /// "unreachable" for a goal that really is reachable, and never panics
    /// doing so.
    #[test]
    fn reachability_matches_independent_bfs_oracle((grid, start, goal) in grid_and_endpoints()) {
        let a_star_found = find_path(&grid, start, goal).is_some();
        let bfs_found = bfs_reachable(&grid, start, goal);
        prop_assert_eq!(a_star_found, bfs_found, "start={:?} goal={:?}", start, goal);
    }

    /// Same grid + same endpoints -> byte-identical (well, `Vec`-identical)
    /// output on every call. This is the specific property the determinism
    /// contract (DESIGN.md §4.2) needs from pathfinding: the documented
    /// f-ascending / lower-cell-index tie-break is a *total* order, so there
    /// is no room for run-to-run variation.
    #[test]
    fn deterministic_across_repeated_calls((grid, start, goal) in grid_and_endpoints()) {
        let a = find_path(&grid, start, goal);
        let b = find_path(&grid, start, goal);
        prop_assert_eq!(a, b);
    }

    /// Off-grid start/goal (transiently produced by pathfinding-adjacent
    /// code that doesn't pre-clamp) must return `None`, never panic — a
    /// property the in-bounds-only strategy above can't reach, so this one
    /// deliberately allows coordinates outside `[0,w)x[0,h)`.
    ///
    /// Excludes `start == goal`: `find_path` short-circuits identical
    /// endpoints to `Some(empty)` *before* the off-grid/passability check
    /// (see the `returned_path_is_valid` comment on the same short-circuit),
    /// so an off-grid `start == goal` currently returns `Some([])`, not
    /// `None` — a real inconsistency with the doc comment's "`None` means
    /// the goal is off-grid" contract, just not one this property is about;
    /// flagged separately (see the test report) rather than papered over
    /// here by excluding it silently.
    #[test]
    fn off_grid_endpoints_return_none_without_panic(
        w in 4i32..16, h in 4i32..16,
        sx in -5i32..20, sy in -5i32..20,
        gx in -5i32..20, gy in -5i32..20,
    ) {
        let n = (w * h) as usize;
        let grid = Passability::new(w, h, vec![true; n]);
        let start = CellCoord::new(sx, sy);
        let goal = CellCoord::new(gx, gy);
        let on_grid = |c: CellCoord| c.x >= 0 && c.x < w && c.y >= 0 && c.y < h;
        // Post-fix, endpoint validity is checked before the `start == goal`
        // short-circuit, so *any* off-grid endpoint yields `None` — including
        // the degenerate `start == goal` case (pinned separately below).
        if !on_grid(start) || !on_grid(goal) {
            prop_assert_eq!(find_path(&grid, start, goal), None);
        }
    }

    /// Pinned-finding fix (was
    /// `off_grid_equal_start_and_goal_short_circuits_before_bounds_check`). An
    /// off-grid `start == goal` used to short-circuit to `Some(empty)` *before*
    /// the bounds check; it now returns `None`, because endpoint validity is
    /// checked first. Guards the corrected contract.
    #[test]
    fn off_grid_equal_start_and_goal_returns_none(
        w in 4i32..16, h in 4i32..16, x in 20i32..30, y in 20i32..30,
    ) {
        let n = (w * h) as usize;
        let grid = Passability::new(w, h, vec![true; n]);
        let c = CellCoord::new(x, y); // off-grid: x,y >= w,h by construction
        prop_assert_eq!(find_path(&grid, c, c), None);
    }
}

/// One real-map determinism check: `scg01ea`'s actual passability grid
/// (derived the same way the client does, via `ra_data::passability::build`)
/// between two of its real starting-unit cells. Skips cleanly without the
/// real assets (same policy as every other real-asset test in this repo).
#[test]
fn real_map_astar_is_deterministic() {
    let Some(grid) = real_scg01ea_passability() else {
        eprintln!(
            "SKIP: real assets not found (set RA_ASSETS_DIR or copy main.mix into assets/ to run this test)"
        );
        return;
    };

    // The real JEEP (63,50) and HARV (72,60) starting cells (scg01ea.ini
    // [UNITS], confirmed passable since real units spawn there).
    let start = CellCoord::new(63, 50);
    let goal = CellCoord::new(72, 60);
    assert!(
        grid.is_passable(start),
        "JEEP spawn cell should be passable"
    );
    assert!(grid.is_passable(goal), "HARV spawn cell should be passable");

    let a = find_path(&grid, start, goal);
    let b = find_path(&grid, start, goal);
    assert_eq!(a, b, "A* over the real scg01ea grid is not deterministic");
    let path = a.expect("scg01ea JEEP and HARV starting cells should be mutually reachable");
    assert_eq!(path.last(), Some(&goal));
}

/// Load `scg01ea`'s scenario and build its passability grid exactly as the
/// client does (`ra_data::passability::build`), or `None` if the real
/// archives aren't present under `RA_ASSETS_DIR` / `<workspace>/assets`.
fn real_scg01ea_passability() -> Option<Passability> {
    use std::path::PathBuf;

    let dir = std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"));
    if !dir.join("main.mix").is_file() {
        return None;
    }

    let main_bytes = std::fs::read(dir.join("main.mix")).ok()?;
    let main = ra_formats::mix::MixArchive::parse(&main_bytes).ok()?;
    let general = main.open_nested("general.mix").ok()?;
    let ini_bytes = general.get("scg01ea.ini")?;
    let ini_text = String::from_utf8_lossy(ini_bytes);
    let ini = ra_formats::ini::Ini::parse(&ini_text);
    let scenario = ra_data::scenario::Scenario::from_ini(&ini).ok()?;

    let mask = ra_data::passability::build(&scenario);
    Some(Passability::new(128, 128, mask))
}
