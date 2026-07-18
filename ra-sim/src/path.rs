//! Grid A* pathfinding over the map's passability grid (DESIGN.md §3.7: "Grid
//! A* with a proper open list", replacing the original's edge-following bug
//! algorithm). Movement is 8-directional; diagonal steps may not cut the
//! corner of an impassable cell.
//!
//! **Determinism.** The open list is a binary heap ordered by `f = g + h`
//! ascending, with ties broken by **lower linear cell index first**
//! (`y*128 + x`). That is a total order with no dependence on insertion order,
//! hash seeds, or pointer identity, so the same start/goal/grid always yields
//! the same path on every target (§4.2). Costs are integers: 10 per orthogonal
//! step, 14 per diagonal (a fixed-point stand-in for √2), and the heuristic is
//! the matching octile distance, which is admissible — A* returns a shortest
//! path under these costs.

use core::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::coords::{CellCoord, MAP_CELL_H, MAP_CELL_W};

/// Cost of an orthogonal (N/E/S/W) step.
const ORTHO_COST: i32 = 10;
/// Cost of a diagonal step (~√2 · 10, rounded).
const DIAG_COST: i32 = 14;

/// The 8 neighbour offsets, in a fixed order. `(dx, dy, cost)`.
const NEIGHBORS: [(i32, i32, i32); 8] = [
    (0, -1, ORTHO_COST), // N
    (1, -1, DIAG_COST),  // NE
    (1, 0, ORTHO_COST),  // E
    (1, 1, DIAG_COST),   // SE
    (0, 1, ORTHO_COST),  // S
    (-1, 1, DIAG_COST),  // SW
    (-1, 0, ORTHO_COST), // W
    (-1, -1, DIAG_COST), // NW
];

/// A read-only passability grid: `true` = a mover may occupy the cell.
#[derive(Clone, Debug)]
pub struct Passability {
    width: i32,
    height: i32,
    cells: Vec<bool>,
}

impl Passability {
    /// Build a grid from a row-major `width*height` passability mask.
    pub fn new(width: i32, height: i32, cells: Vec<bool>) -> Passability {
        assert_eq!(cells.len(), (width * height) as usize);
        Passability {
            width,
            height,
            cells,
        }
    }

    /// A fully-passable grid of the standard 128×128 map size.
    pub fn all_passable() -> Passability {
        Passability::new(
            MAP_CELL_W,
            MAP_CELL_H,
            vec![true; (MAP_CELL_W * MAP_CELL_H) as usize],
        )
    }

    /// Grid width in cells.
    pub fn width(&self) -> i32 {
        self.width
    }

    /// Grid height in cells.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Whether `cell` is on-grid and passable.
    pub fn is_passable(&self, cell: CellCoord) -> bool {
        if cell.x < 0 || cell.y < 0 || cell.x >= self.width || cell.y >= self.height {
            return false;
        }
        self.cells[(cell.y * self.width + cell.x) as usize]
    }

    fn linear(&self, cell: CellCoord) -> u32 {
        (cell.y * self.width + cell.x) as u32
    }
}

/// Heap node ordered so the smallest `f` (ties: smallest cell index) is
/// considered first. `Ord` is defined for a *min*-first pop, then the heap is
/// fed `Reverse(node)`; see [`find_path`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct Node {
    f: i32,
    cell_index: u32,
}

