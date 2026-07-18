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

use ra_data::buildings::building_stats;
use ra_data::combat::{resolve_unit_combat, WeaponDef};
use ra_data::house::{
    build_house_remaps, house_from_name, identity_remap, RemapTable, HOUSE_COUNT,
};
use ra_data::rules::unit_stats;
use ra_data::scenario::{parse_units, Scenario};
use ra_data::templates;
use ra_formats::cps::Cps;
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_formats::pal::Palette as PalFile;
use ra_formats::tmpl::Template;

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildItem, BuildingProto, Catalog, EconRules, Handle, MoveStats, OreField, Passability,
    UnitProto, World,
};

use crate::appcore::AppCore;
use crate::appcore::SoundEvent;
use crate::compositor::Palette;
use crate::terrain::{build_passability_masks, rasterize, TileSet};
use crate::unit_render::{InfantryAnim, UnitSprite};

use ra_data::landtype::{LOCO_FOOT, LOCO_TRACK, LOCO_WHEEL};

/// Build a per-locomotor [`Passability`] grid from a scenario's land types and
/// the rules.ini land-cost sections (M7.6 real land-type passability). Replaces
/// the M3 water-only `passability::build` at every game/scenario boot.
fn make_passability(scenario: &Scenario, tiles: &TileSet, rules: &Ini) -> Passability {
    let (foot, track, wheel) = build_passability_masks(scenario, tiles, rules);
    Passability::per_locomotor(128, 128, foot, track, wheel)
}

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
    let grid = make_passability(&scenario, &loaded.tiles, &rules);
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
        world.set_unit_max_health(handle, max_strength.clamp(1, u16::MAX as i32) as u16);
        world.set_unit_sight(handle, stats.sight);
        // Combat stats (armor, primary weapon, turret) resolved from rules.ini.
        if let Some(combat) = resolve_unit_combat(&rules, &key) {
            world.set_unit_combat(
                handle,
                combat.armor,
                combat.weapon.as_ref().map(weapon_to_profile),
                combat.has_turret,
            );
        }
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

/// A scripted 1-v-1 battle set up for the M4 verification path: a real 2TNK
/// attacker and an enemy HARV, spawned adjacent on a real scenario's terrain,
/// with real rules-driven weapon/armor stats. The bin drives the attack through
/// the [`AppCore`] seam and dumps a PNG sequence.
pub struct BattleSetup {
    /// Terrain + the two combatants, ready to drive.
    pub core: AppCore,
    /// The attacking 2TNK (house 0).
    pub attacker: Handle,
    /// The 2TNK's spawn cell.
    pub attacker_cell: CellCoord,
    /// The target HARV (house 1).
    pub target: Handle,
    /// The HARV's spawn cell.
    pub target_cell: CellCoord,
    /// The attacker's resolved primary weapon (for the damage-math report).
    pub weapon: WeaponDef,
    /// The target's armor class index (for the damage-math report).
    pub target_armor: u8,
    /// The target's max strength (for the shots-to-kill report).
    pub target_max_hp: u16,
}

/// Load a scripted 2TNK-vs-HARV battle from the archives under `dir`.
pub fn load_battle_from_dir(
    dir: &Path,
    scenario_name: &str,
) -> Result<BattleSetup, Box<dyn Error>> {
    let main_bytes =
        std::fs::read(dir.join("main.mix")).map_err(|e| format!("reading main.mix: {e}"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))
        .map_err(|e| format!("reading redalert.mix: {e}"))?;
    load_battle_from_bytes(&main_bytes, &redalert_bytes, scenario_name)
}

/// Build the scripted battle from in-memory archives. Terrain and palette come
/// from `scenario_name`; the two combatants are spawned by this function (not
/// from the scenario `[UNITS]`) so the fight is controlled and reproducible.
pub fn load_battle_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
) -> Result<BattleSetup, Box<dyn Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    // Terrain raster + palette.
    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let raster = rasterize(&loaded.scenario, &loaded.tiles);
    let palette = loaded.palette;
    let scenario = loaded.scenario;

    // rules.ini + remaps.
    let local = redalert.open_nested("local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(
        local.get("rules.ini").ok_or("rules.ini not found")?,
    ));
    let remaps: Vec<RemapTable> = match local.get("palette.cps") {
        Some(cps_bytes) => match Cps::parse(cps_bytes) {
            Ok(cps) => build_house_remaps(&cps).to_vec(),
            Err(_) => vec![identity_remap(); HOUSE_COUNT],
        },
        None => vec![identity_remap(); HOUSE_COUNT],
    };

    let conquer = main.open_nested("conquer.mix")?;
    let grid = make_passability(&scenario, &loaded.tiles, &rules);
    let mut world = World::new(grid, 0x1234_5678);
    let mut sprites: Vec<UnitSprite> = Vec::new();

    // Place the two combatants two cells apart near the playable-rect centre, on
    // passable ground (2TNK's 90mm range is 4.75 cells, so this starts in range).
    let cx = scenario.map_x as i32 + scenario.map_width as i32 / 2;
    let cy = scenario.map_y as i32 + scenario.map_height as i32 / 2;
    let attacker_cell = CellCoord::new(cx - 1, cy);
    let target_cell = CellCoord::new(cx + 2, cy);

    let (attacker, weapon, _) = spawn_named(
        &mut world,
        &mut sprites,
        &conquer,
        &rules,
        "2TNK",
        0,
        attacker_cell,
    )?;
    let (target, _, target_armor) = spawn_named(
        &mut world,
        &mut sprites,
        &conquer,
        &rules,
        "HARV",
        1,
        target_cell,
    )?;
    let target_max_hp = world.units.get(target).map(|u| u.max_health).unwrap_or(1);
    let weapon = weapon.ok_or("2TNK resolved without a weapon")?;

    let mut core = AppCore::with_sim(raster, palette, world, sprites, remaps);
    // M7 verification: render on the full game surface (so the cosmetic effect
    // layer is composited) with the explosion + ore art installed. Empty
    // buildables — this is a combat-only harness, house 0 is the controlled side.
    core.enable_sidebar(0, Vec::new());
    core.enable_radar();
    let theater_mix = main.open_nested(scenario.theater.mix_name()).ok();
    if let Some(t) = &theater_mix {
        let suffix = scenario.theater.suffix();
        core.set_ore_art(
            load_overlay_tiles(t, "GOLD", 4, suffix),
            load_overlay_tiles(t, "GEM", 4, suffix),
        );
    }
    let explosion: Vec<UnitSprite> = load_shp_opt(&conquer, "FBALL1.SHP").into_iter().collect();
    core.set_effect_art(explosion, Vec::new());
    Ok(BattleSetup {
        core,
        attacker,
        attacker_cell,
        target,
        target_cell,
        weapon,
        target_armor,
        target_max_hp,
    })
}

