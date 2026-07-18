//! `ra-client` — M2 terrain viewer. Two frontends over one windowless
//! [`ra_client::AppCore`]:
//!
//! - `dump` — headless: composite a scenario's map (or a cell rect) to a PNG.
//!   Needs no display; this is the M2 verification path.
//! - `window` — the macroquad shell with arrow-key + edge scrolling (only built
//!   with the default `window` feature).
//!
//! Usage:
//! - `ra-client dump [--assets DIR] [--scenario NAME] [--out PATH.png] [--rect CX CY CW CH] [--playable]`
//! - `ra-client window [--assets DIR] [--scenario NAME]`

use std::process::ExitCode;

use ra_client::assets::{self, LoadedGame, LoadedTerrain};
use ra_client::input::{InputEvent, MouseButton, Rect};
use ra_client::platform;
use ra_client::png;
use ra_client::AppCore;
use ra_formats::tmpl::{ICON_HEIGHT, ICON_WIDTH};
use ra_sim::coords::CellCoord;

type BoxErr = Box<dyn std::error::Error>;

const DEFAULT_SCENARIO: &str = "scg01ea.ini";
const CELL: u32 = ICON_WIDTH as u32; // 24; ICON_WIDTH == ICON_HEIGHT

fn main() -> ExitCode {
    let _ = ICON_HEIGHT; // documents the square-cell assumption
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ra-client: {e}");
            ExitCode::FAILURE
        }
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    if let Some(i) = args.iter().position(|a| a == flag) {
        if i + 1 < args.len() {
            let v = args.remove(i + 1);
            args.remove(i);
            return Some(v);
        }
        args.remove(i);
    }
    None
}

fn has_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == flag) {
        args.remove(i);
        return true;
    }
    false
}

fn run(args: &[String]) -> Result<(), BoxErr> {
    let mut args = args.to_vec();
    let cmd = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };
    match cmd.as_str() {
        "dump" => cmd_dump(args),
        "window" => cmd_window(args),
        "sim" => cmd_sim(args),
        "battle" => cmd_battle(args),
        _ => {
            eprintln!(
                "usage:\n  ra-client dump   [--assets DIR] [--scenario NAME] [--out PATH.png] [--rect CX CY CW CH] [--playable]\n  ra-client window [--assets DIR] [--scenario NAME] [--smoke-seconds N]\n  ra-client sim    [--assets DIR] [--scenario NAME] [--out-dir DIR]\n  ra-client battle [--assets DIR] [--scenario NAME] [--out-dir DIR]"
            );
            Err("unknown or missing subcommand".into())
        }
    }
}

fn load(args: &mut Vec<String>) -> Result<LoadedTerrain, BoxErr> {
    let assets_flag = take_flag(args, "--assets");
    let scenario = take_flag(args, "--scenario").unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());
    assets::load_from_dir(&dir, &scenario)
}

fn describe(loaded: &LoadedTerrain) {
    let s = &loaded.scenario;
    eprintln!(
        "theater: {:?}  playable rect: x={} y={} w={} h={} cells  templates loaded: {}",
        s.theater,
        s.map_x,
        s.map_y,
        s.map_width,
        s.map_height,
        loaded.tiles.len()
    );
}

fn cmd_dump(mut args: Vec<String>) -> Result<(), BoxErr> {
    let out = take_flag(&mut args, "--out").unwrap_or_else(|| "map.png".to_string());
    let playable = has_flag(&mut args, "--playable");
    let rect_cells = take_rect(&mut args, "--rect")?;

    let loaded = load(&mut args)?;
    describe(&loaded);

    // Decide the cell rectangle to render.
    let (cx, cy, cw, ch) = if let Some(r) = rect_cells {
        r
    } else if playable {
        let s = &loaded.scenario;
        (
            s.map_x as u32,
            s.map_y as u32,
            s.map_width as u32,
            s.map_height as u32,
        )
    } else {
        (0, 0, 128, 128) // full map
    };

    let core = loaded.into_appcore();
    let rect = Rect {
        x: (cx * CELL) as i64,
        y: (cy * CELL) as i64,
        width: cw * CELL,
        height: ch * CELL,
    };
    let frame = core.compose(rect);
    report_frame(&frame, cx, cy, cw, ch);

    let bytes = png::encode_rgba(frame.width, frame.height, &frame.pixels);
    std::fs::write(&out, &bytes)?;
    eprintln!(
        "wrote {out} ({}x{} px, {} bytes)",
        frame.width,
        frame.height,
        bytes.len()
    );
    Ok(())
}