impl Ord for Node {
    fn cmp(&self, other: &Node) -> Ordering {
        // Smallest f first, then smallest cell index — a total order.
        self.f
            .cmp(&other.f)
            .then(self.cell_index.cmp(&other.cell_index))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Node) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Octile-distance heuristic scaled to the step costs (admissible).
fn heuristic(a: CellCoord, b: CellCoord) -> i32 {
    let dx = (a.x - b.x).abs();
    let dy = (a.y - b.y).abs();
    let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
    DIAG_COST * lo + ORTHO_COST * (hi - lo)
}

/// Find a shortest path from `start` to `goal` over `grid`.
///
/// Returns the ordered list of cells to step through **after** `start`, up to
/// and including `goal`. An empty vec means `start == goal` **and** that cell is
/// on-grid and passable. `None` means either endpoint is off-grid or impassable,
/// or the goal is unreachable.
///
/// **Pinned-finding fix.** The endpoint validity check now runs *before* the
/// `start == goal` short-circuit: an off-grid (or impassable) cell asked to
/// "path to itself" returns `None`, not `Some(empty)`. Short-circuiting first
/// let a degenerate off-grid start slip through as a spurious success.
pub fn find_path(grid: &Passability, start: CellCoord, goal: CellCoord) -> Option<Vec<CellCoord>> {
    if !grid.is_passable(start) || !grid.is_passable(goal) {
        return None;
    }
    if start == goal {
        return Some(Vec::new());
    }

    let n = (grid.width * grid.height) as usize;
    let mut g_score = vec![i32::MAX; n];
    let mut came_from = vec![u32::MAX; n];
    let mut closed = vec![false; n];

    let start_i = grid.linear(start) as usize;
    g_score[start_i] = 0;

    let mut open = BinaryHeap::new();
    open.push(core::cmp::Reverse(Node {
        f: heuristic(start, goal),
        cell_index: start_i as u32,
    }));

    let goal_i = grid.linear(goal);

    while let Some(core::cmp::Reverse(node)) = open.pop() {
        let cur_i = node.cell_index as usize;
        if closed[cur_i] {
            continue;
        }
        closed[cur_i] = true;

        if node.cell_index == goal_i {
            return Some(reconstruct(&came_from, grid, start_i as u32, goal_i));
        }

        let cur = CellCoord::new((cur_i as i32) % grid.width, (cur_i as i32) / grid.width);
        let cur_g = g_score[cur_i];

        for &(dx, dy, cost) in &NEIGHBORS {
            let next = CellCoord::new(cur.x + dx, cur.y + dy);
            if !grid.is_passable(next) {
                continue;
            }
            // No corner cutting: a diagonal step needs both shared orthogonal
            // neighbours passable.
            if dx != 0 && dy != 0 {
                let side_a = CellCoord::new(cur.x + dx, cur.y);
                let side_b = CellCoord::new(cur.x, cur.y + dy);
                if !grid.is_passable(side_a) || !grid.is_passable(side_b) {
                    continue;
                }
            }
            let next_i = grid.linear(next) as usize;
            if closed[next_i] {
                continue;
            }
            let tentative = cur_g + cost;
            if tentative < g_score[next_i] {
                g_score[next_i] = tentative;
                came_from[next_i] = cur_i as u32;
                open.push(core::cmp::Reverse(Node {
                    f: tentative + heuristic(next, goal),
                    cell_index: next_i as u32,
                }));
            }
        }
    }
    None
}

fn reconstruct(came_from: &[u32], grid: &Passability, start_i: u32, goal_i: u32) -> Vec<CellCoord> {
    let mut path = Vec::new();
    let mut cur = goal_i;
    while cur != start_i {
        path.push(CellCoord::new(
            (cur as i32) % grid.width,
            (cur as i32) / grid.width,
        ));
        cur = came_from[cur as usize];
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_grid() -> Passability {
        Passability::all_passable()
    }

    #[test]
    fn same_cell_is_empty_path() {
        let g = open_grid();
        let c = CellCoord::new(5, 5);
        assert_eq!(find_path(&g, c, c), Some(Vec::new()));
    }

    #[test]
    fn straight_line_open_field() {
        let g = open_grid();
        let path = find_path(&g, CellCoord::new(0, 0), CellCoord::new(3, 0)).unwrap();
        assert_eq!(
            path,
            vec![
                CellCoord::new(1, 0),
                CellCoord::new(2, 0),
                CellCoord::new(3, 0)
            ]
        );
    }

    #[test]
    fn diagonal_is_taken_in_open_field() {
        let g = open_grid();
        let path = find_path(&g, CellCoord::new(0, 0), CellCoord::new(3, 3)).unwrap();
        // Pure diagonal: 3 steps, each incrementing both axes.
        assert_eq!(path.len(), 3);
        assert_eq!(path.last(), Some(&CellCoord::new(3, 3)));
        for step in &path {
            assert_eq!(step.x, step.y);
        }
    }

    #[test]
    fn routes_around_a_wall() {
        // Vertical wall at x=2 for y=0..=4 with a gap at y=5.
        let w = MAP_CELL_W;
        let h = MAP_CELL_H;
        let mut cells = vec![true; (w * h) as usize];
        for y in 0..=4 {
            cells[(y * w + 2) as usize] = false;
        }
        let g = Passability::new(w, h, cells);
        let path = find_path(&g, CellCoord::new(0, 0), CellCoord::new(4, 0)).unwrap();
        // Must reach the goal and never step onto the wall.
        assert_eq!(path.last(), Some(&CellCoord::new(4, 0)));
        for step in &path {
            assert!(
                !(step.x == 2 && step.y <= 4),
                "path crossed the wall at {step:?}"
            );
        }
    }

    #[test]
    fn unreachable_returns_none() {
        // Fully wall off the goal cell's 3x3 neighbourhood.
        let w = MAP_CELL_W;
        let h = MAP_CELL_H;
        let mut cells = vec![true; (w * h) as usize];
        let goal = CellCoord::new(10, 10);
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                cells[((goal.y + dy) * w + goal.x + dx) as usize] = false;
            }
        }
        let g = Passability::new(w, h, cells);
        assert_eq!(find_path(&g, CellCoord::new(0, 0), goal), None);
    }

    #[test]
    fn no_corner_cutting() {
        // Two impassable cells forming a corner the path must not slip through.
        let w = MAP_CELL_W;
        let h = MAP_CELL_H;
        let mut cells = vec![true; (w * h) as usize];
        cells[1] = false; // (1,0)
        cells[w as usize] = false; // (0,1)
        let g = Passability::new(w, h, cells);
        // Going from (0,0) to (1,1): the direct diagonal is blocked by the
        // corner, and both orthogonal detours are walled, so it's unreachable.
        assert_eq!(
            find_path(&g, CellCoord::new(0, 0), CellCoord::new(1, 1)),
            None
        );
    }

    #[test]
    fn deterministic_across_runs() {
        let g = open_grid();
        let a = find_path(&g, CellCoord::new(2, 7), CellCoord::new(40, 33)).unwrap();
        let b = find_path(&g, CellCoord::new(2, 7), CellCoord::new(40, 33)).unwrap();
        assert_eq!(a, b);
    }
}