/// Spawn one named unit (resolving sprite, movement, and combat stats from the
/// archives) at `cell`, returning its handle, resolved weapon, and armor. A new
/// sprite is appended for each call; `type_id` is the sprite index.
fn spawn_named(
    world: &mut World,
    sprites: &mut Vec<UnitSprite>,
    conquer: &MixArchive,
    rules: &Ini,
    name: &str,
    house: u8,
    cell: CellCoord,
) -> Result<(Handle, Option<WeaponDef>, u8), Box<dyn Error>> {
    let key = name.to_ascii_uppercase();
    let stats = unit_stats(rules, &key).ok_or_else(|| format!("no rules stats for {key}"))?;
    let shp = format!("{}.shp", key.to_ascii_lowercase());
    let sprite = conquer
        .get(&shp)
        .and_then(|b| UnitSprite::from_shp_bytes(b).ok())
        .ok_or_else(|| format!("no sprite {shp}"))?;
    let type_id = sprites.len() as u32;
    sprites.push(sprite);

    let max_hp = stats.strength.clamp(1, u16::MAX as i32) as u16;
    let handle = world.spawn_unit(
        type_id,
        house,
        cell,
        Facing(0),
        max_hp,
        MoveStats {
            max_speed: stats.max_speed_leptons(),
            rot: stats.rot,
        },
    );
    world.set_unit_max_health(handle, max_hp);
    world.set_unit_sight(handle, stats.sight);
    let combat = resolve_unit_combat(rules, &key);
    let (weapon, armor) = match &combat {
        Some(c) => {
            world.set_unit_combat(
                handle,
                c.armor,
                c.weapon.as_ref().map(weapon_to_profile),
                c.has_turret,
            );
            (c.weapon, c.armor)
        }
        None => (None, 0),
    };
    Ok((handle, weapon, armor))
}

// ===========================================================================
// M5 economy: build catalog, ore, houses, and a playable/verifiable game.
// ===========================================================================

/// The starter buildable content (catalog + decoded sprites + the sidebar list),
/// lifted from rules.ini + the code-defined footprint table into the sim's
/// plain `Catalog` (DESIGN.md §4.9 M5). Building type ids and unit-proto ids are
/// the fixed indices assigned below; the sim references them, the client renders
/// them.
pub struct GameContent {
    /// The sim's immutable build data.
    pub catalog: Catalog,
    /// Unit body sprites, indexed by unit `sprite_id` (== unit-proto id here).
    pub unit_sprites: Vec<UnitSprite>,
    /// Building idle sprites, indexed by building type id.
    pub building_sprites: Vec<UnitSprite>,
    /// Optional per-building overlay shape drawn over the base sprite
    /// (the war factory's WEAP2 roof/door; building.cpp:513). Same indexing.
    pub building_overlays: Vec<Option<UnitSprite>>,
    /// Sidebar buildable list, in display order.
    pub buildables: Vec<BuildItem>,
    /// Per-unit-type infantry animation layout, indexed by unit `sprite_id`
    /// (`None` for vehicles). Drives the Do-table frame selection in the client.
    pub infantry_anim: Vec<Option<InfantryAnim>>,
}

/// Read the economy constants from rules.ini `[General]`/`[AI]` (defaults are
/// the RA stock values).
fn econ_rules(rules: &Ini) -> EconRules {
    let d = EconRules::default();
    EconRules {
        gold_value: rules
            .get_int("General", "GoldValue")
            .unwrap_or(d.gold_value as i64) as i32,
        gem_value: rules
            .get_int("General", "GemValue")
            .unwrap_or(d.gem_value as i64) as i32,
        bail_count: rules
            .get_int("General", "BailCount")
            .unwrap_or(d.bail_count as i64) as u16,
        ore_dump_rate: rules
            .get_int("General", "OreTruckRate")
            .unwrap_or(d.ore_dump_rate as i64) as u16,
        refund_percent: rules
            .get_int("General", "RefundPercent")
            .unwrap_or(d.refund_percent as i64) as i32,
        growth_rate: rules
            .get_int("General", "GrowthRate")
            .unwrap_or(d.growth_rate as i64)
            .max(1) as i32,
        ..d
    }
}