/// Parse an optional `--rect CX CY CW CH` (four following integers, in cells).
fn take_rect(args: &mut Vec<String>, flag: &str) -> Result<Option<(u32, u32, u32, u32)>, BoxErr> {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return Ok(None);
    };
    if i + 4 >= args.len() {
        return Err(format!("{flag} needs four integers: CX CY CW CH").into());
    }
    let mut vals = [0u32; 4];
    for (k, v) in vals.iter_mut().enumerate() {
        *v = args[i + 1 + k]
            .parse()
            .map_err(|_| format!("{flag}: '{}' is not an integer", args[i + 1 + k]))?;
    }
    for _ in 0..5 {
        args.remove(i);
    }
    Ok(Some((vals[0], vals[1], vals[2], vals[3])))
}

/// Print a quick plausibility summary of a composed frame (distinct colors,
/// coverage) so the headless path is self-verifying.
fn report_frame(frame: &ra_client::Frame, cx: u32, cy: u32, cw: u32, ch: u32) {
    let mut nonblack = 0usize;
    let mut colors = std::collections::BTreeSet::new();
    for px in frame.pixels.chunks_exact(4) {
        if px[0] != 0 || px[1] != 0 || px[2] != 0 {
            nonblack += 1;
        }
        colors.insert((px[0], px[1], px[2]));
    }
    let total = (frame.width as usize) * (frame.height as usize);
    eprintln!(
        "rect cells (x={cx} y={cy} w={cw} h={ch}): {}x{} px, {nonblack}/{total} non-black ({}%), {} distinct colors",
        frame.width,
        frame.height,
        nonblack * 100 / total.max(1),
        colors.len()
    );
}

#[cfg(feature = "window")]
fn cmd_window(mut args: Vec<String>) -> Result<(), BoxErr> {
    let smoke = take_flag(&mut args, "--smoke-seconds")
        .map(|s| s.parse::<f32>())
        .transpose()
        .map_err(|_| "--smoke-seconds needs a number")?;
    // Prefer the full game view (terrain + units); fall back to terrain-only if
    // the unit/rules archives can't be resolved.
    let game = load_game(&mut args);
    let mut core = match game {
        Ok(g) => {
            report_spawns(&g);
            let start = center_camera_start(&g);
            let mut core = g.core;
            core.set_camera(start.0, start.1);
            core
        }
        Err(e) => {
            eprintln!("note: falling back to terrain-only view ({e})");
            let loaded = load(&mut args)?;
            describe(&loaded);
            let start = (
                (loaded.scenario.map_x as f32) * CELL as f32,
                (loaded.scenario.map_y as f32) * CELL as f32,
            );
            let mut core = loaded.into_appcore();
            core.set_camera(start.0, start.1);
            core
        }
    };
    let _ = &mut core;
    ra_client::shell::run_window(core, smoke);
    Ok(())
}

#[cfg(not(feature = "window"))]
fn cmd_window(_args: Vec<String>) -> Result<(), BoxErr> {
    Err(
        "this build was compiled without the `window` feature; rebuild with default features"
            .into(),
    )
}

/// Load a fully playable scenario (terrain + units) from the resolved assets.
fn load_game(args: &mut Vec<String>) -> Result<LoadedGame, BoxErr> {
    let assets_flag = take_flag(args, "--assets");
    let scenario = take_flag(args, "--scenario").unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());
    assets::load_game_from_dir(&dir, &scenario)
}

/// Print a one-line manifest of what the loader spawned.
fn report_spawns(g: &LoadedGame) {
    eprintln!("spawned {} unit(s):", g.spawned.len());
    for s in &g.spawned {
        eprintln!(
            "  {} house={} at cell ({},{})",
            s.unit_type, s.house, s.cell.x, s.cell.y
        );
    }
    if !g.skipped.is_empty() {
        eprintln!(
            "  skipped {} placement(s) (no rules/sprite): {}",
            g.skipped.len(),
            g.skipped.join(", ")
        );
    }
}

/// Camera top-left (map pixels) that centres the spawned units in a default
/// 800x600 view.
#[cfg(feature = "window")]
fn center_camera_start(g: &LoadedGame) -> (f32, f32) {
    if g.spawned.is_empty() {
        return (
            g.playable.0 as f32 * CELL as f32,
            g.playable.1 as f32 * CELL as f32,
        );
    }
    let (mut sx, mut sy) = (0i64, 0i64);
    for s in &g.spawned {
        sx += (s.cell.x * CELL as i32) as i64;
        sy += (s.cell.y * CELL as i32) as i64;
    }
    let n = g.spawned.len() as i64;
    let cx = (sx / n) as f32;
    let cy = (sy / n) as f32;
    (cx - 400.0, cy - 300.0)
}

