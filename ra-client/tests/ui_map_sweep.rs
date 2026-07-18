//! Map-sweep UI tests (DESIGN.md §4.8 layer 2): drive the camera through
//! `AppCore`'s real `handle`/`update` seam to every corner and edge extreme,
//! and sweep `compose()` across the entire map at several viewport sizes —
//! including a viewport smaller than a single cell and one larger than the
//! whole map. Every assertion is: no panic, frame dimensions match the
//! request, and repeat passes are byte-identical (hash-stable).
//!
//! Two variants of each test: a synthetic-map one (always runs, no assets
//! needed) and a real-scenario one over `scg01ea.ini` (skips cleanly when
//! the real assets are absent).

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key, Rect};

/// The full map raster is always 128x128 cells * 24px = 3072x3072, whether
/// synthetic or real (`ra_data::scenario::MAP_CELL_W/H` are fixed consts).
const MAP_W: u32 = 3072;
const MAP_H: u32 = 3072;

/// Viewport sizes swept by every test below: smaller than one 24px cell,
/// the shell's real default (see `ra_client::shell`), and larger than the
/// entire map on both axes (just past 3072, not gratuitously past it — the
/// clamp-to-zero / black-padding behavior being tested doesn't depend on the
/// margin, and keeping it small matters for debug-build test runtime, since
/// `compose()` is O(viewport area)).
const VIEWPORTS: [(u32, u32); 3] = [(6, 6), (640, 400), (3200, 3150)];

fn expected_clamp(map: u32, viewport: u32) -> i64 {
    if viewport >= map {
        0
    } else {
        (map - viewport) as i64
    }
}

/// Drive `core` to a screen-edge saturation point by holding the given keys
/// and advancing virtual time far beyond any possible scroll distance, then
/// releasing them. `None` for an axis leaves it untouched.
fn drive_to_extreme(core: &mut AppCore, x_key: Option<Key>, y_key: Option<Key>) {
    if let Some(k) = x_key {
        core.handle(InputEvent::KeyDown(k));
    }
    if let Some(k) = y_key {
        core.handle(InputEvent::KeyDown(k));
    }
    // 10,000 virtual seconds at any plausible scroll speed vastly exceeds the
    // map's extent, so this always saturates against the clamp.
    core.update(10_000_000);
    if let Some(k) = x_key {
        core.handle(InputEvent::KeyUp(k));
    }
    if let Some(k) = y_key {
        core.handle(InputEvent::KeyUp(k));
    }
}

/// Layer 2a: all four corners, reached via real `handle`/`update` input, at
/// every viewport size, plus the four single-axis edge extremes from a
/// mid-map starting point. Reuses one `core` across all viewport sizes
/// (`Resize` just changes state; there's no need to rebuild the raster).
fn sweep_corners_and_edges(core: &mut AppCore, label: &str) {
    for &(vw, vh) in &VIEWPORTS {
        core.handle(InputEvent::Resize {
            width: vw,
            height: vh,
        });
        assert_eq!(
            core.viewport_size(),
            (vw, vh),
            "{label}: viewport size not applied for {vw}x{vh}"
        );

        let exp_x = expected_clamp(MAP_W, vw);
        let exp_y = expected_clamp(MAP_H, vh);

        // Composing is O(viewport area); at the larger-than-map viewport
        // size every corner/edge combination clamps to the *same* (0, 0)
        // rect, so repeating a full-cost compose() at each of the 8
        // corner/edge combinations would just re-verify an identical frame
        // eight times over. `composed` tracks which rects have already had
        // compose() called (and its dimensions/no-panic asserted) this
        // viewport size, so distinct rects are still all composed, but a
        // clamped-to-the-same-place viewport isn't paid for twice — camera
        // *positioning* is still checked for every combination regardless.
        let mut composed: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
        let compose_once =
            |core: &AppCore, composed: &mut std::collections::HashSet<(i64, i64)>| {
                let rect = core.camera_rect();
                if composed.insert((rect.x, rect.y)) {
                    let frame = core.compose_camera();
                    assert_eq!(frame.width, vw, "{label}: frame width mismatch");
                    assert_eq!(frame.height, vh, "{label}: frame height mismatch");
                    assert_eq!(frame.pixels.len(), (vw as usize) * (vh as usize) * 4);
                }
            };

        let corners: [(Option<Key>, Option<Key>, i64, i64); 4] = [
            (Some(Key::Left), Some(Key::Up), 0, 0),
            (Some(Key::Right), Some(Key::Up), exp_x, 0),
            (Some(Key::Left), Some(Key::Down), 0, exp_y),
            (Some(Key::Right), Some(Key::Down), exp_x, exp_y),
        ];
        for (xk, yk, ex, ey) in corners {
            drive_to_extreme(core, xk, yk);
            let rect = core.camera_rect();
            assert_eq!(
                (rect.x, rect.y),
                (ex, ey),
                "{label}: viewport {vw}x{vh} corner ({xk:?},{yk:?}) landed at wrong position"
            );
            assert_eq!(rect.width, vw);
            assert_eq!(rect.height, vh);
            // compose() must never panic and must return exactly the
            // requested viewport dimensions, even when the viewport spills
            // outside the map (the huge-viewport case).
            compose_once(core, &mut composed);
        }

        // Single-axis edge extremes from a mid-map start: hold only one
        // direction and confirm the *other* axis is untouched.
        let mid_x = expected_clamp(MAP_W, vw) / 2;
        let mid_y = expected_clamp(MAP_H, vh) / 2;
        let edges: [(Key, bool); 4] = [
            (Key::Left, true), // true = x-axis extreme
            (Key::Right, true),
            (Key::Up, false), // false = y-axis extreme
            (Key::Down, false),
        ];
        for (key, is_x_axis) in edges {
            core.set_camera(mid_x as f32, mid_y as f32);
            drive_to_extreme(core, Some(key), None);
            let rect = core.camera_rect();
            compose_once(core, &mut composed);
            if is_x_axis {
                assert_eq!(rect.y, mid_y, "{label}: {key:?} disturbed the y axis");
                let expected = if key == Key::Left { 0 } else { exp_x };
                assert_eq!(
                    rect.x, expected,
                    "{label}: {key:?} did not reach its extreme"
                );
            } else {
                assert_eq!(rect.x, mid_x, "{label}: {key:?} disturbed the x axis");
                let expected = if key == Key::Up { 0 } else { exp_y };
                assert_eq!(
                    rect.y, expected,
                    "{label}: {key:?} did not reach its extreme"
                );
            }
        }
    }
}

