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

use ra_client::assets::{self, EconGame, LoadedGame, LoadedTerrain};
use ra_client::input::{InputEvent, Key, MouseButton, Rect};
use ra_client::platform;
use ra_client::png;
use ra_client::AppCore;
use ra_formats::tmpl::{ICON_HEIGHT, ICON_WIDTH};
use ra_sim::coords::CellCoord;
use ra_sim::BuildItem;

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
        "econ" => cmd_econ(args),
        "skirmish" => cmd_skirmish(args),
        _ => {
            eprintln!(
                "usage:\n  ra-client dump     [--assets DIR] [--scenario NAME] [--out PATH.png] [--rect CX CY CW CH] [--playable]\n  ra-client window   [--assets DIR] [--scenario NAME] [--smoke-seconds N]\n  ra-client sim      [--assets DIR] [--scenario NAME] [--out-dir DIR]\n  ra-client battle   [--assets DIR] [--scenario NAME] [--out-dir DIR]\n  ra-client econ     [--assets DIR] [--scenario NAME] [--out-dir DIR] [--credits N]\n  ra-client skirmish [--assets DIR] [--scenario NAME] [--out-dir DIR] [--credits N] [--difficulty easy|normal|hard] [--ticks N]"
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
    let credits: i32 = take_flag(&mut args, "--credits")
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--credits needs an integer")?
        .unwrap_or(8000);
    // Audio off flag (M7): `--mute` boots with an empty sound bank.
    let muted = has_flag(&mut args, "--mute");
    // M6 FIRST PLAYABLE: boot a skirmish (player + 1 AI, shroud, ore growth,
    // win/lose) on a multiplayer map. Fall back to the M5 econ view, then
    // terrain+units, then terrain-only, if archives can't be resolved.
    let assets_flag = args
        .iter()
        .position(|a| a == "--assets")
        .and_then(|i| args.get(i + 1).cloned());
    let scenario = args
        .iter()
        .position(|a| a == "--scenario")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "scm01ea.ini".to_string());
    let skirmish = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or_else(|| BoxErr::from("no assets dir"))
        .and_then(|dir| {
            assets::load_skirmish_from_dir(&dir, &scenario, credits, ra_sim::Difficulty::Normal)
        });

    let core = match skirmish {
        Ok(g) => {
            eprintln!(
                "skirmish: player house {} at ({},{}) vs AI house {} at ({},{}) — \
                 press D to deploy the MCV, click the sidebar to build, right-click to order",
                g.player_house,
                g.player_start.x,
                g.player_start.y,
                g.ai_house,
                g.ai_start.x,
                g.ai_start.y
            );
            let mut core = g.core;
            core.set_camera(
                (g.player_start.x * CELL as i32) as f32 - 430.0,
                (g.player_start.y * CELL as i32) as f32 - 360.0,
            );
            core
        }
        Err(e) => {
            eprintln!("note: skirmish unavailable ({e}); trying the econ view");
            match platform::resolve_assets_dir(assets_flag.as_deref())
                .ok_or_else(|| BoxErr::from("no assets dir"))
                .and_then(|dir| assets::load_econ_from_dir(&dir, "scg05eb.ini", credits))
            {
                Ok(g) => {
                    let mut core = g.core;
                    core.set_camera(
                        (g.start_cell.x * CELL as i32) as f32 - 430.0,
                        (g.start_cell.y * CELL as i32) as f32 - 360.0,
                    );
                    core
                }
                Err(e2) => {
                    eprintln!("note: econ view unavailable ({e2}); trying terrain+units");
                    match load_game(&mut args) {
                        Ok(g) => {
                            report_spawns(&g);
                            let start = center_camera_start(&g);
                            let mut core = g.core;
                            core.set_camera(start.0, start.1);
                            core
                        }
                        Err(e2) => {
                            eprintln!("note: falling back to terrain-only view ({e2})");
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
                    }
                }
            }
        }
    };
    // Decode the sound bank from the resolved assets dir (unless muted). Best
    // effort: an unresolved dir or missing sounds just yields an empty bank.
    let sounds = if muted {
        Vec::new()
    } else {
        platform::resolve_assets_dir(assets_flag.as_deref())
            .map(|dir| assets::load_sound_bank(&dir))
            .unwrap_or_default()
    };
    ra_client::shell::run_window(core, smoke, sounds);
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

    // M7: let the death explosion animate a few cosmetic frames and capture it
    // mid-blast, then toggle the F1 controls overlay and capture that too.
    for _ in 0..5 {
        core.update(67);
    }
    let explosion_frame = core.compose_camera();
    core.handle(InputEvent::KeyDown(Key::Help));
    core.handle(InputEvent::KeyUp(Key::Help));
    let f1_frame = core.compose_camera();

    if write_png {
        for (name, f) in [
            ("battle_before", &before),
            ("battle_mid", &mid),
            ("battle_after", &after),
            ("battle_explosion", &explosion_frame),
            ("battle_f1_overlay", &f1_frame),
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

// ===========================================================================
// M5 economy verification: deploy MCV -> build base -> harvest -> build tank.
// ===========================================================================

/// The `econ` subcommand: drive the full M5 economy loop through the AppCore
/// seam on a real ore-bearing scenario — deploy an MCV into a construction yard,
/// build & place POWR then PROC (which spawns a free harvester), watch the
/// harvester mine ore and credits rise, build & place a WEAP, then produce a
/// 2TNK. Dumps a PNG sequence, reports credit numbers vs hand-computed
/// expectations, and proves determinism by replaying the identical script and
/// comparing the per-tick sim-hash chains.
fn cmd_econ(mut args: Vec<String>) -> Result<(), BoxErr> {
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let credits: i32 = take_flag(&mut args, "--credits")
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--credits needs an integer")?
        .unwrap_or(8000);
    let assets_flag = take_flag(&mut args, "--assets");
    // scg05eb has a large temperate gold field with open ground near the centre
    // (reported by an overlay survey of general.mix); a good early econ map.
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!(
        "assets: {}  scenario: {scenario}  start credits: {credits}",
        dir.display()
    );

    let g1 = assets::load_econ_from_dir(&dir, &scenario, credits)?;
    let g2 = assets::load_econ_from_dir(&dir, &scenario, credits)?;
    eprintln!(
        "controlled house: {}  MCV start cell: ({},{})  nearest ore: {:?}",
        g1.controlled, g1.start_cell.x, g1.start_cell.y, g1.ore_cell
    );

    let (report, hashes1) = drive_econ(g1, &out_dir, true, credits)?;
    let (_r2, hashes2) = drive_econ(g2, &out_dir, false, credits)?;
    eprintln!("--- economy loop ---\n{report}");

    if hashes1 == hashes2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            hashes1.len(),
            hashes1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = hashes1
            .iter()
            .zip(&hashes2)
            .position(|(a, b)| a != b)
            .unwrap_or(hashes1.len());
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// Step the sim `n` virtual frames (~1 tick each), recording the per-tick hash.
fn econ_step(core: &mut AppCore, hashes: &mut Vec<u64>, n: u32) {
    for _ in 0..n {
        core.update(67);
        hashes.push(core.sim_hash());
    }
}

/// Step until `pred` holds or `max` ticks pass. Returns whether it held.
fn econ_wait<F: Fn(&AppCore) -> bool>(
    core: &mut AppCore,
    hashes: &mut Vec<u64>,
    max: u32,
    pred: F,
) -> bool {
    for _ in 0..max {
        if pred(core) {
            return true;
        }
        core.update(67);
        hashes.push(core.sim_hash());
    }
    pred(core)
}

/// The building type id inside a `BuildItem::Building`.
fn building_id(item: BuildItem) -> Option<u32> {
    match item {
        BuildItem::Building(id) => Some(id),
        _ => None,
    }
}

/// Find the sidebar item with the given short name.
fn sidebar_named(core: &AppCore, name: &str) -> Option<ra_client::appcore::SidebarItem> {
    core.sidebar_items().into_iter().find(|i| i.name == name)
}

/// Scan cells around the controlled house's construction yard for the first
/// footprint top-left where `building_id` is a legal placement.
fn find_placement(core: &AppCore, house: u8, id: u32) -> Option<CellCoord> {
    // Anchor on the construction yard, else the first owned building.
    let anchor = core
        .world()
        .buildings
        .iter()
        .find(|(_, b)| b.house == house && b.is_construction_yard)
        .or_else(|| {
            core.world()
                .buildings
                .iter()
                .find(|(_, b)| b.house == house)
        })
        .map(|(_, b)| b.cell)?;
    for r in 1..12 {
        for dy in -r..=r {
            for dx in -r..=r {
                let c = CellCoord::new(anchor.x + dx, anchor.y + dy);
                if core.world().can_place_building(house, id, c) {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// Build one structure end to end: start production, wait for it to finish, then
/// place it at a found cell. Returns the placement cell (or an error string).
fn build_structure(
    core: &mut AppCore,
    hashes: &mut Vec<u64>,
    house: u8,
    name: &str,
) -> Result<CellCoord, String> {
    let item = sidebar_named(core, name).ok_or_else(|| format!("{name} not in sidebar"))?;
    if !item.buildable {
        return Err(format!("{name} not buildable (prereqs/factory/funds)"));
    }
    core.start_production(item.item);
    // Wait for the ready flag.
    let ready = econ_wait(core, hashes, 4000, |c| {
        sidebar_named(c, name).map(|i| i.ready).unwrap_or(false)
    });
    if !ready {
        return Err(format!("{name} never completed"));
    }
    let id = building_id(item.item).ok_or("not a building")?;
    let cell = find_placement(core, house, id).ok_or_else(|| format!("no spot for {name}"))?;
    core.begin_placement(id);
    core.place_building(id, cell);
    econ_step(core, hashes, 2);
    // Confirm it landed.
    let placed = core
        .world()
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.cell == cell);
    if !placed {
        return Err(format!(
            "{name} placement rejected at ({},{})",
            cell.x, cell.y
        ));
    }
    Ok(cell)
}

/// Map a cell centre to a tactical viewport pixel at the current camera.
fn cell_to_screen(core: &AppCore, cell: CellCoord) -> (i32, i32) {
    let r = core.camera_rect();
    (
        (cell.x * CELL as i32 + CELL as i32 / 2 - r.x as i32),
        (cell.y * CELL as i32 + CELL as i32 / 2 - r.y as i32),
    )
}

/// Drive the whole economy loop; returns a report and the per-tick hash chain.
fn drive_econ(
    game: EconGame,
    out_dir: &str,
    write_png: bool,
    start_credits: i32,
) -> Result<(String, Vec<u64>), BoxErr> {
    let EconGame {
        mut core,
        controlled,
        start_cell,
        ..
    } = game;
    let mut hashes: Vec<u64> = Vec::new();
    let mut report = String::new();

    let (vw, vh) = (1000u32, 720u32);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    // Centre the camera on the base start (tactical area width excludes sidebar).
    let tw = core.tactical_width();
    core.set_camera(
        (start_cell.x * CELL as i32) as f32 - tw as f32 / 2.0,
        (start_cell.y * CELL as i32) as f32 - vh as f32 / 2.0,
    );

    let gold_value = core.world().catalog.econ.gold_value;
    let gem_value = core.world().catalog.econ.gem_value;
    let bail_count = core.world().catalog.econ.bail_count;

    // 1) Select the MCV (click it) and deploy it into a construction yard.
    let (mx, my) = cell_to_screen(&core, start_cell);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: mx,
        y: my,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: mx,
        y: my,
    });
    let selected = core.selected_handles().len();
    core.handle(InputEvent::KeyDown(Key::Deploy));
    core.handle(InputEvent::KeyUp(Key::Deploy));
    core.handle(InputEvent::MouseLeft); // stop any edge scroll
    econ_step(&mut core, &mut hashes, 3);
    let has_cy = core
        .world()
        .buildings
        .iter()
        .any(|(_, b)| b.house == controlled && b.is_construction_yard);
    report.push_str(&format!(
        "selected {selected} unit(s); deployed MCV -> construction yard: {has_cy}\n"
    ));
    if !has_cy {
        return Err("MCV failed to deploy".into());
    }
    let credits_after_deploy = core.credits();
    if write_png {
        dump(&core, out_dir, "econ_1_deployed")?;
    }

    // 2) Build + place POWR, then PROC (spawns the free harvester).
    let powr_cell = build_structure(&mut core, &mut hashes, controlled, "POWR")
        .map_err(|e| format!("POWR: {e}"))?;
    report.push_str(&format!(
        "built POWR at ({},{})\n",
        powr_cell.x, powr_cell.y
    ));
    let (po, pd) = core.power();
    report.push_str(&format!("power after POWR: output {po} / drain {pd}\n"));

    let proc_cell = build_structure(&mut core, &mut hashes, controlled, "PROC")
        .map_err(|e| format!("PROC: {e}"))?;
    let harvesters = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.is_harvester)
        .count();
    report.push_str(&format!(
        "built PROC at ({},{}); free harvesters now: {harvesters}\n",
        proc_cell.x, proc_cell.y
    ));
    let credits_before_harvest = core.credits();

    // 3) Let the harvester mine. Capture a frame with it on ore, then wait for a
    // full unload cycle (credits jump).
    let on_ore = econ_wait(&mut core, &mut hashes, 3000, |c| {
        c.world()
            .units
            .iter()
            .any(|(_, u)| u.is_harvester && c.world().ore.has_ore(u.cell()))
    });
    report.push_str(&format!("harvester reached an ore cell: {on_ore}\n"));
    if write_png {
        dump(&core, out_dir, "econ_2_harvesting")?;
    }
    // Wait for the first unload: credits strictly exceed the pre-harvest figure.
    let unloaded = econ_wait(&mut core, &mut hashes, 8000, |c| {
        c.credits() > credits_before_harvest
    });
    let credits_after_harvest = core.credits();
    let gained = credits_after_harvest - credits_before_harvest;
    report.push_str(&format!(
        "first unload happened: {unloaded}; credits {credits_before_harvest} -> {credits_after_harvest} (+{gained})\n"
    ));
    // Hand-check: a full gold load is bail_count * gold_value credits.
    report.push_str(&format!(
        "  expected full gold load = {bail_count} bails * {gold_value} = {} credits (gem bail = {gem_value})\n",
        bail_count as i32 * gold_value
    ));
    if gained > 0 && gained % gold_value == 0 {
        report.push_str(&format!(
            "  gained is an exact multiple of GoldValue: {} gold bails\n",
            gained / gold_value
        ));
    }

    // 4) Build + place WEAP, then produce a 2TNK; confirm the tank spawns.
    let weap_cell = build_structure(&mut core, &mut hashes, controlled, "WEAP")
        .map_err(|e| format!("WEAP: {e}"))?;
    report.push_str(&format!(
        "built WEAP at ({},{})\n",
        weap_cell.x, weap_cell.y
    ));

    let vehicles_before = core.world().units.len();
    let tnk = sidebar_named(&core, "2TNK").ok_or("2TNK not in sidebar")?;
    if !tnk.buildable {
        return Err("2TNK not buildable after WEAP".into());
    }
    let credits_before_tank = core.credits();
    core.start_production(tnk.item);
    let spawned = econ_wait(&mut core, &mut hashes, 4000, |c| {
        c.world().units.len() > vehicles_before
    });
    let credits_after_tank = core.credits();
    report.push_str(&format!(
        "2TNK produced: {spawned}; credits {credits_before_tank} -> {credits_after_tank} (cost {})\n",
        credits_before_tank - credits_after_tank
    ));
    econ_step(&mut core, &mut hashes, 20);
    if write_png {
        dump(&core, out_dir, "econ_3_tank")?;
    }

    // Credit ledger summary.
    report.push_str(&format!(
        "ledger: start {start_credits}  after-deploy {credits_after_deploy}  \
         before-harvest {credits_before_harvest}  after-harvest {credits_after_harvest}  \
         final {}\n",
        core.credits()
    ));
    if !spawned {
        return Err("2TNK never spawned".into());
    }

    Ok((report, hashes))
}

/// Compose the game view and write it as a PNG.
fn dump(core: &AppCore, out_dir: &str, name: &str) -> Result<(), BoxErr> {
    let f = core.compose_camera();
    let path = format!("{out_dir}/{name}.png");
    let bytes = png::encode_rgba(f.width, f.height, &f.pixels);
    std::fs::write(&path, &bytes)?;
    eprintln!("wrote {path} ({}x{} px)", f.width, f.height);
    Ok(())
}

// ===========================================================================
// M6 skirmish verification: player scripted build+attack vs a live AI, headless.
// ===========================================================================

/// The `skirmish` subcommand (M6 FIRST-PLAYABLE verification): boot a player-vs-AI
/// skirmish, script the player's economy + assault through the `AppCore` seam
/// while the AI plays inside the sim, run several thousand ticks headless, dump a
/// PNG timeline (early base / AI base discovered under shroud / battle /
/// victory-or-defeat overlay), report the outcome and that the AI actually built
/// a base + attacked (with tick numbers), and prove determinism by replaying the
/// identical script and comparing per-tick sim-hash chains.
fn cmd_skirmish(mut args: Vec<String>) -> Result<(), BoxErr> {
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let credits: i32 = take_flag(&mut args, "--credits")
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--credits needs an integer")?
        .unwrap_or(8000);
    let ticks: u32 = take_flag(&mut args, "--ticks")
        .map(|s| s.parse())
        .transpose()
        .map_err(|_| "--ticks needs an integer")?
        .unwrap_or(9000);
    let difficulty = match take_flag(&mut args, "--difficulty").as_deref() {
        Some("easy") => ra_sim::Difficulty::Easy,
        Some("hard") => ra_sim::Difficulty::Hard,
        _ => ra_sim::Difficulty::Normal,
    };
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scm01ea.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!(
        "assets: {}  scenario: {scenario}  difficulty: {difficulty:?}",
        dir.display()
    );

    let g1 = assets::load_skirmish_from_dir(&dir, &scenario, credits, difficulty)?;
    let g2 = assets::load_skirmish_from_dir(&dir, &scenario, credits, difficulty)?;
    eprintln!(
        "player house {} at ({},{})  vs  AI house {} at ({},{})",
        g1.player_house,
        g1.player_start.x,
        g1.player_start.y,
        g1.ai_house,
        g1.ai_start.x,
        g1.ai_start.y
    );

    let (report, hashes1) = drive_skirmish(g1, &out_dir, true, ticks)?;
    let (_r2, hashes2) = drive_skirmish(g2, &out_dir, false, ticks)?;
    eprintln!("--- skirmish ---\n{report}");

    if hashes1 == hashes2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            hashes1.len(),
            hashes1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = hashes1
            .iter()
            .zip(&hashes2)
            .position(|(a, b)| a != b)
            .unwrap_or(hashes1.len());
        Err(format!("determinism FAILED: skirmish hash chains diverge at tick {first}").into())
    }
}

/// Nearest live enemy (AI) building to a cell, prioritising its **production
/// core** (construction yard → war factory → refinery → anything) so the assault
/// knocks out the AI's ability to rebuild before mopping up. Ties broken by
/// distance. Returns the building's centre cell to right-click.
fn nearest_ai_building(core: &AppCore, ai_house: u8, from: CellCoord) -> Option<CellCoord> {
    let role_rank = |b: &ra_sim::Building| -> i64 {
        if b.is_construction_yard {
            0
        } else if b.is_war_factory {
            1
        } else if b.is_refinery {
            2
        } else {
            3
        }
    };
    let mut best: Option<(i64, i64, CellCoord)> = None; // (role, dist, cell)
    for (_, b) in core.world().buildings.iter() {
        if b.house == ai_house && b.is_alive() {
            let c = b.center_cell();
            let d = (c.x - from.x) as i64 * (c.x - from.x) as i64
                + (c.y - from.y) as i64 * (c.y - from.y) as i64;
            let rank = role_rank(b);
            if best.map(|(br, bd, _)| (rank, d) < (br, bd)).unwrap_or(true) {
                best = Some((rank, d, c));
            }
        }
    }
    best.map(|(_, _, c)| c)
}

/// Nearest live enemy (AI) unit cell to a cell, if any.
fn nearest_ai_unit(core: &AppCore, ai_house: u8, from: CellCoord) -> Option<CellCoord> {
    let mut best: Option<(i64, CellCoord)> = None;
    for (_, u) in core.world().units.iter() {
        if u.house == ai_house {
            let c = u.cell();
            let d = (c.x - from.x) as i64 * (c.x - from.x) as i64
                + (c.y - from.y) as i64 * (c.y - from.y) as i64;
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, c));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// Drive one skirmish end to end; returns a text report and the per-tick hash
/// chain. The player economy is scripted through the sidebar seam; the assault
/// selects the player's tanks and right-clicks the (scouted) AI base.
fn drive_skirmish(
    game: assets::SkirmishGame,
    out_dir: &str,
    write_png: bool,
    max_ticks: u32,
) -> Result<(String, Vec<u64>), BoxErr> {
    use ra_sim::GameOver;
    let assets::SkirmishGame {
        mut core,
        player_house,
        player_start,
        ai_house,
        ai_start,
    } = game;
    let mut hashes: Vec<u64> = Vec::new();
    let mut report = String::new();

    let (vw, vh) = (1000u32, 720u32);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    let recenter = |core: &mut AppCore, cell: CellCoord| {
        let tw = core.tactical_width();
        core.set_camera(
            (cell.x * CELL as i32) as f32 - tw as f32 / 2.0,
            (cell.y * CELL as i32) as f32 - vh as f32 / 2.0,
        );
    };
    recenter(&mut core, player_start);

    // 1) Deploy the player MCV.
    let (mx, my) = cell_to_screen(&core, player_start);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: mx,
        y: my,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: mx,
        y: my,
    });
    core.handle(InputEvent::KeyDown(Key::Deploy));
    core.handle(InputEvent::KeyUp(Key::Deploy));
    core.handle(InputEvent::MouseLeft);
    econ_step(&mut core, &mut hashes, 3);
    let has_cy = core
        .world()
        .buildings
        .iter()
        .any(|(_, b)| b.house == player_house && b.is_construction_yard);
    report.push_str(&format!(
        "player deployed MCV -> construction yard: {has_cy}\n"
    ));
    // Connectivity probe: can a ground unit path from the player start to open
    // ground beside the AI base? (The AI start cell itself is under its
    // construction yard, so probe a passable neighbour.)
    let probe = (1..6)
        .flat_map(|r| {
            [(r, 0), (0, r), (-r, 0), (0, -r), (r, r), (-r, -r)]
                .into_iter()
                .map(move |(dx, dy)| CellCoord::new(ai_start.x + dx, ai_start.y + dy))
        })
        .find(|&c| core.world().passability().is_passable(c));
    let connected = probe
        .and_then(|c| ra_sim::path::find_path(core.world().passability(), player_start, c))
        .is_some();
    report.push_str(&format!(
        "land route player<->AI area: {connected} (ground-reachable)\n"
    ));

    // 2) Player economy: POWR, PROC, WEAP.
    for name in ["POWR", "PROC", "WEAP"] {
        match build_structure(&mut core, &mut hashes, player_house, name) {
            Ok(cell) => {
                report.push_str(&format!("player built {name} at ({},{})\n", cell.x, cell.y))
            }
            Err(e) => report.push_str(&format!("player {name}: {e}\n")),
        }
    }

    // 3) Kick off the first tank, then dump the early-base frame.
    if let Some(t) = sidebar_named(&core, "2TNK") {
        if t.buildable {
            core.start_production(t.item);
        }
    }
    econ_step(&mut core, &mut hashes, 200);
    if write_png {
        recenter(&mut core, player_start);
        dump(&core, out_dir, "skirmish_1_early_base")?;
    }

    // 4) Continuous produce-and-assault: keep pumping 2TNKs (reinvesting
    // harvested credits) and, every ~90 ticks, throw the whole armed force at the
    // nearest AI structure — advancing across the map, revealing the shroud, and
    // grinding down the AI base. Observe the AI's own base-building + attacks.
    let mut ai_base_tick: Option<u32> = None; // AI placed a 2nd building
    let mut ai_attack_tick: Option<u32> = None; // an AI unit issued an attack
    let mut revealed_dump = false;
    let mut battle_dump = false;
    let mut peak_tanks = 0usize;
    let mut reissue_in = 0u32;
    // Stall tracking: if the AI building count stops dropping, the remnant is
    // either behind its own base or roaming units — hunt AI *units* instead so
    // the army finishes the job rather than fixating on an unreachable structure.
    let mut last_ai_buildings = usize::MAX;
    let mut stall = 0u32;
    // Send one early scout wave to reveal the AI base, then muster the main force.
    let mut scouted = false;

    while core.world().tick_count() < max_ticks && core.game_over() == GameOver::Ongoing {
        // Keep the war factory busy whenever the lane is free and we can afford it.
        if let Some(t) = sidebar_named(&core, "2TNK") {
            if t.buildable {
                core.start_production(t.item);
            }
        }

        // Re-order the whole armed force at the AI every ~90 ticks.
        if reissue_in == 0 {
            reissue_in = 90;
            let ai_bcount = core
                .world()
                .buildings
                .iter()
                .filter(|(_, b)| b.house == ai_house)
                .count();
            if ai_bcount < last_ai_buildings {
                stall = 0;
            } else {
                stall += 1;
            }
            last_ai_buildings = ai_bcount;
            let force: Vec<_> = core
                .world()
                .units
                .handles()
                .into_iter()
                .filter(|&h| {
                    core.world()
                        .units
                        .get(h)
                        .map(|u| u.house == player_house && u.weapon.is_some() && !u.is_harvester)
                        .unwrap_or(false)
                })
                .collect();
            peak_tanks = peak_tanks.max(force.len());
            // Muster a concentrated column before assaulting: feeding tanks in
            // one at a time just trades them into the AI's base defences. Wait
            // until we have a real fist, then commit (and keep committing).
            const MUSTER: usize = 12;
            // Commit once the main column is mustered — but always send the very
            // first wave forward to scout (reveal the AI base under the shroud).
            let committed = peak_tanks >= MUSTER || !scouted;
            if !force.is_empty() && committed {
                scouted = true;
                // Aim at the AI structure/unit nearest the leading tank. When
                // building destruction stalls, hunt AI units first.
                let anchor = core
                    .world()
                    .units
                    .get(force[0])
                    .map(|u| u.cell())
                    .unwrap_or(player_start);
                let hunt_units = stall >= 4;
                let target = if hunt_units {
                    nearest_ai_unit(&core, ai_house, anchor)
                        .or_else(|| nearest_ai_building(&core, ai_house, anchor))
                } else {
                    nearest_ai_building(&core, ai_house, anchor)
                        .or_else(|| nearest_ai_unit(&core, ai_house, anchor))
                };
                if let Some(target) = target {
                    core.select_units(&force);
                    recenter(&mut core, target);
                    let (tx, ty) = cell_to_screen(&core, target);
                    core.handle(InputEvent::MouseDown {
                        button: MouseButton::Right,
                        x: tx,
                        y: ty,
                    });
                    core.handle(InputEvent::MouseLeft);
                    core.drain_commands();
                }
            }
        }
        econ_step(&mut core, &mut hashes, 1);
        reissue_in -= 1;

        let t = core.world().tick_count();
        // AI milestones.
        if ai_base_tick.is_none() {
            let n = core
                .world()
                .buildings
                .iter()
                .filter(|(_, b)| b.house == ai_house)
                .count();
            if n >= 2 {
                ai_base_tick = Some(t);
            }
        }
        if ai_attack_tick.is_none() {
            let attacking = core
                .world()
                .units
                .iter()
                .any(|(_, u)| u.house == ai_house && u.weapon.is_some() && u.has_target());
            if attacking {
                ai_attack_tick = Some(t);
            }
        }
        // PNG: AI base discovered — once the player has explored the cell of ANY
        // AI building (a scout tank pierced the shroud around the enemy base).
        if write_png && !revealed_dump {
            let revealed = core.world().buildings.iter().find_map(|(_, b)| {
                let c = b.center_cell();
                (b.house == ai_house && core.world().shroud.is_explored(player_house, c))
                    .then_some(c)
            });
            if let Some(bcell) = revealed {
                recenter(&mut core, bcell);
                dump(&core, out_dir, "skirmish_2_ai_base_revealed")?;
                revealed_dump = true;
            }
        }
        // PNG: a battle frame once bullets are flying.
        if write_png && !battle_dump && !core.world().bullets.is_empty() {
            // Frame the fight (near the first bullet).
            let cell = core
                .world()
                .bullets
                .iter()
                .next()
                .map(|(_, b)| b.pos.cell());
            if let Some(cell) = cell {
                recenter(&mut core, cell);
            }
            dump(&core, out_dir, "skirmish_3_battle")?;
            battle_dump = true;
        }
    }
    report.push_str(&format!("player peak armed force: {peak_tanks} tank(s)\n"));

    let outcome = core.game_over();
    if write_png {
        // Frame the losing/winning base for the result shot.
        let focus = nearest_ai_building(&core, ai_house, player_start).unwrap_or(player_start);
        recenter(&mut core, focus);
        dump(&core, out_dir, "skirmish_4_result")?;
    }

    let final_tick = core.world().tick_count();
    let ai_buildings = core
        .world()
        .buildings
        .iter()
        .filter(|(_, b)| b.house == ai_house)
        .count();
    let player_buildings = core
        .world()
        .buildings
        .iter()
        .filter(|(_, b)| b.house == player_house)
        .count();
    let ai_units = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.house == ai_house)
        .count();
    let player_units = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.house == player_house)
        .count();
    report.push_str(&format!(
        "AI built a base (2nd building) at tick: {:?}\n\
         AI first issued an attack at tick: {:?}\n\
         final tick {final_tick}: outcome {:?}\n\
         player: {player_buildings} building(s), {player_units} unit(s)  |  \
         AI: {ai_buildings} building(s), {ai_units} unit(s)\n\
         AI start ({},{})",
        ai_base_tick, ai_attack_tick, outcome, ai_start.x, ai_start.y
    ));

    Ok((report, hashes))
}
