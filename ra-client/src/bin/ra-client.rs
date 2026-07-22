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
        "replay-verify" => cmd_replay_verify(args),
        "replay-dump" => cmd_replay_dump(args),
        "verify-m76" => cmd_verify_m76(args),
        "verify-m77" => cmd_verify_m77(args),
        "verify-m77b" => cmd_verify_m77b(args),
        "verify-m77c" => cmd_verify_m77c(args),
        "verify-m78" => cmd_verify_m78(args),
        "verify-terrain" => cmd_verify_terrain(args),
        _ => {
            eprintln!(
                "usage:\n  ra-client dump     [--assets DIR] [--scenario NAME] [--out PATH.png] [--rect CX CY CW CH] [--playable]\n  ra-client window   [--assets DIR] [--scenario NAME] [--smoke-seconds N] [--replay FILE.rarp]\n  ra-client sim      [--assets DIR] [--scenario NAME] [--out-dir DIR]\n  ra-client battle   [--assets DIR] [--scenario NAME] [--out-dir DIR]\n  ra-client econ     [--assets DIR] [--scenario NAME] [--out-dir DIR] [--credits N]\n  ra-client skirmish [--assets DIR] [--scenario NAME] [--out-dir DIR] [--credits N] [--difficulty easy|normal|hard] [--ticks N] [--record FILE.rarp]\n  ra-client replay-verify <FILE.rarp> [--assets DIR]\n  ra-client replay-dump   <FILE.rarp> [--at-tick N] [--assets DIR]"
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
    // Audio off flag (M7): `--mute` boots with an empty sound bank.
    let muted = has_flag(&mut args, "--mute");
    let replay_flag = take_flag(&mut args, "--replay");
    let assets_flag = take_flag(&mut args, "--assets");
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}", dir.display());

    // M7.23 P3: watchable playback. `--replay FILE` reconstructs the world and
    // drives it from a ReplayTransport (recorded bundles on schedule); live
    // input is ignored except the window's quit key.
    if let Some(replay_path) = replay_flag {
        let core = build_replay_playback(&replay_path, assets_flag.as_deref())?;
        let sounds = if muted {
            Vec::new()
        } else {
            assets::load_sound_bank(&dir)
        };
        eprintln!("playing replay: {replay_path}");
        ra_client::shell::run_window(core, smoke, sounds);
        return Ok(());
    }

    // M7.8: boot the main-menu state machine. `load_menu` scans the archive's
    // multiplayer maps + the user maps folder and returns a factory.
    let (maps, factory) = assets::load_menu(&dir)?;
    eprintln!(
        "menu: {} map(s) scanned (user maps dir: {:?})",
        maps.len(),
        platform::user_maps_dir()
    );
    // LAN player name: `--name NAME`, else $USER, else a default (M8-B).
    let player_name = take_flag(&mut args, "--name")
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "COMMANDER".to_string());

    let mut app = ra_client::menu::App::new(maps, Box::new(factory));
    // M7.23 P1: always-on recording for every interactive game started from the
    // windowed client. Replays land in a `replays/` dir beside the assets dir.
    app.enable_recording(replays_dir(&dir));
    // Enable the single-player Allied campaign (scg*ea.ini) if the archives are
    // readable — the factory scans/builds missions on demand — and the LAN
    // multiplayer flow (M8-B), which needs its own archive-backed factory.
    let app = match (
        std::fs::read(dir.join("main.mix")),
        std::fs::read(dir.join("redalert.mix")),
    ) {
        (Ok(m), Ok(r)) => {
            let lan_factory =
                assets::ArchiveFactory::new(m.clone(), r.clone(), platform::user_maps_dir());
            app.with_campaign(Box::new(assets::ArchiveCampaignFactory::new(m, r)))
                .with_lan(
                    Box::new(lan_factory),
                    ra_net::DiscoveryConfig::default(),
                    &player_name,
                )
        }
        _ => app,
    };

    let sounds = if muted {
        Vec::new()
    } else {
        assets::load_sound_bank(&dir)
    };
    ra_client::shell::run_window_app(app, smoke, sounds);
    Ok(())
}

#[cfg(not(feature = "window"))]
fn cmd_window(_args: Vec<String>) -> Result<(), BoxErr> {
    Err(
        "this build was compiled without the `window` feature; rebuild with default features"
            .into(),
    )
}

/// The replays directory beside the assets dir (`<assets>/../replays`), created
/// lazily by the recorder. Falls back to `<assets>/replays` if the assets dir
/// has no parent.
#[cfg(feature = "window")]
fn replays_dir(assets: &std::path::Path) -> std::path::PathBuf {
    match assets.parent() {
        Some(p) => p.join("replays"),
        None => assets.join("replays"),
    }
}

/// Reconstruct a playable core from a replay file and install a
/// [`ra_net::ReplayTransport`] so the windowed shell replays it (M7.23 P3).
#[cfg(feature = "window")]
fn build_replay_playback(path: &str, assets_flag: Option<&str>) -> Result<AppCore, BoxErr> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?;
    let (header, reader) =
        ra_net::ReplayReader::open(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let transport = ra_net::ReplayTransport::from_reader(&header, reader)
        .map_err(|e| format!("{path}: {e}"))?;
    let game = reload_for_replay(&header, assets_flag)?;
    let mut core = game.core;
    // Win/lose read follows the single-player convention during playback.
    core.install_replay(transport, None);
    Ok(core)
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
    let record_path = take_flag(&mut args, "--record");
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

    // M7.23 P1: optional always-on recording of the first (PNG-writing) game.
    let recorder = record_path.as_deref().map(|p| {
        let header = replay_header_for(
            &g1,
            &scenario,
            credits,
            difficulty_to_u8(difficulty),
            now_millis(),
        );
        eprintln!("recording replay -> {p}");
        ra_client::replay::ReplayRecorder::create(std::path::PathBuf::from(p), &header)
    });

    let (report, hashes1) = drive_skirmish(g1, &out_dir, true, ticks, recorder)?;
    let (_r2, hashes2) = drive_skirmish(g2, &out_dir, false, ticks, None)?;
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

// ===========================================================================
// M7.23: replay recording helpers + post-mortem CLI (replay-verify / -dump).
// ===========================================================================

/// Unix-epoch milliseconds (shell layer only — the sim/net core never reads a
/// clock, §4.2). `0` if the clock is before the epoch (never, in practice).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Difficulty → the header's `u8` code (Easy=0, Normal=1, Hard=2).
fn difficulty_to_u8(d: ra_sim::Difficulty) -> u8 {
    match d {
        ra_sim::Difficulty::Easy => 0,
        ra_sim::Difficulty::Normal => 1,
        ra_sim::Difficulty::Hard => 2,
    }
}

/// The header `u8` code → Difficulty (inverse of [`difficulty_to_u8`]).
fn difficulty_from_u8(d: u8) -> ra_sim::Difficulty {
    match d {
        0 => ra_sim::Difficulty::Easy,
        2 => ra_sim::Difficulty::Hard,
        _ => ra_sim::Difficulty::Normal,
    }
}

/// Build a replay header for a loaded skirmish: versions + scenario + seed +
/// settings + catalog content-hash (asset-drift flag) + the caller's timestamp.
fn replay_header_for(
    game: &assets::SkirmishGame,
    scenario: &str,
    credits: i32,
    difficulty: u8,
    start_millis: u64,
) -> ra_net::ReplayHeader {
    let w = game.core.world();
    ra_net::ReplayHeader {
        replay_version: ra_net::REPLAY_VERSION,
        game_version: ra_net::wire::GAME_VERSION,
        protocol_version: ra_net::wire::PROTOCOL_VERSION,
        scenario: scenario.to_string(),
        seed: w.rng_seed(),
        difficulty,
        credits,
        catalog_hash: w.catalog().content_hash(),
        start_millis,
        seats: vec![
            ra_net::ReplaySeat {
                seat: game.player_house,
                house: game.player_house,
                color: game.player_house,
            },
            ra_net::ReplaySeat {
                seat: game.ai_house,
                house: game.ai_house,
                color: game.ai_house,
            },
        ],
    }
}

/// Read a replay file, parse the header, and collect its records — the shared
/// front end for `replay-verify` and `replay-dump`.
fn load_replay(path: &str) -> Result<(ra_net::ReplayHeader, Vec<ra_net::ReplayRecord>), BoxErr> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))?;
    let (header, reader) =
        ra_net::ReplayReader::open(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let records = reader
        .collect_records()
        .map_err(|e| format!("{path}: {e}"))?;
    Ok((header, records))
}