/// Parse an RA INI boolean (`yes`/`no`/`true`/`1`), with a default.
fn ini_bool(rules: &Ini, section: &str, key: &str, default: bool) -> bool {
    match rules.get(section, key) {
        Some(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "yes" | "true" | "1" | "on"
        ),
        None => default,
    }
}

/// Whether ore growth/spread are enabled (`[General] OreGrows`/`OreSpreads`,
/// default yes — `rules.cpp:441-442`). Returned as `(grows, spreads)`.
fn ore_growth_flags(rules: &Ini) -> (bool, bool) {
    (
        ini_bool(rules, "General", "OreGrows", true),
        ini_bool(rules, "General", "OreSpreads", true),
    )
}

/// Fixed building type ids (order matters — the catalog is indexed by these).
const B_FACT: u32 = 0;
const B_POWR: u32 = 1;
const B_PROC: u32 = 2;
const B_WEAP: u32 = 3;
const B_TENT: u32 = 4;
/// Fixed unit-proto ids.
const U_MCV: u32 = 0;
const U_HARV: u32 = 1;
const U_1TNK: u32 = 2;
const U_2TNK: u32 = 3;
const U_JEEP: u32 = 4;
const U_E1: u32 = 5;
const U_E2: u32 = 6;
const U_E3: u32 = 7;

/// Map a prerequisite short-name to its building type id (only the starter set
/// is modelled; unknown prereqs — e.g. `fix` for the MCV — are dropped, which is
/// safe because those items are not in the sidebar).
fn prereq_ids(names: &[String]) -> Vec<u32> {
    names
        .iter()
        .filter_map(|n| match n.as_str() {
            "fact" => Some(B_FACT),
            "powr" => Some(B_POWR),
            "proc" => Some(B_PROC),
            "weap" => Some(B_WEAP),
            "tent" | "barr" => Some(B_TENT),
            _ => None,
        })
        .collect()
}

/// Decode a unit SHP from `conquer.mix` by short name.
fn load_unit_sprite(conquer: &MixArchive, name: &str) -> Result<UnitSprite, Box<dyn Error>> {
    load_unit_sprite_from(&[conquer], name)
}

/// Decode a unit SHP by short name from the first of `archives` that carries it.
/// Vehicle/building art lives in `conquer.mix` (main.mix); infantry art lives in
/// `lores.mix` (redalert.mix), so infantry loading passes both.
fn load_unit_sprite_from(
    archives: &[&MixArchive],
    name: &str,
) -> Result<UnitSprite, Box<dyn Error>> {
    let shp = format!("{}.shp", name.to_ascii_lowercase());
    for a in archives {
        if let Some(bytes) = a.get(&shp) {
            if let Ok(sprite) = UnitSprite::from_shp_bytes(bytes) {
                return Ok(sprite);
            }
        }
    }
    Err(format!("sprite {shp} missing/undecodable").into())
}

/// Decode the M7 sound set to in-memory WAV, keyed by logical [`SoundEvent`].
/// Reads the archives from `dir`; every sound is best-effort (a missing or
/// undecodable AUD is simply skipped, so audio never blocks a boot). Returns
/// empty when the archives can't be opened. Pure data — no audio device is
/// touched here, so this is safe to call in any build (the shell decides whether
/// to play).
pub fn load_sound_bank(dir: &Path) -> Vec<(SoundEvent, Vec<u8>)> {
    let (Ok(main_bytes), Ok(redalert_bytes)) = (
        std::fs::read(dir.join("main.mix")),
        std::fs::read(dir.join("redalert.mix")),
    ) else {
        return Vec::new();
    };
    let (Ok(main), Ok(redalert)) = (
        MixArchive::parse(&main_bytes),
        MixArchive::parse(&redalert_bytes),
    ) else {
        return Vec::new();
    };
    let sounds = main.open_nested("sounds.mix").ok();
    let speech = redalert.open_nested("speech.mix").ok();

    // (event, source mix, AUD name). Weapon/UI SFX live in sounds.mix; EVA voice
    // lines in speech.mix. Names verified present in the shipped archives; any
    // that are absent (e.g. a mission-failed line) are skipped.
    let spec: [(SoundEvent, Option<&MixArchive>, &str); 7] = [
        (SoundEvent::Fire, sounds.as_ref(), "CANNON1.AUD"),
        (SoundEvent::Explosion, sounds.as_ref(), "KABOOM1.AUD"),
        (SoundEvent::Select, sounds.as_ref(), "RABEEP1.AUD"),
        (
            SoundEvent::ConstructionComplete,
            speech.as_ref(),
            "CONSCMP1.AUD",
        ),
        (SoundEvent::LowPower, speech.as_ref(), "NOPOWR1.AUD"),
        (SoundEvent::Victory, speech.as_ref(), "MISNWON1.AUD"),
        (SoundEvent::Defeat, speech.as_ref(), "MISNLST1.AUD"),
    ];
    let mut out = Vec::new();
    for (ev, mix, name) in spec {
        let Some(mix) = mix else { continue };
        if let Some(bytes) = mix.get(name) {
            if let Ok(clip) = ra_formats::aud::decode(bytes) {
                out.push((ev, ra_formats::aud::to_wav(&clip)));
            }
        }
    }
    out
}