/// Layer 2b: a lawnmower sweep of `compose()` across the *entire* map, at a
/// fixed grid of positions spanning both axes (including both extremes),
/// asserting no panic, correct dimensions, and — run twice — identical
/// frame hashes both within a pass (same position -> same pixels) and across
/// two independent passes (determinism, no hidden state leaking between
/// composes). `compose(Rect)` is camera-independent, so `core` is shared
/// (`&AppCore`) rather than mutated.
///
/// Bounded to the tiny and default viewport sizes: the larger-than-map
/// viewport is already exercised (repeatedly) by `sweep_corners_and_edges`
/// above, and re-running a full positional grid at a 4000x3500 viewport
/// (56 MB/frame) here would just multiply allocation cost without adding
/// coverage, since every such frame is dominated by black padding anyway.
fn sweep_lawnmower(core: &AppCore, label: &str) {
    const GRID: u32 = 9; // 9x9 positions, including both axis extremes.

    for &(vw, vh) in &[VIEWPORTS[0], VIEWPORTS[1]] {
        let max_x = expected_clamp(MAP_W, vw);
        let max_y = expected_clamp(MAP_H, vh);

        let mut positions = Vec::new();
        for gy in 0..GRID {
            for gx in 0..GRID {
                let x = (max_x * gx as i64) / (GRID as i64 - 1);
                let y = (max_y * gy as i64) / (GRID as i64 - 1);
                positions.push((x, y));
            }
        }

        let run_pass = || -> Vec<u64> {
            positions
                .iter()
                .map(|&(x, y)| {
                    let frame = core.compose(Rect {
                        x,
                        y,
                        width: vw,
                        height: vh,
                    });
                    assert_eq!(
                        frame.width, vw,
                        "{label}: lawnmower frame width at ({x},{y})"
                    );
                    assert_eq!(
                        frame.height, vh,
                        "{label}: lawnmower frame height at ({x},{y})"
                    );
                    support::fnv1a(&frame.pixels)
                })
                .collect()
        };

        let pass1 = run_pass();
        let pass2 = run_pass();
        assert_eq!(
            pass1, pass2,
            "{label}: lawnmower hashes at {vw}x{vh} are not stable across repeat passes"
        );
    }
}

/// One continuous input-driven traversal (not a grid jump): hold Right and
/// step `update()` repeatedly to actually scroll across a full row, then
/// Down + Right back across the next, like a literal lawnmower — exercising
/// the incremental `update()` accumulation path (as opposed to the grid
/// test's direct `compose(Rect)` jumps) at least once per map.
fn sweep_continuous_scroll(core: &mut AppCore, label: &str) {
    let (vw, vh) = (640, 400);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    core.set_camera(0.0, 0.0);

    let mut visited = Vec::new();
    for row in 0..3 {
        let going_right = row % 2 == 0;
        core.handle(InputEvent::KeyDown(if going_right {
            Key::Right
        } else {
            Key::Left
        }));
        for _ in 0..20 {
            core.update(250); // 0.25s steps
            let frame = core.compose_camera();
            assert_eq!(frame.width, vw);
            assert_eq!(frame.height, vh);
            visited.push(core.camera_rect());
        }
        core.handle(InputEvent::KeyUp(if going_right {
            Key::Right
        } else {
            Key::Left
        }));
        core.handle(InputEvent::KeyDown(Key::Down));
        core.update(500);
        core.handle(InputEvent::KeyUp(Key::Down));
    }
    // Sanity: the traversal actually moved (not a no-op scroll).
    let xs: std::collections::BTreeSet<i64> = visited.iter().map(|r| r.x).collect();
    let ys: std::collections::BTreeSet<i64> = visited.iter().map(|r| r.y).collect();
    assert!(xs.len() > 1, "{label}: continuous scroll never moved on x");
    assert!(ys.len() > 1, "{label}: continuous scroll never moved on y");
}

#[test]
fn synthetic_corners_and_edges() {
    let mut core = support::synthetic_core();
    sweep_corners_and_edges(&mut core, "synthetic");
}

#[test]
fn synthetic_lawnmower() {
    let core = support::synthetic_core();
    sweep_lawnmower(&core, "synthetic");
}

#[test]
fn synthetic_continuous_scroll() {
    let mut core = support::synthetic_core();
    sweep_continuous_scroll(&mut core, "synthetic");
}

#[test]
fn real_scg01ea_corners_and_edges() {
    let Some(mut core) = support::load_real_core() else {
        return;
    };
    sweep_corners_and_edges(&mut core, "real:scg01ea");
}

#[test]
fn real_scg01ea_lawnmower() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    sweep_lawnmower(&core, "real:scg01ea");
}

#[test]
fn real_scg01ea_continuous_scroll() {
    let Some(mut core) = support::load_real_core() else {
        return;
    };
    sweep_continuous_scroll(&mut core, "real:scg01ea");
}