/// Rebuild the initial skirmish world a replay was recorded against, from its
/// header (scenario + credits + difficulty; the loader's seed is deterministic).
/// Warns on catalog/seed drift but proceeds — the hash chain is the real check.
fn reload_for_replay(
    header: &ra_net::ReplayHeader,
    assets_flag: Option<&str>,
) -> Result<assets::SkirmishGame, BoxErr> {
    let dir = platform::resolve_assets_dir(assets_flag)
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    let game = assets::load_skirmish_from_dir(
        &dir,
        &header.scenario,
        header.credits,
        difficulty_from_u8(header.difficulty),
    )?;
    let w = game.core.world();
    if w.rng_seed() != header.seed {
        eprintln!(
            "warning: reloaded seed {:#010x} != recorded {:#010x} (replay may diverge)",
            w.rng_seed(),
            header.seed
        );
    }
    if w.catalog().content_hash() != header.catalog_hash {
        eprintln!(
            "warning: catalog content-hash differs from the recording — assets may have drifted"
        );
    }
    Ok(game)
}

/// `replay-verify <file>`: re-simulate the recorded game and check every hash
/// record. Prints PASS, or FAIL with the first divergent tick.
fn cmd_replay_verify(mut args: Vec<String>) -> Result<(), BoxErr> {
    let assets_flag = take_flag(&mut args, "--assets");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or("usage: ra-client replay-verify <file.rarp> [--assets DIR]")?;

    let (header, records) = load_replay(&path)?;
    eprintln!(
        "replay: scenario={} seed={:#010x} difficulty={} credits={} seats={}",
        header.scenario,
        header.seed,
        header.difficulty,
        header.credits,
        header.seats.len()
    );

    // Index the stream: non-empty tick bundles + the hash chain + the end tick.
    let mut bundles: std::collections::BTreeMap<u32, ra_net::TickBundle> =
        std::collections::BTreeMap::new();
    let mut hash_records: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut final_tick = 0u32;
    let mut end_reason = None;
    for rec in &records {
        match rec {
            ra_net::ReplayRecord::Tick { tick, bundle } => {
                final_tick = final_tick.max(*tick);
                bundles.insert(*tick, bundle.clone());
            }
            ra_net::ReplayRecord::Hash { tick, hash } => {
                final_tick = final_tick.max(*tick);
                hash_records.insert(*tick, *hash);
            }
            ra_net::ReplayRecord::End {
                reason,
                final_tick: ft,
            } => {
                final_tick = final_tick.max(*ft);
                end_reason = Some(*reason);
            }
        }
    }
    if hash_records.is_empty() {
        return Err("replay has no hash records — nothing to verify (corrupt or empty)".into());
    }

    let mut game = reload_for_replay(&header, assets_flag.as_deref())?;

    // Re-simulate tick by tick, feeding the recorded bundles (empty otherwise),
    // and check each hash record as we pass its tick.
    let mut checked = 0usize;
    for t in 0..=final_tick {
        let cmds = match bundles.get(&t) {
            Some(b) => b.flatten(),
            None => Vec::new(),
        };
        let hash = game.core.world_mut().tick(&cmds);
        if let Some(&expected) = hash_records.get(&t) {
            if hash != expected {
                println!(
                    "FAIL: replay diverged at tick {t}: recorded {expected:#018x}, re-sim {hash:#018x} \
                     (checked {checked} earlier hash records OK)"
                );
                return Err(format!("replay verification failed at tick {t}").into());
            }
            checked += 1;
        }
    }

    println!(
        "PASS: {checked} hash record(s) matched over {final_tick} ticks; end={:?}",
        end_reason.unwrap_or(ra_net::EndReason::Quit)
    );
    Ok(())
}

/// `replay-dump <file> [--at-tick N]`: re-simulate to N (default: end) and print
/// a structured world report — the tool that answers "why didn't the game end".
fn cmd_replay_dump(mut args: Vec<String>) -> Result<(), BoxErr> {
    let assets_flag = take_flag(&mut args, "--assets");
    let at_tick = take_flag(&mut args, "--at-tick")
        .map(|s| s.parse::<u32>())
        .transpose()
        .map_err(|_| "--at-tick needs an integer")?;
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or("usage: ra-client replay-dump <file.rarp> [--at-tick N] [--assets DIR]")?;

    let (header, records) = load_replay(&path)?;
    let mut bundles: std::collections::BTreeMap<u32, ra_net::TickBundle> =
        std::collections::BTreeMap::new();
    let mut recorded_end = 0u32;
    let mut end_reason = None;
    for rec in &records {
        match rec {
            ra_net::ReplayRecord::Tick { tick, bundle } => {
                recorded_end = recorded_end.max(*tick);
                bundles.insert(*tick, bundle.clone());
            }
            ra_net::ReplayRecord::Hash { tick, .. } => recorded_end = recorded_end.max(*tick),
            ra_net::ReplayRecord::End { final_tick, reason } => {
                recorded_end = recorded_end.max(*final_tick);
                end_reason = Some(*reason);
            }
        }
    }
    // Default to the recorded end; a requested tick beyond it is clamped (there
    // are no recorded commands past the end to re-simulate faithfully).
    let target = at_tick.unwrap_or(recorded_end).min(recorded_end);

    let mut game = reload_for_replay(&header, assets_flag.as_deref())?;
    for t in 0..target {
        let cmds = bundles.get(&t).map(|b| b.flatten()).unwrap_or_default();
        game.core.world_mut().tick(&cmds);
    }

    let report = render_world_report(&game.core, &header, target, recorded_end, end_reason);
    print!("{report}");
    Ok(())
}

/// Render the structured post-mortem: per house (alive?, credits, units with
/// type/pos/health, buildings), plus the game-over state.
fn render_world_report(
    core: &AppCore,
    header: &ra_net::ReplayHeader,
    at_tick: u32,
    recorded_end: u32,
    end_reason: Option<ra_net::EndReason>,
) -> String {
    use std::fmt::Write as _;
    let w = core.world();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== replay-dump: {} @ tick {at_tick} (recorded end tick {recorded_end}, reason {:?}) ===",
        header.scenario, end_reason
    );
    let _ = writeln!(out, "game_over: {:?}", w.game_over());

    // Houses that appear in the header, plus any that own live objects.
    let mut houses: Vec<u8> = header.seats.iter().map(|s| s.house).collect();
    for (_, u) in w.units.iter() {
        if !houses.contains(&u.house) {
            houses.push(u.house);
        }
    }
    for (_, b) in w.buildings.iter() {
        if !houses.contains(&b.house) {
            houses.push(b.house);
        }
    }
    houses.sort_unstable();
    houses.dedup();

    for h in houses {
        let units: Vec<_> = w.units.iter().filter(|(_, u)| u.house == h).collect();
        let buildings: Vec<_> = w
            .buildings
            .iter()
            .filter(|(_, b)| b.house == h && b.is_alive())
            .collect();
        let alive = !units.is_empty() || !buildings.is_empty();
        let credits = w.house(h).map(|hs| hs.available()).unwrap_or(0);
        let _ = writeln!(
            out,
            "\n-- house {h}: alive={alive}  credits={credits}  units={}  buildings={}",
            units.len(),
            buildings.len()
        );
        for (_, u) in &units {
            let name = w
                .catalog()
                .unit(u.type_id)
                .map(|p| p.name.as_str())
                .unwrap_or("?");
            let c = u.cell();
            let _ = writeln!(
                out,
                "   unit  {name:<6} id={:<3} cell=({:>3},{:>3}) hp={}",
                u.type_id, c.x, c.y, u.health
            );
        }
        for (_, b) in &buildings {
            let name = w
                .catalog()
                .building(b.type_id)
                .map(|p| p.name.as_str())
                .unwrap_or("?");
            let _ = writeln!(
                out,
                "   bldg  {name:<6} id={:<3} cell=({:>3},{:>3}) hp={}",
                b.type_id, b.cell.x, b.cell.y, b.health
            );
        }
    }
    out
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
    recorder: Option<ra_client::replay::ReplayRecorder>,
) -> Result<(String, Vec<u64>), BoxErr> {
    use ra_sim::GameOver;
    let assets::SkirmishGame {
        mut core,
        player_house,
        player_start,
        ai_house,
        ai_start,
    } = game;
    // M7.23 P1: install the always-on recorder before the first tick so it taps
    // the entire game (the scripted player commands cross the same transport the
    // recorder observes; the AI runs in-sim and re-derives on replay).
    if let Some(rec) = recorder {
        core.install_recorder(rec);
    }
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
        .and_then(|c| {
            ra_sim::path::find_path(
                core.world().passability(),
                player_start,
                c,
                ra_sim::coords::Locomotor::Track,
            )
        })
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

    // Finalize the replay. A victory/defeat during the drive already wrote the
    // end record (finish is idempotent); otherwise this closes the stream as a
    // clean Quit so the file is never left open-ended.
    core.finish_recording(ra_net::EndReason::Quit);

    Ok((report, hashes))
}