/// Load one SHP by full (extension-included) name from a mix, `None` if
/// missing/undecodable — used for optional cosmetic art (M7).
fn load_shp_opt(mix: &MixArchive, name: &str) -> Option<UnitSprite> {
    mix.get(name)
        .and_then(|b| UnitSprite::from_shp_bytes(b).ok())
}

/// Load overlay tiles `<BASE>01.<SUF>`..`<BASE>NN.<SUF>` from a theater mix (the
/// ore/gem density tiles are SHP-format despite the theater extension). M7.
fn load_overlay_tiles(
    theater: &MixArchive,
    base: &str,
    count: usize,
    suffix: &str,
) -> Vec<UnitSprite> {
    (1..=count)
        .filter_map(|i| load_shp_opt(theater, &format!("{base}{i:02}.{suffix}")))
        .collect()
}

/// Resolve the short name of a buildable (for cameo `<NAME>ICON.SHP` lookup).
fn buildable_name(catalog: &Catalog, item: BuildItem) -> Option<String> {
    match item {
        BuildItem::Building(id) => catalog.building(id).map(|p| p.name.clone()),
        BuildItem::Unit(id) => catalog.unit(id).map(|p| p.name.clone()),
    }
}

/// Load the M7 cosmetic art set (ore/gem tiles, explosion + per-building buildup
/// anims, sidebar cameos) and install it on `core`. Every piece is optional:
/// missing art degrades to the flat-rectangle / no-anim / text-row fallbacks, so
/// this never fails the load. `theater`/`hires` may be absent.
#[allow(clippy::too_many_arguments)]
fn install_cosmetic_art(
    core: &mut AppCore,
    catalog: &Catalog,
    buildables: &[BuildItem],
    conquer: &MixArchive,
    theater: Option<&MixArchive>,
    theater_suffix: &str,
    hires: Option<&MixArchive>,
) {
    // Ore / gem overlay tiles (GOLD01..04 / GEM01..04).
    if let Some(t) = theater {
        core.set_ore_art(
            load_overlay_tiles(t, "GOLD", 4, theater_suffix),
            load_overlay_tiles(t, "GEM", 4, theater_suffix),
        );
    }
    // Explosion (shared) + per-building construction buildup (<NAME>MAKE.SHP).
    let explosion: Vec<UnitSprite> = load_shp_opt(conquer, "FBALL1.SHP").into_iter().collect();
    let buildups: Vec<Option<UnitSprite>> = catalog
        .buildings
        .iter()
        .map(|b| load_shp_opt(conquer, &format!("{}MAKE.SHP", b.name.to_ascii_uppercase())))
        .collect();
    core.set_effect_art(explosion, buildups);
    // Sidebar cameos (<NAME>ICON.SHP from hires.mix), parallel to `buildables`.
    if let Some(h) = hires {
        let cameos: Vec<Option<UnitSprite>> = buildables
            .iter()
            .map(|&item| {
                buildable_name(catalog, item)
                    .and_then(|n| load_shp_opt(h, &format!("{}ICON.SHP", n.to_ascii_uppercase())))
            })
            .collect();
        core.set_cameo_art(cameos);
    }
    core.enable_radar();
}

