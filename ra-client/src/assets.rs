//! Loading a scenario's terrain from the real archives into an [`AppCore`]:
//! read `main.mix` (scenario INIs + theater tile mixes) and `redalert.mix`
//! (theater palettes in `local.mix`), decode the scenario, resolve every
//! template it references, and rasterize.
//!
//! This crate is allowed I/O (it is the client); this module stays free of any
//! OS-conditional compilation — path discovery lives in [`crate::platform`],
//! the one module permitted such code (DESIGN.md §4.7).

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;

use ra_data::scenario::Scenario;
use ra_data::templates;
use ra_formats::mix::MixArchive;
use ra_formats::pal::Palette as PalFile;
use ra_formats::tmpl::Template;

use crate::appcore::AppCore;
use crate::compositor::Palette;
use crate::terrain::{rasterize, TileSet};

/// Everything needed to render a scenario's terrain.
pub struct LoadedTerrain {
    /// The parsed scenario (theater, map rect, cells).
    pub scenario: Scenario,
    /// The resolved theater templates.
    pub tiles: TileSet,
    /// The theater palette.
    pub palette: Palette,
}

/// Find and read `main.mix` and `redalert.mix` under `dir`, then load the named
/// scenario's terrain.
pub fn load_from_dir(dir: &Path, scenario_name: &str) -> Result<LoadedTerrain, Box<dyn Error>> {
    let main_bytes =
        std::fs::read(dir.join("main.mix")).map_err(|e| format!("reading main.mix: {e}"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))
        .map_err(|e| format!("reading redalert.mix: {e}"))?;
    load_from_bytes(&main_bytes, &redalert_bytes, scenario_name)
}

/// Load a scenario's terrain from in-memory archive bytes (keeps the file I/O
/// out of the parsing path, so tests can feed fixtures).
pub fn load_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
) -> Result<LoadedTerrain, Box<dyn Error>> {
    let main = MixArchive::parse(main_bytes)?;

    // Scenario INIs live in general.mix inside main.mix.
    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found in general.mix"))?;
    let ini_text = String::from_utf8_lossy(ini_bytes);
    let scenario = Scenario::parse(&ini_text)?;

    // Theater templates.
    let theater_mix = main.open_nested(scenario.theater.mix_name())?;
    let suffix = scenario.theater.suffix();

    // Distinct template ids actually used (deterministic order via BTreeSet),
    // plus CLEAR1 which every clear cell needs.
    // CLEAR1 (id 0) is always needed for clear cells. `0xFFFF` and the legacy
    // `255` are "no template" sentinels the renderer draws as clear (see
    // `terrain::is_clear` / `cell.cpp`), so we don't try to resolve them.
    let mut ids: BTreeSet<u16> = BTreeSet::new();
    ids.insert(templates::TEMPLATE_CLEAR1);
    for cell in &scenario.cells {
        if cell.template != templates::TEMPLATE_NONE && cell.template != 255 {
            ids.insert(cell.template);
        }
    }

    let mut tiles = TileSet::new();
    let mut missing = Vec::new();
    for id in ids {
        let Some(filename) = templates::template_filename(id, suffix) else {
            continue; // id outside the catalog
        };
        match theater_mix.get(&filename) {
            Some(bytes) => match Template::parse(bytes) {
                Ok(t) => tiles.insert(id, t),
                Err(_) => missing.push(filename),
            },
            None => missing.push(filename),
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "warning: {} template(s) missing/unparsable for theater {:?}: {}",
            missing.len(),
            scenario.theater,
            missing.join(", ")
        );
    }

    // Palette lives in redalert.mix -> local.mix.
    let redalert = MixArchive::parse(redalert_bytes)?;
    let local = redalert.open_nested("local.mix")?;
    let pal_name = scenario.theater.palette_name();
    let pal_bytes = local
        .get(pal_name)
        .ok_or_else(|| format!("palette '{pal_name}' not found in local.mix"))?;
    let palette: Palette = PalFile::parse(pal_bytes)?.colors;

    Ok(LoadedTerrain {
        scenario,
        tiles,
        palette,
    })
}

impl LoadedTerrain {
    /// Rasterize the terrain and wrap it in an [`AppCore`].
    pub fn into_appcore(self) -> AppCore {
        let raster = rasterize(&self.scenario, &self.tiles);
        AppCore::new(raster, self.palette)
    }
}