/// The `sim` subcommand: load real starting units, dump a "before" frame, issue
/// a scripted move through the AppCore seam, dump an "after" frame, and prove
/// the run is deterministic by replaying the identical script and comparing the
/// sim state hash.
fn cmd_sim(mut args: Vec<String>) -> Result<(), BoxErr> {
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let scenario_for_msg = args
        .iter()
        .position(|a| a == "--scenario")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| DEFAULT_SCENARIO.to_string());

    // Two independent loads so we can run the identical script twice.
    let mut a_args = args.clone();
    let mut b_args = args.clone();
    let g1 = load_game(&mut a_args)?;
    let g2 = load_game(&mut b_args)?;
    report_spawns(&g1);

    let (before, after, hash1, report) = drive_script(g1, &out_dir, true)?;
    let (_b2, _a2, hash2, _r2) = drive_script(g2, &out_dir, false)?;

    let _ = (before, after);
    eprintln!("--- movement ---\n{report}");
    eprintln!("run 1 final sim hash: {hash1:#018x}");
    eprintln!("run 2 final sim hash: {hash2:#018x}");
    if hash1 == hash2 {
        eprintln!("DETERMINISM OK: identical final state hashes ({scenario_for_msg})");
        Ok(())
    } else {
        Err("determinism FAILED: sim hashes differ between identical runs".into())
    }
}

/// Drive one game through the seam: select every visible unit, order a move to a
/// nearby passable cell, step ~10 s of sim, and (when `write_png`) dump before /
/// after PNGs. Returns `(before, after, final_hash, movement_report)`.
fn drive_script(
    g: LoadedGame,
    out_dir: &str,
    write_png: bool,
) -> Result<(Vec<u8>, Vec<u8>, u64, String), BoxErr> {
    let LoadedGame {
        mut core, spawned, ..
    } = g;
    let (vw, vh) = (800u32, 600u32);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    // Centre the camera on the units.
    let (mut sx, mut sy) = (0i64, 0i64);
    for s in &spawned {
        sx += (s.cell.x * CELL as i32) as i64;
        sy += (s.cell.y * CELL as i32) as i64;
    }
    let n = spawned.len().max(1) as i64;
    let cam = (
        (sx / n) as f32 - vw as f32 / 2.0,
        (sy / n) as f32 - vh as f32 / 2.0,
    );
    core.set_camera(cam.0, cam.1);

    // "Before" frame.
    let before_frame = core.compose_camera();
    let before = png::encode_rgba(
        before_frame.width,
        before_frame.height,
        &before_frame.pixels,
    );

    // Record starting cells.
    let start_cells: Vec<(String, CellCoord)> = spawned
        .iter()
        .map(|s| (s.unit_type.clone(), s.cell))
        .collect();

    // Box-select the entire viewport (selects every visible unit).
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: 0,
        y: 0,
    });
    core.handle(InputEvent::MouseMoved {
        x: vw as i32 - 1,
        y: vh as i32 - 1,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: vw as i32 - 1,
        y: vh as i32 - 1,
    });
    let selected = core.selected_handles().len();

    // Pick a destination: scan outward from the first unit for a passable cell
    // ~6 cells away, then translate it to a viewport pixel to right-click.
    let anchor = spawned
        .first()
        .map(|s| s.cell)
        .unwrap_or(CellCoord::new(0, 0));
    let dest = pick_destination(&core, anchor);
    let r = core.camera_rect();
    let dest_vx = (dest.x * CELL as i32) as i64 + CELL as i64 / 2 - r.x;
    let dest_vy = (dest.y * CELL as i32) as i64 + CELL as i64 / 2 - r.y;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: dest_vx as i32,
        y: dest_vy as i32,
    });
    let issued = core.drain_commands().len();

    // Clear the pointer so its resting position (a viewport corner, from the
    // box-select) doesn't edge-scroll the camera while we step the sim.
    core.handle(InputEvent::MouseLeft);

    // Step ~10 seconds of sim at ~1 tick per update.
    for _ in 0..150 {
        core.update(67);
    }

    // "After" frame.
    let after_frame = core.compose_camera();
    let after = png::encode_rgba(after_frame.width, after_frame.height, &after_frame.pixels);

    if write_png {
        let bp = format!("{out_dir}/sim_before.png");
        let ap = format!("{out_dir}/sim_after.png");
        std::fs::write(&bp, &before)?;
        std::fs::write(&ap, &after)?;
        eprintln!("wrote {bp} and {ap}");
    }

    // Movement report: compare start vs end cells.
    let mut report = String::new();
    report.push_str(&format!(
        "selected {selected} unit(s), issued {issued} move command(s), destination cell ({},{})\n",
        dest.x, dest.y
    ));
    let mut moved = 0;
    for (h, (name, start)) in core
        .world()
        .units
        .handles()
        .into_iter()
        .zip(start_cells.iter())
    {
        if let Some(u) = core.world().units.get(h) {
            let end = u.cell();
            let did = if end != *start {
                moved += 1;
                "MOVED"
            } else {
                "stayed"
            };
            report.push_str(&format!(
                "  {name}: ({},{}) -> ({},{}) [{did}]\n",
                start.x, start.y, end.x, end.y
            ));
        }
    }
    report.push_str(&format!("{moved} unit(s) changed cell"));

    Ok((before, after, core.sim_hash(), report))
}

