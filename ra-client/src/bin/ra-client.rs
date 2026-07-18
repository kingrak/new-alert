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

use ra_client::assets::{self, LoadedTerrain};
use ra_client::input::Rect;
use ra_client::platform;
use ra_client::png;
use ra_formats::tmpl::{ICON_HEIGHT, ICON_WIDTH};

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
        _ => {
            eprintln!(
                "usage:\n  ra-client dump   [--assets DIR] [--scenario NAME] [--out PATH.png] [--rect CX CY CW CH] [--playable]\n  ra-client window [--assets DIR] [--scenario NAME]"
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
    let loaded = load(&mut args)?;
    describe(&loaded);
    // Start the camera over the scenario's playable area rather than the map
    // corner (which is mostly border/clear).
    let start = (
        (loaded.scenario.map_x as f32) * CELL as f32,
        (loaded.scenario.map_y as f32) * CELL as f32,
    );
    let mut core = loaded.into_appcore();
    core.set_camera(start.0, start.1);
    ra_client::shell::run_window(core);
    Ok(())
}

#[cfg(not(feature = "window"))]
fn cmd_window(_args: Vec<String>) -> Result<(), BoxErr> {
    Err(
        "this build was compiled without the `window` feature; rebuild with default features"
            .into(),
    )
}