/// Build the starter catalog (CONST/POWR/PROC/WEAP + MCV/HARV/1TNK/2TNK/JEEP)
/// from rules.ini and the building/unit SHPs in `conquer.mix`.
pub fn build_content(
    rules: &Ini,
    conquer: &MixArchive,
    lores: Option<&MixArchive>,
) -> Result<GameContent, Box<dyn Error>> {
    // --- Buildings (ids fixed by declaration order) ---
    // (name, is_construction_yard, is_refinery, is_war_factory, is_barracks)
    let bspecs = [
        ("FACT", true, false, false, false),
        ("POWR", false, false, false, false),
        ("PROC", false, true, false, false),
        ("WEAP", false, false, true, false),
        ("TENT", false, false, false, true), // Allied barracks (infantry factory)
    ];
    let mut buildings = Vec::new();
    let mut building_sprites = Vec::new();
    let mut building_overlays = Vec::new();
    for (id, (name, is_cy, is_ref, is_wf, is_barr)) in bspecs.iter().enumerate() {
        let stats = building_stats(rules, name)
            .ok_or_else(|| format!("no building stats/footprint for {name}"))?;
        building_sprites.push(load_unit_sprite(conquer, name)?);
        // The war factory is two shapes in the original: WEAP (base) plus the
        // WEAP2 roof/door overlay drawn on top (building.cpp:513, bdata.cpp:3052).
        // Missing overlay art degrades gracefully to the base shape alone.
        building_overlays.push(if *is_wf {
            load_unit_sprite(conquer, "weap2").ok()
        } else {
            None
        });
        buildings.push(BuildingProto {
            name: name.to_string(),
            foot_w: stats.foot_w,
            foot_h: stats.foot_h,
            max_health: stats.strength,
            armor: stats.armor,
            power: stats.power,
            cost: stats.cost,
            prereq: prereq_ids(&stats.prereq),
            is_refinery: *is_ref,
            is_construction_yard: *is_cy,
            is_war_factory: *is_wf,
            is_barracks: *is_barr,
            free_harvester_unit: if *is_ref { Some(U_HARV) } else { None },
            sight: stats.sight,
            sprite_id: id as u32,
        });
    }

    // --- Units (ids fixed by declaration order) ---
    // (name, is_harvester, deploys_to, is_infantry, locomotor)
    let uspecs: [(&str, bool, Option<u32>, bool, u8); 8] = [
        ("MCV", false, Some(B_FACT), false, LOCO_TRACK as u8),
        ("HARV", true, None, false, LOCO_TRACK as u8),
        ("1TNK", false, None, false, LOCO_TRACK as u8),
        ("2TNK", false, None, false, LOCO_TRACK as u8),
        ("JEEP", false, None, false, LOCO_WHEEL as u8),
        ("E1", false, None, true, LOCO_FOOT as u8),
        ("E2", false, None, true, LOCO_FOOT as u8),
        ("E3", false, None, true, LOCO_FOOT as u8),
    ];
    let mut units = Vec::new();
    let mut unit_sprites = Vec::new();
    let mut infantry_anim: Vec<Option<InfantryAnim>> = Vec::new();
    // Vehicle art is in conquer.mix; infantry art in lores.mix (redalert.mix).
    let inf_archives: Vec<&MixArchive> = match lores {
        Some(l) => vec![conquer, l],
        None => vec![conquer],
    };
    for (id, (name, is_harv, deploys_to, is_inf, loco)) in uspecs.iter().enumerate() {
        let ustats = unit_stats(rules, name).ok_or_else(|| format!("no unit stats for {name}"))?;
        let combat = resolve_unit_combat(rules, name);
        unit_sprites.push(if *is_inf {
            // Infantry art (lores.mix) is optional: if it is absent the infantry
            // still exist in the catalog (buildable, simulated) with no sprite —
            // the renderer skips a frameless sprite, exactly as cosmetic art
            // degrades elsewhere. Keeps headless/AI harnesses (no lores) working.
            load_unit_sprite_from(&inf_archives, name)
                .unwrap_or_else(|_| UnitSprite { frames: Vec::new() })
        } else {
            load_unit_sprite(conquer, name)?
        });
        infantry_anim.push(if *is_inf {
            Some(InfantryAnim::for_name(name))
        } else {
            None
        });
        let cost = rules.get_int(name, "Cost").unwrap_or(0) as i32;
        let prereq = rules
            .get(name, "Prerequisite")
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty() && t != "none")
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        units.push(UnitProto {
            name: name.to_string(),
            sprite_id: id as u32,
            max_health: ustats.strength.clamp(1, u16::MAX as i32) as u16,
            stats: MoveStats {
                max_speed: ustats.max_speed_leptons(),
                rot: ustats.rot,
            },
            armor: combat.as_ref().map(|c| c.armor).unwrap_or(0),
            weapon: combat
                .as_ref()
                .and_then(|c| c.weapon.as_ref().map(weapon_to_profile)),
            has_turret: combat.as_ref().map(|c| c.has_turret).unwrap_or(false),
            is_harvester: *is_harv,
            is_infantry: *is_inf,
            locomotor: *loco,
            deploys_to: *deploys_to,
            cost,
            prereq: prereq_ids(&prereq),
            sight: ustats.sight,
        });
    }

    let catalog = Catalog {
        buildings,
        units,
        econ: econ_rules(rules),
    };
    // Sidebar order: structures then vehicles then infantry (construction yard +
    // MCV excluded — the yard comes from deploy, the MCV needs a service depot we
    // don't model).
    let buildables = vec![
        BuildItem::Building(B_POWR),
        BuildItem::Building(B_PROC),
        BuildItem::Building(B_WEAP),
        BuildItem::Building(B_TENT),
        BuildItem::Unit(U_1TNK),
        BuildItem::Unit(U_2TNK),
        BuildItem::Unit(U_JEEP),
        BuildItem::Unit(U_HARV),
        BuildItem::Unit(U_E1),
        BuildItem::Unit(U_E2),
        BuildItem::Unit(U_E3),
    ];
    Ok(GameContent {
        catalog,
        unit_sprites,
        building_sprites,
        building_overlays,
        buildables,
        infantry_anim,
    })
}

/// A fully wired M5 economy game: an [`AppCore`] with the build sidebar enabled,
/// a controlled house, an ore overlay, and a starter MCV to deploy.
pub struct EconGame {
    /// The ready-to-drive core (sidebar enabled, camera on the MCV).
    pub core: AppCore,
    /// The controlled ("player") house index.
    pub controlled: u8,
    /// The starter MCV's handle.
    pub mcv: Handle,
    /// The cell the MCV starts on (its construction yard centres here on deploy).
    pub start_cell: CellCoord,
    /// A nearby ore cell (for reporting / camera framing).
    pub ore_cell: Option<CellCoord>,
    /// The unit-proto id used for the MCV (== `U_MCV`).
    pub mcv_unit_id: u32,
}

/// Load a fully playable M5 economy game from the archives under `dir`.
pub fn load_econ_from_dir(
    dir: &Path,
    scenario_name: &str,
    starting_credits: i32,
) -> Result<EconGame, Box<dyn Error>> {
    let main_bytes =
        std::fs::read(dir.join("main.mix")).map_err(|e| format!("reading main.mix: {e}"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))
        .map_err(|e| format!("reading redalert.mix: {e}"))?;
    load_econ_from_bytes(
        &main_bytes,
        &redalert_bytes,
        scenario_name,
        starting_credits,
    )
}