/// The `battle` subcommand (M4 verification): spawn a real 2TNK and an enemy
/// HARV adjacent on a scenario's terrain, drive the attack through the AppCore
/// seam (select attacker, right-click enemy), dump a before/mid/after PNG
/// sequence, verify the damage math against hand-computed rules.ini values, and
/// prove determinism by replaying the identical script and comparing hash chains.
fn cmd_battle(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_client::assets::load_battle_from_dir;
    use ra_sim::modify_damage;

    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario =
        take_flag(&mut args, "--scenario").unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());

    // Two independent loads so the identical script can run twice.
    let s1 = load_battle_from_dir(&dir, &scenario)?;
    let s2 = load_battle_from_dir(&dir, &scenario)?;

    // --- Hand-computed damage expectations from rules.ini values ---
    let w = &s1.weapon;
    let verses_steel = w.verses[s1.target_armor as usize];
    let dmg_point_blank = modify_damage(
        w.damage,
        &ra_sim::WarheadProfile {
            spread: w.spread,
            verses: w.verses,
        },
        s1.target_armor,
        0,
        w.min_damage,
        w.max_damage,
    );
    let dmg_at_200 = modify_damage(
        w.damage,
        &ra_sim::WarheadProfile {
            spread: w.spread,
            verses: w.verses,
        },
        s1.target_armor,
        200,
        w.min_damage,
        w.max_damage,
    );
    let expected_shots = s1.target_max_hp.div_ceil(dmg_point_blank.max(1) as u16);
    eprintln!("--- damage math (2TNK 90mm, AP warhead, vs HARV 'heavy'=steel armor) ---");
    eprintln!(
        "  base Damage={}  Verses[steel]={} raw16.16 ({}%)  Spread={}",
        w.damage,
        verses_steel,
        verses_steel * 100 / 65536,
        w.spread
    );
    eprintln!("  damage at distance 0   = {dmg_point_blank}  (expected 30)");
    eprintln!("  damage at distance 200 = {dmg_at_200}   (falloff: 30 / (200/(3*5)=13) = 2)");
    eprintln!(
        "  target Strength={}  => shots-to-kill = {expected_shots} (expected 20)",
        s1.target_max_hp
    );

    let (report, hashes1) = drive_battle(s1, &out_dir, true, dmg_point_blank as u16)?;
    let (_r2, hashes2) = drive_battle(s2, &out_dir, false, dmg_point_blank as u16)?;
    eprintln!("--- battle ---\n{report}");

    if hashes1 == hashes2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            hashes1.len(),
            hashes1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        // Find first divergence for the report.
        let first = hashes1
            .iter()
            .zip(&hashes2)
            .position(|(a, b)| a != b)
            .unwrap_or(hashes1.len());
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// Drive one battle through the seam: center camera, select the 2TNK, right-click
/// the HARV to issue an attack, then step the sim dumping before / mid (health
/// bar + turret tracking) / after (target destroyed) PNGs. Returns a text report
/// and the per-tick sim-hash chain.
fn drive_battle(
    setup: ra_client::assets::BattleSetup,
    out_dir: &str,
    write_png: bool,
    expected_per_shot: u16,
) -> Result<(String, Vec<u64>), BoxErr> {
    let ra_client::assets::BattleSetup {
        mut core,
        attacker,
        attacker_cell,
        target,
        target_cell,
        target_max_hp,
        ..
    } = setup;
    let (vw, vh) = (800u32, 600u32);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    // Center camera between the two combatants.
    let midx = (attacker_cell.x + target_cell.x + 1) * CELL as i32 / 2;
    let midy = (attacker_cell.y + target_cell.y + 1) * CELL as i32 / 2;
    core.set_camera(midx as f32 - vw as f32 / 2.0, midy as f32 - vh as f32 / 2.0);

    let cam = core.camera_rect();
    let screen = |cell: CellCoord| -> (i32, i32) {
        (
            (cell.x * CELL as i32 + CELL as i32 / 2 - cam.x as i32),
            (cell.y * CELL as i32 + CELL as i32 / 2 - cam.y as i32),
        )
    };
    let (ax, ay) = screen(attacker_cell);
    let (tx, ty) = screen(target_cell);

    // "Before" frame.
    let before = core.compose_camera();

    // Select the attacker (click) then right-click the target (attack).
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: ax,
        y: ay,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: ax,
        y: ay,
    });
    let selected = core.selected_handles();
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: tx,
        y: ty,
    });
    let issued = core.drain_commands();
    core.handle(InputEvent::MouseLeft); // stop edge-scroll

    // Step the sim, tracking the target's health, shot drops, and PNG captures.
    let mut prev_hp = target_max_hp;
    let mut drops: Vec<u16> = Vec::new();
    let mut mid: Option<ra_client::Frame> = None;
    let mut hashes = Vec::new();
    let mut after: Option<ra_client::Frame> = None;
    let mut kill_tick = None;
    for t in 0..3000u32 {
        core.update(67); // ~1 tick per update at 15 Hz
        hashes.push(core.sim_hash());
        let hp = core.world().units.get(target).map(|u| u.health);
        match hp {
            Some(hp) => {
                if hp < prev_hp {
                    drops.push(prev_hp - hp);
                    prev_hp = hp;
                }
                // Capture "mid" once the target drops to ~half health, while it
                // is still alive (shows health bar + tracked turret).
                if mid.is_none() && hp <= target_max_hp / 2 {
                    mid = Some(core.compose_camera());
                }
            }
            None => {
                // Target removed on death — capture "after" once and stop.
                after = Some(core.compose_camera());
                kill_tick = Some(t);
                break;
            }
        }
    }
    // If it never died (shouldn't happen), still grab an "after".
    let after = after.unwrap_or_else(|| core.compose_camera());
    let mid = mid.unwrap_or_else(|| before.clone());

    if write_png {
        for (name, f) in [
            ("battle_before", &before),
            ("battle_mid", &mid),
            ("battle_after", &after),
        ] {
            let path = format!("{out_dir}/{name}.png");
            let bytes = png::encode_rgba(f.width, f.height, &f.pixels);
            std::fs::write(&path, &bytes)?;
            eprintln!("wrote {path} ({}x{} px)", f.width, f.height);
        }
    }

    let all_full_shots = drops.iter().all(|&d| d == expected_per_shot);
    let mut report = String::new();
    report.push_str(&format!(
        "selected {} unit(s) (attacker={}), issued {} command(s)\n",
        selected.len(),
        selected.first().map(|h| h.index).unwrap_or(u32::MAX),
        issued.len(),
    ));
    report.push_str(&format!(
        "target HARV: {} shots landed, per-shot damage drops = {:?}\n",
        drops.len(),
        drops
    ));
    report.push_str(&format!(
        "every shot dealt exactly {expected_per_shot} damage: {all_full_shots}\n"
    ));
    match kill_tick {
        Some(t) => report.push_str(&format!("target destroyed and removed at tick {t}")),
        None => report.push_str("target survived (unexpected)"),
    }
    let _ = attacker;
    Ok((report, hashes))
}

/// Find a passable destination cell ~6 cells from `anchor` by scanning a small
/// spiral; falls back to `anchor` if nothing passable is near.
fn pick_destination(core: &AppCore, anchor: CellCoord) -> CellCoord {
    let grid = core.world().passability();
    // Preferred straight offsets first (looks like a clean march), then a ring.
    let candidates = [
        (6, 0),
        (-6, 0),
        (0, 6),
        (0, -6),
        (4, 4),
        (-4, 4),
        (4, -4),
        (-4, -4),
        (3, 0),
        (0, 3),
    ];
    for (dx, dy) in candidates {
        let c = CellCoord::new(anchor.x + dx, anchor.y + dy);
        if grid.is_passable(c) {
            return c;
        }
    }
    anchor
}
