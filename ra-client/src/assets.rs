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

use ra_data::house::{build_house_remaps, identity_remap, RemapTable, HOUSE_COUNT};
use ra_data::passability;
use ra_data::rules::unit_stats;
use ra_data::scenario::{parse_units, Scenario};
use ra_data::templates;
use ra_formats::cps::Cps;
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_formats::pal::Palette as PalFile;
use ra_formats::tmpl::Template;

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{MoveStats, Passability, World};

use crate::appcore::AppCore;
use crate::compositor::Palette;
use crate::terrain::{rasterize, TileSet};
use crate::unit_render::UnitSprite;

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

/// A description of one spawned unit, returned by the game loader so the CLI /
/// verification path can report and locate what is on the map.
#[derive(Debug, Clone)]
pub struct SpawnInfo {
    /// The unit's runtime handle (into `AppCore::world().units`).
    pub handle: ra_sim::Handle,
    /// Unit type name, e.g. `"JEEP"`.
    pub unit_type: String,
    /// Owning house index.
    pub house: u8,
    /// Spawn cell.
    pub cell: CellCoord,
}

/// A fully loaded, playable scenario: an [`AppCore`] with terrain, spawned
/// units, sprites, and house remaps, plus a manifest of what was spawned.
pub struct LoadedGame {
    /// The ready-to-drive core.
    pub core: AppCore,
    /// Every unit that was spawned, in scenario order.
    pub spawned: Vec<SpawnInfo>,
    /// Playable rectangle top-left cell (for centring the camera).
    pub playable: (u32, u32, u32, u32),
    /// Names of placements skipped for want of rules stats or a sprite.
    pub skipped: Vec<String>,
}

/// Find and read the archives under `dir`, then load a fully playable scenario
/// (terrain + units). See [`load_game_from_bytes`].
pub fn load_game_from_dir(dir: &Path, scenario_name: &str) -> Result<LoadedGame, Box<dyn Error>> {
    let main_bytes =
        std::fs::read(dir.join("main.mix")).map_err(|e| format!("reading main.mix: {e}"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))
        .map_err(|e| format!("reading redalert.mix: {e}"))?;
    load_game_from_bytes(&main_bytes, &redalert_bytes, scenario_name)
}

/// Load a fully playable scenario from in-memory archives: terrain, the
/// `[UNITS]` placements spawned into a [`World`] with rules-driven stats, unit
/// sprites from `conquer.mix`, and per-house remaps from `PALETTE.CPS`.
pub fn load_game_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
) -> Result<LoadedGame, Box<dyn Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    // --- Scenario INI: terrain + unit placements ---
    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found in general.mix"))?;
    let ini_text = String::from_utf8_lossy(ini_bytes);
    let ini = Ini::parse(&ini_text);
    let scenario = Scenario::from_ini(&ini)?;
    let placements = parse_units(&ini);

    // --- Terrain raster + palette (reuse the terrain path) ---
    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let raster = rasterize(&loaded.scenario, &loaded.tiles);
    let palette = loaded.palette;

    // --- rules.ini + PALETTE.CPS from redalert.mix -> local.mix ---
    let local = redalert.open_nested("local.mix")?;
    let rules_bytes = local
        .get("rules.ini")
        .ok_or("rules.ini not found in local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(rules_bytes));

    let remaps: Vec<RemapTable> = match local.get("palette.cps") {
        Some(cps_bytes) => match Cps::parse(cps_bytes) {
            Ok(cps) => build_house_remaps(&cps).to_vec(),
            Err(_) => vec![identity_remap(); HOUSE_COUNT],
        },
        None => vec![identity_remap(); HOUSE_COUNT],
    };

    // --- Unit sprites from main.mix -> conquer.mix ---
    let conquer = main.open_nested("conquer.mix")?;

    // --- Build the world and spawn units ---
    let passable = passability::build(&scenario);
    let grid = Passability::new(128, 128, passable);
    let mut world = World::new(grid, 0x1234_5678);

    let mut sprites: Vec<UnitSprite> = Vec::new();
    // type name -> (type_id, MaxStrength)
    let mut type_ids: std::collections::BTreeMap<String, (u32, i32)> =
        std::collections::BTreeMap::new();
    let mut spawned = Vec::new();
    let mut skipped = Vec::new();

    for p in &placements {
        let key = p.unit_type.to_ascii_uppercase();
        // Resolve (or load) the type's sprite + stats.
        if !type_ids.contains_key(&key) {
            let stats = unit_stats(&rules, &key);
            let shp_name = format!("{}.shp", key.to_ascii_lowercase());
            let sprite = conquer
                .get(&shp_name)
                .and_then(|b| UnitSprite::from_shp_bytes(b).ok());
            match (stats, sprite) {
                (Some(stats), Some(sprite)) => {
                    let id = sprites.len() as u32;
                    sprites.push(sprite);
                    type_ids.insert(key.clone(), (id, stats.strength.max(1)));
                }
                _ => {
                    skipped.push(p.unit_type.clone());
                    continue;
                }
            }
        }
        let (type_id, max_strength) = match type_ids.get(&key) {
            Some(v) => *v,
            None => continue,
        };
        let stats = unit_stats(&rules, &key).expect("stats present if type resolved");
        let cell = CellCoord::from_index(p.cell);
        let health = ((p.strength as i32) * max_strength / 256).clamp(0, u16::MAX as i32) as u16;
        let handle = world.spawn_unit(
            type_id,
            p.house,
            cell,
            Facing(p.facing),
            health,
            MoveStats {
                max_speed: stats.max_speed_leptons(),
                rot: stats.rot,
            },
        );
        spawned.push(SpawnInfo {
            handle,
            unit_type: p.unit_type.clone(),
            house: p.house,
            cell,
        });
    }

    let playable = (
        scenario.map_x as u32,
        scenario.map_y as u32,
        scenario.map_width as u32,
        scenario.map_height as u32,
    );
    let core = AppCore::with_sim(raster, palette, world, sprites, remaps);
    Ok(LoadedGame {
        core,
        spawned,
        playable,
        skipped,
    })
}