// ===========================================================================
// M7.6 infantry + barracks verification (scripted, real assets).
// ===========================================================================

// Fixed catalog ids (mirror ra_client::assets::build_content declaration order).
const V_FACT: u32 = 0;
const V_POWR: u32 = 1;
const V_TENT: u32 = 4;
const V_JEEP: u32 = 4;
const V_E1: u32 = 5;
const V_E2: u32 = 6;
const V_E3: u32 = 7;

/// Scripted end-to-end verification of infantry + barracks (M7.6): place a
/// barracks, produce an E1/E2/E3 squad, pack it into one cell (5-per-cell sub-cell
/// spots), have it attack a JEEP and a building, have the JEEP kill infantry
/// (Armor=none), and dump PNG evidence. Runs the whole script twice and asserts
/// identical hash chains.
fn cmd_verify_m76(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_sim::{Command, Target};
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());

    let run = |dump: bool| -> Result<(String, Vec<u64>), BoxErr> {
        let game = assets::load_econ_from_dir(&dir, &scenario, 20000)?;
        let mut core = game.core;
        let house = game.controlled;
        let enemy: u8 = if house == 2 { 3 } else { 2 };
        let base = game.start_cell;
        let mut hashes: Vec<u64> = Vec::new();
        let mut report = String::new();

        // --- Setup: stamp a base (yard/power/barracks) for the player house and
        // an enemy structure, directly (the loader path, no build UI needed). ---
        {
            let w = core.world_mut();
            let yard = CellCoord::new(base.x, base.y);
            w.spawn_building(V_FACT, house, yard);
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 4, base.y));
            w.spawn_building(V_TENT, house, CellCoord::new(base.x, base.y + 5));
            // Enemy power plant a few cells away — an infantry attack target.
            w.spawn_building(V_POWR, enemy, CellCoord::new(base.x + 10, base.y + 6));
        }
        report.push_str(&format!(
            "player {house} base at ({},{}); barracks at ({},{})\n",
            base.x,
            base.y,
            base.x,
            base.y + 5
        ));

        // --- Produce an E1/E2/E3 squad through the barracks strip. ---
        let squad_types = [V_E1, V_E1, V_E3, V_E2, V_E2];
        let mut squad: Vec<ra_sim::Handle> = Vec::new();
        for &ut in &squad_types {
            match produce_one_infantry(&mut core, house, ut, &mut hashes) {
                Some(h) => squad.push(h),
                None => return Err(format!("infantry proto {ut} never spawned").into()),
            }
        }
        report.push_str(&format!(
            "produced {} infantry via TENT strip\n",
            squad.len()
        ));

        // --- Pack the squad into ONE cell — verify distinct sub-cell spots. ---
        let pack_cell = CellCoord::new(base.x + 6, base.y + 10);
        for &h in &squad {
            core.inject_command(Command::Move {
                unit: h,
                dest: pack_cell,
                house,
            });
        }
        // Step until they settle (paths empty), up to a budget.
        for _ in 0..600 {
            core.update(67);
            hashes.push(core.sim_hash());
            if squad.iter().all(|&h| {
                core.world()
                    .units
                    .get(h)
                    .map(|u| !u.is_moving())
                    .unwrap_or(true)
            }) {
                break;
            }
        }
        // Report the cells + spots the squad settled into.
        let mut packed_cells = std::collections::BTreeMap::<i64, Vec<u8>>::new();
        for &h in &squad {
            if let Some(u) = core.world().units.get(h) {
                let c = u.cell();
                packed_cells
                    .entry((c.y as i64) * 1000 + c.x as i64)
                    .or_default()
                    .push(u.sub_cell);
            }
        }
        let max_per_cell = packed_cells.values().map(|v| v.len()).max().unwrap_or(0);
        report.push_str(&format!(
            "squad packed into {} cell(s), up to {} infantry/cell, spots: {:?}\n",
            packed_cells.len(),
            max_per_cell,
            packed_cells.values().collect::<Vec<_>>()
        ));
        // Frame camera on the squad and dump the "packed" PNG.
        center_camera_on(&mut core, pack_cell);
        if dump {
            dump_game_png(&mut core, &out_dir, "m76_squad_packed.png")?;
        }

        // --- Spawn an enemy JEEP; squad attacks it and the enemy building; the
        // JEEP fires back and kills infantry (Armor=none takes full damage). ---
        let jeep_cell = CellCoord::new(pack_cell.x + 3, pack_cell.y);
        let jeep = spawn_combat_unit(&mut core, V_JEEP, enemy, jeep_cell);
        let enemy_bldg = core
            .world()
            .buildings
            .iter()
            .find(|(_, b)| b.house == enemy)
            .map(|(h, _)| h);
        // First two infantry target the building, the rest the JEEP.
        for (i, &h) in squad.iter().enumerate() {
            let target = if i < 2 {
                enemy_bldg
                    .map(Target::Building)
                    .unwrap_or(Target::Unit(jeep))
            } else {
                Target::Unit(jeep)
            };
            core.inject_command(Command::Attack {
                unit: h,
                target,
                house,
            });
        }
        // The JEEP fires on the nearest infantryman.
        if let Some(&first) = squad.first() {
            core.inject_command(Command::Attack {
                unit: jeep,
                target: Target::Unit(first),
                house: enemy,
            });
        }
        // Mid-fight snapshot after a few ticks.
        for _ in 0..25 {
            core.update(67);
            hashes.push(core.sim_hash());
        }
        if dump {
            dump_game_png(&mut core, &out_dir, "m76_midfight.png")?;
        }
        // Resolve the skirmish.
        let mut jeep_dead_tick = None;
        for _ in 0..400 {
            core.update(67);
            hashes.push(core.sim_hash());
            if jeep_dead_tick.is_none() && !core.world().units.contains(jeep) {
                jeep_dead_tick = Some(core.world().tick_count());
            }
        }
        let alive_inf = squad
            .iter()
            .filter(|&&h| core.world().units.contains(h))
            .count();
        let jeep_alive = core.world().units.contains(jeep);
        let bldg_alive = enemy_bldg
            .map(|h| core.world().buildings.get(h).is_some_and(|b| b.is_alive()))
            .unwrap_or(false);
        report.push_str(&format!(
            "after fight: {alive_inf}/{} infantry alive, JEEP alive={jeep_alive} (died tick {:?}), \
             enemy building alive={bldg_alive}\n",
            squad.len(),
            jeep_dead_tick
        ));
        if dump {
            dump_game_png(&mut core, &out_dir, "m76_aftermath.png")?;
        }
        Ok((report, hashes))
    };

    let (report, h1) = run(true)?;
    let (_r2, h2) = run(false)?;
    eprintln!("--- M7.6 infantry verification ---\n{report}");
    if h1 == h2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            h1.len(),
            h1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = h1.iter().zip(&h2).position(|(a, b)| a != b).unwrap_or(0);
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// M7.7 Chunk A showcase: the two-strip sidebar, an artillery arc mid-flight, a
/// mammoth (4TNK) switching weapons vs. a tank then infantry, and a
/// same-script-twice determinism check. Dumps PNG evidence and prints
/// hand-checkable stat lines. `V_*` unit ids match the loader's declaration order
/// (`assets.rs`): 3TNK=8, 4TNK=9, ARTY=10, V2RL=11, APC=12, TRUK=13, MNLY=14.
fn cmd_verify_m77(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_sim::{Command, Target};
    const V_WEAP: u32 = 3;
    const U_2TNK: u32 = 3;
    const U_3TNK: u32 = 8;
    const U_4TNK: u32 = 9;
    const U_ARTY: u32 = 10;
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());

    let run = |dump: bool| -> Result<(String, Vec<u64>), BoxErr> {
        let game = assets::load_econ_from_dir(&dir, &scenario, 20000)?;
        let mut core = game.core;
        let house = game.controlled;
        let enemy: u8 = if house == 2 { 3 } else { 2 };
        let base = game.start_cell;
        let mut hashes: Vec<u64> = Vec::new();
        let mut report = String::new();

        // Stamp a war factory so the sidebar's vehicle strip is fully buildable.
        {
            let w = core.world_mut();
            w.spawn_building(V_FACT, house, CellCoord::new(base.x, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 4, base.y));
            w.spawn_building(V_WEAP, house, CellCoord::new(base.x, base.y + 4));
        }

        // --- Scene C: the two-strip sidebar with the full roster. Scroll the
        // units column to prove the scroll works, then dump. ---
        core.handle(InputEvent::SidebarScroll {
            column: 1,
            up: false,
        });
        core.handle(InputEvent::SidebarScroll {
            column: 1,
            up: false,
        });
        center_camera_on(&mut core, base);
        if dump {
            dump_game_png(&mut core, &out_dir, "m77_sidebar_two_strip.png")?;
        }
        report.push_str(&format!(
            "sidebar: {} buildables; units-column scroll offset = {}\n",
            core.sidebar_items().len(),
            core.sidebar_scroll(1)
        ));

        // --- Scene A: ARTY lobs an arcing 155mm shell at a distant target. ---
        let arty_cell = CellCoord::new(base.x + 2, base.y + 12);
        let arty = spawn_combat_unit(&mut core, U_ARTY, house, arty_cell);
        let arty_target_cell = CellCoord::new(arty_cell.x + 6, arty_cell.y); // ~6 cells = in range
        let arty_victim = spawn_combat_unit(&mut core, U_2TNK, enemy, arty_target_cell);
        core.inject_command(Command::Attack {
            unit: arty,
            target: Target::Unit(arty_victim),
            house,
        });
        // Step until an arcing shell is airborne at a clearly-arced height.
        let mut peak_h = 0i32;
        let mut arc_dumped = false;
        for _ in 0..200 {
            core.update(67);
            hashes.push(core.sim_hash());
            let airborne: Option<i32> = core
                .world()
                .bullets
                .iter()
                .filter(|(_, b)| b.arcing)
                .map(|(_, b)| b.height)
                .max();
            if let Some(h) = airborne {
                peak_h = peak_h.max(h);
                if h > 96 && !arc_dumped && dump {
                    let mid = CellCoord::new((arty_cell.x + arty_target_cell.x) / 2, arty_cell.y);
                    center_camera_on(&mut core, mid);
                    dump_game_png(&mut core, &out_dir, "m77_arty_arc_midflight.png")?;
                    arc_dumped = true;
                }
            }
        }
        report.push_str(&format!(
            "ARTY 155mm arcing shell: peak height {peak_h} leptons ({} px) mid-flight\n",
            peak_h / (256 / CELL as i32).max(1)
        ));

        // --- Scene B: 4TNK (mammoth) fires its 120mm cannon at a heavy tank, then
        // its MammothTusk missiles at infantry — the weapon switch is visible in
        // the spawned bullet's Damage (120mm=40 vs MammothTusk=75). ---
        let mammoth_cell = CellCoord::new(base.x + 12, base.y + 2);
        let mammoth = spawn_combat_unit(&mut core, U_4TNK, house, mammoth_cell);
        // A heavy-armor enemy tank right next to it.
        let etank = spawn_combat_unit(
            &mut core,
            U_3TNK,
            enemy,
            CellCoord::new(mammoth_cell.x + 2, mammoth_cell.y),
        );
        let first_bullet_damage = |core: &AppCore| -> Option<i32> {
            core.world()
                .bullets
                .iter()
                .find(|(_, b)| b.source_unit == mammoth)
                .map(|(_, b)| b.damage)
        };
        core.inject_command(Command::Attack {
            unit: mammoth,
            target: Target::Unit(etank),
            house,
        });
        let mut vs_tank_dmg = None;
        for _ in 0..60 {
            core.update(67);
            hashes.push(core.sim_hash());
            if let Some(d) = first_bullet_damage(&core) {
                vs_tank_dmg = Some(d);
                break;
            }
        }
        if dump {
            center_camera_on(&mut core, mammoth_cell);
            dump_game_png(&mut core, &out_dir, "m77_4tnk_vs_tank.png")?;
        }
        // Now retarget the mammoth at an infantryman (armor none).
        let einf = {
            let w = core.world_mut();
            let h = w.spawn_unit(
                0,
                enemy,
                CellCoord::new(mammoth_cell.x + 2, mammoth_cell.y + 1),
                ra_sim::coords::Facing(0),
                50,
                spawn_stats(),
            );
            w.set_unit_combat(h, 0, None, false);
            if let Some(u) = w.units.get_mut(h) {
                u.make_infantry(1);
            }
            h
        };
        core.inject_command(Command::Attack {
            unit: mammoth,
            target: Target::Unit(einf),
            house,
        });
        let mut vs_inf_dmg = None;
        for _ in 0..80 {
            core.update(67);
            hashes.push(core.sim_hash());
            // Wait for a *fresh* mammoth bullet whose damage differs from the tank shot.
            if let Some(d) = first_bullet_damage(&core) {
                if Some(d) != vs_tank_dmg {
                    vs_inf_dmg = Some(d);
                    break;
                }
            }
        }
        if dump {
            center_camera_on(&mut core, mammoth_cell);
            dump_game_png(&mut core, &out_dir, "m77_4tnk_vs_infantry.png")?;
        }
        report.push_str(&format!(
            "4TNK weapon switch: vs heavy tank bullet Damage={vs_tank_dmg:?} (expect 120mm=40), \
             vs infantry bullet Damage={vs_inf_dmg:?} (expect MammothTusk=75)\n"
        ));

        // --- Hand-checkable stats read straight from the loaded catalog. ---
        for (id, name) in [(U_3TNK, "3TNK"), (U_4TNK, "4TNK"), (U_ARTY, "ARTY")] {
            if let Some(p) = core.world().catalog.unit(id) {
                report.push_str(&format!(
                    "  {name}: cost={} strength={} primaryDmg={:?} secondaryDmg={:?}\n",
                    p.cost,
                    p.max_health,
                    p.weapon.map(|w| w.damage),
                    p.secondary.map(|w| w.damage),
                ));
            }
        }
        Ok((report, hashes))
    };

    let (report, h1) = run(true)?;
    let (_r2, h2) = run(false)?;
    eprintln!("--- M7.7 Chunk A verification ---\n{report}");
    if h1 == h2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            h1.len(),
            h1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = h1.iter().zip(&h2).position(|(a, b)| a != b).unwrap_or(0);
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// M7.7 Chunk B showcase: a defended base (TSLA + PBOX + GUN) breaking an enemy
/// attack wave, a wall blocking pathing, defense cameos in the structures strip,
/// hand-checkable costs/damage, and a same-script-twice determinism check.
/// Building ids match the loader (`assets.rs`): PBOX=5, GUN=7, TSLA=9, SBAG=10.
fn cmd_verify_m77b(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_sim::{Command, Target};
    const V_PBOX: u32 = 5;
    const V_GUN: u32 = 7;
    const V_TSLA: u32 = 9;
    const V_SBAG: u32 = 10;
    const V_2TNK: u32 = 3; // unit id (distinct id space from buildings)
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());

    let run = |dump: bool| -> Result<(String, Vec<u64>), BoxErr> {
        let game = assets::load_econ_from_dir(&dir, &scenario, 20000)?;
        let mut core = game.core;
        let house = game.controlled;
        let enemy: u8 = if house == 2 { 3 } else { 2 };
        let base = game.start_cell;
        let mut hashes: Vec<u64> = Vec::new();
        let mut report = String::new();

        // --- Stamp a defended base: yard/power/factory + PBOX, GUN, TSLA. ---
        {
            let w = core.world_mut();
            w.spawn_building(V_FACT, house, CellCoord::new(base.x, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 4, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 6, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 8, base.y)); // TSLA needs power
            w.spawn_building(V_PBOX, house, CellCoord::new(base.x + 2, base.y + 6));
            w.spawn_building(V_GUN, house, CellCoord::new(base.x + 5, base.y + 6));
            w.spawn_building(V_TSLA, house, CellCoord::new(base.x + 8, base.y + 6));
        }
        let (pout, pdrain) = core.power();
        report.push_str(&format!(
            "defended base: PBOX+GUN+TSLA at row {}, power {pout}/{pdrain}\n",
            base.y + 6
        ));

        // --- Wall blocking pathing: a wall line, an enemy ordered across it. ---
        let wall_y = base.y + 10;
        for dx in 0..6 {
            core.world_mut()
                .spawn_building(V_SBAG, enemy, CellCoord::new(base.x + dx, wall_y));
        }
        let crosser = spawn_combat_unit(
            &mut core,
            V_JEEP,
            enemy,
            CellCoord::new(base.x + 2, wall_y + 3),
        );
        core.inject_command(Command::Move {
            unit: crosser,
            dest: CellCoord::new(base.x + 2, wall_y - 3),
            house: enemy,
        });
        let mut ever_on_wall = false;
        for _ in 0..80 {
            core.update(67);
            hashes.push(core.sim_hash());
            if let Some(u) = core.world().units.get(crosser) {
                if u.cell().y == wall_y && (base.x..base.x + 6).contains(&u.cell().x) {
                    ever_on_wall = true;
                }
            }
        }
        report.push_str(&format!(
            "wall blocking: enemy JEEP {} entered a wall cell (should be false)\n",
            ever_on_wall
        ));

        // --- Attack wave: several enemy units approach; defenses break it. ---
        // The wave forms just inside defense range (north of its own wall) and
        // pushes the base, so the pillbox/gun/tesla visibly break it. A mix of a
        // couple of medium tanks (soak) and infantry (the pillbox shreds these).
        let wave: Vec<ra_sim::Handle> = (0..6)
            .map(|i| {
                let kind = if i < 2 { V_2TNK } else { V_E1 };
                let u = spawn_combat_unit(
                    &mut core,
                    kind,
                    enemy,
                    CellCoord::new(base.x + 1 + i, base.y + 9),
                );
                if kind == V_E1 {
                    if let Some(un) = core.world_mut().units.get_mut(u) {
                        un.make_infantry(1);
                    }
                }
                u
            })
            .collect();
        for &u in &wave {
            core.inject_command(Command::Attack {
                unit: u,
                target: Target::Cell(CellCoord::new(base.x + 4, base.y + 6)),
                house: enemy,
            });
        }
        // Step; capture a frame while the TSLA is charging/zapping.
        let mut tsla_fired = false;
        let mut zap_dumped = false;
        for _ in 0..400 {
            core.update(67);
            hashes.push(core.sim_hash());
            // Detect the tesla firing (charge cycling / bullets from buildings).
            let charging = core
                .world()
                .buildings
                .iter()
                .any(|(_, b)| b.charges && (b.charge > 0 || b.arm > 100));
            if charging {
                tsla_fired = true;
            }
            if tsla_fired && !zap_dumped && dump {
                center_camera_on(&mut core, CellCoord::new(base.x + 5, base.y + 8));
                dump_game_png(&mut core, &out_dir, "m77b_defended_base_zap.png")?;
                zap_dumped = true;
            }
        }
        let survivors = wave
            .iter()
            .filter(|&&u| core.world().units.contains(u))
            .count();
        report.push_str(&format!(
            "attack wave: {}/{} attackers survived the defenses; tesla engaged={tsla_fired}\n",
            survivors,
            wave.len()
        ));
        if dump {
            center_camera_on(&mut core, CellCoord::new(base.x + 5, base.y + 8));
            dump_game_png(&mut core, &out_dir, "m77b_defended_base_after.png")?;
        }

        // --- Hand-checkable stats from the loaded catalog. ---
        for (id, name) in [(V_PBOX, "PBOX"), (V_GUN, "GUN"), (V_TSLA, "TSLA")] {
            if let Some(p) = core.world().catalog.building(id) {
                report.push_str(&format!(
                    "  {name}: cost={} strength={} power={} weaponDmg={:?} charges={} turret={}\n",
                    p.cost,
                    p.max_health,
                    p.power,
                    p.weapon.map(|w| w.damage),
                    p.charges,
                    p.has_turret,
                ));
            }
        }
        Ok((report, hashes))
    };

    let (report, h1) = run(true)?;
    let (_r2, h2) = run(false)?;
    eprintln!("--- M7.7 Chunk B verification ---\n{report}");
    if h1 == h2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            h1.len(),
            h1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = h1.iter().zip(&h2).position(|(a, b)| a != b).unwrap_or(0);
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// M7.7 Chunk C showcase: DOME radar gated on a powered dome, SILO storage cap,
/// FIX unit repair, engineer capture, and medic healing — with hand-checkable
/// numbers and a same-script-twice determinism check. Building ids match the
/// loader: DOME=13, SILO=14, FIX=15. Unit ids: 2TNK=3, E1=5, MEDI=17, E6=18.
fn cmd_verify_m77c(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_sim::{Command, Target};
    const V_PROC: u32 = 2; // refinery (building id)
    const V_DOME: u32 = 13;
    const V_SILO: u32 = 14;
    const V_FIX: u32 = 15;
    const V_2TNK: u32 = 3;
    const V_E1: u32 = 5;
    const V_MEDI: u32 = 17;
    const V_E6: u32 = 18;
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}  scenario: {scenario}", dir.display());

    let run = |dump: bool| -> Result<(String, Vec<u64>), BoxErr> {
        let game = assets::load_econ_from_dir(&dir, &scenario, 5000)?;
        let mut core = game.core;
        let house = game.controlled;
        let enemy: u8 = if house == 2 { 3 } else { 2 };
        let base = game.start_cell;
        let mut hashes: Vec<u64> = Vec::new();
        let mut report = String::new();

        // --- Base with power so a DOME can run. ---
        {
            let w = core.world_mut();
            w.spawn_building(V_FACT, house, CellCoord::new(base.x, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 4, base.y));
            w.spawn_building(V_POWR, house, CellCoord::new(base.x + 6, base.y));
        }

        // --- DOME radar gate: no radar until a powered dome exists. ---
        let radar_before = core.has_radar();
        let dome =
            core.world_mut()
                .spawn_building(V_DOME, house, CellCoord::new(base.x, base.y + 4));
        core.update(67);
        let radar_with_dome = core.has_radar();
        center_camera_on(&mut core, base);
        if dump {
            dump_game_png(&mut core, &out_dir, "m77c_radar_on.png")?;
        }
        // Force the house into low power (drain exceeds output) so the dome loses
        // power and the radar switches off.
        if let Some(h) = core.world_mut().houses.get_mut(house as usize) {
            h.power_drain = h.power_output + 500;
        }
        core.update(67);
        let radar_no_power = core.has_radar();
        if dump {
            dump_game_png(&mut core, &out_dir, "m77c_radar_off.png")?;
        }
        report.push_str(&format!(
            "DOME radar: before={radar_before} withPoweredDome={radar_with_dome} afterPowerLost={radar_no_power} (expect false/true/false)\n"
        ));
        let _ = dome;

        // --- SILO storage cap: capacity = sum of Storage; harvest beyond wasted. ---
        {
            let w = core.world_mut();
            w.spawn_building(V_PROC, house, CellCoord::new(base.x, base.y + 8));
            // refinery Storage=2000
        }
        let cap_1 = core.world().house_capacity(house);
        core.world_mut()
            .spawn_building(V_SILO, house, CellCoord::new(base.x + 4, base.y + 8));
        core.world_mut()
            .spawn_building(V_SILO, house, CellCoord::new(base.x + 6, base.y + 8));
        let cap_3 = core.world().house_capacity(house);
        // Book harvest income directly (the capped-storage path, `Harvested`): with
        // no given credits, the harvested pool fills to capacity and stops — the
        // excess is wasted. Deliver 6000 into a 5000-capacity base.
        core.world_mut().set_house_credits(house, 0);
        let deposit = 3000;
        if let Some(h) = core.world_mut().houses.get_mut(house as usize) {
            h.add_harvest(deposit, cap_3);
        }
        let after1 = core.world().house_credits(house);
        if let Some(h) = core.world_mut().houses.get_mut(house as usize) {
            h.add_harvest(deposit, cap_3);
        }
        let after2 = core.world().house_credits(house);
        let wasted = (2 * deposit - cap_3).max(0);
        report.push_str(&format!(
            "SILO cap: capacity(1 refinery)={cap_1} capacity(+2 silos)={cap_3}; harvested \
             {deposit}+{deposit} -> banked {after1} then capped at {after2} (= {cap_3}), \
             wasted {wasted}\n"
        ));

        // --- FIX service depot: a damaged tank on the depot heals, credits drain. ---
        {
            let w = core.world_mut();
            w.spawn_building(V_FIX, house, CellCoord::new(base.x + 10, base.y));
        }
        core.world_mut().set_house_credits(house, 5000);
        let fix_credits_pre = core.world().house_credits(house);
        // Park the tank in the depot's adjacency ring (just below its footprint).
        let tank = spawn_combat_unit(
            &mut core,
            V_2TNK,
            house,
            CellCoord::new(base.x + 10, base.y + 2),
        );
        if let Some(u) = core.world_mut().units.get_mut(tank) {
            u.health = 100; // damaged (max ~400)
        }
        let tank_hp_pre = core.world().units.get(tank).map(|u| u.health).unwrap_or(0);
        for _ in 0..200 {
            core.update(67);
            hashes.push(core.sim_hash());
        }
        let tank_hp_post = core.world().units.get(tank).map(|u| u.health).unwrap_or(0);
        let fix_credits_post = core.world().house_credits(house);
        report.push_str(&format!(
            "FIX repair: tank HP {tank_hp_pre} -> {tank_hp_post} (rising), credits {fix_credits_pre} -> {fix_credits_post} (draining)\n"
        ));
        if dump {
            center_camera_on(&mut core, CellCoord::new(base.x + 10, base.y + 1));
            dump_game_png(&mut core, &out_dir, "m77c_fix_repair.png")?;
        }

        // --- Engineer capture: E6 captures a weak enemy building. ---
        let ebldg = core
            .world_mut()
            .spawn_building(V_POWR, enemy, CellCoord::new(base.x + 3, base.y + 14))
            .unwrap();
        if let Some(b) = core.world_mut().buildings.get_mut(ebldg) {
            b.health = (b.max_health as i32 / 5) as u16; // 20% (<= capture level)
        }
        let owner_pre = core.world().buildings.get(ebldg).map(|b| b.house);
        let eng = spawn_infantry_unit(
            &mut core,
            V_E6,
            house,
            CellCoord::new(base.x + 3, base.y + 12),
        );
        core.inject_command(Command::Attack {
            unit: eng,
            target: Target::Building(ebldg),
            house,
        });
        for _ in 0..120 {
            core.update(67);
            hashes.push(core.sim_hash());
            if core.world().buildings.get(ebldg).map(|b| b.house) == Some(house) {
                break;
            }
        }
        let owner_post = core.world().buildings.get(ebldg).map(|b| b.house);
        let eng_alive = core.world().units.contains(eng);
        report.push_str(&format!(
            "Engineer capture: building owner {owner_pre:?} -> {owner_post:?} (flipped to {house}), engineer consumed={}\n",
            !eng_alive
        ));
        if dump {
            center_camera_on(&mut core, CellCoord::new(base.x + 3, base.y + 13));
            dump_game_png(&mut core, &out_dir, "m77c_engineer_capture.png")?;
        }

        // --- Medic heal: MEDI heals a wounded friendly infantryman. ---
        let patient = spawn_infantry_unit(
            &mut core,
            V_E1,
            house,
            CellCoord::new(base.x + 15, base.y + 3),
        );
        if let Some(u) = core.world_mut().units.get_mut(patient) {
            u.health = 10;
        }
        let medic = spawn_infantry_unit(
            &mut core,
            V_MEDI,
            house,
            CellCoord::new(base.x + 15, base.y + 4),
        );
        let _ = medic;
        let patient_hp_pre = core
            .world()
            .units
            .get(patient)
            .map(|u| u.health)
            .unwrap_or(0);
        for _ in 0..120 {
            core.update(67);
            hashes.push(core.sim_hash());
        }
        let patient_hp_post = core
            .world()
            .units
            .get(patient)
            .map(|u| u.health)
            .unwrap_or(0);
        report.push_str(&format!(
            "Medic heal: wounded infantry HP {patient_hp_pre} -> {patient_hp_post} (rising)\n"
        ));

        // --- Hand-checkable stats. ---
        for (id, name) in [(V_MEDI, "MEDI"), (V_E6, "E6")] {
            if let Some(p) = core.world().catalog.unit(id) {
                report.push_str(&format!(
                    "  {name}: cost={} strength={} weaponDmg={:?}\n",
                    p.cost,
                    p.max_health,
                    p.weapon.map(|w| w.damage)
                ));
            }
        }
        for (id, name) in [(V_SILO, "SILO"), (V_DOME, "DOME"), (V_FIX, "FIX")] {
            if let Some(p) = core.world().catalog.building(id) {
                report.push_str(&format!(
                    "  {name}: cost={} storage={} power={}\n",
                    p.cost, p.storage, p.power
                ));
            }
        }
        Ok((report, hashes))
    };

    let (report, h1) = run(true)?;
    let (_r2, h2) = run(false)?;
    eprintln!("--- M7.7 Chunk C verification ---\n{report}");
    if h1 == h2 {
        eprintln!(
            "DETERMINISM OK: identical {}-tick hash chains across two runs (final {:#018x})",
            h1.len(),
            h1.last().copied().unwrap_or(0)
        );
        Ok(())
    } else {
        let first = h1.iter().zip(&h2).position(|(a, b)| a != b).unwrap_or(0);
        Err(format!("determinism FAILED: hash chains diverge at tick {first}").into())
    }
}