/// Build the economy game from in-memory archives: terrain + palette + remaps,
/// the starter catalog + sprites, ore from the scenario overlay, all eight
/// houses, a controlled house (from `[Basic] Player=`, else Greece), and a
/// starter MCV placed on open ground near an ore field.
pub fn load_econ_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
    starting_credits: i32,
) -> Result<EconGame, Box<dyn Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    // Terrain raster + palette (reuse the terrain path).
    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let raster = rasterize(&loaded.scenario, &loaded.tiles);
    let palette = loaded.palette;
    let scenario = loaded.scenario;

    // rules.ini + PALETTE.CPS remaps + the [Basic] player house.
    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found"))?;
    let scen_ini = Ini::parse(&String::from_utf8_lossy(ini_bytes));

    let local = redalert.open_nested("local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(
        local.get("rules.ini").ok_or("rules.ini not found")?,
    ));
    let remaps: Vec<RemapTable> = match local.get("palette.cps") {
        Some(cps_bytes) => match Cps::parse(cps_bytes) {
            Ok(cps) => build_house_remaps(&cps).to_vec(),
            Err(_) => vec![identity_remap(); HOUSE_COUNT],
        },
        None => vec![identity_remap(); HOUSE_COUNT],
    };
    let controlled = scen_ini
        .get("Basic", "Player")
        .and_then(house_from_name)
        .unwrap_or(1); // default Greece

    // Catalog + sprites.
    let conquer = main.open_nested("conquer.mix")?;
    let lores = redalert.open_nested("lores.mix").ok();
    let content = build_content(&rules, &conquer, lores.as_ref())?;

    // World: passability + houses + ore + catalog.
    let grid = make_passability(&scenario, &loaded.tiles, &rules);
    let mut world = World::new(grid, 0x1234_5678);
    world.set_catalog(content.catalog.clone());
    world.init_houses(HOUSE_COUNT, starting_credits);
    world.set_ore(OreField::from_overlay(128, 128, &scenario.overlay));
    // Ore growth/spread is on whenever rules.ini enables it (default yes). This is
    // the deferred M5 step; it legitimately consumes the sync RNG (see the
    // ore-growth pin update in `ui_economy_determinism`).
    let (grows, spreads) = ore_growth_flags(&rules);
    world.set_ore_growth(grows, spreads);

    // Pick a base start on open ground near ore, within the playable rect.
    let near = CellCoord::new(
        scenario.map_x as i32 + scenario.map_width as i32 / 2,
        scenario.map_y as i32 + scenario.map_height as i32 / 2,
    );
    let (start_cell, ore_cell) = find_base_start(world.passability(), &world.ore, near);

    // Spawn the starter MCV for the controlled house.
    let mcv_proto = &content.catalog.units[U_MCV as usize];
    let mcv = world.spawn_unit(
        mcv_proto.sprite_id,
        controlled,
        start_cell,
        Facing(0),
        mcv_proto.max_health,
        mcv_proto.stats,
    );
    world.set_unit_max_health(mcv, mcv_proto.max_health);
    world.set_unit_combat(mcv, mcv_proto.armor, mcv_proto.weapon, mcv_proto.has_turret);
    world.set_unit_sight(mcv, mcv_proto.sight);

    let mut core = AppCore::with_sim(raster, palette, world, content.unit_sprites, remaps);
    core.set_building_sprites(content.building_sprites);
    core.set_building_overlays(content.building_overlays);
    core.set_infantry_anim(content.infantry_anim.clone());
    core.enable_sidebar(controlled, content.buildables.clone());
    // M7 cosmetic art (ore/gem tiles, explosion/buildup anims, cameos, radar) so
    // the econ view shows real ore fields thinning as they are harvested.
    let theater_mix = main.open_nested(scenario.theater.mix_name()).ok();
    let hires = redalert.open_nested("hires.mix").ok();
    install_cosmetic_art(
        &mut core,
        &content.catalog,
        &content.buildables,
        &conquer,
        theater_mix.as_ref(),
        scenario.theater.suffix(),
        hires.as_ref(),
    );

    Ok(EconGame {
        core,
        controlled,
        mcv,
        start_cell,
        ore_cell,
        mcv_unit_id: U_MCV,
    })
}

// ===========================================================================
// M6 skirmish: player house + 1 AI house, shroud, ore growth, win/lose.
// ===========================================================================

/// A fully wired M6 skirmish: player house vs one AI house, each starting with an
/// MCV and starting credits, on a multiplayer map with the shroud enabled and
/// ore growing. This is the "first playable" configuration `window` boots.
pub struct SkirmishGame {
    /// The ready-to-drive core (sidebar + shroud enabled, camera on the player).
    pub core: AppCore,
    /// The controlled player house index.
    pub player_house: u8,
    /// The player MCV's start cell (camera framing).
    pub player_start: CellCoord,
    /// The AI-controlled house index.
    pub ai_house: u8,
    /// The AI MCV's start cell.
    pub ai_start: CellCoord,
}

/// Load an M6 skirmish from the archives under `dir`. `difficulty` tunes the AI.
pub fn load_skirmish_from_dir(
    dir: &Path,
    scenario_name: &str,
    starting_credits: i32,
    difficulty: ra_sim::Difficulty,
) -> Result<SkirmishGame, Box<dyn Error>> {
    let main_bytes =
        std::fs::read(dir.join("main.mix")).map_err(|e| format!("reading main.mix: {e}"))?;
    let redalert_bytes = std::fs::read(dir.join("redalert.mix"))
        .map_err(|e| format!("reading redalert.mix: {e}"))?;
    load_skirmish_from_bytes(
        &main_bytes,
        &redalert_bytes,
        scenario_name,
        starting_credits,
        difficulty,
    )
}

/// Build the skirmish from in-memory archives: terrain/palette/remaps + the
/// starter catalog + ore (growing), the shroud enabled, and two houses (the
/// `[Basic] Player=` house vs an AI house) each seeded with an MCV at a
/// multiplayer start waypoint.
pub fn load_skirmish_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
    starting_credits: i32,
    difficulty: ra_sim::Difficulty,
) -> Result<SkirmishGame, Box<dyn Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let raster = rasterize(&loaded.scenario, &loaded.tiles);
    let palette = loaded.palette;
    let scenario = loaded.scenario;

    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found"))?;
    let scen_ini = Ini::parse(&String::from_utf8_lossy(ini_bytes));

    let local = redalert.open_nested("local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(
        local.get("rules.ini").ok_or("rules.ini not found")?,
    ));
    let remaps: Vec<RemapTable> = match local.get("palette.cps") {
        Some(cps_bytes) => match Cps::parse(cps_bytes) {
            Ok(cps) => build_house_remaps(&cps).to_vec(),
            Err(_) => vec![identity_remap(); HOUSE_COUNT],
        },
        None => vec![identity_remap(); HOUSE_COUNT],
    };

    // Player house from [Basic] Player (default Greece=1); AI is a distinct
    // house (USSR=2, or Spain=0 if the player already is USSR).
    let player_house = scen_ini
        .get("Basic", "Player")
        .and_then(house_from_name)
        .unwrap_or(1);
    let ai_house = if player_house == 2 { 0 } else { 2 };

    let conquer = main.open_nested("conquer.mix")?;
    let lores = redalert.open_nested("lores.mix").ok();
    let content = build_content(&rules, &conquer, lores.as_ref())?;

    let grid = make_passability(&scenario, &loaded.tiles, &rules);
    let mut world = World::new(grid, 0x1234_5678);
    world.set_catalog(content.catalog.clone());
    world.init_houses(HOUSE_COUNT, starting_credits);
    world.set_ore(OreField::from_overlay(128, 128, &scenario.overlay));
    world.enable_shroud();
    let (grows, spreads) = ore_growth_flags(&rules);
    world.set_ore_growth(grows, spreads);
    world.set_player_house(player_house);
    world.set_ai(vec![ra_sim::AiPlayer::new(ai_house, difficulty)]);

    // Two well-separated starts, preferring multiplayer waypoints.
    let (player_start, ai_start) = pick_two_starts(&world, &scenario, &scen_ini);

    // Spawn an MCV for each house at its start.
    let mcv_proto = content.catalog.units[U_MCV as usize].clone();
    let spawn_mcv = |world: &mut World, house: u8, cell: CellCoord| -> Handle {
        let h = world.spawn_unit(
            mcv_proto.sprite_id,
            house,
            cell,
            Facing(0),
            mcv_proto.max_health,
            mcv_proto.stats,
        );
        world.set_unit_max_health(h, mcv_proto.max_health);
        world.set_unit_combat(h, mcv_proto.armor, mcv_proto.weapon, mcv_proto.has_turret);
        world.set_unit_sight(h, mcv_proto.sight);
        h
    };
    spawn_mcv(&mut world, player_house, player_start);
    spawn_mcv(&mut world, ai_house, ai_start);

    let mut core = AppCore::with_sim(raster, palette, world, content.unit_sprites, remaps);
    core.set_building_sprites(content.building_sprites);
    core.set_building_overlays(content.building_overlays);
    core.set_infantry_anim(content.infantry_anim.clone());
    core.enable_sidebar(player_house, content.buildables.clone());

    // M7 cosmetic art: ore/gem tiles, explosion + buildup anims, cameos, radar.
    let theater_mix = main.open_nested(scenario.theater.mix_name()).ok();
    let hires = redalert.open_nested("hires.mix").ok();
    install_cosmetic_art(
        &mut core,
        &content.catalog,
        &content.buildables,
        &conquer,
        theater_mix.as_ref(),
        scenario.theater.suffix(),
        hires.as_ref(),
    );

    Ok(SkirmishGame {
        core,
        player_house,
        player_start,
        ai_house,
        ai_start,
    })
}