/// Spawn an infantry unit of catalog proto `unit_id` (convert to sub-cell infantry).
fn spawn_infantry_unit(
    core: &mut AppCore,
    unit_id: u32,
    house: u8,
    cell: CellCoord,
) -> ra_sim::Handle {
    let h = spawn_combat_unit(core, unit_id, house, cell);
    if let Some(u) = core.world_mut().units.get_mut(h) {
        u.make_infantry(1);
    }
    h
}

/// Default move stats for a scratch-spawned unit in the M7.7 showcase.
fn spawn_stats() -> ra_sim::MoveStats {
    ra_sim::MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

/// Start one infantryman of proto `unit_id` at `house`'s barracks and step until
/// it spawns; returns its handle (or `None` on timeout).
fn produce_one_infantry(
    core: &mut AppCore,
    house: u8,
    unit_id: u32,
    hashes: &mut Vec<u64>,
) -> Option<ra_sim::Handle> {
    let before: std::collections::BTreeSet<u32> = core
        .world()
        .units
        .handles()
        .into_iter()
        .map(|h| h.index)
        .collect();
    core.inject_command(ra_sim::Command::StartProduction {
        house,
        item: BuildItem::Unit(unit_id),
    });
    for _ in 0..4000 {
        core.update(67);
        hashes.push(core.sim_hash());
        // Find a brand-new infantry unit of this house.
        let found = core
            .world()
            .units
            .iter()
            .find(|(h, u)| u.house == house && u.is_infantry() && !before.contains(&h.index));
        if let Some((h, _)) = found {
            return Some(h);
        }
    }
    None
}

/// Spawn a combat-ready unit of catalog proto `unit_id` for `house` at `cell`
/// (wiring stats from the catalog), returning its handle.
fn spawn_combat_unit(
    core: &mut AppCore,
    unit_id: u32,
    house: u8,
    cell: CellCoord,
) -> ra_sim::Handle {
    let proto = core
        .world()
        .catalog
        .unit(unit_id)
        .cloned()
        .expect("proto present");
    let w = core.world_mut();
    let h = w.spawn_unit(
        proto.sprite_id,
        house,
        cell,
        ra_sim::coords::Facing(0),
        proto.max_health,
        proto.stats,
    );
    w.set_unit_max_health(h, proto.max_health);
    w.set_unit_combat(h, proto.armor, proto.weapon, proto.has_turret);
    w.set_unit_secondary(h, proto.secondary);
    h
}

/// Centre the camera on `cell` (map-space) for a game-surface dump.
fn center_camera_on(core: &mut AppCore, cell: CellCoord) {
    let px = (cell.x * CELL as i32) as f32 - core.tactical_width() as f32 / 2.0;
    let py = (cell.y * CELL as i32) as f32 - core.viewport_size().1 as f32 / 2.0;
    core.set_camera(px.max(0.0), py.max(0.0));
}

/// Demonstrate real land-type passability: find an impassable barrier
/// (rock/cliff/water) with drivable ground on both sides, spawn a tank on one
/// side, order it to the far side, and show it routes *around* the barrier (never
/// onto an impassable cell) rather than driving over it. Dumps a PNG mid-route.
fn cmd_verify_terrain(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_sim::coords::Locomotor;
    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let scenario = take_flag(&mut args, "--scenario").unwrap_or_else(|| "scg05eb.ini".to_string());
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;

    let game = assets::load_econ_from_dir(&dir, &scenario, 5000)?;
    let mut core = game.core;
    let pass = core.world().passability();

    // Scan for a start/goal pair straddling an impassable barrier: a passable
    // start, a passable goal a few cells east, with at least one impassable cell
    // on the straight line between them, and a real (routed-around) path.
    let mut chosen: Option<(CellCoord, CellCoord, usize, usize)> = None;
    'outer: for y in 2..126 {
        for x in 2..114 {
            let start = CellCoord::new(x, y);
            let goal = CellCoord::new(x + 10, y);
            if !pass.is_passable(start) || !pass.is_passable(goal) {
                continue;
            }
            let barrier = (1..10).any(|d| !pass.is_passable(CellCoord::new(x + d, y)));
            if !barrier {
                continue;
            }
            if let Some(path) = ra_sim::path::find_path(pass, start, goal, Locomotor::Track) {
                // A routed-around path is strictly longer than the 10-cell straight line.
                if path.len() > 10 {
                    chosen = Some((start, goal, path.len(), 10));
                    break 'outer;
                }
            }
        }
    }
    let Some((start, goal, path_len, straight)) = chosen else {
        return Err("no impassable barrier with a routed-around path found on this map".into());
    };
    eprintln!(
        "barrier demo: tank at ({},{}) -> ({},{}); straight line = {straight} cells crosses \
         impassable terrain, A* route = {path_len} cells (goes around)",
        start.x, start.y, goal.x, goal.y
    );

    let tank = spawn_combat_unit(&mut core, V_JEEP, game.controlled, start);
    core.inject_command(ra_sim::Command::Move {
        unit: tank,
        dest: goal,
        house: game.controlled,
    });
    // Step and record every cell the tank occupies; assert none is impassable.
    let mut crossed_impassable = false;
    for _ in 0..600 {
        core.update(67);
        let c = core.world().units.get(tank).map(|u| u.cell());
        if let Some(c) = c {
            if !core.world().passability().is_passable(c) {
                crossed_impassable = true;
            }
        }
        if core
            .world()
            .units
            .get(tank)
            .map(|u| !u.is_moving())
            .unwrap_or(true)
        {
            break;
        }
    }
    let end = core.world().units.get(tank).map(|u| u.cell());
    eprintln!(
        "tank ended at {end:?} (goal {goal:?}); ever stood on an impassable cell: {crossed_impassable}"
    );
    center_camera_on(&mut core, CellCoord::new((start.x + goal.x) / 2, start.y));
    dump_game_png(&mut core, &out_dir, "m76_terrain_route.png")?;
    if crossed_impassable {
        return Err("tank drove over impassable terrain — land-type passability failed".into());
    }
    eprintln!("LAND-TYPE OK: tank routed around the barrier without crossing impassable terrain");
    Ok(())
}

/// Compose the game surface and write it to `out_dir/name`.
fn dump_game_png(core: &mut AppCore, out_dir: &str, name: &str) -> Result<(), BoxErr> {
    let f = core.compose_game();
    let bytes = png::encode_rgba(f.width, f.height, &f.pixels);
    let path = std::path::Path::new(out_dir).join(name);
    std::fs::write(&path, &bytes)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

// ===========================================================================
// M7.8 verification: scripted drive of the pre-game state machine, headless.
// ===========================================================================

/// The `verify-m78` subcommand: drive the M7.8 state machine end to end without a
/// window — MainMenu → SkirmishSetup → InGame → Paused → resume → quit-to-menu →
/// a second game — asserting the World was built with the chosen settings, that
/// the pause overlay freezes the sim tick count, that a second game starts fresh
/// (no state leakage), that same-settings-same-seed builds are byte-identical, and
/// that a user-supplied map appears in the list. Dumps PNG evidence (main menu,
/// setup screen with map list + minimap, pause overlay).
#[allow(clippy::too_many_lines)]
fn cmd_verify_m78(mut args: Vec<String>) -> Result<(), BoxErr> {
    use ra_client::input::{Key, MouseButton};
    use ra_client::menu::{App, AppState, GameFactory, MapSource, ResolvedSkirmish, HOUSES};

    let out_dir = take_flag(&mut args, "--out-dir").unwrap_or_else(|| ".".to_string());
    let assets_flag = take_flag(&mut args, "--assets");
    let dir = platform::resolve_assets_dir(assets_flag.as_deref())
        .ok_or("could not find an assets directory (try --assets DIR or RA_ASSETS_DIR)")?;
    eprintln!("assets: {}", dir.display());

    let main_bytes = std::fs::read(dir.join("main.mix"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))?;
    let user_dir = platform::user_maps_dir();

    // Scan the archive multiplayer maps.
    let mut maps = assets::scan_archive_maps(&main_bytes, &redalert_bytes);
    eprintln!("scanned {} archive map(s):", maps.len());
    for m in maps.iter().take(6) {
        eprintln!(
            "  {} \"{}\" {}P {}x{}",
            m.filename, m.name, m.players, m.width, m.height
        );
    }
    if maps.len() < 2 {
        return Err("need at least 2 archive maps for the verification".into());
    }

    // User maps folder: drop a copy of an archive scenario in and confirm it shows
    // up as a USER map in the scan.
    let sample_name = maps[0].filename.clone();
    let mut user_map_ok = false;
    if let Some(ud) = &user_dir {
        let text = assets::scenario_text_from_archive(&main_bytes, &sample_name)?;
        let up = ud.join("verify_user_map.mpr");
        std::fs::write(&up, text.as_bytes())?;
        let user_maps = assets::scan_user_maps(&main_bytes, &redalert_bytes, ud);
        user_map_ok = user_maps
            .iter()
            .any(|m| m.source == MapSource::User && m.filename == "verify_user_map.mpr");
        eprintln!(
            "user maps dir {}: dropped verify_user_map.mpr -> appears in scan: {user_map_ok}",
            ud.display()
        );
        maps.extend(user_maps);
    }
    if !user_map_ok {
        eprintln!("warning: user maps folder not writable/available; skipping that check");
    }

    let factory = assets::ArchiveFactory::new(main_bytes, redalert_bytes, user_dir.clone());

    // Chosen settings for the primary drive: map #2, Hard, USSR, 10000, radar off.
    let map2 = maps[1].filename.clone();
    let ussr_house = HOUSES.iter().find(|(n, _)| *n == "USSR").unwrap().1; // 2
    let res = ResolvedSkirmish {
        map_filename: map2.clone(),
        player_house: ussr_house,
        color_house: 2,
        credits: 10000,
        difficulty: ra_sim::Difficulty::Hard,
        classic_radar: false,
    };

    // Determinism: build the same settings twice, tick, compare hash chains.
    let (mut c1, _) = factory.build(&res)?;
    let (mut c2, _) = factory.build(&res)?;
    let mut h1 = Vec::new();
    let mut h2 = Vec::new();
    for _ in 0..200 {
        c1.update(67);
        c2.update(67);
        h1.push(c1.sim_hash());
        h2.push(c2.sim_hash());
    }
    let det_ok = h1 == h2;
    eprintln!(
        "determinism (same settings+seed, 200 ticks): identical hash chains = {det_ok} (final {:#018x})",
        h1.last().copied().unwrap_or(0)
    );
    if !det_ok {
        return Err("determinism FAILED: same-settings builds diverged".into());
    }

    // --- Drive the state machine through handle()/update()/compose(). ---
    let mut app = App::new(maps.clone(), Box::new(factory));
    let vw = 1024i32;
    let vh = 768i32;
    app.handle(ra_client::InputEvent::Resize {
        width: vw as u32,
        height: vh as u32,
    });
    assert_eq!(app.state(), AppState::MainMenu, "boots to main menu");
    dump_app_png(&app, &out_dir, "m78_1_main_menu.png")?;

    // Click SKIRMISH (first main-menu button; center at viewport middle).
    let click = |app: &mut App, x: i32, y: i32| {
        app.handle(ra_client::InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
        });
        app.handle(ra_client::InputEvent::MouseUp {
            button: MouseButton::Left,
            x,
            y,
        });
    };
    click(&mut app, vw / 2, vh / 2 - 2);
    assert_eq!(
        app.state(),
        AppState::SkirmishSetup,
        "SKIRMISH click enters setup"
    );

    // Set the chosen options (via the config seam) and select map #2.
    app.select_map(1);
    {
        let cfg = app.config_mut();
        cfg.difficulty = 2; // HARD
        cfg.house = HOUSES.iter().position(|(n, _)| *n == "USSR").unwrap();
        cfg.credits = 3; // 10000
        cfg.classic_radar = false;
    }
    dump_app_png(&app, &out_dir, "m78_2_setup.png")?;

    app.start_game();
    assert_eq!(app.state(), AppState::InGame, "START enters the game");

    // Assert the World was built with the chosen settings.
    let core = app.core().ok_or("no core after start")?;
    let ph = core.world().player_house();
    let credits = core.world().house_credits(ussr_house);
    let ai_house = if ussr_house == 2 { 0 } else { 2 };
    let ai_diff = core.world().ai_difficulty(ai_house);
    let radar = core.has_radar();
    eprintln!(
        "game built: player_house={ph:?} (want Some({ussr_house}))  credits={credits} (want 10000)  \
         ai_difficulty[{ai_house}]={ai_diff:?} (want Hard)  radar={radar} (want true: classic OFF = always-on)"
    );
    assert_eq!(ph, Some(ussr_house), "player house threaded through");
    assert_eq!(credits, 10000, "starting credits threaded through");
    assert_eq!(
        ai_diff,
        Some(ra_sim::Difficulty::Hard),
        "difficulty threaded"
    );
    assert!(radar, "classic-radar OFF should make radar always-on");

    // Play a few ticks, then pause and assert the tick count freezes.
    for _ in 0..30 {
        app.update(67);
    }
    let ticks_before_pause = app.core().unwrap().world().tick_count();
    app.handle(ra_client::InputEvent::KeyDown(Key::Menu)); // Esc -> pause
    assert_eq!(app.state(), AppState::Paused, "Esc opens the pause overlay");
    dump_app_png(&app, &out_dir, "m78_3_pause.png")?;
    for _ in 0..30 {
        app.update(67); // must NOT tick the sim while paused
    }
    let ticks_paused = app.core().unwrap().world().tick_count();
    eprintln!(
        "pause freeze: tick_count {ticks_before_pause} -> {ticks_paused} after 30 paused updates (want equal)"
    );
    assert_eq!(
        ticks_before_pause, ticks_paused,
        "the sim tick count must not advance while paused"
    );

    // Resume (click RESUME) and confirm the sim ticks again.
    click(&mut app, vw / 2, vh / 2 - 30 + 18);
    assert_eq!(app.state(), AppState::InGame, "RESUME returns to the game");
    for _ in 0..30 {
        app.update(67);
    }
    let ticks_resumed = app.core().unwrap().world().tick_count();
    eprintln!("resume: tick_count advanced to {ticks_resumed} (want > {ticks_paused})");
    assert!(ticks_resumed > ticks_paused, "sim resumes ticking");

    // Quit to menu: pause, then click QUIT TO MENU.
    app.handle(ra_client::InputEvent::KeyDown(Key::Menu));
    click(&mut app, vw / 2, vh / 2 - 30 + 36 + 14 + 18);
    assert_eq!(
        app.state(),
        AppState::MainMenu,
        "quit-to-menu returns to menu"
    );
    assert!(
        app.core().is_none(),
        "the game World is dropped on quit-to-menu"
    );

    // Start a second game (no state leakage) — a fresh World.
    click(&mut app, vw / 2, vh / 2 - 2);
    app.select_map(0);
    {
        let cfg = app.config_mut();
        cfg.difficulty = 0; // EASY
        cfg.house = 0; // GREECE
        cfg.credits = 0; // 2500
        cfg.classic_radar = true;
    }
    app.start_game();
    assert_eq!(app.state(), AppState::InGame, "second game starts");
    let core2 = app.core().ok_or("no core for 2nd game")?;
    eprintln!(
        "second game: fresh World tick_count={} credits={} player_house={:?} radar={} (classic ON, no dome yet)",
        core2.world().tick_count(),
        core2.world().house_credits(1),
        core2.world().player_house(),
        core2.has_radar()
    );
    assert!(
        core2.world().tick_count() <= 1,
        "second game starts from a fresh World (no leaked ticks)"
    );
    assert_eq!(core2.world().house_credits(1), 2500, "second game credits");
    assert_eq!(core2.world().player_house(), Some(1), "second game house");

    eprintln!("M7.8 OK: state machine, settings threading, pause freeze, fresh restart, determinism, user maps");
    Ok(())
}

/// Compose an [`App`] frame and write it to `out_dir/name`.
fn dump_app_png(app: &ra_client::menu::App, out_dir: &str, name: &str) -> Result<(), BoxErr> {
    let f = app.compose();
    let bytes = png::encode_rgba(f.width, f.height, &f.pixels);
    let path = std::path::Path::new(out_dir).join(name);
    std::fs::write(&path, &bytes)?;
    eprintln!("wrote {}", path.display());
    Ok(())
}