/// Choose two well-separated base starts **on the same landmass** — so a
/// ground-only skirmish (no transports yet) can actually resolve. The player
/// takes the first multiplayer `[Waypoints]` start (or the map centre); the AI
/// takes the farthest base-buildable cell that is BFS-reachable from the player
/// over passable terrain. Guaranteeing connectivity avoids the naval-map trap
/// where one base sits on an unreachable island the assault can never finish.
fn pick_two_starts(world: &World, scenario: &Scenario, scen_ini: &Ini) -> (CellCoord, CellCoord) {
    let passable = world.passability();
    let ore = &world.ore;

    // Player start: waypoint 0 if present, else the playable-rect centre.
    let player = scen_ini
        .section_entries("Waypoints")
        .and_then(|e| {
            e.iter()
                .filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.parse::<u32>().ok()?)))
                .filter(|(idx, _)| *idx < 8)
                .min_by_key(|(idx, _)| *idx)
                .map(|(_, cell)| CellCoord::from_index(cell))
        })
        .map(|near| find_base_start(passable, ore, near).0)
        .unwrap_or_else(|| {
            let c = CellCoord::new(
                scenario.map_x as i32 + scenario.map_width as i32 / 2,
                scenario.map_y as i32 + scenario.map_height as i32 / 2,
            );
            find_base_start(passable, ore, c).0
        });

    // BFS the connected passable component from the player start. The AI base is
    // held to a *radius-4* open plain (a 9×9 clear) so it sits on simple,
    // fully-reachable terrain — enough room for a real base whose production core
    // a ground assault can actually reach and destroy, rather than a fringe cell
    // wedged against water/cliffs that leaves an unfinishable remnant.
    let open5 = |c: CellCoord| -> bool {
        for dy in -4..=4 {
            for dx in -4..=4 {
                if !passable.is_passable(CellCoord::new(c.x + dx, c.y + dy)) {
                    return false;
                }
            }
        }
        true
    };
    let near_ore = |c: CellCoord| -> bool {
        for dy in -10..=10 {
            for dx in -10..=10 {
                if ore.has_ore(CellCoord::new(c.x + dx, c.y + dy)) {
                    return true;
                }
            }
        }
        false
    };
    let (w, h) = (passable.width(), passable.height());
    let idx = |c: CellCoord| (c.y * w + c.x) as usize;
    let mut seen = vec![false; (w * h) as usize];
    let mut queue = std::collections::VecDeque::new();
    if passable.is_passable(player) {
        seen[idx(player)] = true;
        queue.push_back(player);
    }
    // Farthest connected base-buildable cell near ore (a viable AI economy), with
    // a plain open-5×5 fallback if nothing reachable sits near ore.
    let key = |c: CellCoord| -> i64 {
        let dx = (c.x - player.x) as i64;
        let dy = (c.y - player.y) as i64;
        dx * dx + dy * dy
    };
    let mut best_ore: Option<CellCoord> = None;
    let mut best_open: Option<CellCoord> = None;
    while let Some(c) = queue.pop_front() {
        if open5(c) {
            if best_open.map(|b| key(c) > key(b)).unwrap_or(true) {
                best_open = Some(c);
            }
            if near_ore(c) && best_ore.map(|b| key(c) > key(b)).unwrap_or(true) {
                best_ore = Some(c);
            }
        }
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = CellCoord::new(c.x + dx, c.y + dy);
            if n.x < 0 || n.y < 0 || n.x >= w || n.y >= h {
                continue;
            }
            if !seen[idx(n)] && passable.is_passable(n) {
                seen[idx(n)] = true;
                queue.push_back(n);
            }
        }
    }

    // The AI base is the farthest BFS-connected cell that is near ore (so the AI
    // has a viable economy) — fully ground-reachable from the player by
    // construction, with a bare open-cell fallback. We deliberately do *not* use
    // a raw multiplayer waypoint: on naval maps those sit across water the AI
    // then expands onto, leaving an unreachable remnant the assault can't finish.
    let ai = best_ore.or(best_open).unwrap_or(player);
    (player, ai)
}

/// Find a base start: a cell whose 5×5 neighbourhood is passable (room for the
/// construction yard + expansion) and that is within ~10 cells of an ore field,
/// searching outward from `near`. Falls back to any wide-open cell, then `near`.
/// Returns `(start_cell, nearest_ore_cell)`.
pub fn find_base_start(
    passable: &Passability,
    ore: &OreField,
    near: CellCoord,
) -> (CellCoord, Option<CellCoord>) {
    let open5 = |c: CellCoord| -> bool {
        for dy in -2..=2 {
            for dx in -2..=2 {
                if !passable.is_static_passable(CellCoord::new(c.x + dx, c.y + dy)) {
                    return false;
                }
            }
        }
        true
    };
    let nearest_ore = |c: CellCoord| -> Option<(i32, CellCoord)> {
        let mut best: Option<(i32, CellCoord)> = None;
        for dy in -12..=12 {
            for dx in -12..=12 {
                let o = CellCoord::new(c.x + dx, c.y + dy);
                if ore.has_ore(o) {
                    let d = dx.abs().max(dy.abs());
                    if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                        best = Some((d, o));
                    }
                }
            }
        }
        best
    };

    // Spiral outward from `near` looking for an open cell close to ore.
    for r in 0i32..60 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue; // ring only
                }
                let c = CellCoord::new(near.x + dx, near.y + dy);
                if open5(c) {
                    if let Some((d, ocell)) = nearest_ore(c) {
                        if d <= 10 {
                            return (c, Some(ocell));
                        }
                    }
                }
            }
        }
    }
    // Fallback: first open cell anywhere on the playable band.
    for y in 2..126 {
        for x in 2..126 {
            let c = CellCoord::new(x, y);
            if open5(c) {
                return (c, nearest_ore(c).map(|(_, o)| o));
            }
        }
    }
    (near, None)
}

/// Lift a `ra-data` resolved [`WeaponDef`] (plain numbers) into the sim's
/// runtime `WeaponProfile`, exactly as `MoveStats` is lifted from `UnitStats`.
/// The crate split keeps `ra-sim` off the INI layer (DESIGN.md §4.1).
pub fn weapon_to_profile(w: &WeaponDef) -> ra_sim::WeaponProfile {
    ra_sim::WeaponProfile {
        damage: w.damage,
        rof: w.rof,
        range: w.range,
        proj_speed: w.proj_speed,
        proj_rot: w.proj_rot,
        invisible: w.invisible,
        instant: w.instant,
        warhead: ra_sim::WarheadProfile {
            spread: w.spread,
            verses: w.verses,
        },
        warhead_ap: w.warhead_ap,
        arcing: w.arcing,
        ballistic_scatter: w.ballistic_scatter,
        homing_scatter: w.homing_scatter,
        min_damage: w.min_damage,
        max_damage: w.max_damage,
    }
}
