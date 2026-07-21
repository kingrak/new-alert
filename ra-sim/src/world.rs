//! `World` — the single explicit game-state value (DESIGN.md §3.2, §4.2) and
//! the command pipeline that is the *only* way to mutate it (§4.4).
//!
//! Each tick runs a fixed, explicit sequence of systems — **commands, then
//! movement** — over arenas iterated in slot order, with one seeded RNG owned
//! here. At the end of a tick the whole mutable state is folded into a 64-bit
//! FNV-1a hash (§4.2): the hash chain is the determinism backbone, asserted in
//! replays and multiplayer alike.

use crate::ai::{AiPlayer, Difficulty};
use crate::arena::{Arena, Handle};
use crate::building::Building;
use crate::bullet::Bullet;
use crate::campaign::{self, Campaign};
use crate::catalog::Catalog;
use crate::combat::{aligned_to_fire, modify_damage, Target, WeaponProfile};
use crate::coords::{
    coord_move, isqrt, leptons_distance, spot_index, CellCoord, Facing, Locomotor, WorldCoord,
    LEPTONS_PER_CELL, MAP_CELL_H, MAP_CELL_W, SUBCELL_COUNT,
};
use crate::hash::Fnv1a;
use crate::house::{BuildItem, House, ProdKind, Production};
use crate::occupancy::UnitGrid;
use crate::ore::OreField;
use crate::path::{find_path, find_path_avoiding, Passability};
use crate::rng::RandomLcg;
use crate::shroud::Shroud;
use crate::unit::{AirState, HarvStatus, Mission, MoveStats, Passenger, Unit, FLIGHT_LEVEL};

/// A player order. Every command carries the **issuing house** explicitly
/// (§4.6): ownership is validated by the sim, never inferred from a connection,
/// so the same schema serves single-player, LAN, and relay play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Order `unit` to move to `dest` (a cell). Ignored if the issuing house
    /// does not own the unit, the unit is stale, or no path exists. Clears any
    /// attack target (a move order overrides an attack, like the original).
    Move {
        /// The unit to move.
        unit: Handle,
        /// Destination cell.
        dest: CellCoord,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Order `unit` to halt where it is (and stop attacking).
    Stop {
        /// The unit to stop.
        unit: Handle,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Order `unit` to attack `target` — an enemy unit handle, or a ground cell
    /// for force-fire. Ignored if the issuing house does not own `unit`, the
    /// unit is stale, or the unit has no weapon. The unit will approach until in
    /// range, aim, and fire on ROF cadence. This is the TarCom assignment.
    Attack {
        /// The attacking unit (must belong to `house` and be armed).
        unit: Handle,
        /// What to shoot at.
        target: Target,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Deploy an MCV into a construction yard (`unit.cpp:1437`). The yard's 3×3
    /// footprint is placed with its top-left at the MCV's cell minus (1,1) — the
    /// original's `Adjacent_Cell(cell, FACING_NW)` origin — so it sits centred on
    /// the MCV. Ignored unless the issuing house owns the MCV, the unit is an MCV
    /// (`UnitProto::deploys_to`), and the footprint is a legal placement.
    Deploy {
        /// The MCV to deploy.
        unit: Handle,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Begin producing a building or unit at `house`'s factory. Validated
    /// against prerequisites, the required factory (construction yard for
    /// buildings, war factory for vehicles), funds, and the one-slot-per-kind
    /// rule. Deducts nothing up front — cost is paid in installments as it
    /// builds (`FactoryClass::AI`).
    StartProduction {
        /// House issuing the order.
        house: u8,
        /// What to build.
        item: BuildItem,
    },
    /// Place a completed building at `cell` (its footprint top-left). Requires
    /// the house's structure production to be finished and to match
    /// `building`. Validated against the footprint (on-map, passable,
    /// unoccupied) and the proximity-to-own-building rule
    /// (`Passes_Proximity_Check`, `display.cpp:671`). A refinery spawns its free
    /// harvester on placement (`building.cpp:2640`).
    PlaceBuilding {
        /// House issuing the order.
        house: u8,
        /// Building type id being placed (must match the ready structure).
        building: u32,
        /// Footprint top-left cell.
        cell: CellCoord,
    },
    /// Cancel `house`'s in-progress production of the given kind, refunding the
    /// credits spent so far (`FactoryClass::Abandon`, money refunded).
    CancelProduction {
        /// House issuing the order.
        house: u8,
        /// Which lane to cancel.
        kind: ProdKind,
    },
    /// Sell one of `house`'s own buildings, refunding `Rule.RefundPercent` of its
    /// cost (default 50%, `techno.cpp:6417`) and clearing its footprint. Ignored
    /// if the issuing house does not own the building or it is already gone.
    Sell {
        /// House issuing the order (must own `building`).
        house: u8,
        /// The building to sell.
        building: Handle,
    },
    /// Toggle repair on one of `house`'s own buildings (`BuildingClass::Repair(-1)`,
    /// `building.cpp:2725`). While repairing, the building heals on the global
    /// repair cadence, charging `RepairPercent × RepairStep` of its cost per step
    /// until full or the house runs out of money. Ignored if the issuing house
    /// does not own the building, it is a wall, or it is already gone (M7.9 P1).
    Repair {
        /// House issuing the order (must own `building`).
        house: u8,
        /// The building to toggle repair on.
        building: Handle,
    },
    /// Order an infantry `passenger` to board `transport` (an own transport with
    /// spare `Passengers=` capacity). If already adjacent it boards immediately;
    /// otherwise it walks to the transport and boards on arrival (M7.5-B P1).
    /// Ignored unless `house` owns both and there is room.
    Load {
        /// The infantry to load.
        passenger: Handle,
        /// The transport to board.
        transport: Handle,
        /// House issuing the order (must own both).
        house: u8,
    },
    /// Order `transport` to unload all its passengers to free adjacent spots
    /// (`Mission_Unload`). Ignored unless `house` owns it and it carries cargo.
    Unload {
        /// The transport to unload.
        transport: Handle,
        /// House issuing the order (must own `transport`).
        house: u8,
    },
}

/// The complete simulation state. Fields are plain and serialisable; there are
/// no back-pointers, no `HashMap` iteration, no floats.
#[derive(Clone, Debug)]
pub struct World {
    /// Live movable units, addressed by generational handle.
    pub units: Arena<Unit>,
    /// Placed buildings — the second entity arena (§4.9 M5, §5 per-kind arenas).
    pub buildings: Arena<Building>,
    /// Projectiles in flight (their own arena, per §5's per-kind arena plan).
    pub bullets: Arena<Bullet>,
    /// Per-house economic state (credits, power, production), indexed by house.
    pub houses: Vec<House>,
    /// The map's harvestable ore overlay.
    pub ore: OreField,
    /// Immutable build data (footprints, costs, prerequisites, stats).
    pub catalog: Catalog,
    /// Map passability grid: static terrain + dynamic building occupancy.
    passable: Passability,
    /// Per-house explored/shroud state (M6). Disabled until a skirmish enables it.
    pub shroud: Shroud,
    /// Ore growth/spread scheduler state (M6). `None` until growth is enabled.
    ore_growth: Option<OreGrowth>,
    /// Skirmish AI controllers, one per AI-controlled house (M6). Run in
    /// house-index order each tick before player commands (see [`apply`]).
    ai: Vec<AiPlayer>,
    /// The tracked player house for win/lose, if this is a skirmish (M6).
    player_house: Option<u8>,
    /// Terminal game state once a house-elimination check resolves (M6).
    game_over: GameOver,
    /// The sim RNG, seeded and owned here.
    rng: RandomLcg,
    /// The current tick number (advances once per [`World::tick`]).
    tick_count: u32,
    /// Campaign scenario-scripting state (triggers/teamtypes/waypoints/globals),
    /// or `None` for a skirmish. Evaluated by `run_campaign`; hashed only when
    /// present, so every skirmish world is byte-identical (M7.5).
    campaign: Option<Campaign>,
    /// House alliance matrix: `alliances[a]` has bit `b` set when house `a` is
    /// allied with house `b` (`HouseClass::Is_Ally`, `house.cpp`). `None` in a
    /// skirmish (where "ally" means "same house"). Hashed only when present.
    alliances: Option<Vec<u64>>,
    /// Campaign enemy-activation state (M7.5-C): the `IsAlerted`/`IsStarted`
    /// latches, `AlertTime`, and the `[Base]` rebuild list that drive scripted
    /// computer-house autocreate teams + production. `None` for a skirmish and for
    /// campaigns that never touch it; hashed only when a house has actually been
    /// alerted or begun production (`run_enemy_activation`).
    enemy_activation: Option<crate::campaign::EnemyActivation>,
}

/// Terminal outcome of a skirmish, from the player's point of view (M6, item 4).
/// House elimination = **all buildings AND all units destroyed** (the classic
/// multiplayer defeat check, `house.cpp:1290`: `!ActiveBScan && !UScan && …`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GameOver {
    /// The game is still in progress.
    #[default]
    Ongoing,
    /// The tracked player house won (every enemy house eliminated).
    Victory,
    /// The tracked player house was eliminated.
    Defeat,
}

/// Ore growth/spread scheduler state — a faithful port of the `MapClass::Logic`
/// incremental scan (`map.cpp:1308-1432`). A persistent cursor sweeps the map a
/// slice per tick, reservoir-sampling grow/spread candidates (drawing the sync
/// RNG); one grow+spread wave fires each time the cursor wraps the whole map.
#[derive(Clone, Debug, Default)]
struct OreGrowth {
    /// Whether ore density grows (`Rule.IsTGrowth` ← `[General] OreGrows`).
    grows: bool,
    /// Whether dense ore spreads to neighbours (`Rule.IsTSpread` ← `OreSpreads`).
    spreads: bool,
    /// The scan cursor (`TiberiumScan`), a linear cell index.
    scan: i32,
    /// Accumulated grow candidates this sweep (`TiberiumGrowth[]`, cap 64).
    grow_list: Vec<CellCoord>,
    /// Grow-eligible cells seen this sweep (`TiberiumGrowthExcess`).
    grow_excess: i32,
    /// Accumulated spread candidates this sweep (`TiberiumSpread[]`, cap 64).
    spread_list: Vec<CellCoord>,
    /// Spread-eligible cells seen this sweep (`TiberiumSpreadExcess`).
    spread_excess: i32,
}

/// `TiberiumGrowth` / `TiberiumSpread` array capacity (`MAP_CELL_W / 2 = 64`).
const TIB_LIST_CAP: usize = 64;

impl World {
    /// Create a world over a passability grid, seeding the sim RNG. Houses, the
    /// ore field, and the build catalog start empty — movement/combat-only
    /// worlds (M3/M4 tests) never touch them; the loader populates them for M5.
    pub fn new(passable: Passability, seed: u32) -> World {
        let (w, h) = (passable.width(), passable.height());
        World {
            units: Arena::new(),
            buildings: Arena::new(),
            bullets: Arena::new(),
            houses: Vec::new(),
            ore: OreField::empty(w, h),
            catalog: Catalog::new(),
            passable,
            shroud: Shroud::new(w, h),
            ore_growth: None,
            ai: Vec::new(),
            player_house: None,
            game_over: GameOver::Ongoing,
            rng: RandomLcg::new(seed),
            tick_count: 0,
            campaign: None,
            alliances: None,
            enemy_activation: None,
        }
    }

    /// Install a campaign's scenario-scripting state (M7.5). Presence flips
    /// win/lose from "eliminate every AI" to trigger-driven (see
    /// `update_game_over`).
    pub fn set_campaign(&mut self, campaign: Campaign) {
        self.campaign = Some(campaign);
    }

    /// Install the campaign enemy-activation state (M7.5-C). The loader resolves
    /// the `[Base]` list + tech level and sizes the per-house latch vectors; the
    /// `TACTION_AUTOCREATE`/`TACTION_BEGIN_PRODUCTION` actions flip the latches at
    /// runtime, and [`run_enemy_activation`] acts on them.
    pub fn set_enemy_activation(&mut self, ea: crate::campaign::EnemyActivation) {
        self.enemy_activation = Some(ea);
    }

    /// Borrow the enemy-activation state, if present (verification hook).
    pub fn enemy_activation(&self) -> Option<&crate::campaign::EnemyActivation> {
        self.enemy_activation.as_ref()
    }

    /// Apply single-player **campaign difficulty handicaps** (M7.5-C P0), matching
    /// `HouseClass::Assign_Handicap`'s campaign call sites: the reference constructs
    /// **every** house with the *computer* difficulty `Scen.CDifficulty`
    /// (house.cpp:742) and then overrides only the `Player=` house with the *player*
    /// difficulty `Scen.Difficulty` (scenario.cpp:2332). The classic difficulty
    /// slider maps a selection to a **pair** (init.cpp:681-705): the player is
    /// *buffed* on Easy and *nerfed* on Hard, the mirror of the computers.
    ///
    /// Our [`Catalog::difficulty_handicap`] table already inverts label→rules.ini
    /// section for AI opponents (a "Hard" AI gets the buffed `[Easy]` biases — see
    /// QUIRKS Q15), so the computer houses take `difficulty_handicap(chosen)`
    /// directly (`Scen.CDifficulty`), and the player takes the **inverse label**'s
    /// handicap (`Scen.Difficulty`): Easy game → player gets the buff, Hard game →
    /// player gets the nerf, Normal → neutral. On **Normal every house is neutral**
    /// (the `[Normal]` section is all-`1.0`), a byte-exact no-op that never perturbs
    /// a golden — the campaign default.
    pub fn set_campaign_difficulty(&mut self, player_house: u8, difficulty: Difficulty) {
        let computer = self.catalog.difficulty_handicap(difficulty);
        let player_diff = match difficulty {
            Difficulty::Easy => Difficulty::Hard, // player buffed ([Easy] section)
            Difficulty::Normal => Difficulty::Normal, // symmetric
            Difficulty::Hard => Difficulty::Easy, // player nerfed ([Difficult] section)
        };
        let player = self.catalog.difficulty_handicap(player_diff);
        for (i, h) in self.houses.iter_mut().enumerate() {
            h.handicap = if i as u8 == player_house {
                player
            } else {
                computer
            };
        }
    }

    /// Borrow the campaign scripting state, if this is a mission.
    pub fn campaign(&self) -> Option<&Campaign> {
        self.campaign.as_ref()
    }

    /// Mutable campaign state (the client drains cosmetic outputs — text/speech/
    /// reveal — through this).
    pub fn campaign_mut(&mut self) -> Option<&mut Campaign> {
        self.campaign.as_mut()
    }

    /// Install the house alliance matrix (bitmask per house). See
    /// [`World::are_allies`].
    pub fn set_alliances(&mut self, alliances: Vec<u64>) {
        self.alliances = Some(alliances);
    }

    /// Whether house `a` treats house `b` as an ally (never a target). A house is
    /// always its own ally; with no alliance matrix (skirmish) only same-house
    /// counts, so skirmish targeting is byte-identical to before.
    pub fn are_allies(&self, a: u8, b: u8) -> bool {
        if a == b {
            return true;
        }
        match &self.alliances {
            Some(m) => m
                .get(a as usize)
                .map(|&bits| bits & (1u64 << (b as u64 & 63)) != 0)
                .unwrap_or(false),
            None => false,
        }
    }

    /// Force the terminal game state (used by campaign WIN/LOSE trigger actions).
    /// First terminal state sticks.
    pub fn set_game_over(&mut self, over: GameOver) {
        if self.game_over == GameOver::Ongoing {
            self.game_over = over;
        }
    }

    /// Install the build catalog (footprints, costs, prerequisites, protos).
    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
    }

    /// Mark a cell as a static movement obstacle (campaign `[TERRAIN]` trees etc.
    /// — a render-only object with occupancy, `TerrainClass::Occupy_List`,
    /// `terrain.cpp`). Ground movers route around it or hold, exactly like a
    /// building footprint (M7.5 P1).
    pub fn block_cell(&mut self, cell: CellCoord) {
        self.passable.set_occupied(cell, true);
    }

    /// Install the ore overlay.
    pub fn set_ore(&mut self, ore: OreField) {
        self.ore = ore;
    }

    /// Create `n` houses, each starting with `credits`.
    pub fn init_houses(&mut self, n: usize, credits: i32) {
        self.houses = (0..n).map(|_| House::new(credits)).collect();
    }

    /// Enable the per-house shroud (skirmish setup). Until this is called every
    /// cell reads as explored, so movement/combat/economy worlds are unaffected.
    pub fn enable_shroud(&mut self) {
        self.shroud.enable();
    }

    /// Reveal the shroud disc around `cell` for `house` (public so the loader can
    /// pre-reveal a scenario's starting positions).
    pub fn reveal_shroud(&mut self, house: u8, cell: CellCoord, sight: u8) {
        self.shroud.reveal(house, cell, sight);
    }

    /// Enable ore growth (`grows`) and/or spread (`spreads`) — the deferred M5
    /// economy step (rules.ini `OreGrows`/`OreSpreads`). Off by default so
    /// existing worlds draw no sim RNG; a skirmish turns it on, at which point
    /// growth legitimately consumes the sync RNG (see [`run_ore_growth`]).
    pub fn set_ore_growth(&mut self, grows: bool, spreads: bool) {
        self.ore_growth = if grows || spreads {
            Some(OreGrowth {
                grows,
                spreads,
                ..Default::default()
            })
        } else {
            None
        };
    }

    /// Install the skirmish AI controllers (one per AI-controlled house). They
    /// run inside the sim each tick, issuing the same [`Command`]s a player would.
    ///
    /// Assigning an AI to a house also copies that difficulty's **stat handicap**
    /// (M7.9 P2a) from the catalog onto the house (`HouseClass::Assign_Handicap`,
    /// house.cpp:278): firepower/armor/ROF/groundspeed/cost/build-time biases that
    /// the combat, movement, and production sites then apply house-scoped. Houses
    /// with no AI (the human player) keep the neutral all-`1.0` handicap.
    /// Read-only view of the installed AI controllers (for tests/showcases to
    /// inspect AI decision state — designated enemy, rubber-band caps, active team).
    pub fn ai(&self) -> &[AiPlayer] {
        &self.ai
    }

    pub fn set_ai(&mut self, ai: Vec<AiPlayer>) {
        // A computer-controlled (skirmish/multiplayer) house runs at `Rule.MaxIQ`
        // (`scenario.cpp:2890`: `Session.Type != GAME_NORMAL → IQ = Rule.MaxIQ`),
        // which unlocks the IQ-gated automatic behaviours (scatter, harvester
        // replacement, guard-area). The human keeps the default IQ 0.
        let max_iq = self.catalog.econ.iq.max_iq;
        for a in &ai {
            let h = self.catalog.difficulty_handicap(a.difficulty);
            if let Some(house) = self.houses.get_mut(a.house as usize) {
                house.handicap = h;
                house.iq = max_iq;
            }
        }
        self.ai = ai;
    }

    /// Designate the tracked player house for win/lose resolution (skirmish).
    pub fn set_player_house(&mut self, house: u8) {
        self.player_house = Some(house);
    }

    /// Directly set a house's **IQ rating** (`HouseClass::IQ`, house.h). Normally
    /// [`World::set_ai`] does this (an AI house runs at `Rule.MaxIQ`); this seam
    /// lets a test give a house the IQ that gates its automatic behaviours (scatter,
    /// guard-area, harvester replacement) **without** installing an active
    /// controller — so the behaviour under test isn't perturbed by the AI issuing
    /// its own commands. `0` is the human default.
    pub fn set_house_iq(&mut self, house: u8, iq: i32) {
        if let Some(h) = self.houses.get_mut(house as usize) {
            h.iq = iq;
        }
    }

    /// The tracked player house, if one was designated.
    pub fn player_house(&self) -> Option<u8> {
        self.player_house
    }

    /// The difficulty of the AI controlling `house`, if any (verification hook —
    /// lets a scripted drive assert the chosen difficulty was threaded through).
    pub fn ai_difficulty(&self, house: u8) -> Option<Difficulty> {
        self.ai
            .iter()
            .find(|a| a.house == house)
            .map(|a| a.difficulty)
    }

    /// The current terminal game state (`Ongoing` until a house is eliminated).
    pub fn game_over(&self) -> GameOver {
        self.game_over
    }

    /// Whether `house` is still alive — it owns at least one live building **or**
    /// one live unit. Elimination is "all buildings AND all units destroyed"
    /// (the classic MP defeat check, `house.cpp:1290`).
    pub fn house_alive(&self, house: u8) -> bool {
        // Walls (SBAG/CYCL/BRIK) are modeled as 1×1 buildings but are *not* base
        // structures — a house whose only remaining "buildings" are walls is
        // defeated, matching the original where walls are overlays, not buildings
        // (QUIRKS Q9).
        self.buildings
            .iter()
            .any(|(_, b)| b.house == house && b.is_alive() && !b.is_wall)
            || self.units.iter().any(|(_, u)| u.house == house)
    }

    /// Read a house's credits (0 if the house index is out of range).
    pub fn house_credits(&self, house: u8) -> i32 {
        self.houses
            .get(house as usize)
            .map(|h| h.available())
            .unwrap_or(0)
    }

    /// A house's credit-storage capacity (sum of `Storage=` over live buildings).
    pub fn house_capacity(&self, house: u8) -> i32 {
        house_storage_capacity(self, house)
    }

    /// Set a house's spendable money (no-op if out of range). For the loader /
    /// tests: resets the harvested-tiberium pool and puts the whole amount in the
    /// given-credits pool, so `house_credits` reads back exactly `credits`.
    pub fn set_house_credits(&mut self, house: u8, credits: i32) {
        if let Some(h) = self.houses.get_mut(house as usize) {
            h.credits = credits;
            h.tiberium = 0;
        }
    }

    /// Borrow a house, if it exists.
    pub fn house(&self, house: u8) -> Option<&House> {
        self.houses.get(house as usize)
    }

    /// Whether building type `building_id` may be placed at footprint top-left
    /// `cell` for `house` (footprint on-map/passable/clear **and** the proximity
    /// rule). Surfaced for the client's green/red placement preview and tests.
    pub fn can_place_building(&self, house: u8, building_id: u32, cell: CellCoord) -> bool {
        footprint_placeable(self, building_id, cell)
            && passes_proximity(self, house, building_id, cell)
    }

    /// The current tick number.
    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    /// Borrow the passability grid.
    pub fn passability(&self) -> &Passability {
        &self.passable
    }

    /// Read-only view of the sim RNG seed (also folded into the state hash).
    pub fn rng_seed(&self) -> u32 {
        self.rng.seed()
    }

    /// Spawn a unit at a cell, returning its handle.
    pub fn spawn_unit(
        &mut self,
        type_id: u32,
        house: u8,
        cell: CellCoord,
        facing: Facing,
        health: u16,
        stats: MoveStats,
    ) -> Handle {
        self.units
            .insert(Unit::new(type_id, house, cell, facing, health, stats))
    }

    /// Attach resolved combat stats (armor, weapon, turret) to an already-spawned
    /// unit. Separate from [`World::spawn_unit`] so movement-only callers and
    /// tests are unaffected; the client calls this right after spawning.
    pub fn set_unit_combat(
        &mut self,
        unit: Handle,
        armor: u8,
        weapon: Option<crate::combat::WeaponProfile>,
        has_turret: bool,
    ) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_combat(armor, weapon, has_turret);
        }
    }

    /// Attach a `Secondary=` weapon to a spawned unit (mammoth dual armament).
    pub fn set_unit_secondary(
        &mut self,
        unit: Handle,
        secondary: Option<crate::combat::WeaponProfile>,
    ) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_secondary(secondary);
        }
    }

    /// Set a spawned unit's maximum strength (for the client's health bar).
    pub fn set_unit_max_health(&mut self, unit: Handle, max_health: u16) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_max_health(max_health);
        }
    }

    /// Mark a spawned unit as a harvester (drives the harvest FSM).
    pub fn set_unit_harvester(&mut self, unit: Handle, is_harvester: bool) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_harvester(is_harvester);
        }
    }

    /// Set a spawned unit's standing [`Mission`] (from its scenario INI order).
    /// Area-Guard units record their spawn cell as the guard post they leash to.
    pub fn set_unit_mission(&mut self, unit: Handle, mission: crate::unit::Mission) {
        if let Some(u) = self.units.get_mut(unit) {
            u.mission = mission;
            if mission == crate::unit::Mission::AreaGuard {
                u.guard_post = Some(u.cell());
            }
        }
    }

    /// Set a spawned unit's passenger capacity (`Passengers=`), making it a
    /// transport (e.g. APC). 0 leaves it a normal unit.
    pub fn set_unit_capacity(&mut self, unit: Handle, capacity: u8) {
        if let Some(u) = self.units.get_mut(unit) {
            u.capacity = capacity;
        }
    }

    /// Set a spawned unit's sight range in cells (from its type's `Sight=`), then
    /// reveal the shroud around it for its owning house.
    pub fn set_unit_sight(&mut self, unit: Handle, sight: u8) {
        let revealed = if let Some(u) = self.units.get_mut(unit) {
            u.set_sight(sight);
            Some((u.house, u.cell(), u.sight))
        } else {
            None
        };
        if let Some((house, cell, sight)) = revealed {
            self.shroud.reveal(house, cell, sight);
        }
    }

    /// Directly place building type `building_id` for `house` at footprint
    /// top-left `cell`, stamping occupancy and updating the house's power totals
    /// and building-type count. Used by the loader for scenario-provided
    /// structures and internally by [`Command::Deploy`] / [`Command::PlaceBuilding`].
    /// Does **not** validate the footprint (callers that need validation use the
    /// commands). Returns the new building's handle, or `None` for a bad id.
    pub fn spawn_building(
        &mut self,
        building_id: u32,
        house: u8,
        cell: CellCoord,
    ) -> Option<Handle> {
        let proto = self.catalog.building(building_id)?.clone();
        let building = Building {
            type_id: building_id,
            house,
            cell,
            foot_w: proto.foot_w,
            foot_h: proto.foot_h,
            health: proto.max_health,
            max_health: proto.max_health,
            armor: proto.armor,
            sight: proto.sight.min(10),
            cost: proto.cost,
            power: proto.power,
            is_refinery: proto.is_refinery,
            is_construction_yard: proto.is_construction_yard,
            is_war_factory: proto.is_war_factory,
            is_barracks: proto.is_barracks,
            weapon: proto.weapon,
            has_turret: proto.has_turret,
            charges: proto.charges,
            turret_facing: Facing(0),
            arm: 0,
            charge: 0,
            target: None,
            is_wall: proto.is_wall,
            storage: proto.storage,
            is_repairing: false,
            trigger: None,
        };
        let handle = self.buildings.insert(building);
        // Stamp occupancy.
        let cells: Vec<CellCoord> = self
            .buildings
            .get(handle)
            .map(|b| b.footprint().collect())
            .unwrap_or_default();
        for c in cells {
            self.passable.set_occupied(c, true);
        }
        // Power + ownership bookkeeping.
        if let Some(hs) = self.houses.get_mut(house as usize) {
            if proto.power >= 0 {
                hs.power_output += proto.power;
            } else {
                hs.power_drain += -proto.power;
            }
            hs.adjust_building_count(building_id, 1);
        }
        // Reveal the shroud around the new structure. The original reveals sight
        // from the building's whole footprint, not a single centre cell
        // (`Sight_From` runs over the occupied cells, `map.cpp:576`); revealing
        // from each footprint cell closes the disc gap that a single-centre
        // reveal leaves around a multi-cell building's corners.
        let sight = proto.sight.min(10);
        let foot: Vec<CellCoord> = self
            .buildings
            .get(handle)
            .map(|b| b.footprint().collect())
            .unwrap_or_else(|| vec![cell]);
        for c in foot {
            self.shroud.reveal(house, c, sight);
        }
        Some(handle)
    }

    /// Advance one tick: apply `commands` (in order), run movement, then return
    /// the post-tick state hash. This is the function replays and the lockstep
    /// net layer drive; the returned hash is chained and compared.
    pub fn tick(&mut self, commands: &[Command]) -> u64 {
        apply(self, self.tick_count, commands);
        self.state_hash()
    }

    /// Fold all mutable state into a 64-bit hash, in a fixed field order.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u32(self.tick_count);
        h.write_u32(self.rng.seed());
        h.write_u32(self.units.len());
        for (handle, unit) in self.units.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            unit.hash_into(&mut h);
        }
        h.write_u32(self.bullets.len());
        for (handle, bullet) in self.bullets.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            bullet.hash_into(&mut h);
        }
        h.write_u32(self.buildings.len());
        for (handle, building) in self.buildings.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            building.hash_into(&mut h);
        }
        h.write_u32(self.houses.len() as u32);
        for house in &self.houses {
            house.hash_into(&mut h);
        }
        self.ore.hash_into(&mut h);
        // M6 state folds in ONLY when active, so a non-skirmish world's hash is
        // byte-identical to M5 (the M3/M4 golden chains stay pinned unchanged).
        self.shroud.hash_into(&mut h);
        // Ore-growth scan cursor + candidate counts (drives future RNG draws).
        if let Some(g) = &self.ore_growth {
            h.write_u8(0xE0);
            h.write_i32(g.scan);
            h.write_i32(g.grow_excess);
            h.write_i32(g.spread_excess);
            h.write_u32(g.grow_list.len() as u32);
            h.write_u32(g.spread_list.len() as u32);
        }
        // AI controller state (timers/decisions affect future commands).
        if !self.ai.is_empty() {
            h.write_u8(0xA1);
            h.write_u32(self.ai.len() as u32);
            for a in &self.ai {
                a.hash_into(&mut h);
            }
        }
        if let Some(p) = self.player_house {
            h.write_u8(0x50);
            h.write_u8(p);
        }
        if self.game_over != GameOver::Ongoing {
            h.write_u8(0x60);
            h.write_u8(self.game_over as u8);
        }
        // Campaign scripting + alliance matrix fold in ONLY when present, so every
        // skirmish/combat/movement golden is byte-identical (M7.5).
        if let Some(c) = &self.campaign {
            c.hash_into(&mut h);
        }
        if let Some(a) = &self.alliances {
            h.write_u8(0xAA);
            for &bits in a {
                h.write_u32((bits & 0xFFFF_FFFF) as u32);
                h.write_u32((bits >> 32) as u32);
            }
        }
        // Enemy-activation latches (M7.5-C) fold in ONLY once a house is actually
        // alerted or has begun production, so every campaign that never fires those
        // triggers (Allied mission 1, all skirmish/synthetic worlds) is byte-identical.
        if let Some(ea) = &self.enemy_activation {
            if ea.is_active() {
                ea.hash_into(&mut h);
            }
        }
        h.finish()
    }
}

/// Apply one tick's worth of systems to `world`, in the canonical fixed order:
///
/// 1. **commands** — apply player/AI orders (in the given canonical order),
/// 2. **production** — advance each house's factories, pay installments,
///    complete buildings (→ ready-to-place) and units (→ spawn at the exit),
/// 3. **harvesters** — the 5-state harvest FSM: scan/drive/mine/dock/unload,
/// 4. **combat** — targeting, turret/body rotation, and firing (spawns bullets,
///    consumes the sim RNG on inaccurate shots — see [`run_combat`]),
/// 5. **movement** — advance units along their paths,
/// 6. **bullets** — advance projectiles, detonate, apply damage, remove the dead.
///
/// This fixed, explicit order is itself a determinism requirement (§4.2). This
/// is the single mutation entry point for the sim (§4.4). `tick` must equal the
/// world's current tick. Production and harvesting draw **no** sim RNG (the only
/// RNG path stays combat scatter, M4); ore growth — the original's RNG-consuming
/// economy step — is deferred to M6 (see [`crate::ore`]).
pub fn apply(world: &mut World, tick: u32, commands: &[Command]) {
    debug_assert_eq!(
        tick, world.tick_count,
        "commands applied to the wrong tick (replay/order bug)"
    );

    // System 0: skirmish AI — each AI house issues Commands like a player would
    // (§3.10), applied before the incoming player/net commands.
    run_ai(world);

    // System 1: commands. Applied in the given (canonical) order.
    for &cmd in commands {
        apply_command(world, cmd);
    }

    // System 2: production (factories advance, credits paid, objects complete).
    run_production(world);

    // System 3: harvesters (the harvest FSM sets paths and books credits).
    run_harvesters(world);

    // System 3.5: service-depot (FIX) unit repair.
    run_repair(world);

    // System 3.6: building self-repair (player/AI repair toggle).
    run_building_repair(world);

    // System 3.9: submarine surfacing FSM (naval arc) — sets each sub's submerged
    // (cloak) state before combat/targeting so the stealth-acquisition gate is
    // consistent this tick. Inert for any world with no submarines.
    run_submarines(world);

    // System 4: combat (targeting + rotation + firing).
    run_combat(world);

    // System 4.25: engineers march to a targeted enemy building and capture it.
    run_engineers(world);

    // System 4.5: defense-building combat (auto-acquire + turret + fire), after
    // unit combat so RNG (bullet scatter) is drawn in a fixed unit-then-building
    // order.
    run_building_combat(world);

    // System 4.6: aircraft flight + combat FSM (helicopters/fixed-wing). After
    // building combat so bullet-scatter RNG is drawn in a fixed
    // unit→building→aircraft order; inert for any world with no aircraft.
    run_aircraft(world);

    // System 5: movement.
    move_units(world);

    // System 5.5: transports — a passenger walking to board a transport boards it
    // once it arrives adjacent (M7.5-B P1).
    run_transports(world);

    // System 6: bullets (flight + detonation + damage + death).
    run_bullets(world);

    // System 7: ore growth/spread (draws sync RNG when enabled) — deferred M5 item.
    run_ore_growth(world);

    // System 7.5: campaign scenario-scripting (M7.5) — evaluate triggers, fire
    // actions (reinforcements/reveal/globals/win-lose). After combat/movement so
    // this tick's deaths are visible to DESTROYED events; before win/lose so a
    // WIN/LOSE action resolves this same tick.
    run_campaign(world);

    // System 7.6: campaign enemy activation (M7.5-C) — alerted computer houses form
    // autocreate teams on the AlertTime cadence, and production-started houses build
    // from their factories + rebuild their [Base]. Inert (and RNG-free) until a
    // TACTION_AUTOCREATE/BEGIN_PRODUCTION action flips a latch, so it never touches a
    // scripted-only mission (Allied mission 1) or any skirmish.
    run_enemy_activation(world);

    // System 8: house-elimination / win-lose resolution.
    update_game_over(world);

    world.tick_count = world.tick_count.wrapping_add(1);
}

/// System 0: run each installed skirmish AI in house-index order, applying the
/// commands it issues. The AI reads a shared borrow of `world` and draws from a
/// **copy** of the sim RNG (it is `Copy`), which is written back afterwards — so
/// the AI's `Random_Pick`-equivalent draws advance the same seed the rest of the
/// sim uses, in a fixed order, without a borrow conflict.
fn run_ai(world: &mut World) {
    if world.ai.is_empty() {
        return;
    }
    let mut ai = std::mem::take(&mut world.ai);
    let mut rng = world.rng;
    let mut cmds: Vec<Command> = Vec::new();
    for a in &mut ai {
        a.step(world, &mut rng, &mut cmds);
    }
    world.rng = rng;
    world.ai = ai;
    for cmd in cmds {
        apply_command(world, cmd);
    }
}

/// System 8: resolve win/lose for a skirmish. A house is eliminated once it owns
/// no live buildings **and** no live units (`house.cpp:1290`, the classic MP
/// defeat). Only runs when a player house has been designated
/// ([`World::set_player_house`]). Player eliminated → Defeat; every AI house
/// eliminated → Victory. First terminal state sticks.
fn update_game_over(world: &mut World) {
    if world.game_over != GameOver::Ongoing {
        return;
    }
    let Some(player) = world.player_house else {
        return;
    };
    if !world.house_alive(player) {
        world.game_over = GameOver::Defeat;
        return;
    }
    // In a campaign, victory is **trigger-driven** (a `TACTION_WIN` fires
    // `set_game_over(Victory)`); the "every AI eliminated" auto-win does not
    // apply (a mission can leave neutral/enemy houses standing). Defeat on player
    // elimination still holds above.
    if world.campaign.is_some() {
        return;
    }
    // Skirmish: victory once every AI-controlled house has been eliminated.
    if !world.ai.is_empty() && world.ai.iter().all(|a| !world.house_alive(a.house)) {
        world.game_over = GameOver::Victory;
    }
}

// ===========================================================================
// Campaign scenario-scripting engine (M7.5). Ports trigger.cpp `Spring`,
// tevent.cpp `operator()`, taction.cpp `operator()`, reinf.cpp `Do_Reinforcements`.
// ===========================================================================

/// System 7.5: evaluate the campaign triggers and fire their actions.
fn run_campaign(world: &mut World) {
    let Some(mut camp) = world.campaign.take() else {
        return;
    };
    let tpm = world.catalog.econ.ticks_per_minute.max(1);
    let ticks_per_tenth = (tpm / 10).max(1);

    // First-tick init: seed each TIME event's countdown + carrier baselines.
    if !camp.started {
        for i in 0..camp.triggers.len() {
            let (e1_time, e2_time) = {
                let t = &camp.triggers[i];
                (
                    if t.e1.code == campaign::tevent::TIME {
                        t.e1.data.max(0) * ticks_per_tenth
                    } else {
                        -1
                    },
                    if t.e2.code == campaign::tevent::TIME {
                        t.e2.data.max(0) * ticks_per_tenth
                    } else {
                        -1
                    },
                )
            };
            camp.state[i].e1_timer = e1_time;
            camp.state[i].e2_timer = e2_time;
        }
        let counts = carrier_counts(world, camp.triggers.len());
        for (i, st) in camp.state.iter_mut().enumerate() {
            st.carriers = counts[i];
            st.carriers_init = counts[i];
        }
        camp.started = true;
    }

    // Update carriers + latch VOLATILE `DESTROYED` when a carrier count drops.
    let counts = carrier_counts(world, camp.triggers.len());
    for (i, st) in camp.state.iter_mut().enumerate() {
        if counts[i] < st.carriers {
            st.any_destroyed = true;
        }
        st.carriers = counts[i];
    }

    // Advance timers.
    for st in camp.state.iter_mut() {
        if st.e1_timer > 0 {
            st.e1_timer -= 1;
        }
        if st.e2_timer > 0 {
            st.e2_timer -= 1;
        }
    }
    if let Some(t) = camp.mission_timer.as_mut() {
        if *t > 0 {
            *t -= 1;
        }
    }

    // Evacuation: a friendly civilian VIP on a DZ cell is evacuated (our simplified
    // stand-in for the aircraft-leaves-map path — see QUIRKS).
    process_evac(world, &mut camp);

    // Evaluate every trigger in INI order.
    let mut forced: Vec<usize> = Vec::new();
    for i in 0..camp.triggers.len() {
        maybe_spring(world, &mut camp, i, false, ticks_per_tenth, &mut forced);
    }
    // Resolve FORCE_TRIGGER chains (bounded).
    let mut guard = 0;
    while let Some(idx) = forced.pop() {
        guard += 1;
        if guard > 512 {
            break;
        }
        maybe_spring(world, &mut camp, idx, true, ticks_per_tenth, &mut forced);
    }

    world.campaign = Some(camp);
}

/// Count live object carriers (units + buildings) per trigger index.
fn carrier_counts(world: &World, n: usize) -> Vec<i32> {
    let mut counts = vec![0i32; n];
    for (_, u) in world.units.iter() {
        if !u.is_alive() {
            continue;
        }
        if let Some(t) = u.trigger {
            if let Some(c) = counts.get_mut(t as usize) {
                *c += 1;
            }
        }
    }
    for (_, b) in world.buildings.iter() {
        if !b.is_alive() {
            continue;
        }
        if let Some(t) = b.trigger {
            if let Some(c) = counts.get_mut(t as usize) {
                *c += 1;
            }
        }
    }
    counts
}

/// Try to spring trigger `i`. `forced` means a `FORCE_TRIGGER` bypassing the event
/// checks (`TriggerClass::Spring(..., forced=true)`).
fn maybe_spring(
    world: &mut World,
    camp: &mut Campaign,
    i: usize,
    forced: bool,
    ticks_per_tenth: i32,
    forced_out: &mut Vec<usize>,
) {
    let (persist, ectrl, actctrl) = {
        let t = &camp.triggers[i];
        (t.persist, t.event_ctrl, t.action_ctrl)
    };
    // Destroyed by DESTROY_TRIGGER: suppress ALL evaluation, regardless of
    // persistence (taction.cpp:568-578 deletes the instance outright).
    if camp.state[i].destroyed {
        return;
    }
    // Already sprung (and not persistent, not a forced re-spring): skip.
    if camp.state[i].sprung && persist != campaign::persist::PERSISTANT && !forced {
        return;
    }

    let (e1, e2) = if forced {
        (true, false)
    } else {
        (
            eval_event(world, camp, i, true),
            eval_event(world, camp, i, false),
        )
    };
    let satisfied = if forced {
        true
    } else {
        match ectrl {
            campaign::multi::ONLY => e1,
            campaign::multi::AND => e1 && e2,
            _ => e1 || e2, // OR + LINKED both spring on either event
        }
    };
    if !satisfied {
        return;
    }

    // Which actions run (trigger.cpp:301-323). For LINKED, action1 runs iff its
    // event fired *or the spring was forced*, and action2 iff its event fired
    // *and it was not forced* — so a **forced** LINKED trigger runs action1 only,
    // regardless of `action_ctrl` (`if (e1 || forced) Action1; if (e2 && !forced)
    // Action2;`). Non-LINKED runs action1 always + action2 unless MULTI_ONLY.
    let (run_a1, run_a2) = if ectrl == campaign::multi::LINKED {
        (e1 || forced, e2 && !forced)
    } else {
        (true, actctrl != campaign::multi::ONLY)
    };

    // Clone the actions so we can mutate `camp`/`world` without holding a borrow.
    let (a1, a2) = {
        let t = &camp.triggers[i];
        (t.a1.clone(), t.a2.clone())
    };
    if run_a1 {
        run_action(world, camp, &a1, i, ticks_per_tenth, forced_out);
    }
    if run_a2 {
        run_action(world, camp, &a2, i, ticks_per_tenth, forced_out);
    }

    if persist != campaign::persist::PERSISTANT {
        camp.state[i].sprung = true;
    } else {
        // A PERSISTANT (or SEMI-with-survivors) trigger is not deleted after
        // firing; the reference re-arms its events (`Class->Event1.Reset(...)`,
        // trigger.cpp:355-360 → tevent.cpp:181-187), which for a TIME event resets
        // its countdown to `Data.Value * (TICKS_PER_MINUTE/10)`. Without this a
        // PERSISTANT TIME trigger would sit at timer 0 and re-fire every tick;
        // with it, it fires once per interval.
        let (e1_time, e1_data, e2_time, e2_data) = {
            let t = &camp.triggers[i];
            (
                t.e1.code == campaign::tevent::TIME,
                t.e1.data,
                t.e2.code == campaign::tevent::TIME,
                t.e2.data,
            )
        };
        if e1_time {
            camp.state[i].e1_timer = e1_data.max(0) * ticks_per_tenth;
        }
        if e2_time {
            camp.state[i].e2_timer = e2_data.max(0) * ticks_per_tenth;
        }
    }
}

/// Evaluate one event (`TEventClass::operator()`). `is_e1` selects event1/event2.
fn eval_event(world: &World, camp: &Campaign, i: usize, is_e1: bool) -> bool {
    use campaign::tevent::*;
    let t = &camp.triggers[i];
    let ev = if is_e1 { &t.e1 } else { &t.e2 };
    let st = &camp.state[i];
    match ev.code {
        NONE => false,
        // Countdown reached zero (tevent.cpp:256).
        TIME => (if is_e1 { st.e1_timer } else { st.e2_timer }) == 0,
        // Object-attached destruction. SEMIPERSISTANT springs only when *all*
        // attached carriers are gone; VOLATILE/PERSISTANT on the first death.
        DESTROYED => {
            if t.persist == campaign::persist::SEMI {
                st.carriers == 0 && st.carriers_init > 0
            } else {
                st.any_destroyed
            }
        }
        ATTACKED => st.any_attacked,
        GLOBAL_SET => camp.globals.get(ev.data as usize).copied().unwrap_or(false),
        GLOBAL_CLEAR => !camp.globals.get(ev.data as usize).copied().unwrap_or(false),
        EVAC_CIVILIAN => camp.is_civ_evacuated(t.house.max(0) as u8),
        LOW_POWER => world
            .houses
            .get(ev.data.max(0) as usize)
            .map(|h| h.low_power())
            .unwrap_or(false),
        PLAYER_ENTERED | CROSS_HORIZONTAL | CROSS_VERTICAL => {
            player_entered(world, camp, i as u16, ev.data.max(0) as u8)
        }
        // House-scan "all destroyed" family (tevent.cpp:463-483).
        ALL_DESTROYED => {
            let h = ev.data.max(0) as u8;
            !house_has_any(world, h, true, true)
        }
        UNITS_DESTROYED => {
            let h = ev.data.max(0) as u8;
            !house_has_any(world, h, true, false)
        }
        BUILDINGS_DESTROYED => {
            let h = ev.data.max(0) as u8;
            !house_has_any(world, h, false, true)
        }
        BUILDING_EXISTS => {
            let h = t.house.max(0) as u8;
            world
                .buildings
                .iter()
                .any(|(_, b)| b.house == h && b.is_alive() && !b.is_wall)
        }
        _ => false,
    }
}

/// Whether house `h` has any live unit and/or building (the `Active*Scan` test).
fn house_has_any(world: &World, h: u8, units: bool, buildings: bool) -> bool {
    if units
        && world
            .units
            .iter()
            .any(|(_, u)| u.house == h && u.is_alive())
    {
        return true;
    }
    if buildings
        && world
            .buildings
            .iter()
            .any(|(_, b)| b.house == h && b.is_alive() && !b.is_wall)
    {
        return true;
    }
    false
}

/// Whether a unit of house `house` occupies a cell carrying trigger `trig`.
fn player_entered(world: &World, camp: &Campaign, trig: u16, house: u8) -> bool {
    for &(cell, t) in &camp.cell_triggers {
        if t != trig {
            continue;
        }
        let cc = CellCoord::from_index(cell);
        if world
            .units
            .iter()
            .any(|(_, u)| u.house == house && u.is_alive() && u.cell() == cc)
        {
            return true;
        }
    }
    false
}

/// Execute one action (`TActionClass::operator()`).
fn run_action(
    world: &mut World,
    camp: &mut Campaign,
    a: &crate::campaign::TActionDef,
    trig: usize,
    ticks_per_tenth: i32,
    forced_out: &mut Vec<usize>,
) {
    use campaign::taction::*;
    match a.code {
        WIN => world.set_game_over(GameOver::Victory),
        LOSE => world.set_game_over(GameOver::Defeat),
        TEXT_TRIGGER => camp.pending_texts.push(a.data),
        PLAY_SPEECH => camp.pending_speech.push(a.data),
        SET_GLOBAL => set_global(camp, a.data, true),
        CLEAR_GLOBAL => set_global(camp, a.data, false),
        DZ => {
            if let Some(c) = camp.waypoint_cell(a.data) {
                if !camp.evac_cells.contains(&c) {
                    camp.evac_cells.push(c);
                }
            }
        }
        REVEAL_ALL => {
            camp.reveal_all = true;
            reveal_whole_map(world);
        }
        REVEAL_SOME => {
            if let Some(c) = camp.waypoint_cell(a.data) {
                if let Some(idx) = c.to_index() {
                    camp.reveal_cells.push(idx);
                }
                if let Some(p) = world.player_house() {
                    world.reveal_shroud(p, c, 6);
                }
            }
        }
        REINFORCEMENTS => spawn_team(world, camp, a.team, true),
        CREATE_TEAM => spawn_team(world, camp, a.team, false),
        ALL_HUNT => set_all_hunt(world, a.data.max(0) as u8),
        DESTROY_OBJECT => destroy_trigger_object(world, trig as u16),
        FORCE_TRIGGER => {
            if a.trigger >= 0 && (a.trigger as usize) < camp.triggers.len() {
                forced_out.push(a.trigger as usize);
            }
        }
        DESTROY_TRIGGER => {
            if a.trigger >= 0 {
                if let Some(s) = camp.state.get_mut(a.trigger as usize) {
                    // Delete the target outright (taction.cpp:568-578): stops it
                    // firing regardless of persistence, unlike setting `sprung`
                    // (which PERSISTANT triggers ignore).
                    s.destroyed = true;
                }
            }
        }
        START_TIMER | SET_TIMER => {
            camp.mission_timer = Some(a.data.max(0) * ticks_per_tenth);
        }
        STOP_TIMER => camp.mission_timer = None,
        // AUTOCREATE (M7.5-C P1): alert the target house so it forms autocreate teams
        // on the AlertTime cadence (`TACTION_AUTOCREATE` → `House->IsAlerted = true`,
        // taction.cpp:645).
        AUTOCREATE => {
            if let Some(h) = action_house(a.data, camp.triggers[trig].house) {
                if let Some(ea) = world.enemy_activation.as_mut() {
                    grow_house_flag(&mut ea.alerted, h, true);
                    grow_house_flag_i32(&mut ea.alert_timer, h, 0); // fires immediately
                }
            }
        }
        // BEGIN_PRODUCTION (M7.5-C P2): flag the target house to produce from its
        // factories + rebuild its [Base] (`TACTION_BEGIN_PRODUCTION` →
        // `House->Begin_Production()` → `IsStarted = true`, house.h:781).
        BEGIN_PRODUCTION => {
            if let Some(h) = action_house(a.data, camp.triggers[trig].house) {
                if let Some(ea) = world.enemy_activation.as_mut() {
                    grow_house_flag(&mut ea.production, h, true);
                }
            }
        }
        // FIRE_SALE / ALLOWWIN / DESTROY_TEAM / PLAY_MOVIE / etc. — inert or deferred
        // (documented in QUIRKS).
        _ => {}
    }
}

/// Resolve the target house of a house-scoped trigger action from its raw `Data`
/// value. RA stores the house in the **low byte** of the action's `Data.Value`
/// union (`TActionClass`, taction.cpp:226 writes `Data.Value`; the handlers read
/// `Data.House`, taction.cpp:625/645), so an editor-encoded value like scg03ea's
/// `-247` resolves as `-247 & 0xFF = 9` (BadGuy) and a bare positive index (`9`)
/// resolves to itself; `0xFF` is `HOUSE_NONE`. A none/out-of-range value falls back
/// to the **trigger's own house** (the scenario intent — the enemy that owns the
/// trigger activates itself).
fn action_house(data: i32, trigger_house: i32) -> Option<u8> {
    let byte = (data & 0xFF) as u8;
    if byte != 0xFF && (byte as usize) < crate::campaign::CAMPAIGN_HOUSE_SLOTS {
        Some(byte)
    } else if trigger_house >= 0 && (trigger_house as usize) < crate::campaign::CAMPAIGN_HOUSE_SLOTS
    {
        Some(trigger_house as u8)
    } else {
        None
    }
}

/// Grow a per-house `bool` latch vector to include `house` and set it.
fn grow_house_flag(v: &mut Vec<bool>, house: u8, val: bool) {
    let i = house as usize;
    if v.len() <= i {
        v.resize(i + 1, false);
    }
    v[i] = val;
}

/// Grow a per-house `i32` vector to include `house` and set it.
fn grow_house_flag_i32(v: &mut Vec<i32>, house: u8, val: i32) {
    let i = house as usize;
    if v.len() <= i {
        v.resize(i + 1, -1);
    }
    v[i] = val;
}

/// `Rule.AutocreateTime` (rules.cpp:173, `AutocreateTime(5)`): the multiplier on the
/// randomized autocreate interval. `AlertTime = AutocreateTime × Random_Pick(
/// TICKS_PER_MINUTE/2, TICKS_PER_MINUTE*2)` (house.cpp:1056).
const AUTOCREATE_TIME: i32 = 5;

/// System 7.6: campaign enemy activation (M7.5-C). Runs the two scripted-enemy
/// behaviours that `TACTION_AUTOCREATE` / `TACTION_BEGIN_PRODUCTION` unlock — a
/// faithful-but-scoped port of `HouseClass::AI`'s autocreate loop (house.cpp:1042)
/// and factory/base AI (house.cpp:5700, building.cpp:5600). It is a **no-op until a
/// house is alerted or has begun production**, so a scripted-only mission draws no
/// RNG and hashes identically.
///
/// **Sync RNG.** Where the original draws `Scen.RandomNumber` we draw the sim RNG
/// (a `Copy` snapshot, written back — the [`run_ai`] pattern), in a fixed order per
/// house in house-index order: the wave count (`Random_Pick(2,…)`, house.cpp:1047),
/// each team pick (`Random_Pick`, teamtype.cpp:490), the AlertTime reset
/// (house.cpp:1056), then the production weighted pick (house.cpp:6186).
fn run_enemy_activation(world: &mut World) {
    let active = world
        .enemy_activation
        .as_ref()
        .map(|ea| ea.is_active())
        .unwrap_or(false);
    if !active || world.campaign.is_none() {
        return;
    }
    let mut camp = world.campaign.take().unwrap();
    let mut ea = world.enemy_activation.take().unwrap();
    let mut rng = world.rng;
    let tpm = world.catalog.econ.ticks_per_minute.max(1);

    // --- Autocreate teams (P1): each alerted, live house on the AlertTime cadence.
    for house in 0..ea.alerted.len() {
        if !ea.alerted[house] || !world.house_alive(house as u8) {
            continue;
        }
        let t = ea.alert_timer.get(house).copied().unwrap_or(0);
        if t > 0 {
            ea.alert_timer[house] = t - 1;
            continue;
        }
        autocreate_wave(world, &mut camp, house as u8, ea.tech_level, &mut rng);
        // Re-arm: AlertTime = AutocreateTime × Random_Pick(TPM/2, TPM*2) (house.cpp:1056).
        let reset = AUTOCREATE_TIME * rng.range(tpm / 2, tpm * 2);
        grow_house_flag_i32(&mut ea.alert_timer, house as u8, reset);
    }

    // --- Scripted production + base rebuild (P2): each production-started house.
    for house in 0..ea.production.len() {
        if !ea.production[house] || !world.house_alive(house as u8) {
            continue;
        }
        campaign_produce_units(world, house as u8, &mut rng);
        if house as u8 == ea.base_house {
            campaign_rebuild_base(world, &ea, house as u8);
        }
    }

    world.rng = rng;
    world.campaign = Some(camp);
    world.enemy_activation = Some(ea);
}

/// Form up to `Random_Pick(2, (TechLevel-1)/3+1)` autocreate teams for `house`
/// (house.cpp:1047). Each team is a uniform random pick among the house's
/// autocreate-flagged team types (`Suggested_New_Team(true)`, teamtype.cpp:414),
/// created by recruiting existing idle units of the house (`Create_One_Of` →
/// `TeamClass` recruit, team.cpp:1179) via the shared CREATE_TEAM path.
fn autocreate_wave(
    world: &mut World,
    camp: &mut Campaign,
    house: u8,
    tech_level: i32,
    rng: &mut RandomLcg,
) {
    // Random_Pick(2, (TechLevel-1)/3 + 1) — clamps to ≥2 (the reference's low-tech
    // "centre on 2"; `range` swaps min/max, so a hi<2 still yields 2 on the nose).
    let hi = ((tech_level - 1) / 3 + 1).max(2);
    let maxteams = rng.range(2, hi);
    for _ in 0..maxteams {
        let choices: Vec<usize> = camp
            .teamtypes
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.house == house as i32 && (t.flags & crate::campaign::team_flags::AUTOCREATE) != 0
            })
            .map(|(i, _)| i)
            .collect();
        if choices.is_empty() {
            break;
        }
        let pick = choices[rng.range(0, choices.len() as i32 - 1) as usize];
        // Recruit existing idle house units into the team and run its mission list.
        spawn_team(world, camp, pick as i32, false);
    }
}

/// Produce vehicles + infantry for a production-started campaign house from its
/// **live factories**, using the AI weighted table (house.cpp:6172: armed vehicle
/// weight 20, unarmed 1; offensive infantry only). Money is drawn from the house's
/// scenario `Credits=` pool by the existing production machinery — no free money
/// (`FactoryClass::AI`, factory.cpp:203). One item per lane per pass.
fn campaign_produce_units(world: &mut World, house: u8, rng: &mut RandomLcg) {
    let has_factory = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_war_factory && b.is_alive());
    let has_barracks = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_barracks && b.is_alive());

    // Resolve both picks up front (all `catalog`/`house` borrows released) so the
    // mutating `apply_start_production` calls below hold no immutable borrow.
    let vehicle_pick: Option<u32> = {
        let cat = &world.catalog;
        let hs = world.houses.get(house as usize);
        if has_factory
            && hs.map(|h| h.unit_prod.is_none()).unwrap_or(false)
            && world.house_credits(house) > 0
        {
            let hs = hs.unwrap();
            let eligible: Vec<(u32, i32)> = cat
                .units
                .iter()
                .enumerate()
                .filter(|(_id, p)| {
                    !p.is_harvester
                        && !p.is_infantry
                        && p.deploys_to.is_none()
                        && p.prereq.iter().all(|&pre| hs.owns_building(pre))
                })
                .map(|(id, p)| (id as u32, if p.weapon.is_some() { 20 } else { 1 }))
                .collect();
            let total: i32 = eligible.iter().map(|(_, w)| *w).sum();
            if total > 0 {
                // Weighted walk over the counter array (house.cpp:6186).
                let mut choice = rng.range(0, total - 1);
                let mut picked = None;
                for (id, w) in &eligible {
                    if choice < *w {
                        picked = Some(*id);
                        break;
                    }
                    choice -= *w;
                }
                picked
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(id) = vehicle_pick {
        apply_start_production(world, house, BuildItem::Unit(id));
    }

    let infantry_pick: Option<u32> = {
        let cat = &world.catalog;
        let hs = world.houses.get(house as usize);
        if has_barracks
            && hs.map(|h| h.infantry_prod.is_none()).unwrap_or(false)
            && world.house_credits(house) > 0
        {
            let hs = hs.unwrap();
            let eligible: Vec<u32> = cat
                .units
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.is_infantry
                        && p.weapon.map(|w| w.damage > 0).unwrap_or(false)
                        && p.prereq.iter().all(|&pre| hs.owns_building(pre))
                })
                .map(|(id, _)| id as u32)
                .collect();
            if !eligible.is_empty() {
                Some(eligible[rng.range(0, eligible.len() as i32 - 1) as usize])
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(id) = infantry_pick {
        apply_start_production(world, house, BuildItem::Unit(id));
    }
}

/// Rebuild the first destroyed `[Base]` node (list order = priority,
/// `Next_Buildable`, base.cpp:377) when the base house owns a construction yard.
/// Starts the structure through the normal production lane and places the completed
/// building back on its scripted cell (`building.cpp:2196` rebuilds at `node->Cell`).
fn campaign_rebuild_base(world: &mut World, ea: &crate::campaign::EnemyActivation, house: u8) {
    let Some(hs) = world.houses.get(house as usize) else {
        return;
    };
    // A completed base building awaiting placement: drop it on its own node cell
    // (the reference rebuilds at the exact `node->Cell`, bypassing proximity —
    // building.cpp:2196).
    if let Some(ready) = hs.ready_building {
        if let Some((_, cell)) = ea
            .base_nodes
            .iter()
            .find(|(id, cell)| *id == ready && !base_node_built(world, house, *id, *cell))
            .copied()
        {
            place_building_inner(world, house, ready, cell, false);
        }
        return;
    }
    // A structure already building: wait for it.
    if hs.building_prod.is_some() {
        return;
    }
    // The construction yard is required to build structures (BuildingFactories,
    // house.cpp:6828) — no yard, no rebuild (apply_start_production also guards this).
    let has_yard = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_construction_yard && b.is_alive());
    if !has_yard {
        return;
    }
    // First unbuilt node in list order (Next_Buildable), if the house can build it.
    for (id, cell) in &ea.base_nodes {
        if base_node_built(world, house, *id, *cell) {
            continue;
        }
        let ok = world
            .catalog
            .building(*id)
            .map(|p| p.prereq.iter().all(|&pre| hs.owns_building(pre)))
            .unwrap_or(false);
        if ok {
            apply_start_production(world, house, BuildItem::Building(*id));
            return;
        }
    }
}

/// Whether a `[Base]` node currently stands: a live building of `house` with the
/// node's proto id on the node's cell (`BaseClass::Is_Built` / `Get_Building`,
/// base.cpp:229).
fn base_node_built(world: &World, house: u8, id: u32, cell: CellCoord) -> bool {
    world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_alive() && b.type_id == id && b.cell == cell)
}

fn set_global(camp: &mut Campaign, idx: i32, val: bool) {
    if idx < 0 {
        return;
    }
    let i = idx as usize;
    if camp.globals.len() <= i {
        camp.globals.resize(i + 1, false);
    }
    camp.globals[i] = val;
}

/// Reveal the whole map for the player house (`TACTION_REVEAL_ALL`).
fn reveal_whole_map(world: &mut World) {
    let Some(p) = world.player_house() else {
        return;
    };
    let (w, h) = (world.passable.width(), world.passable.height());
    for y in 0..h {
        for x in 0..w {
            world.reveal_shroud(p, CellCoord::new(x, y), 1);
        }
    }
}

/// Set every live unit of house `h` to auto-hunt (`TACTION_ALL_HUNT`).
fn set_all_hunt(world: &mut World, h: u8) {
    let handles: Vec<Handle> = world
        .units
        .iter()
        .filter(|(_, u)| u.house == h && u.is_alive())
        .map(|(hd, _)| hd)
        .collect();
    for hd in handles {
        if let Some(u) = world.units.get_mut(hd) {
            u.hunt = true;
        }
    }
}

/// Kill whatever object currently carries trigger `trig` (`TACTION_DESTROY_OBJECT`).
fn destroy_trigger_object(world: &mut World, trig: u16) {
    let bh: Vec<Handle> = world
        .buildings
        .iter()
        .filter(|(_, b)| b.trigger == Some(trig) && b.is_alive())
        .map(|(h, _)| h)
        .collect();
    for h in bh {
        remove_building(world, h);
    }
    let uh: Vec<Handle> = world
        .units
        .iter()
        .filter(|(_, u)| u.trigger == Some(trig) && u.is_alive())
        .map(|(h, _)| h)
        .collect();
    for h in uh {
        world.units.remove(h);
    }
}

/// Evacuate any friendly civilian VIP standing on a DZ/evac cell: latch its
/// house's `IsCivEvacuated` and remove it from the map.
fn process_evac(world: &mut World, camp: &mut Campaign) {
    if camp.evac_cells.is_empty() {
        return;
    }
    // A VIP standing on, or orthogonally/diagonally adjacent to, a DZ cell counts
    // as reaching the landing zone (the flare marks an area, not a single tile —
    // this also frees the evac from an exact-cell match if the flare tile itself
    // is impassable).
    let near_lz = |c: CellCoord| -> bool {
        camp.evac_cells
            .iter()
            .any(|e| (e.x - c.x).abs() <= 1 && (e.y - c.y).abs() <= 1)
    };
    let evac: Vec<(Handle, u8, Option<u16>)> = world
        .units
        .iter()
        .filter(|(_, u)| u.is_civ_evac && u.is_alive() && near_lz(u.cell()))
        .map(|(h, u)| (h, u.house, u.trigger))
        .collect();
    for (h, house, trig) in evac {
        let i = house as usize;
        if camp.civ_evacuated.len() <= i {
            camp.civ_evacuated.resize(i + 1, false);
        }
        camp.civ_evacuated[i] = true;
        // An evacuated VIP *left the map* — he is not "destroyed". Lower his
        // trigger's carrier baseline so the removal below is not mistaken for a
        // death by a `TEVENT_DESTROYED` on that trigger (e.g. Einstein carries
        // the "he died -> LOSE" trigger; evac must not trip it).
        if let Some(t) = trig {
            if let Some(st) = camp.state.get_mut(t as usize) {
                st.carriers = (st.carriers - 1).max(0);
                st.carriers_init = (st.carriers_init - 1).max(0);
            }
        }
        world.units.remove(h);
    }
}

/// The team's primary movement target + whether it should auto-hunt, from its
/// first actionable mission (`TeamMissionClass`).
fn team_objective(tt: &crate::campaign::TeamType, camp: &Campaign) -> (Option<CellCoord>, bool) {
    use campaign::tmission::*;
    for m in &tt.missions {
        match m.code {
            ATT_WAYPT | PATROL => return (camp.waypoint_cell(m.arg), true),
            MOVE => return (camp.waypoint_cell(m.arg), false),
            MOVECELL => return (Some(CellCoord::from_index(m.arg.max(0) as u32)), false),
            ATTACK => return (None, true),
            GUARD => return (None, false),
            // TMISSION_DO (teamtype.h:57): adopt the MissionType named by `arg`. The
            // autocreate-team script is `DO:MISSION_HUNT` (arg 14) — the team hunts
            // the player; other DO args (guard/area-guard) are stationary defence.
            DO => return (None, m.arg == MISSION_HUNT_ARG),
            _ => {}
        }
    }
    (None, false)
}

/// Spawn (`REINFORCEMENTS`) or recruit (`CREATE_TEAM`) a team and set its mission.
fn spawn_team(world: &mut World, camp: &mut Campaign, team_idx: i32, spawn: bool) {
    if team_idx < 0 {
        return;
    }
    let tt = match camp.teamtypes.get(team_idx as usize) {
        Some(t) => t.clone(),
        None => return,
    };
    let house = tt.house.clamp(0, 255) as u8;
    let (move_target, hunt) = team_objective(&tt, camp);
    let assigned_trigger = if tt.trigger >= 0 {
        Some(tt.trigger as u16)
    } else {
        None
    };

    let mut members: Vec<Handle> = Vec::new();
    if spawn {
        let base = camp
            .waypoint_cell(tt.origin)
            .unwrap_or_else(|| CellCoord::new(1, 1));
        let mut slot = 0i32;
        for cls in &tt.classes {
            let Some(proto) = &cls.proto else {
                continue; // naval/air/unspawnable — deferred
            };
            for _ in 0..cls.count {
                let cell = disperse_cell(world, base, slot);
                slot += 1;
                let spot = (slot as usize % SUBCELL_COUNT) as u8;
                let h = spawn_from_proto(world, proto, house, cell, spot);
                if let Some(u) = world.units.get_mut(h) {
                    u.trigger = assigned_trigger;
                    u.hunt = hunt;
                }
                members.push(h);
            }
        }
    } else {
        // CREATE_TEAM: recruit existing idle house units (no spawn), taking up to
        // the team's total class count. Simplified — no per-class type match.
        let want: u32 = tt
            .classes
            .iter()
            .map(|c| c.count as u32)
            .sum::<u32>()
            .max(1);
        let handles: Vec<Handle> = world
            .units
            .iter()
            .filter(|(_, u)| {
                u.house == house && u.is_alive() && !u.hunt && !u.is_harvester && !u.is_civ_evac
            })
            .map(|(h, _)| h)
            .take(want as usize)
            .collect();
        for h in handles {
            if let Some(u) = world.units.get_mut(h) {
                u.hunt = hunt;
            }
            members.push(h);
        }
    }

    // Issue the movement order (attack-move if hunt).
    // Scripted transport missions (`TMISSION_LOAD`/`TMISSION_UNLOAD`): if the team
    // carries both a transport and foot members and its script says LOAD, the foot
    // members board the transport (they spawn adjacent, so it is immediate); an
    // UNLOAD later in the list flags the transport to disgorge at its objective.
    let has_load = tt
        .missions
        .iter()
        .any(|m| m.code == campaign::tmission::LOAD);
    let has_unload = tt
        .missions
        .iter()
        .any(|m| m.code == campaign::tmission::UNLOAD);
    let transport = members
        .iter()
        .copied()
        .find(|&h| world.units.get(h).map(|u| u.capacity > 0).unwrap_or(false));
    if has_load {
        if let Some(t) = transport {
            let riders: Vec<Handle> = members
                .iter()
                .copied()
                .filter(|&h| h != t && world.units.get(h).map(|u| u.is_infantry()).unwrap_or(false))
                .collect();
            for r in riders {
                // Riders resume the team's stance on unload (Hunt for an attack
                // team, else Guard).
                if let Some(u) = world.units.get_mut(r) {
                    if hunt {
                        u.mission = Mission::Hunt;
                    }
                }
                apply_load(world, r, t, house);
            }
        }
    }

    // Movement: order the on-map members (loaded riders are gone) to the objective.
    if let Some(target) = move_target {
        for &h in &members {
            if world.units.get(h).is_none() {
                continue; // a boarded rider — no longer on the map
            }
            apply_command(
                world,
                Command::Move {
                    unit: h,
                    dest: target,
                    house,
                },
            );
        }
        // A LOAD+UNLOAD assault drops its cargo at the destination.
        if has_unload {
            if let Some(t) = transport {
                if let Some(u) = world.units.get_mut(t) {
                    u.unload_at = Some(target);
                }
            }
        }
    }
}

/// Spawn one unit from a resolved [`crate::campaign::SpawnProto`].
fn spawn_from_proto(
    world: &mut World,
    proto: &crate::campaign::SpawnProto,
    house: u8,
    cell: CellCoord,
    spot: u8,
) -> Handle {
    let h = world.spawn_unit(
        proto.type_id,
        house,
        cell,
        Facing(0),
        proto.max_health,
        proto.stats,
    );
    world.set_unit_max_health(h, proto.max_health);
    world.set_unit_sight(h, proto.sight);
    world.set_unit_combat(h, proto.armor, proto.weapon, proto.has_turret);
    world.set_unit_secondary(h, proto.secondary);
    world.set_unit_harvester(h, proto.is_harvester);
    world.set_unit_capacity(h, proto.passengers);
    if let Some(u) = world.units.get_mut(h) {
        if proto.is_infantry {
            u.make_infantry(spot);
        }
        u.is_civ_evac = proto.is_civ_evac;
    }
    h
}

/// Pick a spawn cell near `base`, spiralling out to avoid stacking every member on
/// one cell. Falls back to `base` if nothing better is on-map.
fn disperse_cell(world: &World, base: CellCoord, slot: i32) -> CellCoord {
    // A fixed spiral of offsets (deterministic, no RNG).
    const OFFS: [(i32, i32); 9] = [
        (0, 0),
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
    ];
    let (w, h) = (world.passable.width(), world.passable.height());
    let (dx, dy) = OFFS[(slot as usize) % OFFS.len()];
    let ring = (slot as usize / OFFS.len()) as i32 + 1;
    let c = CellCoord::new(base.x + dx * ring, base.y + dy * ring);
    if c.x >= 0 && c.y >= 0 && c.x < w && c.y < h {
        c
    } else {
        base
    }
}

/// System 7: ore growth/spread — the deferred M5 economy step (`MapClass::Logic`,
/// `map.cpp:1308-1432`). A persistent cursor sweeps the map a slice per tick,
/// reservoir-sampling grow- and spread-eligible cells (drawing the sync RNG at
/// each eligible cell, `map.cpp:1367,1385`); each time the cursor wraps the whole
/// map, one grow+spread wave fires (density++ on grow cells, and spread cells
/// germinate fresh ore on a random-facing neighbour, `cell.cpp:3150,3176`).
///
/// **This legitimately consumes the sync RNG** — the M5 "economy draws no RNG"
/// pin is updated for exactly this reason (see the ore module docs). Off unless a
/// skirmish enabled it via [`World::set_ore_growth`], so non-skirmish worlds are
/// unaffected.
fn run_ore_growth(world: &mut World) {
    let Some(mut g) = world.ore_growth.take() else {
        return;
    };
    if !g.grows && !g.spreads {
        world.ore_growth = Some(g);
        return;
    }
    let mut rng = world.rng;

    let w = world.ore.width();
    let h = world.ore.height();
    let total = w * h;
    if total <= 0 {
        world.rng = rng;
        world.ore_growth = Some(g);
        return;
    }
    let growth_rate = world.catalog.econ.growth_rate.max(1);
    let ticks_per_minute = world.catalog.econ.ticks_per_minute.max(1);
    // Cells scanned this tick: MAP_CELL_TOTAL / (GrowthRate · TICKS_PER_MINUTE),
    // floored at 1 (map.cpp:1340).
    let subcount = (total / (growth_rate * ticks_per_minute)).max(1);

    for _ in 0..subcount {
        if g.scan >= total {
            break;
        }
        let cell = CellCoord::new(g.scan % w, g.scan / w);
        g.scan += 1;

        if g.grows && world.ore.can_grow(cell) {
            reservoir_record(&mut rng, &mut g.grow_list, &mut g.grow_excess, cell);
        }
        if g.spreads && world.ore.can_spread(cell) {
            reservoir_record(&mut rng, &mut g.spread_list, &mut g.spread_excess, cell);
        }
    }

    // Wave fires once the cursor has swept the whole map (map.cpp:1406).
    if g.scan >= total {
        g.scan = 0;
        let grow_cells = std::mem::take(&mut g.grow_list);
        let spread_cells = std::mem::take(&mut g.spread_list);
        for c in grow_cells {
            world.ore.grow(c);
        }
        for c in spread_cells {
            spread_one(world, &mut rng, c);
        }
        g.grow_excess = 0;
        g.spread_excess = 0;
    }

    world.rng = rng;
    world.ore_growth = Some(g);
}

/// Reservoir-sample `cell` into a capped candidate list, drawing the sync RNG
/// exactly as `MapClass::Logic` does (`map.cpp:1367-1371`): a gate draw of
/// `Random_Pick(0, excess)` compared to the current count, then (when the list is
/// full) a replacement-slot draw. `excess` is incremented after the gate.
fn reservoir_record(
    rng: &mut RandomLcg,
    list: &mut Vec<CellCoord>,
    excess: &mut i32,
    cell: CellCoord,
) {
    let gate = rng.range(0, *excess);
    if gate <= list.len() as i32 {
        if list.len() < TIB_LIST_CAP {
            list.push(cell);
        } else {
            let slot = rng.range(0, TIB_LIST_CAP as i32 - 1) as usize;
            list[slot] = cell;
        }
    }
    *excess += 1;
}

/// Spread one dense ore cell to a neighbour (`CellClass::Spread_Tiberium`,
/// `cell.cpp:3176`): pick a random start facing, scan the eight neighbours from
/// there, and germinate the first empty, buildable one. The facing arithmetic is
/// deliberately **not** wrapped mod 8 (matching the original `index + offset`,
/// which yields out-of-range facings that are simply skipped).
fn spread_one(world: &mut World, rng: &mut RandomLcg, cell: CellCoord) {
    let offset = rng.range(0, 7); // Random_Pick(FACING_N, FACING_NW)
    for index in 0..8i32 {
        let facing = index + offset; // not wrapped — mirrors the original
        let Some((dx, dy)) = facing_delta(facing) else {
            continue;
        };
        let c = CellCoord::new(cell.x + dx, cell.y + dy);
        if germinate_ok(world, c) {
            world.ore.germinate(c);
            // Random_Pick(OVERLAY_GOLD1, OVERLAY_GOLD4) — we only model "gold",
            // but draw to keep the sync-RNG consumption faithful (cell.cpp:3187).
            let _ = rng.range(0, 3);
            return;
        }
    }
}

/// Whether an empty cell may germinate new ore (`Can_Tiberium_Germinate`,
/// `cell.cpp:3209`, simplified): on-map, no ore already, buildable ground, and
/// not covered by a building footprint.
fn germinate_ok(world: &World, cell: CellCoord) -> bool {
    cell.on_map()
        && !world.ore.has_ore(cell)
        && world.passable.is_static_passable(cell)
        && !world.passable.is_occupied(cell)
}

/// The (dx, dy) offset for an RA `FacingType` (N=0, NE=1, E=2, SE=3, S=4, SW=5,
/// W=6, NW=7). Facings ≥ 8 (from the unwrapped `index + offset`) have no
/// neighbour and return `None`, matching `Adjacent_Cell` returning NULL.
fn facing_delta(facing: i32) -> Option<(i32, i32)> {
    match facing {
        0 => Some((0, -1)),
        1 => Some((1, -1)),
        2 => Some((1, 0)),
        3 => Some((1, 1)),
        4 => Some((0, 1)),
        5 => Some((-1, 1)),
        6 => Some((-1, 0)),
        7 => Some((-1, -1)),
        _ => None,
    }
}

/// Validate and enact a single command.
fn apply_command(world: &mut World, cmd: Command) {
    match cmd {
        Command::Move { unit, dest, house } => {
            // Ownership check (§4.6): silently ignore orders for units the
            // issuing house does not own, or stale handles.
            let (start, loco, is_inf, is_air) = match world.units.get(unit) {
                Some(u) if u.house == house => {
                    (u.cell(), u.locomotor, u.is_infantry(), u.is_aircraft())
                }
                _ => return,
            };
            // Aircraft fly straight to the ordered cell (no ground A*): just set the
            // fly destination and clear any attack target; `run_aircraft` flies it
            // there at altitude, ignoring terrain (`Process_Fly_To`).
            if is_air {
                if let Some(u) = world.units.get_mut(unit) {
                    u.dest = Some(dest);
                    u.target = None;
                    u.air_state = AirState::Idle;
                }
                return;
            }
            // Group dispersal (`Adjust_Dest`/scatter, `unit.cpp`): a box-selected
            // group ordered to one cell must not all stack there. Each unit picks
            // the nearest free cell not already claimed by another unit's
            // destination — vehicles one per cell, infantry up to five per cell.
            let goal = pick_dest(world, dest, unit, is_inf, loco);
            if let Some(path) = find_path(&world.passable, start, goal, loco) {
                if let Some(u) = world.units.get_mut(unit) {
                    u.path = path;
                    u.dest = Some(goal);
                    u.target = None; // a move order overrides an attack
                    u.guard_target = false; // player order, not a leashed guard acquire
                                            // A manual move interrupts harvesting; the FSM resumes
                                            // (state `Idle` → `Looking`) once the unit arrives.
                    if u.is_harvester {
                        u.harvest.status = HarvStatus::Idle;
                    }
                }
            }
        }
        Command::Stop { unit, house } => {
            if let Some(u) = world.units.get_mut(unit) {
                if u.house == house {
                    u.path.clear();
                    u.dest = None;
                    u.target = None;
                    u.guard_target = false;
                }
            }
        }
        Command::Attack {
            unit,
            target,
            house,
        } => {
            // Reject the order up front for unowned/stale units and self-targeting.
            // Armed units accept any target (`run_combat` drives approach/aim/fire);
            // an **engineer** (unarmed infantry) accepts a *building* target so it
            // can march in to capture it (`run_engineers`).
            let ok = match world.units.get(unit) {
                Some(u) => {
                    u.house == house
                        && (u.weapon.is_some()
                            || (is_engineer(u) && matches!(target, Target::Building(_))))
                }
                None => false,
            };
            if !ok {
                return;
            }
            match target {
                Target::Unit(t) if t == unit || !world.units.contains(t) => return,
                Target::Building(t) if !world.buildings.contains(t) => return,
                _ => {}
            }
            // A submerged enemy submarine cannot be explicitly targeted by a unit
            // whose house has no detector nearby (naval arc): the sub is invisible,
            // so the click resolves to nothing (matches auto-acquisition gating).
            if let Target::Unit(t) = target {
                if world
                    .units
                    .get(t)
                    .is_some_and(|tu| is_hidden_submarine(world, tu, house))
                {
                    return;
                }
            }
            if let Some(u) = world.units.get_mut(unit) {
                u.target = Some(target);
                u.guard_target = false; // explicit player order — chase, never leash
                                        // Clear a stale movement destination; approach is driven by the
                                        // combat system toward the target, not a prior move order.
                u.dest = None;
                u.path.clear();
            }
        }
        Command::Deploy { unit, house } => apply_deploy(world, unit, house),
        Command::StartProduction { house, item } => apply_start_production(world, house, item),
        Command::PlaceBuilding {
            house,
            building,
            cell,
        } => apply_place_building(world, house, building, cell),
        Command::CancelProduction { house, kind } => apply_cancel_production(world, house, kind),
        Command::Sell { house, building } => apply_sell(world, house, building),
        Command::Repair { house, building } => apply_repair(world, house, building),
        Command::Load {
            passenger,
            transport,
            house,
        } => apply_load(world, passenger, transport, house),
        Command::Unload { transport, house } => apply_unload(world, transport, house),
    }
}

/// Whether two cells are within one cell of each other (Chebyshev ≤ 1), i.e. the
/// passenger stands on or beside the transport's cell.
fn cells_adjacent(a: CellCoord, b: CellCoord) -> bool {
    (a.x - b.x).abs() <= 1 && (a.y - b.y).abs() <= 1
}

/// System 5.5: complete pending `Load` orders. A unit with a `board_target` that
/// has reached its transport (adjacent) boards it; if the transport vanished or
/// filled up, the intent is dropped. Processed in slot order (deterministic).
fn run_transports(world: &mut World) {
    // Scripted auto-unload: a transport with a set drop-off point that has arrived
    // (path empty, within a cell of the target) disgorges its cargo there.
    let arrivers: Vec<(Handle, u8)> = world
        .units
        .iter()
        .filter(|(_, u)| {
            u.unload_at.is_some() && u.is_alive() && !u.cargo.is_empty() && u.path.is_empty()
        })
        .filter(|(_, u)| {
            u.unload_at
                .map(|c| cells_adjacent(u.cell(), c) || u.cell() == c)
                .unwrap_or(false)
        })
        .map(|(h, u)| (h, u.house))
        .collect();
    for (h, house) in arrivers {
        apply_unload(world, h, house);
        if let Some(u) = world.units.get_mut(h) {
            if u.cargo.is_empty() {
                u.unload_at = None;
            }
        }
    }

    let pending: Vec<Handle> = world
        .units
        .iter()
        .filter(|(_, u)| u.board_target.is_some() && u.is_alive())
        .map(|(h, _)| h)
        .collect();
    for h in pending {
        let (target, p_cell) = match world.units.get(h) {
            Some(u) => (u.board_target, u.cell()),
            None => continue,
        };
        let Some(t) = target else { continue };
        // Transport still valid and has room?
        let ok = world
            .units
            .get(t)
            .map(|tr| tr.is_alive() && tr.capacity > 0 && (tr.cargo.len() as u8) < tr.capacity)
            .unwrap_or(false);
        if !ok {
            if let Some(u) = world.units.get_mut(h) {
                u.board_target = None;
            }
            continue;
        }
        let t_cell = world.units.get(t).map(|tr| tr.cell()).unwrap_or(p_cell);
        if cells_adjacent(p_cell, t_cell) {
            board_passenger(world, h, t);
        } else if world
            .units
            .get(h)
            .map(|u| u.path.is_empty())
            .unwrap_or(false)
        {
            // Arrived where we could but not adjacent (transport moved) — re-path.
            let loco = world
                .units
                .get(h)
                .map(|u| u.locomotor)
                .unwrap_or(Locomotor::Foot);
            if let Some(path) = find_path(&world.passable, p_cell, t_cell, loco) {
                if let Some(u) = world.units.get_mut(h) {
                    u.path = path;
                    u.dest = Some(t_cell);
                }
            } else if let Some(u) = world.units.get_mut(h) {
                // Give up if unreachable.
                u.board_target = None;
            }
        }
    }
}

/// `Command::Load`: an infantry boards an adjacent own transport, or walks to it
/// and boards on arrival (`run_transports`). Validates ownership, that the target
/// is a transport with spare capacity, and that the passenger is infantry.
fn apply_load(world: &mut World, passenger: Handle, transport: Handle, house: u8) {
    // Validate transport: own, alive, a transport, with room.
    let (t_cell, room) = match world.units.get(transport) {
        Some(t) if t.house == house && t.is_alive() && t.capacity > 0 => {
            (t.cell(), (t.cargo.len() as u8) < t.capacity)
        }
        _ => return,
    };
    if !room {
        return;
    }
    // Validate passenger: own, alive, infantry, not itself the transport.
    let (p_cell, p_loco) = match world.units.get(passenger) {
        Some(p)
            if p.house == house && p.is_alive() && p.is_infantry() && passenger != transport =>
        {
            (p.cell(), p.locomotor)
        }
        _ => return,
    };
    if cells_adjacent(p_cell, t_cell) {
        board_passenger(world, passenger, transport);
        return;
    }
    // Walk to the transport, remembering the intent so `run_transports` boards it
    // once adjacent.
    if let Some(path) = find_path(&world.passable, p_cell, t_cell, p_loco) {
        if let Some(p) = world.units.get_mut(passenger) {
            p.path = path;
            p.dest = Some(t_cell);
            p.target = None;
            p.guard_target = false;
            p.board_target = Some(transport);
        }
    } else if let Some(p) = world.units.get_mut(passenger) {
        // Can't path all the way (transport cell is occupied by the vehicle) —
        // still record the intent; `run_transports` boards on adjacency.
        p.board_target = Some(transport);
    }
}

/// Remove `passenger` from the map and stow it as cargo on `transport`.
fn board_passenger(world: &mut World, passenger: Handle, transport: Handle) {
    let snapshot = match world.units.get(passenger) {
        Some(p) => Passenger {
            type_id: p.type_id,
            house: p.house,
            health: p.health,
            max_health: p.max_health,
            stats: p.stats,
            armor: p.armor,
            weapon: p.weapon,
            secondary: p.secondary,
            has_turret: p.has_turret,
            sight: p.sight,
            is_infantry: p.is_infantry(),
            mission: p.mission,
        },
        None => return,
    };
    // Only stow if there is still room (guards a race between two Load orders).
    let ok = world
        .units
        .get(transport)
        .map(|t| (t.cargo.len() as u8) < t.capacity)
        .unwrap_or(false);
    if !ok {
        return;
    }
    if let Some(t) = world.units.get_mut(transport) {
        t.cargo.push(snapshot);
    }
    world.units.remove(passenger);
}

/// `Command::Unload`: a transport disgorges every passenger onto a free adjacent
/// spot, resuming each passenger's mission. Passengers with no free spot stay
/// aboard.
fn apply_unload(world: &mut World, transport: Handle, house: u8) {
    let (t_cell, mut cargo) = match world.units.get_mut(transport) {
        Some(t) if t.house == house && t.is_alive() && !t.cargo.is_empty() => {
            (t.cell(), std::mem::take(&mut t.cargo))
        }
        _ => return,
    };
    let mut leftover: Vec<Passenger> = Vec::new();
    for p in cargo.drain(..) {
        match free_unload_cell(world, t_cell, p.is_infantry) {
            Some((cell, spot)) => materialise_passenger(world, &p, cell, spot),
            None => leftover.push(p),
        }
    }
    if let Some(t) = world.units.get_mut(transport) {
        t.cargo = leftover;
    }
}

/// Find a free cell (and infantry sub-cell spot) adjacent to `from` for an
/// unloaded passenger: passable, and — for infantry — with a free sub-cell spot;
/// for a vehicle, not already holding a vehicle. Spiral out one ring.
fn free_unload_cell(world: &World, from: CellCoord, is_infantry: bool) -> Option<(CellCoord, u8)> {
    let loco = if is_infantry {
        Locomotor::Foot
    } else {
        Locomotor::Wheel
    };
    for (dx, dy) in [
        (0, -1),
        (1, 0),
        (0, 1),
        (-1, 0),
        (-1, -1),
        (1, -1),
        (1, 1),
        (-1, 1),
        (0, 0),
    ] {
        let c = CellCoord::new(from.x + dx, from.y + dy);
        if !world.passable.is_passable_loco(c, loco) {
            continue;
        }
        if is_infantry {
            // Free sub-cell spot, and no vehicle sharing the cell (Q5.3).
            if !vehicle_in_cell(world, c, None) {
                let bits = infantry_spot_bits(world, c, None);
                if let Some(spot) = crate::occupancy::closest_free_spot_bits(bits, 0) {
                    return Some((c, spot));
                }
            }
        } else if !vehicle_in_cell(world, c, None) && infantry_spot_bits(world, c, None) & 0x1F == 0
        {
            return Some((c, 0));
        }
    }
    None
}

/// Re-materialise an unloaded passenger as a live unit at `cell`/`spot`.
fn materialise_passenger(world: &mut World, p: &Passenger, cell: CellCoord, spot: u8) {
    let h = world.spawn_unit(p.type_id, p.house, cell, Facing(0), p.health, p.stats);
    world.set_unit_max_health(h, p.max_health);
    world.set_unit_sight(h, p.sight);
    world.set_unit_combat(h, p.armor, p.weapon, p.has_turret);
    world.set_unit_secondary(h, p.secondary);
    if let Some(u) = world.units.get_mut(h) {
        if p.is_infantry {
            u.make_infantry(spot);
        }
        u.mission = p.mission;
        if p.mission == Mission::AreaGuard {
            u.guard_post = Some(cell);
        }
    }
}

/// Deploy an MCV into its construction yard (`unit.cpp:1437`).
fn apply_deploy(world: &mut World, unit: Handle, house: u8) {
    // Ownership + is-MCV check.
    let (unit_cell, u_house, type_id) = match world.units.get(unit) {
        Some(u) => (u.cell(), u.house, u.type_id),
        None => return,
    };
    // Resolve the deploy target from the unit's proto (matched by sprite id).
    let building_id = world
        .catalog
        .units
        .iter()
        .find(|p| p.sprite_id == type_id && p.deploys_to.is_some())
        .and_then(|p| p.deploys_to);
    let (Some(building_id), true) = (building_id, u_house == house) else {
        return;
    };
    // The yard's top-left sits one cell NW of the MCV's cell (centred on it).
    let top_left = CellCoord::new(unit_cell.x - 1, unit_cell.y - 1);
    // The MCV's own cells are currently unoccupied (units don't block), so the
    // footprint check is a pure terrain/occupancy test.
    if !footprint_placeable(world, building_id, top_left) {
        return;
    }
    world.units.remove(unit);
    let bhandle = world.spawn_building(building_id, house, top_left);
    // A construction yard is the first building; refineries elsewhere spawn a
    // harvester, but the CONST does not.
    let _ = bhandle;
}

/// Begin producing an item, with prerequisite/factory/funds validation.
fn apply_start_production(world: &mut World, house: u8, item: BuildItem) {
    let Some(hs) = world.houses.get(house as usize) else {
        return;
    };

    // Resolve cost + prerequisites + the required factory for this item.
    // Infantry (a unit proto with `is_infantry`) build on their own barracks
    // strip, independent of the war factory's vehicle lane (M7.6).
    let (cost, prereq, need_yard, need_factory, need_barracks, need_helipad, need_shipyard, kind) =
        match item {
            BuildItem::Building(id) => match world.catalog.building(id) {
                Some(p) => (
                    p.cost,
                    p.prereq.clone(),
                    true,
                    false,
                    false,
                    false,
                    false,
                    ProdKind::Building,
                ),
                None => return,
            },
            BuildItem::Unit(id) => match world.catalog.unit(id) {
                Some(p) if p.is_infantry => (
                    p.cost,
                    p.prereq.clone(),
                    false,
                    false,
                    true,
                    false,
                    false,
                    ProdKind::Infantry,
                ),
                // Aircraft build from a **helipad** (not the war factory), on the
                // vehicle (Unit) lane. `SPEED_WINGED` protos need a helipad present.
                Some(p) if loco_from_index(p.locomotor) == Locomotor::Air => (
                    p.cost,
                    p.prereq.clone(),
                    false,
                    false,
                    false,
                    true,
                    false,
                    ProdKind::Unit,
                ),
                // Vessels build from a **naval yard** (SYRD/SPEN), on the vehicle
                // (Unit) lane, and spawn into an adjacent water cell (naval arc).
                Some(p) if loco_from_index(p.locomotor) == Locomotor::Water => (
                    p.cost,
                    p.prereq.clone(),
                    false,
                    false,
                    false,
                    false,
                    true,
                    ProdKind::Unit,
                ),
                Some(p) => (
                    p.cost,
                    p.prereq.clone(),
                    false,
                    true,
                    false,
                    false,
                    false,
                    ProdKind::Unit,
                ),
                None => return,
            },
        };

    // Slot must be free.
    let slot_busy = match kind {
        ProdKind::Building => hs.building_prod.is_some() || hs.ready_building.is_some(),
        ProdKind::Unit => hs.unit_prod.is_some(),
        ProdKind::Infantry => hs.infantry_prod.is_some(),
    };
    if slot_busy {
        return;
    }

    // Prerequisites: every required building type must be owned.
    if !prereq.iter().all(|&id| hs.owns_building(id)) {
        return;
    }
    // The producing factory must exist among the house's live buildings.
    let has_yard = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_construction_yard);
    let has_factory = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_war_factory);
    let has_barracks = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_barracks);
    let has_helipad = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_alive() && building_is_helipad(world, b.type_id));
    let has_shipyard = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_alive() && building_is_shipyard(world, b.type_id));
    if (need_yard && !has_yard)
        || (need_factory && !has_factory)
        || (need_barracks && !has_barracks)
        || (need_helipad && !has_helipad)
        || (need_shipyard && !has_shipyard)
    {
        return;
    }
    // Must be able to afford at least the first installment (any credits).
    if world.house_credits(house) <= 0 {
        return;
    }

    // Build time, **snapshotted here at production START** (the original bakes
    // it into the factory Rate once in `FactoryClass::Start` and never recomputes
    // it while the build runs). `Catalog::time_to_build` applies, in the
    // reference's order: `Cost × Rule.BuildSpeedBias × (TICKS_PER_MINUTE/1000)`
    // (techno.cpp:6777), then the discrete low-power ×4/×2.5/×1.5 snapshot
    // (techno.cpp:6832), then the STEP_COUNT rate conversion (factory.cpp:432).
    // M7.9 P0: the `Rule.BuildSpeedBias` (stock `.8`) and STEP_COUNT steps were
    // both missing before, so our builds ran ~25% too slow.
    let handicap = world
        .houses
        .get(house as usize)
        .map(|h| h.handicap)
        .unwrap_or_default();
    let (scale_n, scale_d) = world
        .houses
        .get(house as usize)
        .map(|h| h.build_time_scale())
        .unwrap_or((1, 1));
    let mut total_ticks = world.catalog.time_to_build(cost, scale_n, scale_d);
    // Difficulty handicaps (M7.9 P2a), applied house-scoped:
    //  - BuildTime bias scales the (already STEP_COUNT-quantised) build time. The
    //    original folds it into `Time_To_Build` *before* the STEP_COUNT divide
    //    (`Assign_Handicap` BuildSpeedBias); we apply it after, a benign rounding
    //    difference and a no-op for a neutral (1.0) house.
    //  - Cost bias scales the credits charged (`Cost * House->CostBias`), NOT the
    //    build time (which uses raw cost), matching the original's Purchase_Price.
    if !handicap.is_neutral() {
        total_ticks = crate::house::fx_mul(total_ticks, handicap.build_time).max(1);
    }
    let biased_cost = if handicap.is_neutral() {
        cost
    } else {
        crate::house::fx_mul(cost, handicap.cost).max(0)
    };
    let prod = Production {
        item,
        cost: biased_cost,
        total_ticks,
        progress: 0,
        spent: 0,
        done: false,
    };
    if let Some(hs) = world.houses.get_mut(house as usize) {
        match kind {
            ProdKind::Building => hs.building_prod = Some(prod),
            ProdKind::Unit => hs.unit_prod = Some(prod),
            ProdKind::Infantry => hs.infantry_prod = Some(prod),
        }
    }
}

/// Place a completed building, with footprint + proximity validation.
fn apply_place_building(world: &mut World, house: u8, building: u32, cell: CellCoord) {
    place_building_inner(world, house, building, cell, true);
}

/// Place a house's ready building at `cell`. `require_proximity` enforces the
/// build-adjacency rule for normal (player/AI) placement; the campaign `[Base]`
/// rebuild passes `false` because the reference rebuilds at the exact scripted
/// `node->Cell` (building.cpp:2196), bypassing `Find_Build_Location`.
fn place_building_inner(
    world: &mut World,
    house: u8,
    building: u32,
    cell: CellCoord,
    require_proximity: bool,
) {
    // Must have this exact structure ready.
    let ready = world
        .houses
        .get(house as usize)
        .and_then(|h| h.ready_building);
    if ready != Some(building) {
        return;
    }
    if !footprint_placeable(world, building, cell) {
        return;
    }
    if require_proximity && !passes_proximity(world, house, building, cell) {
        return;
    }
    // Consume the ready slot and place.
    if let Some(hs) = world.houses.get_mut(house as usize) {
        hs.ready_building = None;
    }
    let Some(bhandle) = world.spawn_building(building, house, cell) else {
        return;
    };
    // Refineries spawn a free harvester adjacent-south of their centre
    // (`building.cpp:2640`, DIR_S).
    let (is_refinery, free_unit, center) = world
        .buildings
        .get(bhandle)
        .map(|b| (b.is_refinery, None::<u32>, b.center_cell()))
        .unwrap_or((false, None, cell));
    let _ = free_unit;
    if is_refinery {
        let free = world
            .catalog
            .building(building)
            .and_then(|p| p.free_harvester_unit);
        if let Some(unit_id) = free {
            let dock = CellCoord::new(center.x, center.y + 1);
            spawn_free_harvester(world, unit_id, house, dock, cell);
        }
    }
}

/// Cancel a house's production of the given kind, refunding what was spent.
fn apply_cancel_production(world: &mut World, house: u8, kind: ProdKind) {
    let Some(hs) = world.houses.get_mut(house as usize) else {
        return;
    };
    let refund = match kind {
        ProdKind::Building => {
            let r = hs.building_prod.map(|p| p.spent).unwrap_or(0);
            hs.building_prod = None;
            // A completed-but-unplaced building is also cancellable (full refund
            // of its cost is not tracked separately; ready buildings were fully
            // paid, so cancelling one refunds its cost).
            if let Some(id) = hs.ready_building.take() {
                let cost = world.catalog.building(id).map(|p| p.cost).unwrap_or(0);
                return refund_credits(world, house, cost);
            }
            r
        }
        ProdKind::Unit => {
            let r = hs.unit_prod.map(|p| p.spent).unwrap_or(0);
            hs.unit_prod = None;
            r
        }
        ProdKind::Infantry => {
            let r = hs.infantry_prod.map(|p| p.spent).unwrap_or(0);
            hs.infantry_prod = None;
            r
        }
    };
    refund_credits(world, house, refund);
}

fn refund_credits(world: &mut World, house: u8, amount: i32) {
    if let Some(hs) = world.houses.get_mut(house as usize) {
        hs.credits += amount;
    }
}

/// Sell a building the issuing house owns: refund `RefundPercent` of its cost
/// (default 50%, a flat fraction independent of current health,
/// `techno.cpp:6417`) and clear its footprint. Own-building placement/sell is a
/// player action; the AI never sells (it rebuilds).
fn apply_sell(world: &mut World, house: u8, building: Handle) {
    let (owner, cost) = match world.buildings.get(building) {
        Some(b) if b.is_alive() => (b.house, b.cost),
        _ => return,
    };
    if owner != house {
        return;
    }
    let refund = (cost as i64 * world.catalog.econ.refund_percent as i64 / 100) as i32;
    remove_building(world, building);
    refund_credits(world, house, refund);
}

/// Toggle repair on a building the issuing house owns (`BuildingClass::Repair(-1)`,
/// `building.cpp:2725`). Walls are `OverlayType`s in the original and can never be
/// repaired, so `is_wall` buildings are refused (consistent with QUIRKS Q9/Q11c).
/// Toggling *off* stops the drain; toggling *on* a full-health building is a no-op
/// (the original plays the "scold" cue and does nothing — `building.cpp:2754`).
fn apply_repair(world: &mut World, house: u8, building: Handle) {
    let Some(b) = world.buildings.get_mut(building) else {
        return;
    };
    if b.house != house || !b.is_alive() || b.is_wall {
        return;
    }
    if !b.is_repairing && b.health >= b.max_health {
        return; // already full — nothing to repair (VOC_SCOLD)
    }
    b.is_repairing = !b.is_repairing;
}

/// System 3.6: **building self-repair** (`BuildingClass::Repair_AI`,
/// `building.cpp:5860`). On the same global repair cadence as the service depot,
/// each building whose owner has toggled repair heals `Rule.RepairStep` HP,
/// charging `RepairPercent × (Cost / (MaxStrength / RepairStep))` credits per
/// step (`TechnoTypeClass::Repair_Cost`, `techno.cpp:6907`, floored to ≥1). It
/// stops (clears the toggle) at full health or when the house cannot pay the step
/// — exactly the original's two exit conditions.
fn run_building_repair(world: &mut World) {
    if !world.tick_count.is_multiple_of(REPAIR_INTERVAL) {
        return;
    }
    let handles: Vec<Handle> = world
        .buildings
        .iter()
        .filter(|(_, b)| b.is_repairing && b.is_alive())
        .map(|(h, _)| h)
        .collect();
    for h in handles {
        let (house, cost, max_health, health) = match world.buildings.get(h) {
            Some(b) => (b.house, b.cost, b.max_health as i32, b.health as i32),
            None => continue,
        };
        if health >= max_health {
            if let Some(b) = world.buildings.get_mut(h) {
                b.is_repairing = false;
            }
            continue;
        }
        // Repair_Cost = (Cost / (MaxStrength / RepairStep)) * RepairPercent, ≥ 1.
        // RepairStep / RepairPercent now come from `EconRules` (loaded from
        // rules.ini `[General] RepairStep`/`RepairPercent`; stock values 7 HP and
        // 20% = 1/5, which rules.ini overrides the reference compile-time
        // 5 / 1/4 with — M7.9.1 audit / M7.5 P0 promotion).
        let brepair_step = world.catalog.econ.brepair_step;
        let denom = (max_health / brepair_step.max(1)).max(1);
        let step_cost = ((cost / denom) * world.catalog.econ.brepair_percent_num
            / world.catalog.econ.brepair_percent_den.max(1))
        .max(1);
        if world.house_credits(house) < step_cost {
            // Can't afford this step: stop repairing (building.cpp:5877).
            if let Some(b) = world.buildings.get_mut(h) {
                b.is_repairing = false;
            }
            continue;
        }
        if let Some(hh) = world.houses.get_mut(house as usize) {
            hh.deduct(step_cost);
        }
        if let Some(b) = world.buildings.get_mut(h) {
            b.health = (b.health + brepair_step as u16).min(b.max_health);
            if b.health >= b.max_health {
                b.is_repairing = false;
            }
        }
    }
}

/// Remove a building from the world: clear its footprint occupancy (mirroring
/// `MapClass::Pick_Up` → `CellClass::Occupy_Up`, `map.cpp:1056`), reverse the
/// owning house's power totals and building-type count, then drop it from the
/// arena. Used by combat death (`run_bullets`) and [`Command::Sell`].
///
/// **Deviation.** The original starts an 8-tick explosion countdown before
/// removal (`building.cpp:1343`) and drops debris/survivors (`Drop_Debris`); we
/// remove immediately. Death animations and rubble are a documented M7 seam.
fn remove_building(world: &mut World, handle: Handle) {
    let Some(b) = world.buildings.get(handle) else {
        return;
    };
    let house = b.house;
    let power = b.power;
    let type_id = b.type_id;
    let was_construction_yard = b.is_construction_yard;
    let was_war_factory = b.is_war_factory;
    let was_barracks = b.is_barracks;
    let cells: Vec<CellCoord> = b.footprint().collect();
    for c in cells {
        world.passable.set_occupied(c, false);
    }
    if let Some(hs) = world.houses.get_mut(house as usize) {
        if power >= 0 {
            hs.power_output -= power;
        } else {
            hs.power_drain -= -power;
        }
        hs.adjust_building_count(type_id, -1);
    }
    world.buildings.remove(handle);

    // Abandon orphaned production: removing the last building able to host a
    // production lane abandons its in-flight object and refunds the credits
    // already spent (`BuildingClass::Detach_All` → `FactoryClass::Abandon`,
    // `building.cpp:5138`, `factory.cpp:479` `Refund_Money(money - Balance)`).
    // This runs on *both* Sell and combat destruction, exactly as Detach_All
    // does, and fixes the M6 sell-while-producing soft-lock (a stuck-done unit
    // lane with no factory left to exit from).
    if was_construction_yard && !house_has_construction_yard(world, house) {
        abandon_production_lane(world, house, ProdKind::Building);
    }
    if was_war_factory && !house_has_war_factory(world, house) {
        abandon_production_lane(world, house, ProdKind::Unit);
    }
    if was_barracks && !house_has_barracks(world, house) {
        abandon_production_lane(world, house, ProdKind::Infantry);
    }

    // Reconcile stored tiberium against the now-lower storage capacity
    // *immediately* (M7.8 carried fix a). Selling or destroying a SILO/PROC
    // drops the house's `Capacity`; the original recomputes it on the spot and
    // clamps `House->Tiberium` down, wasting the excess with no refund
    // (`HouseClass::Silo_Redraw`/`Adjust_Capacity`). Previously the excess sat
    // stale until the next harvest tick silently clamped it — same net loss, but
    // deferred and invisible. Reconciling here makes the loss happen at the
    // moment capacity changes, matching the original's timing.
    let new_cap = house_storage_capacity(world, house);
    if let Some(hs) = world.houses.get_mut(house as usize) {
        hs.reconcile_capacity(new_cap);
    }
}

/// Whether `house` still owns a live barracks (infantry factory).
/// A house's total credit-storage capacity: the sum of `Storage=` over its live
/// buildings (refineries + silos), `HouseClass::Capacity` (`house.cpp:46`).
pub fn house_storage_capacity(world: &World, house: u8) -> i32 {
    world
        .buildings
        .iter()
        .filter(|(_, b)| b.house == house && b.is_alive())
        .map(|(_, b)| b.storage)
        .sum()
}

fn house_has_barracks(world: &World, house: u8) -> bool {
    world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_barracks && b.is_alive())
}

/// Whether `house` still owns a live construction yard.
fn house_has_construction_yard(world: &World, house: u8) -> bool {
    world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_construction_yard && b.is_alive())
}

/// Whether `house` still owns a live war factory.
fn house_has_war_factory(world: &World, house: u8) -> bool {
    world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_war_factory && b.is_alive())
}

/// Abandon a house's in-flight production lane, refunding the portion of the
/// cost already paid (`FactoryClass::Abandon`, `factory.cpp:479`). The `spent`
/// field is exactly "money already paid" (the original's `money - Balance`), so
/// refunding it makes the abandoned build net-zero in credits. No-op if the lane
/// is empty. A completed-but-unplaced building (`ready_building`) is left intact
/// — placement does not require the construction yard in our model.
fn abandon_production_lane(world: &mut World, house: u8, kind: ProdKind) {
    let refund = if let Some(hs) = world.houses.get_mut(house as usize) {
        match kind {
            ProdKind::Building => hs.building_prod.take().map(|p| p.spent).unwrap_or(0),
            ProdKind::Unit => hs.unit_prod.take().map(|p| p.spent).unwrap_or(0),
            ProdKind::Infantry => hs.infantry_prod.take().map(|p| p.spent).unwrap_or(0),
        }
    } else {
        0
    };
    refund_credits(world, house, refund);
}

/// Whether building `building_id`'s footprint at top-left `cell` is a legal
/// placement: every footprint cell on-map, statically passable, and unoccupied.
fn footprint_placeable(world: &World, building_id: u32, cell: CellCoord) -> bool {
    let Some(proto) = world.catalog.building(building_id) else {
        return false;
    };
    for dy in 0..proto.foot_h as i32 {
        for dx in 0..proto.foot_w as i32 {
            let c = CellCoord::new(cell.x + dx, cell.y + dy);
            if !world.passable.is_static_passable(c) || world.passable.is_occupied(c) {
                return false;
            }
        }
    }
    // Naval yards (SYRD/SPEN) must be built on a shore: the footprint sits on land
    // (checked above) but at least one cell of its 8-neighbour adjacency ring must
    // be open water, so produced vessels have water to spawn into. Simplified port
    // of the water-adjacency requirement in `BuildingTypeClass` legal-placement
    // (`display.cpp`/`building.cpp` `Passes_Proximity_Check` naval bib).
    if building_is_shipyard(world, building_id) {
        let (x0, y0) = (cell.x - 1, cell.y - 1);
        let (x1, y1) = (cell.x + proto.foot_w as i32, cell.y + proto.foot_h as i32);
        let mut adjacent_water = false;
        for y in y0..=y1 {
            for x in x0..=x1 {
                if world.passable.is_water(CellCoord::new(x, y)) {
                    adjacent_water = true;
                }
            }
        }
        if !adjacent_water {
            return false;
        }
    }
    true
}

/// Simplified `Passes_Proximity_Check` (`display.cpp:671`): the footprint must
/// be adjacent (within one cell, 8-neighbourhood) to a cell owned by one of the
/// house's live buildings. The first building a house places (its construction
/// yard, via [`Command::Deploy`]) bypasses this — deploy does not call it — so a
/// base can be founded on empty ground. Deviation: we omit the radar/shroud
/// gating and the original's extra "one cell further" wall/bib allowances.
fn passes_proximity(world: &World, house: u8, building_id: u32, cell: CellCoord) -> bool {
    let Some(proto) = world.catalog.building(building_id) else {
        return false;
    };
    // Any house building at all? (Should always be true post-deploy.) If the
    // house owns nothing yet, allow placement (founding case).
    let owns_any = world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_alive());
    if !owns_any {
        return true;
    }
    // Expand the footprint by one cell and test overlap with owned footprints.
    let (x0, y0) = (cell.x - 1, cell.y - 1);
    let (x1, y1) = (cell.x + proto.foot_w as i32, cell.y + proto.foot_h as i32);
    world.buildings.iter().any(|(_, b)| {
        b.house == house
            && b.is_alive()
            && b.footprint()
                .any(|fc| fc.x >= x0 && fc.x <= x1 && fc.y >= y0 && fc.y <= y1)
    })
}

/// Spawn a refinery's free harvester at `dock` (falling back to a nearby free
/// cell), fully set up to harvest (`building.cpp:2644`).
fn spawn_free_harvester(
    world: &mut World,
    unit_id: u32,
    house: u8,
    dock: CellCoord,
    refinery_cell: CellCoord,
) {
    let Some(proto) = world.catalog.unit(unit_id).cloned() else {
        return;
    };
    // Find a free, passable, vehicle-unoccupied spawn cell: prefer the dock, else
    // scan outward (the dock/ring must not already hold a vehicle, or the free
    // harvester would violate the one-vehicle-per-cell rule).
    let free = |w: &World, c: CellCoord| w.passable.is_passable(c) && !vehicle_in_cell(w, c, None);
    let spawn_cell = if free(world, dock) {
        dock
    } else {
        let mut found = dock;
        'search: for r in 1..8 {
            for dy in -r..=r {
                for dx in -r..=r {
                    let c = CellCoord::new(refinery_cell.x + dx, refinery_cell.y + dy);
                    if free(world, c) {
                        found = c;
                        break 'search;
                    }
                }
            }
        }
        found
    };
    let handle = world.spawn_unit(
        proto.sprite_id,
        house,
        spawn_cell,
        Facing(192), // DIR_W, as the original places it
        proto.max_health,
        proto.stats,
    );
    world.set_unit_max_health(handle, proto.max_health);
    world.set_unit_combat(handle, proto.armor, proto.weapon, proto.has_turret);
    world.set_unit_secondary(handle, proto.secondary);
    world.set_unit_harvester(handle, proto.is_harvester);
    if let Some(u) = world.units.get_mut(handle) {
        u.set_locomotor(loco_from_index(proto.locomotor));
    }
    world.set_unit_sight(handle, proto.sight);
}

/// Pick primary vs. secondary weapon for a target of class `armor` at `dist`
/// leptons — a port of `TechnoClass::What_Weapon_Should_I_Use` (`techno.cpp:360`):
/// score each weapon by its warhead's `Verses[armor]` modifier, doubled when the
/// target is already within that weapon's range, and take the secondary only when
/// it *strictly* outscores the primary. With no secondary (the common case), the
/// primary is always used — so single-weapon units are byte-identical. This is
/// what makes a mammoth tank use its 120mm cannon (AP, high vs. heavy) against
/// tanks and its MammothTusk missiles (HE, high vs. none) against infantry, all
/// from the `Verses` table with no per-unit special-casing.
///
/// `pub` (not the file-private `fn` this started as) solely so
/// `ra-sim/tests/weapon_selection_matrix.rs` — an external-crate integration
/// test — can hand this a hand-derived truth table directly, the way the
/// task brief asks for. A visibility-only change, no logic touched; flagged
/// per ra-tester convention as a production-code edit made while writing
/// tests.
pub fn select_weapon(
    primary: WeaponProfile,
    secondary: Option<WeaponProfile>,
    armor: u8,
    dist: i32,
) -> WeaponProfile {
    let Some(sec) = secondary else {
        return primary;
    };
    let ai = (armor as usize).min(crate::combat::ARMOR_COUNT - 1);
    let score = |w: &WeaponProfile| -> i64 {
        let mut v = w.warhead.verses[ai] as i64;
        if dist <= w.range {
            v *= 2; // in-range bonus (`In_Range(target) => value *= 2`)
        }
        v
    };
    if score(&sec) > score(&primary) {
        sec
    } else {
        primary
    }
}

/// Combat system: for each unit (in slot order) decrement its rearm timer,
/// rotate its turret/body toward its target, approach if out of range, and fire
/// when aimed and rearmed. Ported from `UnitClass::Rotation_AI` +
/// `Firing_AI` + `Can_Fire` (`unit.cpp`). Iterating in slot order keeps the
/// sim-RNG draw sequence (bullet scatter) deterministic.
/// Ticks a surfaced submarine stays visible after losing its target before it
/// re-submerges — the reference's `PulseCountDown` recloak grace
/// (`Is_Allowed_To_Recloak`, vessel.cpp:2044). ~3s at 15 fps.
const SUB_RECLOAK_TICKS: u16 = 45;

/// Naval submarine surfacing FSM (naval arc P0): keep each submarine **submerged**
/// (cloaked, hidden from non-detector enemies) while idle, **surface** it while it
/// has a target, and hold it surfaced for a recloak grace window after it loses
/// the target (`VesselClass::Is_Allowed_To_Recloak`, vessel.cpp:2044). Runs before
/// `run_combat` so the `submerged` flag the stealth-acquisition gate reads this
/// tick reflects the prior tick's decision — deterministic, float-free. Inert (no
/// iteration cost beyond the `is_submarine` short-circuit) for any world with no
/// submarines, so every non-naval golden is byte-identical.
fn run_submarines(world: &mut World) {
    for handle in world.units.handles() {
        let Some(u) = world.units.get(handle) else {
            continue;
        };
        if !u.is_submarine || !u.is_alive() {
            continue;
        }
        let (submerged, recloak) = if u.target.is_some() {
            (false, SUB_RECLOAK_TICKS)
        } else if u.recloak > 0 {
            (false, u.recloak - 1)
        } else {
            (true, 0)
        };
        if let Some(u) = world.units.get_mut(handle) {
            u.submerged = submerged;
            u.recloak = recloak;
        }
    }
}

fn run_combat(world: &mut World) {
    for handle in world.units.handles() {
        // Aircraft run their own flight+combat FSM (`run_aircraft`); the ground
        // combat/approach path (which pathfinds on the ground grid) never touches
        // them.
        if world.units.get(handle).is_some_and(|u| u.is_aircraft()) {
            continue;
        }
        // Decrement the rearm countdown regardless of whether we fire.
        if let Some(u) = world.units.get_mut(handle) {
            if u.arm > 0 {
                u.arm -= 1;
            }
        }

        // Medic (MEDI) auto-acquire: a healer (a weapon whose Damage is negative)
        // with no live wounded target picks the nearest wounded friendly infantry
        // in range. It then fires its `Heal` weapon at it through the normal path;
        // `modify_damage` applies the negative damage as healing at point-blank on
        // unarmored targets (`combat.cpp:83`).
        maybe_acquire_heal_target(world, handle);

        // Guard / Area-Guard auto-acquire (`Target_Something_Nearby`, M7.5-B): a
        // unit standing at its post acquires an enemy that enters its engagement
        // envelope. This is what makes scenario "Guard" units actually fight.
        maybe_acquire_guard_target(world, handle);

        // Campaign auto-hunt (`MISSION_HUNT`): a hunting unit with no target and no
        // path acquires the nearest enemy anywhere on the map and attacks it. Set
        // by `TACTION_ALL_HUNT`, campaign attack-teams, or an INI `Hunt` mission.
        maybe_acquire_hunt_target(world, handle);

        // Snapshot what we need without holding a borrow across the RNG draw.
        let (primary, secondary, coord, turret, body, has_turret, rot, target) =
            match world.units.get(handle) {
                Some(u) => match (u.target, u.weapon) {
                    (Some(t), Some(w)) => (
                        w,
                        u.secondary,
                        u.coord,
                        u.turret_facing,
                        u.facing,
                        u.has_turret,
                        u.stats.rot,
                        t,
                    ),
                    _ => continue,
                },
                None => continue,
            };

        // Resolve the target's current aim point + armor class; drop stale/dead
        // targets. Armor drives the primary-vs-secondary weapon pick below.
        let drop_target = |world: &mut World| {
            if let Some(u) = world.units.get_mut(handle) {
                u.target = None;
                u.guard_target = false;
            }
        };
        let (target_coord, target_armor) = match target {
            Target::Unit(t) => match world.units.get(t) {
                Some(tu) if tu.is_alive() => (tu.coord, tu.armor),
                _ => {
                    drop_target(world);
                    continue;
                }
            },
            Target::Building(t) => match world.buildings.get(t) {
                Some(tb) if tb.is_alive() => (tb.center_cell().center(), tb.armor),
                _ => {
                    drop_target(world);
                    continue;
                }
            },
            // A ground/force-fire cell has no object: the original presumes
            // ARMOR_WOOD (index 1) for weapon selection (`techno.cpp:373`).
            Target::Cell(c) => (c.center(), 1u8),
        };

        // Pick primary vs. secondary for this target (`What_Weapon_Should_I_Use`,
        // `techno.cpp:360`): the weapon whose warhead does the greater `Verses`
        // modifier against the target's armor, each doubled when already in range.
        let dist_pre = leptons_distance(coord, target_coord);
        let weapon = select_weapon(primary, secondary, target_armor, dist_pre);

        // Desired aim direction toward the target.
        let desired = Facing::toward(coord, target_coord);

        // Rotate turret (turreted) or body (turretless) toward the target.
        if let Some(desired) = desired {
            if let Some(u) = world.units.get_mut(handle) {
                if has_turret {
                    u.turret_facing = u.turret_facing.rotate_toward(desired, rot.wrapping_add(1));
                } else {
                    u.facing = u.facing.rotate_toward(desired, rot.wrapping_add(1));
                    u.turret_facing = u.facing;
                }
            }
        }

        // Range check uses the original's octagonal `Distance` metric.
        let dist = leptons_distance(coord, target_coord);
        let in_range = dist <= weapon.range;

        if !in_range {
            // --- Guard leash (M7.5-B) ---
            // An auto-acquired guard target that has left the engagement envelope
            // is dropped rather than chased. Plain Guard never chases at all;
            // Area Guard chases but races home once it strays more than weapon
            // range from its post (`foot.cpp:1057`). Player Move/Attack orders,
            // retaliation, hunt, and base-alert targets carry `guard_target=false`
            // and always chase (fall through).
            let leash = world
                .units
                .get(handle)
                .filter(|u| u.guard_target)
                .map(|u| (u.mission, u.guard_post));
            if let Some((mission, post)) = leash {
                match mission {
                    Mission::Guard => {
                        // `Target_Something_Nearby(THREAT_RANGE)` drops an
                        // out-of-range target (`In_Range` → `Assign_Target(NONE)`).
                        if let Some(u) = world.units.get_mut(handle) {
                            u.target = None;
                            u.guard_target = false;
                        }
                        continue;
                    }
                    Mission::AreaGuard => {
                        let home = post.unwrap_or_else(|| coord.cell());
                        let strayed = leptons_distance(coord, home.center()) > weapon.range;
                        if strayed {
                            let loco = world
                                .units
                                .get(handle)
                                .map(|u| u.locomotor)
                                .unwrap_or(Locomotor::Track);
                            let path = find_path(&world.passable, coord.cell(), home, loco);
                            if let Some(u) = world.units.get_mut(handle) {
                                u.target = None;
                                u.guard_target = false;
                                if let Some(p) = path {
                                    u.path = p;
                                    u.dest = Some(home);
                                }
                            }
                            continue;
                        }
                        // else fall through and chase within the leash.
                    }
                    _ => {}
                }
            }
            // Approach: path toward the target. For a *building* the target cell
            // sits inside an impassable footprint, so path to the nearest passable
            // footprint-adjacent cell instead (else `find_path` to the occupied
            // centre returns `None` and the attacker never closes in).
            //
            // **Naval bombardment (naval arc P0).** A *vessel* (Water locomotor)
            // cannot stand on a land-adjacent cell, so `nearest_adjacent_passable`
            // (a *ground*-passable cell) is unreachable to it and it could never
            // shell a coastal base. Instead it closes to the nearest **water** cell
            // within weapon range of the building (`nearest_water_approach`) and
            // bombards from there — a cruiser/destroyer shelling the shore. This is
            // Water-locomotor-gated, so ground attackers are byte-identical.
            let atk_loco = world
                .units
                .get(handle)
                .map(|u| u.locomotor)
                .unwrap_or(Locomotor::Track);
            let goal = match target {
                Target::Building(t) => world
                    .buildings
                    .get(t)
                    .and_then(|b| {
                        if atk_loco == Locomotor::Water {
                            nearest_water_approach(&world.passable, b, coord.cell(), weapon.range)
                        } else {
                            nearest_adjacent_passable(&world.passable, b, coord.cell())
                        }
                    })
                    .unwrap_or_else(|| target_coord.cell()),
                _ => target_coord.cell(),
            };
            let need_path = world
                .units
                .get(handle)
                .map(|u| u.path.is_empty() || u.dest != Some(goal))
                .unwrap_or(false);
            if need_path {
                let loco = world
                    .units
                    .get(handle)
                    .map(|u| u.locomotor)
                    .unwrap_or(Locomotor::Track);
                if let Some(path) = find_path(&world.passable, coord.cell(), goal, loco) {
                    if let Some(u) = world.units.get_mut(handle) {
                        u.path = path;
                        u.dest = Some(goal);
                    }
                }
            }
            continue;
        }

        // In range: hold position (stop approaching) and try to fire.
        if let Some(u) = world.units.get_mut(handle) {
            u.path.clear();
            u.dest = None;
        }

        let aim = if has_turret { turret } else { body };
        let arm_ready = world.units.get(handle).map(|u| u.arm == 0).unwrap_or(false);
        let aligned = desired
            .map(|d| aligned_to_fire(aim, d, weapon.proj_rot))
            .unwrap_or(true);

        if arm_ready && aligned {
            let shooter_house = world.units.get(handle).map(|u| u.house).unwrap_or(0);
            fire(
                world,
                handle,
                shooter_house,
                coord,
                aim,
                target,
                target_coord,
                &weapon,
            );
            // ROF handicap (M7.9 P2a): the shooter's house scales its rearm delay
            // (`techno.cpp:3066`, `ROF * House->ROFBias`). Neutral = exact.
            let rof = house_rof_scaled(world, shooter_house, weapon.rof);
            if let Some(u) = world.units.get_mut(handle) {
                u.arm = rof;
            }
        }
    }
}

/// Scale a weapon's rearm delay by `house`'s ROF handicap (M7.9 P2a); neutral
/// houses return it unchanged (exact). At least 1 tick so firing can't become
/// instantaneous through rounding.
fn house_rof_scaled(world: &World, house: u8, rof: u16) -> u16 {
    match world.houses.get(house as usize) {
        Some(h) if !h.handicap.is_neutral() => {
            crate::house::fx_mul(rof as i32, h.handicap.rof).max(1) as u16
        }
        _ => rof,
    }
}

/// Tesla-coil charge-up time in ticks — the coil charges when it has a target,
/// then looses an instant bolt (`Charging_AI`, `building.cpp:45`: 9 stages of the
/// charge animation, `Fetch_Stage() >= 9`). Approximated at ~1s.
const TESLA_CHARGE_TICKS: u16 = 15;

/// Building turret rotation rate (`GUN` `ROT=12`). Emplaced defenses without a
/// turret fire in any direction (no alignment gate).
const BUILDING_TURRET_ROT: u8 = 12;

/// A handle that matches no live unit — the "shooter" of a building's shot (a
/// building is not a unit, so its bullet's `source_unit` is this sentinel; the
/// blast-exclusion and retaliation-naming lookups simply never resolve it).
const BUILDING_SHOOTER: Handle = Handle {
    index: u32::MAX,
    gen: u32::MAX,
};

/// System 4.5: **defense buildings** (PBOX/HBOX/GUN/FTUR/TSLA) — a building
/// analogue of [`run_combat`]. Each armed, alive building (slot order) decrements
/// its rearm timer, keeps or auto-acquires the nearest in-range enemy unit
/// (`BuildingClass::Mission_Guard` → `Greatest_Threat`/`Target_Something_Nearby`,
/// `building.cpp:3568`, `techno.cpp:5912` — simplified to nearest enemy *unit*),
/// rotates its turret (GUN only), and fires through the shared bullet path. Tesla
/// coils charge up first and require power (`Charging_AI`).
fn run_building_combat(world: &mut World) {
    for handle in world.buildings.handles() {
        let (weapon, has_turret, charges, house, center) = match world.buildings.get(handle) {
            Some(b) if b.is_alive() => match b.weapon {
                Some(w) => (
                    w,
                    b.has_turret,
                    b.charges,
                    b.house,
                    b.center_cell().center(),
                ),
                None => continue,
            },
            _ => continue,
        };
        // Decrement rearm.
        if let Some(b) = world.buildings.get_mut(handle) {
            if b.arm > 0 {
                b.arm -= 1;
            }
        }

        // Keep a still-valid target, else auto-acquire the nearest enemy unit. An
        // AA emplacement (AGUN/SAM) targets only airborne aircraft; every other
        // defense targets only ground units.
        let aa = world
            .buildings
            .get(handle)
            .map(|b| building_is_aa(world, b.type_id))
            .unwrap_or(false);
        let cur = world.buildings.get(handle).and_then(|b| b.target);
        let target = validate_building_target(world, cur, house, center, weapon.range, aa)
            .or_else(|| acquire_nearest_enemy(world, house, center, weapon.range, aa));
        if let Some(b) = world.buildings.get_mut(handle) {
            b.target = target;
        }
        let Some(target) = target else {
            if let Some(b) = world.buildings.get_mut(handle) {
                b.charge = 0; // no target — abandon any charge
            }
            continue;
        };
        let Some(target_coord) = building_target_position(world, target) else {
            continue;
        };

        // Aim: a turret (GUN) rotates and must align; a fixed emplacement fires
        // in any direction.
        let desired = Facing::toward(center, target_coord);
        if has_turret {
            if let (Some(d), Some(b)) = (desired, world.buildings.get_mut(handle)) {
                b.turret_facing = b.turret_facing.rotate_toward(d, BUILDING_TURRET_ROT);
            }
        }
        let aim = world
            .buildings
            .get(handle)
            .map(|b| b.turret_facing)
            .unwrap_or(Facing(0));
        let aligned = if has_turret {
            desired
                .map(|d| aligned_to_fire(aim, d, weapon.proj_rot))
                .unwrap_or(true)
        } else {
            true
        };
        let in_range = leptons_distance(center, target_coord) <= weapon.range;
        let arm_ready = world
            .buildings
            .get(handle)
            .map(|b| b.arm == 0)
            .unwrap_or(false);
        if !(in_range && aligned && arm_ready) {
            if let Some(b) = world.buildings.get_mut(handle) {
                b.charge = 0;
            }
            continue;
        }

        // Tesla coils charge up (requiring power) before the bolt (`Charging_AI`,
        // `House->Power_Fraction() >= 1`). Other defenses fire immediately.
        if charges {
            let powered = world.house(house).map(|h| !h.low_power()).unwrap_or(true);
            if !powered {
                if let Some(b) = world.buildings.get_mut(handle) {
                    b.charge = 0;
                }
                continue;
            }
            let c = world.buildings.get(handle).map(|b| b.charge).unwrap_or(0);
            if c + 1 < TESLA_CHARGE_TICKS {
                if let Some(b) = world.buildings.get_mut(handle) {
                    b.charge = c + 1;
                }
                continue; // still charging
            }
            if let Some(b) = world.buildings.get_mut(handle) {
                b.charge = 0;
            }
        }

        fire(
            world,
            BUILDING_SHOOTER,
            house,
            center,
            aim,
            target,
            target_coord,
            &weapon,
        );
        // ROF handicap (M7.9 P2a): the defense's house scales its rearm delay.
        let rof = house_rof_scaled(world, house, weapon.rof);
        if let Some(b) = world.buildings.get_mut(handle) {
            b.arm = rof;
        }
    }
}

/// Aircraft altitude change per tick, in leptons (`Pixel_To_Lepton(1)` — the
/// per-frame climb/descent rate in `Landing_Takeoff_AI`, `aircraft.cpp:4195`).
/// `256/24 ≈ 10`, i.e. [`crate::combat::PIXEL_LEPTON_W`].
const ALT_STEP: i32 = crate::combat::PIXEL_LEPTON_W;

/// Leptons within which an aircraft is considered "arrived" over its fly goal —
/// `Process_Fly_To` stops the craft at `< 0x0010` (`aircraft.cpp:2233`).
const AIR_ARRIVE: i32 = 0x0010;

/// System 4.6: **aircraft** flight + combat FSM (helicopters/fixed-wing). A
/// slimmed, deterministic port of the `AircraftClass` mission handlers
/// (`aircraft.cpp`): each aircraft flies straight toward its current goal at
/// altitude — ignoring all ground passability (`FlyClass::Physics`, `fly.cpp`) —
/// strafes its target until out of ammo, returns to its home helipad, lands,
/// rearms one round per `ReloadRate` cadence (`BuildingClass::Mission_Repair`
/// `RADIO_RELOAD`, `building.cpp:4433`), takes off, and resumes. Runs after
/// building combat so the RNG (bullet scatter) is still drawn in a fixed
/// unit→building→aircraft order. Aircraft never touch the ground movement/combat
/// systems (they are skipped there), so this is byte-inert for any world with no
/// aircraft — every prior golden is unchanged.
fn run_aircraft(world: &mut World) {
    // Rearm cadence: `Rule.ReloadRate` default `.05` min at full power
    // (`rules.cpp:172`); one round per this many ticks. (The rules.ini override and
    // the power-fraction slowdown, `building.cpp:4438`, are a documented deferral.)
    let reload_ticks = (world.catalog.econ.ticks_per_minute * 5 / 100).max(1) as u16;

    for handle in world.units.handles() {
        // Fire cooldown ticks down every frame (like `run_combat`).
        match world.units.get_mut(handle) {
            Some(u) if u.is_aircraft() => {
                if u.arm > 0 {
                    u.arm -= 1;
                }
            }
            _ => continue,
        }

        // Snapshot immutable-for-this-frame state.
        let (
            coord,
            altitude,
            ammo,
            max_ammo,
            mut state,
            target,
            home,
            house,
            weapon,
            secondary,
            dest,
            hunt,
        ) = match world.units.get(handle) {
            Some(u) => (
                u.coord,
                u.altitude,
                u.ammo,
                u.max_ammo,
                u.air_state,
                u.target,
                u.home,
                u.house,
                u.weapon,
                u.secondary,
                u.dest,
                u.hunt,
            ),
            None => continue,
        };

        // Resolve/validate the current target's aim point (ground objects only —
        // aircraft attack the ground). A stale/dead/ally/airborne target is dropped.
        let target_info: Option<(WorldCoord, u8)> = match target {
            Some(Target::Unit(t)) => match world.units.get(t) {
                Some(tu)
                    if tu.is_alive() && !world.are_allies(house, tu.house) && !tu.is_airborne() =>
                {
                    Some((tu.coord, tu.armor))
                }
                _ => None,
            },
            Some(Target::Building(t)) => match world.buildings.get(t) {
                Some(tb) if tb.is_alive() && !world.are_allies(house, tb.house) => {
                    Some((tb.center_cell().center(), tb.armor))
                }
                _ => None,
            },
            Some(Target::Cell(c)) => Some((c.center(), 1)),
            None => None,
        };
        // Drop an invalidated target.
        if target.is_some() && target_info.is_none() {
            if let Some(u) = world.units.get_mut(handle) {
                u.target = None;
            }
        }

        // A hunting aircraft with a magazine and nothing to shoot acquires the
        // nearest enemy **ground** target (unit, else building) — the AI/attack-team
        // path (`Mission_Guard` → juicy-target hunt, `aircraft.cpp:3902`).
        let mut target_info = target_info;
        if target_info.is_none() && hunt && ammo > 0 && weapon.is_some() {
            if let Some((tgt, info)) = acquire_air_ground_target(world, house, coord) {
                if let Some(u) = world.units.get_mut(handle) {
                    u.target = Some(tgt);
                }
                target_info = Some(info);
            }
        }

        // --- Decide the goal for this frame ---
        // Out of ammo (with a weapon) → return to base to rearm.
        if ammo == 0 && weapon.is_some() && !matches!(state, AirState::Rearming) {
            state = AirState::Returning;
        } else if target_info.is_some() && ammo > 0 && matches!(state, AirState::Idle) {
            state = AirState::Attack;
        } else if target_info.is_none() && matches!(state, AirState::Attack) {
            state = AirState::Idle;
        }

        let speed = world
            .units
            .get(handle)
            .map(|u| u.stats.max_speed)
            .unwrap_or(0)
            .max(1);
        let rot = world.units.get(handle).map(|u| u.stats.rot).unwrap_or(0);

        match state {
            AirState::Attack => {
                let (Some((tcoord, tarmor)), Some(primary)) = (target_info, weapon) else {
                    if let Some(u) = world.units.get_mut(handle) {
                        u.air_state = AirState::Idle;
                    }
                    continue;
                };
                // Take off to flight altitude before engaging.
                if altitude < FLIGHT_LEVEL {
                    ascend(world, handle);
                    continue;
                }
                let w = select_weapon(primary, secondary, tarmor, leptons_distance(coord, tcoord));
                let dist = leptons_distance(coord, tcoord);
                if dist > w.range {
                    // Fly toward the target.
                    fly_toward(world, handle, tcoord, speed, rot);
                } else {
                    // In range: hover, aim, and fire on ROF cadence.
                    let desired = Facing::toward(coord, tcoord);
                    if let (Some(d), Some(u)) = (desired, world.units.get_mut(handle)) {
                        u.facing = u.facing.rotate_toward(d, rot.wrapping_add(1));
                        u.turret_facing = u.facing;
                    }
                    let (aim, arm_ready) = world
                        .units
                        .get(handle)
                        .map(|u| (u.facing, u.arm == 0))
                        .unwrap_or((Facing(0), false));
                    let aligned = desired
                        .map(|d| aligned_to_fire(aim, d, w.proj_rot))
                        .unwrap_or(true);
                    if arm_ready && aligned {
                        let tgt = world
                            .units
                            .get(handle)
                            .and_then(|u| u.target)
                            .unwrap_or(Target::Cell(tcoord.cell()));
                        fire(world, handle, house, coord, aim, tgt, tcoord, &w);
                        let rof = house_rof_scaled(world, house, w.rof);
                        if let Some(u) = world.units.get_mut(handle) {
                            u.arm = rof;
                            u.ammo = u.ammo.saturating_sub(1);
                            if u.ammo == 0 {
                                u.air_state = AirState::Returning;
                            }
                        }
                    }
                }
            }
            AirState::Returning => {
                // Resolve (or re-find) the home helipad.
                let pad = valid_home(world, home, house).or_else(|| find_helipad(world, house));
                if let Some(u) = world.units.get_mut(handle) {
                    u.home = pad;
                }
                let Some(pad) = pad else {
                    // Nowhere to land — hover in place (a helicopter, unlike a
                    // fixed-wing, does not crash without a pad; `Mission_Retreat`
                    // fly-off is deferred to the Chinook arc).
                    if let Some(u) = world.units.get_mut(handle) {
                        u.air_state = AirState::Idle;
                    }
                    continue;
                };
                let pad_center = world
                    .buildings
                    .get(pad)
                    .map(|b| b.center_cell().center())
                    .unwrap_or(coord);
                let over_pad = leptons_distance(coord, pad_center) <= AIR_ARRIVE;
                if !over_pad {
                    fly_toward(world, handle, pad_center, speed, rot);
                } else {
                    // Descend onto the pad; when landed, begin rearming.
                    descend(world, handle);
                    let landed = world
                        .units
                        .get(handle)
                        .map(|u| u.altitude == 0)
                        .unwrap_or(false);
                    if landed {
                        if let Some(u) = world.units.get_mut(handle) {
                            u.air_state = AirState::Rearming;
                            u.rearm_timer = reload_ticks;
                        }
                    }
                }
            }
            AirState::Rearming => {
                // Docked. Reload one round per `reload_ticks`; when full, take off.
                if ammo >= max_ammo && max_ammo > 0 {
                    ascend(world, handle);
                    let airborne = world
                        .units
                        .get(handle)
                        .map(|u| u.altitude >= FLIGHT_LEVEL)
                        .unwrap_or(false);
                    if airborne {
                        if let Some(u) = world.units.get_mut(handle) {
                            u.air_state = if u.target.is_some() {
                                AirState::Attack
                            } else {
                                AirState::Idle
                            };
                        }
                    }
                } else if let Some(u) = world.units.get_mut(handle) {
                    if u.rearm_timer > 0 {
                        u.rearm_timer -= 1;
                    }
                    if u.rearm_timer == 0 {
                        u.ammo = (u.ammo + 1).min(u.max_ammo);
                        u.rearm_timer = reload_ticks;
                    }
                }
            }
            AirState::Idle => {
                // Idle: fly to an explicit move destination if ordered; otherwise
                // return home and settle on the pad (`Enter_Idle_Mode`). With no pad
                // and no order, hover in place.
                if let Some(goal) = dest {
                    let gc = goal.center();
                    if leptons_distance(coord, gc) <= AIR_ARRIVE {
                        if let Some(u) = world.units.get_mut(handle) {
                            u.dest = None;
                        }
                    } else {
                        // Ensure airborne, then fly to the ordered cell.
                        if altitude < FLIGHT_LEVEL {
                            ascend(world, handle);
                        } else {
                            fly_toward(world, handle, gc, speed, rot);
                        }
                    }
                    continue;
                }
                let pad = valid_home(world, home, house).or_else(|| find_helipad(world, house));
                if let Some(u) = world.units.get_mut(handle) {
                    u.home = pad;
                }
                if let Some(pad) = pad {
                    let pad_center = world
                        .buildings
                        .get(pad)
                        .map(|b| b.center_cell().center())
                        .unwrap_or(coord);
                    if leptons_distance(coord, pad_center) > AIR_ARRIVE {
                        fly_toward(world, handle, pad_center, speed, rot);
                    } else {
                        descend(world, handle); // park on the pad
                    }
                }
                // else: no home — hover (hold position and altitude).
            }
        }
    }
}

/// Raise an aircraft's altitude one [`ALT_STEP`] toward [`FLIGHT_LEVEL`] (takeoff).
fn ascend(world: &mut World, handle: Handle) {
    if let Some(u) = world.units.get_mut(handle) {
        u.altitude = (u.altitude + ALT_STEP).min(FLIGHT_LEVEL);
    }
}

/// Lower an aircraft's altitude one [`ALT_STEP`] toward the ground (landing).
fn descend(world: &mut World, handle: Handle) {
    if let Some(u) = world.units.get_mut(handle) {
        u.altitude = (u.altitude - ALT_STEP).max(0);
    }
}

/// Fly `handle` one frame toward `goal`: rotate the flight facing toward it at the
/// craft's ROT and translate `speed` leptons along the (rotated) facing, snapping
/// to the goal when within one step — a port of `FlyClass::Physics` +
/// `Process_Fly_To` (`fly.cpp:58`, `aircraft.cpp:2206`). Straight-line, terrain-
/// ignoring, clamped to the map so an off-map step is refused (`IMPACT_EDGE`,
/// `fly.cpp:92`).
fn fly_toward(world: &mut World, handle: Handle, goal: WorldCoord, speed: i32, rot: u8) {
    let Some(u) = world.units.get(handle) else {
        return;
    };
    let coord = u.coord;
    let dist = leptons_distance(coord, goal);
    let facing = match Facing::toward(coord, goal) {
        Some(d) => u.facing.rotate_toward(d, rot.wrapping_add(1)),
        None => u.facing,
    };
    let new_coord = if dist <= speed {
        goal
    } else {
        let stepped = coord_move(coord, facing, speed);
        // Refuse an off-map step (edge), holding position that axis.
        let max_x = MAP_CELL_W * LEPTONS_PER_CELL - 1;
        let max_y = MAP_CELL_H * LEPTONS_PER_CELL - 1;
        WorldCoord::new(stepped.x.0.clamp(0, max_x), stepped.y.0.clamp(0, max_y))
    };
    if let Some(u) = world.units.get_mut(handle) {
        u.coord = new_coord;
        u.facing = facing;
        u.turret_facing = facing;
    }
}

/// The house's home helipad handle if `home` still names a live, owned helipad;
/// else `None` (forcing a re-find).
fn valid_home(world: &World, home: Option<Handle>, house: u8) -> Option<Handle> {
    let h = home?;
    let b = world.buildings.get(h)?;
    if b.is_alive() && b.house == house && building_is_helipad(world, b.type_id) {
        Some(h)
    } else {
        None
    }
}

/// Find any live helipad owned by `house` (slot order), for docking/rearm.
fn find_helipad(world: &World, house: u8) -> Option<Handle> {
    world
        .buildings
        .iter()
        .find(|(_, b)| b.is_alive() && b.house == house && building_is_helipad(world, b.type_id))
        .map(|(h, _)| h)
}

/// Whether a building is a **helipad** (HPAD) — the aircraft dock/rearm structure,
/// identified by catalog name (the DOME/FIX table-free role pattern, §3.8).
fn building_is_helipad(world: &World, type_id: u32) -> bool {
    world
        .catalog
        .building(type_id)
        .map(|b| b.name.as_str() == "HPAD")
        .unwrap_or(false)
}

/// Nearest enemy **ground** target for a hunting aircraft: the closest live enemy
/// non-airborne unit, else the closest live enemy (non-wall) building. Returns the
/// [`Target`] and its `(aim_coord, armor)`.
fn acquire_air_ground_target(
    world: &World,
    house: u8,
    from: WorldCoord,
) -> Option<(Target, (WorldCoord, u8))> {
    let mut best_u: Option<(i32, Handle, WorldCoord, u8)> = None;
    for (h, u) in world.units.iter() {
        if !u.is_alive() || world.are_allies(house, u.house) || u.is_airborne() {
            continue;
        }
        let d = leptons_distance(from, u.coord);
        if best_u.map(|(bd, ..)| d < bd).unwrap_or(true) {
            best_u = Some((d, h, u.coord, u.armor));
        }
    }
    if let Some((_, h, c, a)) = best_u {
        return Some((Target::Unit(h), (c, a)));
    }
    let mut best_b: Option<(i32, Handle, WorldCoord, u8)> = None;
    for (h, b) in world.buildings.iter() {
        if !b.is_alive() || b.is_wall || world.are_allies(house, b.house) {
            continue;
        }
        let c = b.center_cell().center();
        let d = leptons_distance(from, c);
        if best_b.map(|(bd, ..)| d < bd).unwrap_or(true) {
            best_b = Some((d, h, c, b.armor));
        }
    }
    best_b.map(|(_, h, c, a)| (Target::Building(h), (c, a)))
}

/// If `handle` is a **medic** (a unit whose weapon does *negative* damage) and it
/// has no live wounded target, acquire the nearest wounded friendly infantry
/// within its heal range as the target (so the normal combat path fires the heal).
fn maybe_acquire_heal_target(world: &mut World, handle: Handle) {
    let (house, coord, range) = match world.units.get(handle) {
        Some(u) => match u.weapon {
            Some(w) if w.damage < 0 => (u.house, u.coord, w.range),
            _ => return,
        },
        None => return,
    };
    // Keep a still-valid wounded-friendly **infantry** target. The `is_infantry`
    // clause here is the symmetric guard added in M7.8 (carried fix d): a heal is
    // only ever valid on a friendly infantryman, so the "keep the current target"
    // fast path applies the *same* validity test the fresh-acquisition scan below
    // does (`is_infantry` + friendly + alive + wounded). Without it, an explicit
    // `Command::Attack` onto a friendly *vehicle* survived re-validation forever
    // (and healed it) while the identical order onto an *enemy* was clobbered —
    // an asymmetry. Now both invalid explicit orders are cleared identically the
    // same tick, so a medic can only ever heal friendly infantry.
    let keep = match world.units.get(handle).and_then(|u| u.target) {
        Some(Target::Unit(t)) => world
            .units
            .get(t)
            .map(|tu| {
                tu.is_alive() && tu.house == house && tu.is_infantry() && tu.health < tu.max_health
            })
            .unwrap_or(false),
        _ => false,
    };
    if keep {
        return;
    }
    // Nearest wounded friendly infantry in range (excluding the medic itself).
    let mut best: Option<(i32, Handle)> = None;
    for (h, u) in world.units.iter() {
        if h == handle || u.house != house || !u.is_infantry() || !u.is_alive() {
            continue;
        }
        if u.health >= u.max_health {
            continue;
        }
        let d = leptons_distance(coord, u.coord);
        if d <= range && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, h));
        }
    }
    if let Some(u) = world.units.get_mut(handle) {
        u.target = best.map(|(_, h)| Target::Unit(h));
    }
}

/// Whether a unit is an **engineer** (E6): an unarmed, non-harvester infantryman.
/// Derived rather than flagged — E6 is the only unarmed infantry in the roster,
/// so "unarmed foot soldier" is a sufficient, table-free capability test (§3.8).
fn is_engineer(u: &crate::unit::Unit) -> bool {
    u.is_infantry() && u.weapon.is_none() && !u.is_harvester
}

/// `Rule.EngineerCaptureLevel = ConditionRed = 1/4` (`rules.cpp:281`): a building
/// at or below this health fraction is captured; above it, the engineer only
/// damages it. Expressed as a numerator/denominator to stay integer.
const ENGINEER_CAPTURE_NUM: i32 = 1;
const ENGINEER_CAPTURE_DEN: i32 = 4;
/// `Rule.EngineerDamage = 1/3` (`rules.cpp:280`): fraction of a building's max
/// strength an engineer removes when it cannot yet capture it.
const ENGINEER_DAMAGE_NUM: i32 = 1;
const ENGINEER_DAMAGE_DEN: i32 = 3;

/// System 4.25: **engineers** (E6). An engineer ordered to attack a building
/// marches to the footprint and, on arrival, acts by the target's ownership
/// (`InfantryClass::Per_Cell_Process`, `MISSION_CAPTURE`, `infantry.cpp:636-680`):
///
/// - **Enemy building:** **captures** it (health ≤ `EngineerCaptureLevel`,
///   ownership flips) or else **damages** it by `EngineerDamage` of its max
///   strength (`infantry.cpp:659`).
/// - **Friendly building** (carried fix b): **renovates** it — heals it to full
///   strength (`Renovate` → `Strength = MaxStrength`, `techno.cpp:3988`). This is
///   the classic RA "engineer instant-repairs your own building" mechanic. The
///   brief hypothesised RA engineers *don't* repair friendly buildings; the
///   reference source shows the opposite (both the vanilla and the
///   `FIXIT_ENGINEER_CAPTURE` branches call `Renovate()`), so we follow the
///   source.
///
/// The engineer is **consumed on every action** — capture, damage, *or* renovate
/// (`delete this`, the shared terminal at `infantry.cpp:782`). The one path that
/// does **not** consume it is a *refused* order:
///
/// - **Wall segment** (carried fix c): walls are `OverlayType`s in the original,
///   not `BuildingClass`es, so they are never valid capture/enter targets
///   (`Can_Capture` requires `RTTI_BUILDING` + `IsCaptureable`,
///   `building.cpp:3537`; `object.cpp:421` returns false; wall stubs default
///   `IsCaptureable=false`, `bdata.cpp:2746`). We model walls as 1×1 buildings
///   (QUIRKS Q9), so we gate them out explicitly: an engineer ordered onto a wall
///   (friend or foe) refuses, is not consumed, and the wall is untouched.
fn run_engineers(world: &mut World) {
    for handle in world.units.handles() {
        let (house, coord, target) = match world.units.get(handle) {
            Some(u) if is_engineer(u) => match u.target {
                Some(Target::Building(t)) => (u.house, u.coord, t),
                _ => continue,
            },
            _ => continue,
        };
        // Target still a live enemy building?
        let (bcell, bw, bh, bhouse, bhealth, bmax, bwall) = match world.buildings.get(target) {
            Some(b) if b.is_alive() => (
                b.cell,
                b.foot_w as i32,
                b.foot_h as i32,
                b.house,
                b.health as i32,
                b.max_health as i32,
                b.is_wall,
            ),
            _ => {
                if let Some(u) = world.units.get_mut(handle) {
                    u.target = None;
                }
                continue;
            }
        };
        if bwall {
            // A wall (fix c) is never a valid engineer target (friend or foe).
            // Refuse: drop the order, the engineer is NOT consumed, wall untouched.
            if let Some(u) = world.units.get_mut(handle) {
                u.target = None;
            }
            continue;
        }
        // Adjacent to the footprint?
        let c = coord.cell();
        let adjacent =
            c.x >= bcell.x - 1 && c.x <= bcell.x + bw && c.y >= bcell.y - 1 && c.y <= bcell.y + bh;
        if !adjacent {
            // March to the nearest passable footprint-adjacent cell.
            let need_path = world
                .units
                .get(handle)
                .map(|u| u.path.is_empty())
                .unwrap_or(false);
            if need_path {
                if let Some(b) = world.buildings.get(target) {
                    if let Some(goal) = nearest_adjacent_passable(&world.passable, b, c) {
                        if let Some(path) = find_path(&world.passable, c, goal, Locomotor::Foot) {
                            if let Some(u) = world.units.get_mut(handle) {
                                u.path = path;
                                u.dest = Some(goal);
                            }
                        }
                    }
                }
            }
            continue;
        }
        // Arrived. Act by ownership:
        if bhouse == house {
            // Friendly (fix b): renovate — heal to full strength
            // (`TechnoClass::Renovate`, `techno.cpp:3988`).
            if let Some(b) = world.buildings.get_mut(target) {
                b.health = b.max_health;
            }
        } else {
            // Enemy: capture (health at/below the capture level) or damage.
            let capturable = bhealth * ENGINEER_CAPTURE_DEN <= bmax * ENGINEER_CAPTURE_NUM;
            if capturable {
                capture_building(world, target, house);
            } else {
                let dmg = (bmax * ENGINEER_DAMAGE_NUM / ENGINEER_DAMAGE_DEN).min(bhealth - 1);
                if let Some(b) = world.buildings.get_mut(target) {
                    b.health = (b.health as i32 - dmg).max(1) as u16;
                }
            }
        }
        // The engineer is consumed on every action — capture, damage, or
        // renovate (`delete this`, the shared terminal at `infantry.cpp:782`).
        world.units.remove(handle);
    }
}

/// Flip a building's ownership to `new_house` (engineer capture): move its power
/// and owned-building count between houses and re-key its combat target. Power
/// and the build-count bookkeeping mirror `BuildingClass::Captured`
/// (`building.cpp:3207`).
fn capture_building(world: &mut World, building: Handle, new_house: u8) {
    let (old_house, type_id, power) = match world.buildings.get(building) {
        Some(b) => (b.house, b.type_id, b.power),
        None => return,
    };
    if old_house == new_house {
        return;
    }
    // Move power + build-count from the old house to the new one.
    if let Some(oh) = world.houses.get_mut(old_house as usize) {
        if power >= 0 {
            oh.power_output -= power;
        } else {
            oh.power_drain -= -power;
        }
        oh.adjust_building_count(type_id, -1);
    }
    if let Some(nh) = world.houses.get_mut(new_house as usize) {
        if power >= 0 {
            nh.power_output += power;
        } else {
            nh.power_drain += -power;
        }
        nh.adjust_building_count(type_id, 1);
    }
    if let Some(b) = world.buildings.get_mut(building) {
        b.house = new_house;
        b.target = None; // a captured defense re-acquires for its new owner
    }
    // Capturing a storage building shrinks the former owner's capacity — clamp
    // its stored tiberium down immediately, same reconcile as a sale/destruction
    // (carried fix a). The new owner's capacity only grows, so it needs none.
    let old_cap = house_storage_capacity(world, old_house);
    if let Some(oh) = world.houses.get_mut(old_house as usize) {
        oh.reconcile_capacity(old_cap);
    }
}

/// Ticks between repair steps at a service depot (`Rule.RepairRate = .016` min ≈
/// 0.96 s ≈ 14 ticks; we use 15 ≈ 1 s). A global cadence (no per-building timer).
///
/// The four repair magnitudes (`URepairStep`/`URepairPercent`/`RepairStep`/
/// `RepairPercent`) are **no longer module constants** — they were promoted into
/// [`crate::EconRules`] (M7.5 P0) so they load from rules.ini like
/// `BuildSpeedBias` and cannot silently drift in code (they had already drifted
/// once, the M7.9.1 audit). Read them from `world.catalog.econ.*repair_*`.
const REPAIR_INTERVAL: u32 = 15;

/// System 3.5: **service depot (FIX)** unit repair. On the global repair cadence,
/// each FIX heals one friendly, damaged **vehicle** parked on/adjacent to its
/// footprint by `URepairStep` HP, charging `URepairPercent` of the unit's build
/// cost proportional to the HP restored (`BuildingClass::Repair`, the
/// `Rule.URepair*` economy). Simplified dock: no radio/`MISSION_ENTER` protocol —
/// the nearest adjacent damaged friendly vehicle is repaired.
fn run_repair(world: &mut World) {
    if !world.tick_count.is_multiple_of(REPAIR_INTERVAL) {
        return;
    }
    // Which building type id is the service depot (by catalog name).
    let fix_id = world
        .catalog
        .buildings
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case("FIX"));
    let Some(fix_id) = fix_id else {
        return;
    };
    let fix_id = fix_id as u32;

    // Snapshot the depots (handle, house, footprint anchor/size) in slot order.
    let depots: Vec<(u8, CellCoord, i32, i32)> = world
        .buildings
        .iter()
        .filter(|(_, b)| b.type_id == fix_id && b.is_alive())
        .map(|(_, b)| (b.house, b.cell, b.foot_w as i32, b.foot_h as i32))
        .collect();

    for (house, tl, w, h) in depots {
        // The nearest friendly, damaged, non-infantry unit adjacent to the depot.
        let mut best: Option<(i32, Handle)> = None;
        for (uh, u) in world.units.iter() {
            // Aircraft service at the helipad, not the ground depot (FIX repairs
            // ground vehicles only).
            if u.house != house
                || u.is_infantry()
                || u.is_aircraft()
                || u.health >= u.max_health
                || !u.is_alive()
            {
                continue;
            }
            let c = u.cell();
            let adj = c.x >= tl.x - 1 && c.x <= tl.x + w && c.y >= tl.y - 1 && c.y <= tl.y + h;
            if !adj {
                continue;
            }
            let d = (c.x - tl.x).abs() + (c.y - tl.y).abs();
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, uh));
            }
        }
        let Some((_, uh)) = best else {
            continue;
        };
        let (max_health, missing, type_id) = match world.units.get(uh) {
            Some(u) => (
                u.max_health as i32,
                (u.max_health - u.health) as i32,
                u.type_id,
            ),
            None => continue,
        };
        let step = world.catalog.econ.urepair_step.min(missing);
        if step <= 0 {
            continue;
        }
        // Cost of one step = URepairPercent * unit_cost * step_hp / max_health.
        let unit_cost = world.catalog.unit(type_id).map(|p| p.cost).unwrap_or(0);
        let step_cost = (unit_cost * world.catalog.econ.urepair_percent_num * step
            / world.catalog.econ.urepair_percent_den.max(1)
            / max_health.max(1))
        .max(0);
        // Only repair if the house can pay (`CreditReserve` simplified to "has it").
        if world.house_credits(house) < step_cost {
            continue;
        }
        if let Some(hh) = world.houses.get_mut(house as usize) {
            hh.deduct(step_cost);
        }
        if let Some(u) = world.units.get_mut(uh) {
            u.health = (u.health + step as u16).min(u.max_health);
        }
    }
}

/// The world position of a defense building's current target (unit centre /
/// building centre / force-fire cell).
fn building_target_position(world: &World, target: Target) -> Option<WorldCoord> {
    match target {
        Target::Unit(t) => world.units.get(t).filter(|u| u.is_alive()).map(|u| u.coord),
        Target::Building(t) => world
            .buildings
            .get(t)
            .filter(|b| b.is_alive())
            .map(|b| b.center_cell().center()),
        Target::Cell(c) => Some(c.center()),
    }
}

/// Whether a defense building is an **anti-air** emplacement (AGUN / SAM),
/// identified by its catalog name — the table-free single-role check the codebase
/// uses for DOME/FIX (Q10, §3.8). An AA emplacement fires *only* at airborne
/// aircraft (its projectile is `AA=yes, AG=no`, `bbdata.cpp`); every other armed
/// building fires only at ground targets. Deriving this by name avoids threading an
/// `anti_air` flag through the widely-constructed `WeaponProfile`/`BuildingProto`
/// literals (which would churn every combat golden's struct construction).
fn building_is_aa(world: &World, type_id: u32) -> bool {
    world
        .catalog
        .building(type_id)
        .map(|b| matches!(b.name.as_str(), "AGUN" | "SAM"))
        .unwrap_or(false)
}

/// Whether `type_id` is a **naval yard** (SYRD shipyard / SPEN sub pen): the
/// factory that produces vessels, which must be placed adjacent to water and
/// spawns its vessels into an adjacent water cell (the naval equivalent of the
/// helipad, identified by catalog name — the table-free role pattern, §3.8).
fn building_is_shipyard(world: &World, type_id: u32) -> bool {
    world
        .catalog
        .building(type_id)
        .map(|b| matches!(b.name.as_str(), "SYRD" | "SPEN"))
        .unwrap_or(false)
}

/// The submarine/detector capability of a vessel, derived from its rules.ini
/// name (the §3.8 table-free role pattern, like AA/helipad): SS/MSUB are
/// submarines, DD is a detector (destroyer). Returns `(is_submarine, is_detector)`.
fn vessel_flags(name: &str) -> (bool, bool) {
    match name.to_ascii_uppercase().as_str() {
        // Submarines (`Cloakable=yes`): SS, missile sub.
        "SS" | "MSUB" => (true, false),
        // Detectors (`Sensors=Yes`): destroyer + cruiser reveal nearby subs.
        "DD" | "CA" => (false, true),
        _ => (false, false),
    }
}

/// Detection radius (leptons) within which a **detector** (destroyer) reveals a
/// submerged enemy submarine to itself and its allies — a simplified stand-in for
/// the reference's sight/cloak-detection radius (`Is_Cloaked`, techno.cpp). ~5
/// cells (`0x0500`).
const SUB_DETECT_RANGE: i32 = 0x0500;

/// Whether `target` is an enemy **submarine that is hidden** from `observer_house`
/// — i.e. it is submerged and no detector allied to the observer is within
/// [`SUB_DETECT_RANGE`] of it. A hidden sub is not auto-acquirable / not a valid
/// explicit target (`VesselClass` cloak: a cloaked object is `MOVE_CLOAK`/
/// untargetable to non-detectors, vessel.cpp:296). Surface vessels and allied subs
/// are never hidden; a surfaced sub (firing / recloak grace) is visible to all.
fn is_hidden_submarine(world: &World, target: &Unit, observer_house: u8) -> bool {
    if !(target.is_submarine && target.submerged) {
        return false;
    }
    if world.are_allies(observer_house, target.house) {
        return false;
    }
    // Revealed if any live detector allied to the observer is within range.
    let revealed = world.units.iter().any(|(_, d)| {
        d.is_detector
            && d.is_alive()
            && world.are_allies(observer_house, d.house)
            && leptons_distance(d.coord, target.coord) <= SUB_DETECT_RANGE
    });
    !revealed
}

/// Keep the building's current target only if it is still a live **enemy** unit
/// within `range` leptons of `center` **and** of the right air/ground class for
/// this weapon (`aa` = anti-air emplacement → target must be airborne; otherwise
/// the target must be a ground target); else `None` (forcing a re-acquire).
fn validate_building_target(
    world: &World,
    cur: Option<Target>,
    house: u8,
    center: WorldCoord,
    range: i32,
    aa: bool,
) -> Option<Target> {
    match cur {
        Some(Target::Unit(t)) => {
            let u = world.units.get(t)?;
            if u.is_alive()
                && u.house != house
                && u.is_airborne() == aa
                && leptons_distance(center, u.coord) <= range
            {
                Some(Target::Unit(t))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The nearest live enemy **unit** within `range` leptons of `center`, in slot
/// order (ties broken by the earlier handle) — the defense's auto-acquire scan
/// (`Greatest_Threat`, simplified to nearest by distance). Buildings and the
/// force-fire cell are not auto-targeted. `aa` selects the target class: an
/// anti-air emplacement (`aa == true`) only sees **airborne** aircraft; a normal
/// defense (`aa == false`) only sees **ground** targets (never an airborne craft
/// it cannot hit).
fn acquire_nearest_enemy(
    world: &World,
    house: u8,
    center: WorldCoord,
    range: i32,
    aa: bool,
) -> Option<Target> {
    let mut best: Option<(i32, Handle)> = None;
    for (h, u) in world.units.iter() {
        if u.house == house || !u.is_alive() || u.is_airborne() != aa {
            continue;
        }
        // A submerged enemy submarine is hidden from a non-detector (naval arc).
        if is_hidden_submarine(world, u, house) {
            continue;
        }
        let d = leptons_distance(center, u.coord);
        if d <= range && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, h));
        }
    }
    best.filter(|&(_, h)| {
        !world.are_allies(house, world.units.get(h).map(|u| u.house).unwrap_or(house))
    })
    .map(|(_, h)| Target::Unit(h))
}

/// Acquire radius bound for Area-Guard (`Threat_Range(1)` is `Bound(..,0,0x0A00)`,
/// `techno.cpp:5231` — ten cells).
const AREA_GUARD_MAX_RANGE: i32 = 0x0A00;

/// Guard-mission auto-acquisition (`TechnoClass::Target_Something_Nearby`,
/// `techno.cpp:5912`; scan range from `Threat_Range`, `techno.cpp:5194`). A live,
/// armed Guard/Area-Guard unit sitting idle at its post (no target, no path)
/// targets the nearest enemy within its acquire radius:
/// - **Guard** (`THREAT_RANGE`, `foot.cpp:609`): weapon range, centred on itself.
/// - **Area Guard** (`THREAT_AREA`, `foot.cpp:1067`): twice weapon range, centred
///   on the guard post (`ArchiveTarget`).
///
/// The acquired target is flagged [`Unit::guard_target`] so [`run_combat`] applies
/// the leash (plain Guard never chases; Area Guard chases but races home when it
/// strays more than weapon range from its post).
fn maybe_acquire_guard_target(world: &mut World, handle: Handle) {
    // Proactive guard acquisition (and the base-alert in `explosion_damage`) runs
    // in **all** worlds — skirmish and campaign alike (M7.11). This is the
    // original's universal behaviour: `Enter_Idle_Mode` puts every produced/placed
    // unit into `MISSION_GUARD` (`unit.cpp:1343`), and a guarding unit auto-acquires
    // via `Target_Something_Nearby` (`foot.cpp:594`, `techno.cpp:5912`) regardless of
    // single-player-vs-skirmish. The M7.5-B campaign-only gate (QUIRKS Q18) has been
    // removed: the playtest complaint ("AI players still don't do active fight") is
    // skirmish-specific, and the skirmish AI is retuned (M7.11 P1) to stay decisive
    // with active defenders rather than by suppressing them.
    let (mission, house, coord, post, range) = match world.units.get(handle) {
        Some(u) => {
            let range = match u.weapon {
                // A **healer** (negative-damage "weapon", e.g. the medic's Heal —
                // capability derived from `damage < 0`, Q10/Q11d) must NOT proactively
                // guard-acquire: it would target the nearest *enemy* and then fire its
                // heal at it, healing the enemy. Medics only ever act through
                // `maybe_acquire_heal_target` (friendly wounded infantry). Exclude
                // them here (and in `alert_nearby_guards`) so universal guard
                // acquisition doesn't turn medics into enemy-healers.
                Some(w) if w.damage >= 0 => w.range,
                _ => return,
            };
            if !u.mission.is_guarding() || !u.is_alive() || u.target.is_some() || !u.path.is_empty()
            {
                return;
            }
            (u.mission, u.house, u.coord, u.guard_post, range)
        }
        None => return,
    };
    let (center, radius) = match mission {
        Mission::AreaGuard => (
            post.map(|c| c.center()).unwrap_or(coord),
            range.saturating_mul(2).min(AREA_GUARD_MAX_RANGE),
        ),
        // Guard (and any other guarding mission): THREAT_RANGE = weapon range.
        _ => (coord, range),
    };
    let mut best: Option<(i32, Handle)> = None;
    for (h, u) in world.units.iter() {
        // A ground defender's weapon is not anti-air (no unit-mounted AA in P0), so
        // it cannot acquire an airborne aircraft (`Can_Fire`, `techno.cpp:2895`).
        if !u.is_alive() || world.are_allies(house, u.house) || u.is_airborne() {
            continue;
        }
        // A submerged enemy submarine is hidden from a non-detector (naval arc).
        if is_hidden_submarine(world, u, house) {
            continue;
        }
        let d = leptons_distance(center, u.coord);
        if d <= radius && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, h));
        }
    }
    if let Some((_, h)) = best {
        if let Some(u) = world.units.get_mut(handle) {
            u.target = Some(Target::Unit(h));
            u.guard_target = true;
        }
    }
}

/// Wake idle friendly guards near an enemy's shot to acquire the attacker — the
/// "base under attack" propagation (`FootClass::Take_Damage → Team->Took_Damage`,
/// `foot.cpp:1157`; the house/team alert), simplified to a proximity broadcast.
/// Any live, armed Guard/Area-Guard unit that is an enemy of `source_house`,
/// currently idle, and within [`GUARD_ALERT_CELLS`] of `impact_cell` targets the
/// attacker — so a base fights a raider as a whole, and a guard whose sight is
/// shrouded still turns to a hostile within its weapon range.
///
/// The alerted target **is** flagged `guard_target`, so the responder is leashed
/// like any guard acquisition: it engages the attacker if within weapon range but
/// does not abandon its post to chase across the map. (A no-leash version let one
/// raider drag a whole base's defenders on an endless cross-map chase, so neither
/// AI could concentrate force to a decision — the scg05ea AI-vs-AI stall.)
fn alert_nearby_guards(
    world: &mut World,
    impact_cell: CellCoord,
    source_house: u8,
    source: Handle,
) {
    let handles: Vec<Handle> = world
        .units
        .iter()
        .filter(|(_, u)| {
            u.is_alive()
                && u.mission.is_guarding()
                // Armed with a real (damage-dealing) weapon — a healer (negative
                // damage) is excluded, same reason as `maybe_acquire_guard_target`.
                && u.weapon.map(|w| w.damage >= 0).unwrap_or(false)
                && u.target.is_none()
                && u.path.is_empty()
                && !world.are_allies(u.house, source_house)
                && {
                    let c = u.cell();
                    (c.x - impact_cell.x).abs() <= GUARD_ALERT_CELLS
                        && (c.y - impact_cell.y).abs() <= GUARD_ALERT_CELLS
                }
        })
        .map(|(h, _)| h)
        .collect();
    for h in handles {
        if let Some(u) = world.units.get_mut(h) {
            u.target = Some(Target::Unit(source));
            u.guard_target = true;
        }
    }
}

/// Radius (in cells) within which an enemy shot wakes friendly guards to the
/// attacker (base-under-attack alert). Not from a single reference constant —
/// stands in for the house/team alert propagation with a modest local radius.
const GUARD_ALERT_CELLS: i32 = 4;

/// Auto-hunt acquisition: if `handle` is a live, armed, hunting unit with no
/// target and no path, target the nearest enemy unit (else the nearest enemy
/// building) anywhere on the map. Alliance-aware via [`World::are_allies`].
fn maybe_acquire_hunt_target(world: &mut World, handle: Handle) {
    let (house, coord, armed, idle) = match world.units.get(handle) {
        Some(u) => (
            u.house,
            u.coord,
            u.weapon.is_some(),
            (u.hunt || u.mission == Mission::Hunt)
                && u.is_alive()
                && u.target.is_none()
                && u.path.is_empty(),
        ),
        None => return,
    };
    if !armed || !idle {
        return;
    }
    // Nearest enemy unit (ground weapons cannot chase airborne aircraft).
    let mut best_u: Option<(i32, Handle)> = None;
    for (h, u) in world.units.iter() {
        if !u.is_alive() || world.are_allies(house, u.house) || u.is_airborne() {
            continue;
        }
        // A submerged enemy submarine is hidden from a non-detector (naval arc).
        if is_hidden_submarine(world, u, house) {
            continue;
        }
        let d = leptons_distance(coord, u.coord);
        if best_u.map(|(bd, _)| d < bd).unwrap_or(true) {
            best_u = Some((d, h));
        }
    }
    if let Some((_, h)) = best_u {
        if let Some(u) = world.units.get_mut(handle) {
            u.target = Some(Target::Unit(h));
        }
        return;
    }
    // Else nearest enemy (non-wall) building.
    let mut best_b: Option<(i32, Handle)> = None;
    for (h, b) in world.buildings.iter() {
        if !b.is_alive() || b.is_wall || world.are_allies(house, b.house) {
            continue;
        }
        let d = leptons_distance(coord, b.center_cell().center());
        if best_b.map(|(bd, _)| d < bd).unwrap_or(true) {
            best_b = Some((d, h));
        }
    }
    if let Some((_, h)) = best_b {
        if let Some(u) = world.units.get_mut(handle) {
            u.target = Some(Target::Building(h));
        }
    }
}

/// The **water** cell nearest to `from` that lies within `range` leptons of the
/// building — a naval bombardment station from which a vessel can shell a coastal
/// structure (naval arc P0). Returns `None` if no open-water cell sits within
/// weapon range of the building (a purely inland structure no ship can reach).
/// Scans a box sized to `range` around the building centre.
pub(crate) fn nearest_water_approach(
    passable: &Passability,
    building: &Building,
    from: CellCoord,
    range: i32,
) -> Option<CellCoord> {
    let center = building.center_cell();
    // Leptons → cells (256 leptons/cell), plus a one-cell margin.
    let r_cells = (range / 256) + 1;
    let mut best: Option<(i32, CellCoord)> = None;
    for dy in -r_cells..=r_cells {
        for dx in -r_cells..=r_cells {
            let c = CellCoord::new(center.x + dx, center.y + dy);
            if !passable.is_water(c) {
                continue;
            }
            if leptons_distance(c.center(), center.center()) > range {
                continue;
            }
            // Manhattan distance to the vessel — nearest bombardment station wins
            // (deterministic; ties resolve to the scan's row-major order).
            let d = (c.x - from.x).abs() + (c.y - from.y).abs();
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, c));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// The passable cell in a building's one-cell footprint ring that is nearest to
/// `from` — an approach/attack target for a ground unit that cannot enter the
/// (impassable) footprint itself. `None` if the whole ring is blocked.
fn nearest_adjacent_passable(
    passable: &Passability,
    building: &Building,
    from: CellCoord,
) -> Option<CellCoord> {
    let (tl, w, h) = (
        building.cell,
        building.foot_w as i32,
        building.foot_h as i32,
    );
    let mut best: Option<(i32, CellCoord)> = None;
    for y in (tl.y - 1)..=(tl.y + h) {
        for x in (tl.x - 1)..=(tl.x + w) {
            let c = CellCoord::new(x, y);
            if !building.adjacent(c) || !passable.is_passable(c) {
                continue;
            }
            let d = (c.x - from.x).abs() + (c.y - from.y).abs();
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, c));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// Spawn one projectile from `shooter` at `target`. Computes the impact point,
/// applying the original's inaccuracy scatter for AP shots at ground/infantry —
/// the one combat path that consumes the sim RNG (`bullet.cpp:763-782`,
/// `Random_Pick(0, scatterdist)` = `Scen.RandomNumber`, the sync RNG). Accurate
/// shots (any shot at a vehicle) draw no RNG, exactly as the original.
#[allow(clippy::too_many_arguments)]
fn fire(
    world: &mut World,
    shooter: Handle,
    source_house: u8,
    muzzle: WorldCoord,
    aim: Facing,
    target: Target,
    target_coord: WorldCoord,
    weapon: &crate::combat::WeaponProfile,
) {
    // Direction the projectile is launched (toward the target; non-homing
    // bullets fire straight at it — `bullet.cpp:751`).
    let dir = Facing::toward(muzzle, target_coord).unwrap_or(aim);

    // Inaccuracy: an AP warhead trained on a ground cell (or infantry — none in
    // M4) scatters. Flat (non-arcing) projectiles use the ballistic branch.
    let is_ground = matches!(target, Target::Cell(_));
    let inaccurate = weapon.warhead_ap && is_ground;
    let impact = if inaccurate && !weapon.arcing {
        // scatterdist = (Distance/16) - 0x40, capped at BallisticScatter, >= 0.
        let d = leptons_distance(muzzle, target_coord);
        let mut scatterdist = (d / 16) - 0x40;
        scatterdist = scatterdist.min(weapon.ballistic_scatter).max(0);
        // Genuine sim-RNG draw (skips the draw when scatterdist == 0, matching
        // RandomClass::operator()(min,max) returning min without a draw).
        let offset = world.rng.range(0, scatterdist);
        coord_move(target_coord, dir, offset)
    } else {
        target_coord
    };

    // Arcing (ballistic lob) setup — `bullet.cpp:809/838`. The horizontal speed
    // rises with distance (`MaxSpeed + Distance/32`, min 25) and the launch
    // `riser` is sized so the parabola returns to the ground as the shell reaches
    // impact (`((Distance/2)/(speed+1))*Gravity`, min 10).
    let (speed, arcing, height, riser) = if weapon.arcing {
        let d = leptons_distance(muzzle, target_coord);
        let speed = (weapon.proj_speed + d / 32).max(25);
        let riser = (((d / 2) / (speed + 1)) * crate::bullet::GRAVITY).max(10);
        (speed, true, 1, riser)
    } else {
        (weapon.proj_speed, false, 0, 0)
    };

    // Firepower handicap (M7.9 P2a): the shooter's house scales the damage it
    // deals (`techno.cpp:3303`, `firepower = Attack * House->FirepowerBias`). Only
    // positive damage is scaled — the medic's negative "heal" is left untouched
    // (the original guards `if (firepower > 0)`). Neutral (1.0) houses are exact.
    let dealt = if weapon.damage > 0 {
        let fp = world
            .houses
            .get(source_house as usize)
            .map(|h| h.handicap.firepower)
            .unwrap_or(crate::house::FX_ONE);
        crate::house::fx_mul(weapon.damage, fp)
    } else {
        weapon.damage
    };

    let bullet = Bullet {
        pos: if weapon.instant { impact } else { muzzle },
        impact,
        target,
        speed,
        facing: dir,
        damage: dealt,
        warhead: weapon.warhead,
        min_damage: weapon.min_damage,
        max_damage: weapon.max_damage,
        source_house,
        source_unit: shooter,
        instant: weapon.instant,
        invisible: weapon.invisible,
        arcing,
        height,
        riser,
    };
    world.bullets.insert(bullet);

    // Artillery/grenade-dodge (M7.14 audit P2b): a slow projectile lets its target
    // cell's occupiers run away (`MaxSpeed < Rule.Incoming` → `Incoming` on the
    // TarCom cell, infantry.cpp:3826-3842). `econ.incoming_speed` is 0 for synthetic
    // catalogs, so this is inert there — no RNG draw, no golden churn. Real assets
    // ship `Incoming=10` (scaled 25): the E2 grenade (Speed 5 → 12) and the cruiser
    // 8Inch shell (6 → 15) trip it, but ARTY's 155mm (12 → 30) and the V2's SCUD
    // (25 → 64) are *faster* than the threshold and do NOT (a fidelity correction to
    // the brief's "arty/V2" — only genuinely slow projectiles dodge, per rules.ini).
    let incoming = world.catalog.econ.incoming_speed;
    if incoming > 0 && weapon.proj_speed < incoming {
        incoming_scatter(world, target_coord.cell(), muzzle);
    }
}

/// Leptons within which a warhead's detonation damages nearby objects — the
/// original's fixed blast radius `range = ICON_LEPTON_W + (ICON_LEPTON_W >> 1)`
/// (`combat.cpp:177` = 256 + 128 = 384 = 1.5 cells). "Damage never spills
/// further than one cell away" (`combat.cpp:186`): the object scan only visits
/// the impact cell and its 8 neighbours, then this radius filters by true
/// distance.
const EXPLOSION_RANGE: i32 = 384;

/// Port of `Explosion_Damage` (`combat.cpp:162-243`): apply a warhead's blast at
/// `impact` to every unit and building within [`EXPLOSION_RANGE`] leptons whose
/// cell lies in the impact cell's 3×3 neighbourhood, with the distance falloff
/// handled by [`modify_damage`] (which `Take_Damage` calls, `object.cpp:1661`).
///
/// Faithful details: the firing unit is excluded from its own blast
/// (`object != source`, `combat.cpp:203`); a building occupying the impact cell
/// takes a **direct hit** at distance 0 (`combat.cpp:230`); each object is
/// damaged at most once (our arena handles are unique, so no explicit
/// `IsToDamage` dedup is needed). Allies are **not** spared — splash is
/// full friendly-fire, exactly as the original (only `source` is immune).
///
/// This path draws **no** sim RNG (the original's only RNG here is bridge
/// destruction, `combat.cpp:270`, which we do not model), so the combat RNG draw
/// sequence — and every determinism golden that depends on it — is unchanged.
///
/// Wall/overlay/tiberium erosion (`combat.cpp:249-261`) is deferred (see the M7
/// report): warheads here damage only units and buildings.
#[allow(clippy::too_many_arguments)]
fn explosion_damage(
    world: &mut World,
    impact: WorldCoord,
    damage: i32,
    warhead: &crate::combat::WarheadProfile,
    min_damage: i32,
    max_damage: i32,
    source: Handle,
    attacker_house: u8,
    dead_units: &mut Vec<Handle>,
    dead_buildings: &mut Vec<Handle>,
) {
    if damage == 0 {
        return;
    }
    let impact_cell = impact.cell();
    // The attacker's house, if it is still alive — retaliation only fires against
    // a live enemy source (never an ally caught in friendly-fire splash).
    let source_house = world
        .units
        .get(source)
        .filter(|u| u.is_alive())
        .map(|u| u.house);
    // A ground unit cannot retaliate against — nor can nearby guards be alerted to
    // — an **airborne** attacker they have no way to hit (no anti-air weapon). Only
    // an AA emplacement engages aircraft, and that runs through building combat.
    let source_airborne = world.units.get(source).is_some_and(|u| u.is_airborne());

    // --- Units in the 3×3 neighbourhood ---
    for h in world.units.handles() {
        if h == source {
            continue;
        }
        let (coord, armor, target_house, airborne) = match world.units.get(h) {
            Some(u) if u.is_alive() => (u.coord, u.armor, u.house, u.is_airborne()),
            _ => continue,
        };
        let uc = coord.cell();
        if (uc.x - impact_cell.x).abs() > 1 || (uc.y - impact_cell.y).abs() > 1 {
            continue;
        }
        let distance = leptons_distance(impact, coord);
        if distance >= EXPLOSION_RANGE {
            continue;
        }
        let mut dmg = modify_damage(damage, warhead, armor, distance, min_damage, max_damage);
        // An airborne aircraft takes **half** damage (`AircraftClass::Take_Damage`,
        // `aircraft.cpp:1685`: `if (Height) damage /= 2`). Applied only to positive
        // damage (a heal never targets an aircraft anyway).
        if airborne && dmg > 0 {
            dmg /= 2;
        }
        // Armor handicap (M7.9 P2a): the *target's* house scales incoming damage
        // (`techno.cpp:4099`, `damage = damage * House->ArmorBias`). Only positive
        // damage — a heal (negative) is never armor-scaled. Neutral = exact.
        let dmg = house_armor_scaled(world, target_house, dmg);
        // Healing (negative damage, e.g. the medic's Heal weapon): raise health,
        // capped at max; never kills, never triggers retaliation.
        if dmg < 0 {
            if let Some(u) = world.units.get_mut(h) {
                u.health = (u.health as i32 - dmg).min(u.max_health as i32) as u16;
            }
            continue;
        }
        if dmg == 0 {
            continue;
        }
        // Last-attacker + kill attribution (M7.10 Expert_AI scoring): a hit from
        // a different house marks it as this house's last attacker, and a lethal
        // one tallies the kill. Only cross-house damage counts (never self/ally
        // splash). Guarded by `target_house != attacker_house`.
        if target_house != attacker_house {
            if let Some(th) = world.houses.get_mut(target_house as usize) {
                th.last_attacker = Some(attacker_house);
            }
        }
        let mut killed = false;
        if let Some(u) = world.units.get_mut(h) {
            u.health = u.health.saturating_sub(dmg as u16);
            if u.health == 0 {
                killed = true;
                if !dead_units.contains(&h) {
                    dead_units.push(h);
                }
            } else if source_house.is_some_and(|sh| sh != u.house) && !source_airborne {
                // Auto-retaliation (guard-mission return fire, item 2):
                // `FootClass::Take_Damage` assigns the attacker as TarCom when
                // the unit survives, is allowed to retaliate, and is idle. A
                // ground unit does not retaliate against an airborne attacker it
                // cannot hit.
                assign_retaliation(u, source);
            }
        }
        if killed && target_house != attacker_house {
            if let Some(th) = world.houses.get_mut(target_house as usize) {
                th.record_unit_killed_by(attacker_house);
            }
        }
    }

    // --- Buildings covering the 3×3 neighbourhood ---
    for h in world.buildings.handles() {
        let (covers_impact, near, center, armor, target_house) = match world.buildings.get(h) {
            Some(b) if b.is_alive() => {
                let mut near = false;
                'scan: for dy in -1..=1 {
                    for dx in -1..=1 {
                        if b.covers(CellCoord::new(impact_cell.x + dx, impact_cell.y + dy)) {
                            near = true;
                            break 'scan;
                        }
                    }
                }
                (
                    b.covers(impact_cell),
                    near,
                    b.center_cell().center(),
                    b.armor,
                    b.house,
                )
            }
            _ => continue,
        };
        if !near {
            continue;
        }
        // A building occupying the impact cell takes a direct hit (combat.cpp:230).
        let distance = if covers_impact {
            0
        } else {
            leptons_distance(impact, center)
        };
        if distance >= EXPLOSION_RANGE {
            continue;
        }
        let dmg = modify_damage(damage, warhead, armor, distance, min_damage, max_damage);
        // Armor handicap (M7.9 P2a): the target building's house scales the hit.
        let dmg = house_armor_scaled(world, target_house, dmg);
        if dmg <= 0 {
            continue;
        }
        if target_house != attacker_house {
            if let Some(th) = world.houses.get_mut(target_house as usize) {
                th.last_attacker = Some(attacker_house);
            }
        }
        let mut killed = false;
        if let Some(b) = world.buildings.get_mut(h) {
            b.health = b.health.saturating_sub(dmg as u16);
            if b.health == 0 && !dead_buildings.contains(&h) {
                killed = true;
                dead_buildings.push(h);
            }
        }
        if killed && target_house != attacker_house {
            if let Some(th) = world.houses.get_mut(target_house as usize) {
                th.record_building_killed_by(attacker_house);
            }
        }
    }

    // --- Base-under-attack alert (M7.5-B; universal since M7.11) ---
    // A live enemy shot landing here wakes nearby idle friendly guards to the
    // attacker, so a guarded base fights back as a whole even when the shooter is
    // out of an individual guard's sight/acquire range. Runs in all worlds now
    // (skirmish + campaign), matching the removal of the guard-acquisition gate
    // above — see `maybe_acquire_guard_target` and QUIRKS Q18.
    if let Some(sh) = source_house {
        if !source_airborne {
            alert_nearby_guards(world, impact_cell, sh, source);
        }
    }
}

/// Scale positive `dmg` by `house`'s armor handicap (M7.9 P2a). A neutral house
/// (bias 1.0) returns `dmg` unchanged (exact), and negative `dmg` (a heal) is
/// never scaled — armor only mitigates real damage (`techno.cpp:4097` `damage > 0`).
fn house_armor_scaled(world: &World, house: u8, dmg: i32) -> i32 {
    if dmg <= 0 {
        return dmg;
    }
    match world.houses.get(house as usize) {
        Some(h) if !h.handicap.is_neutral() => crate::house::fx_mul(dmg, h.handicap.armor),
        _ => dmg,
    }
}

/// Assign the attacker as a damaged unit's target if it is idle and armed — the
/// guard-mission return-fire path (`FootClass::Take_Damage` →
/// `Is_Allowed_To_Retaliate` → `Assign_Target(source)`, `foot.cpp:1176-1189`).
///
/// Simplifications vs. the original, all documented deviations:
/// - We retaliate only when the unit has **no current target and no move path**
///   (truly idle/guarding), so an explicit player Move/Attack order is never
///   hijacked — the original also snaps out of sticky modes but keeps an
///   existing TarCom/NavCom. This directly fixes the playtest complaint ("units
///   stand and die") without overriding orders.
/// - `Is_Allowed_To_Retaliate` gates human houses behind `Rule.IsSmartDefense`
///   (`techno.cpp:5641`); we enable retaliation for **all** houses (the
///   skirmish-friendly default), so the player's guarding units fight back.
/// - The warhead-can-harm-source and threat-comparison gates are omitted;
///   we require only that the unit is armed and the source is a live enemy.
fn assign_retaliation(unit: &mut Unit, source: Handle) {
    if unit.weapon.is_none() || unit.target.is_some() || !unit.path.is_empty() {
        return;
    }
    // Sleep/Sticky are "no-threat" missions: their handlers never touch TarCom
    // (`MissionClass::Mission_Sleep`, `mission.cpp:93`), so they neither
    // auto-acquire nor return fire. A held-still ambusher stays hidden.
    if matches!(unit.mission, Mission::Sleep | Mission::Sticky) {
        return;
    }
    unit.target = Some(Target::Unit(source));
}

/// Bullet system: advance every projectile; on detonation apply damage to its
/// target (with distance falloff from the actual impact point) and remove any
/// unit whose health reaches zero. Processed in slot order.
///
/// **Death seam (M4 → M7).** A unit at zero health is removed from the arena
/// here. Death animations, wreck/crater smudges, and score/credit effects are a
/// deliberate later-milestone TODO — the removal point is the single seam they
/// will hook.
fn run_bullets(world: &mut World) {
    let mut dead_units: Vec<Handle> = Vec::new();
    let mut dead_buildings: Vec<Handle> = Vec::new();
    for handle in world.bullets.handles() {
        let detonated = match world.bullets.get_mut(handle) {
            Some(b) => b.advance(),
            None => continue,
        };
        if !detonated {
            continue;
        }
        // Detonate: pull the bullet out and apply its warhead as an area blast at
        // the impact point (`Explosion_Damage`, combat.cpp:162). This unifies all
        // three target kinds — an accurate unit/building hit lands the primary
        // object at distance 0 (identical to the old single-target math) while
        // also catching neighbours; a force-fire `Cell` now does real ground-blast
        // damage instead of detonating harmlessly.
        if let Some(b) = world.bullets.remove(handle) {
            explosion_damage(
                world,
                b.impact,
                b.damage,
                &b.warhead,
                b.min_damage,
                b.max_damage,
                b.source_unit,
                b.source_house,
                &mut dead_units,
                &mut dead_buildings,
            );
        }
    }
    // Remove the dead (their handles go stale; attackers drop the target next
    // tick via the stale-handle check in `run_combat`). A destroyed building must
    // also free its footprint occupancy and reverse the house's power/count
    // bookkeeping — that all lives in `remove_building`.
    for h in dead_units {
        world.units.remove(h);
    }
    for h in dead_buildings {
        remove_building(world, h);
    }
}

// ===========================================================================
// Production system (§4.9 M5)
// ===========================================================================

/// Production system: advance each house's two factory lanes in house-index
/// order (a fixed order — determinism, §4.2). Ported from `FactoryClass::AI`
/// (`factory.cpp:194`): cost is paid in installments so the total spent equals
/// the item cost exactly; a step that can't be afforded stalls.
fn run_production(world: &mut World) {
    for hi in 0..world.houses.len() {
        advance_production(world, hi, ProdKind::Building);
        advance_production(world, hi, ProdKind::Unit);
        advance_production(world, hi, ProdKind::Infantry);
    }
}

/// Advance one production lane of one house by one tick.
fn advance_production(world: &mut World, house_idx: usize, kind: ProdKind) {
    // Take the production out to sidestep the borrow checker.
    let mut prod = match world.houses.get_mut(house_idx) {
        Some(h) => match kind {
            ProdKind::Building => h.building_prod.take(),
            ProdKind::Unit => h.unit_prod.take(),
            ProdKind::Infantry => h.infantry_prod.take(),
        },
        None => return,
    };
    let Some(p) = prod.as_mut() else {
        return;
    };
    if p.done {
        // Unit builds spawn on completion; retry the exit each tick until clear.
        // Building builds move to `ready_building` immediately (handled below on
        // the tick they finish), so a lingering done building lane shouldn't
        // occur — but guard anyway.
        finish_or_retry(world, house_idx, kind, prod);
        return;
    }

    // Low power no longer throttles per tick: the ×4/×2.5/×1.5 slowdown is baked
    // into `total_ticks` once at production start (see `apply_start_production`),
    // exactly as the original snapshots the factory Rate in `FactoryClass::Start`.

    // --- Installment payment (faithful to factory.cpp:203-227) ---
    let target_spent =
        (p.cost as i64 * (p.progress + 1) as i64 / p.total_ticks.max(1) as i64) as i32;
    let installment = (target_spent - p.spent).max(0);
    if world.house_credits(house_idx as u8) < installment {
        // Can't afford this step: stall (no progress), leave lane in place.
        store_production(world, house_idx, kind, prod);
        return;
    }
    if let Some(h) = world.houses.get_mut(house_idx) {
        h.deduct(installment);
    }
    p.spent += installment;
    p.progress += 1;

    if p.progress >= p.total_ticks {
        p.done = true;
        finish_or_retry(world, house_idx, kind, prod);
    } else {
        store_production(world, house_idx, kind, prod);
    }
}

/// Store a production back into its lane.
fn store_production(world: &mut World, house_idx: usize, kind: ProdKind, prod: Option<Production>) {
    if let Some(h) = world.houses.get_mut(house_idx) {
        match kind {
            ProdKind::Building => h.building_prod = prod,
            ProdKind::Unit => h.unit_prod = prod,
            ProdKind::Infantry => h.infantry_prod = prod,
        }
    }
}

/// Handle a completed production: a building becomes ready-to-place; a unit
/// spawns at the war-factory exit (retrying next tick if the exit is blocked).
fn finish_or_retry(world: &mut World, house_idx: usize, kind: ProdKind, prod: Option<Production>) {
    let Some(p) = prod else { return };
    match p.item {
        BuildItem::Building(id) => {
            if let Some(h) = world.houses.get_mut(house_idx) {
                h.ready_building = Some(id);
                h.building_prod = None;
            }
        }
        BuildItem::Unit(id) => {
            let house = house_idx as u8;
            // Infantry exit the barracks; vehicles exit the war factory. The exit
            // ring is searched with the **produced unit's own locomotor** (P0,
            // M7.7) — a wheeled JEEP/APC/HARV must not be handed a Track-only exit
            // cell, and the sub-cell-spot rule (not the whole-cell rule) governs a
            // foot unit's exit — so the exit cell is guaranteed enterable by the
            // unit that spawns there.
            let loco = world
                .catalog
                .unit(id)
                .map(|p| loco_from_index(p.locomotor))
                .unwrap_or(Locomotor::Track);
            let exit = match kind {
                ProdKind::Infantry => find_barracks_exit(world, house),
                // Aircraft materialise on their home helipad (airborne), not a
                // ground factory exit (`BuildingClass::Exit_Object` for STRUCT_HELIPAD).
                _ if loco == Locomotor::Air => find_helipad(world, house)
                    .and_then(|h| world.buildings.get(h))
                    .map(|b| b.center_cell()),
                // Vessels exit the naval yard into an adjacent **water** cell
                // (the exit ring searched with the Water locomotor, naval arc).
                _ if loco == Locomotor::Water => find_shipyard_exit(world, house),
                _ => find_factory_exit(world, house, loco),
            };
            match exit {
                Some(exit) => {
                    spawn_produced_unit(world, id, house, exit);
                    if let Some(h) = world.houses.get_mut(house_idx) {
                        match kind {
                            ProdKind::Infantry => h.infantry_prod = None,
                            _ => h.unit_prod = None,
                        }
                    }
                }
                None => {
                    // Exit blocked: keep the done production and retry next tick.
                    store_production(world, house_idx, kind, Some(p));
                }
            }
        }
    }
}

/// A free passable cell adjacent to the house's war factory, for a completed
/// vehicle to exit onto. Prefers the cell south of the factory centre, then
/// scans the footprint's adjacency ring. Deviation: the original exits at
/// building-specific coordinates via `Exit_Coord`/`Exit_Object`
/// (`building.cpp:2106`); we use the nearest free adjacent cell instead.
fn find_factory_exit(world: &World, house: u8, loco: Locomotor) -> Option<CellCoord> {
    factory_exit_ring(world, house, |b| b.is_war_factory, loco)
}

/// A free passable cell adjacent to the house's barracks for a completed
/// infantryman to exit onto (foot locomotor; the cell must have a free sub-cell
/// spot, not just be empty of vehicles).
fn find_barracks_exit(world: &World, house: u8) -> Option<CellCoord> {
    factory_exit_ring(world, house, |b| b.is_barracks, Locomotor::Foot)
}

/// A free adjacent **water** cell for a completed vessel to spawn onto — the naval
/// yard's exit ring, searched with the [`Locomotor::Water`] mask so the exit cell
/// is guaranteed floatable (and unoccupied by another vessel). `None` if the yard
/// has no free adjacent water (production retries next tick, like a blocked
/// factory exit).
fn find_shipyard_exit(world: &World, house: u8) -> Option<CellCoord> {
    factory_exit_ring(
        world,
        house,
        |b| building_is_shipyard(world, b.type_id),
        Locomotor::Water,
    )
}

/// Shared factory-exit search: a passable, unoccupied cell in the producing
/// building's adjacency ring (south-of-centre preferred). "Unoccupied" now also
/// respects **unit** occupancy — a vehicle exit rejects a cell already holding a
/// vehicle; an infantry (foot) exit rejects a cell whose five spots are full — so
/// produced units never stack (the exit-blocked retry in `finish_or_retry`
/// handles a fully blocked ring).
fn factory_exit_ring(
    world: &World,
    house: u8,
    is_kind: impl Fn(&Building) -> bool,
    loco: Locomotor,
) -> Option<CellCoord> {
    let factory = world
        .buildings
        .iter()
        .find(|(_, b)| b.house == house && is_kind(b) && b.is_alive())
        .map(|(_, b)| (b.cell, b.foot_w as i32, b.foot_h as i32, b.center_cell()))?;
    let (tl, w, h, center) = factory;
    let free = |c: CellCoord| -> bool {
        if !world.passable.is_passable_loco(c, loco) {
            return false;
        }
        // Co-occupancy rule (QUIRKS Q5.3): the exit cell must be free of the
        // *other* unit kind too — a foot unit needs a free spot and no vehicle;
        // a vehicle needs no vehicle and no infantry at all.
        if loco == Locomotor::Foot {
            !infantry_cell_full(world, c, None) && !vehicle_in_cell(world, c, None)
        } else {
            !vehicle_in_cell(world, c, None) && infantry_spot_bits(world, c, None) & 0x1F == 0
        }
    };
    // Preferred: straight south of centre.
    let south = CellCoord::new(center.x, tl.y + h);
    if free(south) {
        return Some(south);
    }
    // Otherwise the whole 1-cell ring around the footprint.
    for x in (tl.x - 1)..=(tl.x + w) {
        for y in (tl.y - 1)..=(tl.y + h) {
            let c = CellCoord::new(x, y);
            let on_ring = x == tl.x - 1 || x == tl.x + w || y == tl.y - 1 || y == tl.y + h;
            if on_ring && free(c) {
                return Some(c);
            }
        }
    }
    None
}

/// Whether a **vehicle** other than `except` currently occupies `cell` (a live
/// vehicle unit whose cell is `cell`). O(n) scan — used at command/spawn time
/// where the per-tick [`UnitGrid`] is not maintained.
fn vehicle_in_cell(world: &World, cell: CellCoord, except: Option<Handle>) -> bool {
    world
        .units
        .iter()
        .any(|(h, u)| !u.is_infantry() && !u.is_aircraft() && u.cell() == cell && Some(h) != except)
}

/// The infantry sub-cell spot occupancy bitmask of `cell` (bits 0..5), excluding
/// `except`. Built by scanning infantry resting in / assigned to the cell.
fn infantry_spot_bits(world: &World, cell: CellCoord, except: Option<Handle>) -> u8 {
    let mut bits = 0u8;
    for (h, u) in world.units.iter() {
        if u.is_infantry() && u.cell() == cell && Some(h) != except {
            bits |= 1 << u.sub_cell;
        }
    }
    bits
}

/// Whether `cell`'s five infantry spots are all taken.
fn infantry_cell_full(world: &World, cell: CellCoord, except: Option<Handle>) -> bool {
    (infantry_spot_bits(world, cell, except) & 0x1F) == 0x1F
}

/// Whether a vehicle other than `except` is *heading to* `cell` (its ordered
/// `dest`). Used with [`vehicle_in_cell`] so a same-tick group move spreads.
fn vehicle_targeting(world: &World, cell: CellCoord, except: Option<Handle>) -> bool {
    world.units.iter().any(|(h, u)| {
        !u.is_infantry() && !u.is_aircraft() && Some(h) != except && u.dest == Some(cell)
    })
}

/// How many infantry other than `except` are resting in **or** heading to
/// `cell` — the load against the five-per-cell cap for dispersal.
fn infantry_load(world: &World, cell: CellCoord, except: Option<Handle>) -> i32 {
    let resting = (infantry_spot_bits(world, cell, except) & 0x1F).count_ones() as i32;
    let heading = world
        .units
        .iter()
        .filter(|(h, u)| {
            u.is_infantry() && Some(*h) != except && u.dest == Some(cell) && u.cell() != cell
        })
        .count() as i32;
    resting + heading
}

/// Whether `cell` can be a destination for `unit`: passable for its locomotor
/// and not already claimed. Vehicles need the cell free of any vehicle (present
/// or inbound); infantry need fewer than five infantry claiming it.
fn dest_ok(
    world: &World,
    cell: CellCoord,
    unit: Handle,
    is_infantry: bool,
    loco: Locomotor,
) -> bool {
    if !world.passable.is_passable_loco(cell, loco) {
        return false;
    }
    if is_infantry {
        // Co-occupancy (Q5.3): infantry cannot share a cell with a vehicle.
        infantry_load(world, cell, Some(unit)) < SUBCELL_COUNT as i32
            && !vehicle_in_cell(world, cell, Some(unit))
    } else {
        // Co-occupancy (Q5.3): a vehicle cannot share a cell with any infantry.
        !vehicle_in_cell(world, cell, Some(unit))
            && !vehicle_targeting(world, cell, Some(unit))
            && infantry_spot_bits(world, cell, Some(unit)) & 0x1F == 0
    }
}

/// Choose a destination cell for a move order, dispersing off an occupied/claimed
/// target to the nearest free cell (`Adjust_Dest` scatter, `unit.cpp`). Spirals
/// outward from `dest` in rings; falls back to `dest` if nothing free is found.
fn pick_dest(
    world: &World,
    dest: CellCoord,
    unit: Handle,
    is_infantry: bool,
    loco: Locomotor,
) -> CellCoord {
    if dest_ok(world, dest, unit, is_infantry, loco) {
        return dest;
    }
    for r in 1..=12i32 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue; // ring perimeter only
                }
                let c = CellCoord::new(dest.x + dx, dest.y + dy);
                if dest_ok(world, c, unit, is_infantry, loco) {
                    return c;
                }
            }
        }
    }
    dest
}

/// Map a catalog locomotor index (0=Foot,1=Track,2=Wheel) to [`Locomotor`].
fn loco_from_index(i: u8) -> Locomotor {
    match i {
        0 => Locomotor::Foot,
        2 => Locomotor::Wheel,
        3 => Locomotor::Air,
        4 => Locomotor::Water,
        _ => Locomotor::Track,
    }
}

/// Locomotor index for the [`Locomotor::Air`] aircraft class (matches
/// [`loco_from_index`]). Used by the loader when wiring an aircraft proto.
pub const LOCO_AIR_INDEX: u8 = 3;

/// Locomotor index for the [`Locomotor::Water`] naval class (matches
/// [`loco_from_index`]). Used by the loader when wiring a vessel proto.
pub const LOCO_WATER_INDEX: u8 = 4;

/// Spawn a produced unit at `cell`, wiring its stats from the catalog proto. A
/// unit whose proto is infantry is placed into a free sub-cell spot of the exit
/// cell (up to five share a cell); a vehicle takes the whole cell.
fn spawn_produced_unit(world: &mut World, unit_id: u32, house: u8, cell: CellCoord) {
    let Some(proto) = world.catalog.unit(unit_id).cloned() else {
        return;
    };
    let handle = world.spawn_unit(
        proto.sprite_id,
        house,
        cell,
        Facing(128), // face south, out of the factory
        proto.max_health,
        proto.stats,
    );
    world.set_unit_max_health(handle, proto.max_health);
    world.set_unit_combat(handle, proto.armor, proto.weapon, proto.has_turret);
    world.set_unit_secondary(handle, proto.secondary);
    world.set_unit_harvester(handle, proto.is_harvester);
    if let Some(u) = world.units.get_mut(handle) {
        u.set_locomotor(loco_from_index(proto.locomotor));
    }
    if proto.is_infantry {
        let bits = infantry_spot_bits(world, cell, Some(handle));
        let spot = crate::occupancy::closest_free_spot_bits(bits, 0).unwrap_or(0);
        if let Some(u) = world.units.get_mut(handle) {
            u.make_infantry(spot);
        }
    }
    world.set_unit_sight(handle, proto.sight);

    // Vessels (Water locomotor): wire the submarine/detector capability (a sub
    // spawns submerged). They reuse the ground vehicle movement/combat systems
    // over water, keeping the default Guard mission (auto-acquire in weapon range).
    if loco_from_index(proto.locomotor) == Locomotor::Water {
        let (is_sub, is_det) = vessel_flags(&proto.name);
        if let Some(u) = world.units.get_mut(handle) {
            u.make_vessel(is_sub, is_det);
        }
        return;
    }

    // Aircraft (Air locomotor): spawn airborne with a full magazine, homed to the
    // house's helipad (`AircraftClass` ctor, `aircraft.cpp:254`). They run the
    // flight FSM (`run_aircraft`) and never take the ground guard/area-guard path.
    if loco_from_index(proto.locomotor) == Locomotor::Air {
        let home = find_helipad(world, house);
        if let Some(u) = world.units.get_mut(handle) {
            u.make_aircraft(proto.ammo);
            u.home = home;
        }
        return;
    }

    // Area-Guard on produce (M7.14 audit P2a). A **computer** house at `IQ >=
    // IQGuardArea` starts each produced, weapon-equipped unit in **Guard Area**
    // mode instead of plain Guard — it guards a zone (2× weapon range) around its
    // factory-exit post rather than leashing at exactly weapon range
    // (`Enter_Idle_Mode`: `IQ >= Rule.IQGuardArea && Is_Weapon_Equipped →
    // MISSION_GUARD_AREA`, infantry.cpp:1849-1856; the same idle-mode rule governs
    // vehicles via `DriveClass`/`FootClass`). A human (IQ 0 < IQGuardArea) keeps
    // plain Guard. Harvesters and unarmed/healer units are unaffected. The
    // spawn-exit cell is the guard post it leashes to. Inert for every synthetic
    // house (IQ 0), so no non-AI golden moves.
    let iq = world.houses.get(house as usize).map(|h| h.iq).unwrap_or(0);
    let armed = proto.weapon.map(|w| w.damage >= 0).unwrap_or(false);
    if !proto.is_harvester && armed && iq >= world.catalog.econ.iq.guard_area {
        if let Some(u) = world.units.get_mut(handle) {
            u.mission = crate::unit::Mission::AreaGuard;
            u.guard_post = Some(cell);
        }
    }
}

// ===========================================================================
// Harvester system (§4.9 M5) — port of Mission_Harvest (unit.cpp:2898)
// ===========================================================================

/// Harvester system: run the 5-state FSM for every harvester, in slot order.
fn run_harvesters(world: &mut World) {
    for handle in world.units.handles() {
        let is_harv = world
            .units
            .get(handle)
            .map(|u| u.is_harvester)
            .unwrap_or(false);
        if is_harv {
            process_harvester(world, handle);
        }
    }
}

/// One harvester's FSM tick. Simplifications from the original, each documented
/// at its site: adjacency docking (no `RADIO_HELLO`/`MISSION_ENTER` protocol),
/// single all-at-once unload (no staged `Harvester_Dump_List`), gold/gem bail
/// accounting collapsed to the original's bail model (`Credit_Load`).
fn process_harvester(world: &mut World, handle: Handle) {
    let (cell, house, mut hs, has_path) = match world.units.get(handle) {
        Some(u) => (u.cell(), u.house, u.harvest, !u.path.is_empty()),
        None => return,
    };
    let loco = world
        .units
        .get(handle)
        .map(|u| u.locomotor)
        .unwrap_or(Locomotor::Track);
    let cap = world.catalog.econ.bail_count;
    let dump_rate = world.catalog.econ.ore_dump_rate;

    // No refinery for this house -> guard/idle (unit.cpp:2922).
    if !house_has_refinery(world, house) {
        hs.status = HarvStatus::Idle;
        write_harvest(world, handle, hs);
        return;
    }

    use HarvStatus::*;
    match hs.status {
        Idle => {
            // Resume once the unit is no longer executing a manual order.
            if !has_path {
                hs.status = if hs.cargo >= cap { FindHome } else { Looking };
            }
        }
        Looking => {
            if hs.cargo >= cap {
                hs.status = FindHome;
            } else if has_path {
                // En route to an ore patch; wait for arrival.
            } else if world.ore.has_ore(cell) {
                hs.status = Harvesting;
                hs.timer = dump_rate;
            } else {
                // Find the nearest reachable ore and drive to it.
                match nearest_reachable_ore(world, cell, world.catalog.econ.long_scan_cells, loco) {
                    Some((dest, path)) => set_path(world, handle, dest, path),
                    None => {
                        hs.status = if hs.cargo > 0 { FindHome } else { Idle };
                    }
                }
            }
        }
        Harvesting => {
            if !world.ore.has_ore(cell) {
                hs.status = if hs.cargo >= cap { FindHome } else { Looking };
            } else if hs.timer > 0 {
                hs.timer -= 1;
            } else {
                hs.timer = dump_rate;
                let want = (cap - hs.cargo).min(1); // one bail per step (unit.cpp:2412)
                let lifted = world.ore.harvest(cell, want);
                hs.cargo += lifted.bails;
                if lifted.gem {
                    hs.gems += lifted.bails;
                } else {
                    hs.gold += lifted.bails;
                }
                if hs.cargo >= cap {
                    hs.status = FindHome;
                } else if !world.ore.has_ore(cell) {
                    hs.status = Looking;
                }
            }
        }
        FindHome => match nearest_refinery(world, house, cell, loco, handle) {
            Some((refinery, dock, path)) => {
                hs.home = Some(refinery);
                set_path(world, handle, dock, path);
                hs.status = HeadingHome;
            }
            None => hs.status = Idle,
        },
        HeadingHome => {
            if has_path {
                // Driving to the dock; wait.
            } else {
                // Arrived: are we adjacent to (or on the dock of) our refinery?
                let docked = hs
                    .home
                    .and_then(|h| world.buildings.get(h))
                    .map(|b| b.adjacent(cell) || b.covers(cell))
                    .unwrap_or(false);
                if docked {
                    hs.status = Unloading;
                    hs.timer = dump_rate;
                } else {
                    // Lost the dock (blocked/destroyed) — re-home.
                    hs.status = FindHome;
                }
            }
        }
        Unloading => {
            if hs.timer > 0 {
                hs.timer -= 1;
            } else {
                let econ = world.catalog.econ;
                let credits = hs.gold as i32 * econ.gold_value + hs.gems as i32 * econ.gem_value;
                // Storage cap (M7.7 Chunk C): a house can hold at most the sum of
                // its buildings' `Storage=` (refineries + silos); harvest income
                // beyond the cap is **wasted** (`HouseClass::Harvested`,
                // `house.cpp:80`). A house with no storage-declaring building
                // (`cap == 0`, e.g. synthetic test catalogs) stays uncapped, so
                // those goldens are byte-identical.
                let cap = house_storage_capacity(world, house);
                if let Some(hh) = world.houses.get_mut(house as usize) {
                    hh.add_harvest(credits, cap);
                }
                hs.cargo = 0;
                hs.gold = 0;
                hs.gems = 0;
                hs.home = None;
                hs.status = Looking;
            }
        }
    }

    write_harvest(world, handle, hs);
}

/// Persist a harvester's FSM state onto its unit.
fn write_harvest(world: &mut World, handle: Handle, hs: crate::unit::HarvestState) {
    if let Some(u) = world.units.get_mut(handle) {
        u.harvest = hs;
    }
}

/// Assign a movement path + destination to a unit (used by the harvest FSM).
fn set_path(world: &mut World, handle: Handle, dest: CellCoord, path: Vec<CellCoord>) {
    if let Some(u) = world.units.get_mut(handle) {
        u.path = path;
        u.dest = Some(dest);
        u.target = None;
    }
}

/// Whether `house` owns at least one live refinery.
fn house_has_refinery(world: &World, house: u8) -> bool {
    world
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.is_refinery && b.is_alive())
}

/// Find the nearest reachable ore cell within `max_cells` of `from`, returning
/// it and the path to it. Candidates are considered nearest-first (octagonal
/// `leptons_distance`, the sim's own metric); the first with a valid A* path
/// wins. Attempts are capped so a fully walled-off cluster doesn't run A* over
/// every ore cell in range.
fn nearest_reachable_ore(
    world: &World,
    from: CellCoord,
    max_cells: i32,
    loco: Locomotor,
) -> Option<(CellCoord, Vec<CellCoord>)> {
    let mut candidates: Vec<(i32, CellCoord)> = Vec::new();
    for dy in -max_cells..=max_cells {
        for dx in -max_cells..=max_cells {
            let c = CellCoord::new(from.x + dx, from.y + dy);
            if world.ore.has_ore(c) && world.passable.is_passable(c) {
                let d = leptons_distance(from.center(), c.center());
                candidates.push((d, c));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(cell_key(a.1).cmp(&cell_key(b.1))));
    for (_, c) in candidates.into_iter().take(16) {
        if let Some(path) = find_path(&world.passable, from, c, loco) {
            return Some((c, path));
        }
    }
    None
}

/// Find the nearest owned refinery, a free dock cell adjacent to it, and the
/// path there. Prefers the cell south of the refinery centre (matching the free
/// harvester's spawn placement, DIR_S), else any free adjacent cell.
fn nearest_refinery(
    world: &World,
    house: u8,
    from: CellCoord,
    loco: Locomotor,
    except: Handle,
) -> Option<(Handle, CellCoord, Vec<CellCoord>)> {
    let mut refineries: Vec<(i32, Handle)> = world
        .buildings
        .iter()
        .filter(|(_, b)| b.house == house && b.is_refinery && b.is_alive())
        .map(|(h, b)| (leptons_distance(from.center(), b.center_cell().center()), h))
        .collect();
    refineries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.index.cmp(&b.1.index)));
    for (_, rhandle) in refineries {
        let Some(b) = world.buildings.get(rhandle) else {
            continue;
        };
        // Candidate docks: the whole 1-cell ring, south-centre first.
        let mut docks: Vec<CellCoord> = Vec::new();
        let center = b.center_cell();
        docks.push(CellCoord::new(center.x, b.cell.y + b.foot_h as i32));
        for x in (b.cell.x - 1)..=(b.cell.x + b.foot_w as i32) {
            for y in (b.cell.y - 1)..=(b.cell.y + b.foot_h as i32) {
                let c = CellCoord::new(x, y);
                if b.adjacent(c) {
                    docks.push(c);
                }
            }
        }
        for dock in docks {
            // Skip a dock a *different* vehicle already sits on, so two harvesters
            // sharing a refinery pick distinct dock cells instead of one blocking
            // the other under the one-vehicle-per-cell rule.
            if !world.passable.is_passable(dock) || vehicle_in_cell(world, dock, Some(except)) {
                continue;
            }
            if let Some(path) = find_path(&world.passable, from, dock, loco) {
                return Some((rhandle, dock, path));
            }
        }
    }
    None
}

/// A stable linear key for a cell (deterministic tie-break).
fn cell_key(c: CellCoord) -> i64 {
    (c.y as i64) * 100000 + c.x as i64
}

/// Advance every moving unit along its path by up to its per-tick speed,
/// rotating its facing toward the heading. Units are processed in slot order.
///
/// **Cell occupancy (M7.6).** A fresh [`UnitGrid`] is rebuilt from current
/// positions (a non-hashed cache), then maintained through the pass so
/// reservations hold within the tick: a vehicle may not step into a cell already
/// holding another vehicle (one-vehicle-per-cell, `Can_Enter_Cell`), and
/// infantry may not step into a cell whose five sub-cell spots are full. A
/// blocked unit **waits** this tick and retries next tick — a documented
/// simplification of the original's `drive.cpp` ask-the-blocker-to-scatter radio
/// protocol (QUIRKS). On arrival an infantryman settles into the closest free
/// sub-cell spot (`Closest_Free_Spot`), so a group ordered to one cell packs into
/// distinct spots.
fn move_units(world: &mut World) {
    // Baseline vehicle-overlap count *before* moving. Movement must never
    // increase it (the one-vehicle-per-cell invariant). We compare before/after
    // rather than asserting zero so a harness that deliberately spawns stacked
    // units (e.g. splash tests isolating armor at one blast distance) is
    // tolerated — in a real game the baseline is zero, so this enforces exactly
    // "no two vehicles on one cell" every tick.
    let excess_before = vehicle_excess(world);

    let (gw, gh) = (world.passable.width(), world.passable.height());
    let mut grid = UnitGrid::new(gw, gh);
    // Rebuild occupancy from current positions, in slot order.
    for h in world.units.handles() {
        if let Some(u) = world.units.get(h) {
            if u.is_aircraft() {
                continue; // aircraft fly over cells — no ground occupancy
            }
            if u.is_infantry() {
                grid.claim_spot(u.cell(), u.sub_cell);
            } else {
                grid.claim_vehicle(u.cell(), h);
            }
        }
    }

    // Units whose cell changed this tick, for shroud reveal after movement.
    let mut moved: Vec<(u8, CellCoord, u8)> = Vec::new();
    // Vehicles that made real progress (coord changed) this tick, in processing
    // order. Used by the head-on tie-break below: we only *yield* to a lower-index
    // blocker that itself moved this tick — yielding to a stuck unit would just
    // turn this unit into a permanent wall (the harvester-dock deadlock). Membership
    // test only (no iteration), so it stays deterministic.
    let mut moved_this_tick: Vec<Handle> = Vec::new();
    for handle in world.units.handles() {
        let (start_cell, is_inf, start_spot, loco) = match world.units.get(handle) {
            // Aircraft move through `run_aircraft` (altitude flight), never the
            // ground path system.
            Some(u) if !u.path.is_empty() && !u.is_aircraft() => {
                (u.cell(), u.is_infantry(), u.sub_cell, u.locomotor)
            }
            _ => continue,
        };

        // Groundspeed handicap (M7.9 P2a): this unit's house scales its move speed
        // and turn rate (`drive.cpp:648/1354`, `MaxSpeed/ROT * House->GroundspeedBias`).
        // `FX_ONE` for a neutral house yields the raw values exactly, so ordinary
        // movement (and every movement golden) is byte-identical.
        let gs_bias = world
            .units
            .get(handle)
            .map(|u| u.house)
            .and_then(|hh| world.houses.get(hh as usize))
            .map(|h| h.handicap.groundspeed)
            .unwrap_or(crate::house::FX_ONE);
        let eff_speed = |raw: i32| crate::house::fx_mul(raw, gs_bias);

        // Rotate toward the next waypoint before translating.
        if let Some(u) = world.units.get_mut(handle) {
            let target = u.path[0].center();
            if let Some(desired) = Facing::toward(u.coord, target) {
                // Neutral house: use the raw rot exactly (byte-identical to
                // pre-handicap). Biased: scale, clamped to a sane 1..=255.
                let rot = if gs_bias == crate::house::FX_ONE {
                    u.stats.rot
                } else {
                    crate::house::fx_mul(u.stats.rot as i32, gs_bias).clamp(1, 255) as u8
                };
                u.facing = u.facing.rotate_toward(desired, rot.wrapping_add(1));
            }
        }

        // Compute the tentative advance, then validate the **actual landing cell**
        // it lands in against occupancy. Checking the true landing cell — not just
        // the path's next cell — covers a diagonal step that *corner-clips* a
        // neighbour. Cell-ownership rules (QUIRKS Q5): one vehicle per cell, and —
        // since M7.7 — vehicles and infantry do not co-occupy (`Can_Enter_Cell`,
        // `unit.cpp:3400`: no crushing, so an occupied-by-the-other-kind cell is
        // impassable-equivalent). On a block, re-route around occupied cells once
        // (`find_path_avoiding`, the `drive.cpp` reaction to a moving block); if
        // that still can't step, hold position this tick (a documented
        // simplification of the ask-the-blocker-to-scatter radio protocol).
        //
        // Returns `(new_coord, blocked, blocker)`, where `blocker` is the *vehicle*
        // that caused the block (if any) — used for the head-on livelock tie-break.
        let is_blocked = |world: &World, coord: WorldCoord, path: &[CellCoord], grid: &UnitGrid| {
            let budget = world
                .units
                .get(handle)
                .map(|u| eff_speed(u.stats.max_speed))
                .unwrap_or(0);
            let (nc, _) = advance_along_path(coord, path, budget);
            let land = nc.cell();
            if land == start_cell {
                return (nc, false, None);
            }
            let terrain_block = !world.passable.is_passable_loco(land, loco);
            // The vehicle occupying `land`, if it belongs to someone else.
            let veh_other = grid.vehicle_at(land).filter(|&h| h != handle);
            let occ_block = if is_inf {
                // Infantry: blocked by a full spot mask OR any vehicle in the cell.
                !grid.has_free_spot(land) || veh_other.is_some()
            } else {
                // Vehicle: blocked by another vehicle OR any infantry in the cell.
                veh_other.is_some() || (grid.spot_bits(land) & 0x1F) != 0
            };
            (nc, terrain_block || occ_block, veh_other)
        };

        let (coord, path) = match world.units.get(handle) {
            Some(u) => (u.coord, u.path.clone()),
            None => continue,
        };
        let (mut new_coord, blocked, blocker) = is_blocked(world, coord, &path, &grid);
        if blocked {
            // Head-on livelock tie-break (P0, M7.7). Two vehicles of *identical*
            // speed meeting head-on in a passable-width corridor used to re-route
            // in lock-step forever (both detour, both return, repeat). Break the
            // symmetry deterministically by slot order: when the blocker is a
            // lower-index vehicle that **already advanced this tick**, this
            // (higher-index) unit *yields* — it holds this tick instead of
            // re-routing — so only the lower-index unit detours and the pair
            // passes. The "already advanced" guard is essential: yielding to a
            // *stuck* lower-index unit would just make this unit a permanent wall
            // (the harvester-dock deadlock), so we only yield behind a unit that is
            // genuinely making progress. A parked blocker (empty path, never in
            // `moved_this_tick`) therefore never triggers a yield, and neither does
            // a mutually-stuck pair — both fall through to their normal re-route.
            let yield_to_lower = blocker.is_some_and(|b| {
                b.index < handle.index
                    && moved_this_tick.contains(&b)
                    && world.units.get(b).is_some_and(|u| !u.path.is_empty())
            });
            if yield_to_lower {
                continue; // hold this tick; the lower-index unit re-routes
            }
            // Re-route around the occupied cells to our destination.
            let mut rerouted = false;
            if let Some(dest) = world.units.get(handle).and_then(|u| u.dest) {
                if let Some(newpath) =
                    find_path_avoiding(&world.passable, start_cell, dest, loco, &grid, handle)
                {
                    let (nc2, blocked2, _) = is_blocked(world, coord, &newpath, &grid);
                    // Adopt the detour as long as it does not itself land on an
                    // occupied cell. Even a partial in-cell step counts — it turns
                    // the unit onto the detour heading and inches it off the
                    // contested cell, which is what breaks a head-on swap.
                    if !blocked2 {
                        if let Some(u) = world.units.get_mut(handle) {
                            u.path = newpath;
                        }
                        new_coord = nc2;
                        rerouted = true;
                    }
                }
            }
            if !rerouted {
                // No detour exists: every route to our destination runs through the
                // contested cell. This is the original's MOVE_TEMP case — the
                // immediate next step is a *friendly, stationary* blocker (a moving
                // friendly is MOVE_MOVING_BLOCK and handled by the yield tie-break
                // above; an enemy is MOVE_DESTROYABLE/MOVE_NO and never scattered).
                // Radio it to get out of the way (`Start_Of_Move` →
                // `CellClass::Incoming(0,true,true)` → `DriveClass::Scatter`,
                // drive.cpp:1090/1214 → drive.cpp:181; nokidding=true — the mover
                // has committed to this landing cell, so the friendly blocker is
                // forced out regardless of IQ, M7.14 P0). The blocker picks a random
                // adjacent MOVE_OK cell (one SYNC-RNG draw) and steps aside; the
                // mover holds this tick and retries next tick (our indefinite
                // hold-and-retry stands in for the original's `TryTryAgain`/
                // `PATH_RETRY` budget). This completes the Q5 simplification: the
                // 1-wide corridor and harvester-dock deadlocks now resolve instead
                // of waiting forever.
                //
                // Only *vehicle* movers issue the scatter request: the reaction
                // lives in `DriveClass::Start_Of_Move` (drive.cpp), and infantry are
                // `FootClass`/`InfantryClass`, not `DriveClass` — their per-cell
                // movement never fires `Incoming`, so a blocked infantryman still
                // disperses/holds as before (keeping the sub-cell packing behaviour
                // intact). A vehicle *blocker* that is infantry is still scattered
                // — `Incoming` scatters every occupier, `FootClass`/`DriveClass`
                // alike.
                if !is_inf {
                    scatter_friendly_blockers(world, handle, new_coord.cell(), &grid);
                }
                continue; // hold this tick; retry next tick
            }
        }

        let Some(unit) = world.units.get_mut(handle) else {
            continue;
        };
        // Consume the waypoints the (final) advance fully reached.
        let (applied_coord, consumed) =
            advance_along_path(unit.coord, &unit.path, eff_speed(unit.stats.max_speed));
        debug_assert_eq!(applied_coord, new_coord);
        // Record real progress (coord changed) for the head-on tie-break's
        // "already advanced this tick" guard. Vehicles only — infantry never drive
        // the vehicle tie-break.
        if new_coord != unit.coord && !is_inf {
            moved_this_tick.push(handle);
        }
        unit.coord = new_coord;
        for _ in 0..consumed {
            unit.path.remove(0);
        }
        if unit.path.is_empty() {
            unit.dest = None;
        }

        let end_cell = unit.cell();
        let arrived = unit.path.is_empty();

        // Maintain occupancy + assign infantry sub-cell spots.
        if is_inf {
            if end_cell != start_cell {
                grid.release_spot(start_cell, start_spot);
                let desired = spot_index(unit.coord);
                let spot = grid
                    .closest_free_spot(end_cell, desired)
                    .unwrap_or(desired.min(SUBCELL_COUNT as u8 - 1));
                unit.sub_cell = spot;
                grid.claim_spot(end_cell, spot);
                if arrived {
                    unit.coord = end_cell.spot_center(spot);
                }
            } else if arrived {
                // Settled within the same cell — snap onto its assigned spot.
                unit.coord = end_cell.spot_center(unit.sub_cell);
            }
        } else if end_cell != start_cell {
            grid.release_vehicle(start_cell, handle);
            grid.claim_vehicle(end_cell, handle);
        }

        // If the unit crossed into a new cell, note it for a shroud reveal.
        if end_cell != start_cell && unit.sight > 0 {
            moved.push((unit.house, end_cell, unit.sight));
        }
    }

    // Reveal the shroud around every unit that changed cell (incremental Look,
    // techno.cpp:6577). Done after the movement pass so the borrow is clear.
    for (house, cell, sight) in moved {
        world.shroud.reveal(house, cell, sight);
    }

    // Invariant: movement never creates a vehicle-on-vehicle overlap.
    debug_assert!(
        vehicle_excess(world) <= excess_before,
        "cell-occupancy invariant violated: movement put two vehicles on one cell"
    );
}

/// Cell offsets for the eight `FacingType` directions (N, NE, E, SE, S, SW, W,
/// NW), transcribed from `AdjacentCell[FACING_COUNT]` (`const.cpp:303`). Screen-Y
/// grows downward, so North is `-y`.
const ADJACENT_DELTAS: [(i32, i32); 8] = [
    (0, -1),  // N
    (1, -1),  // NE
    (1, 0),   // E
    (1, 1),   // SE
    (0, 1),   // S
    (-1, 1),  // SW
    (-1, 0),  // W
    (-1, -1), // NW
];

/// The cell adjacent to `c` in `FacingType` direction `face` (0..8),
/// `Adjacent_Cell(cell, face)` (`inline.h:854`).
fn adjacent_cell(c: CellCoord, face: u8) -> CellCoord {
    let (dx, dy) = ADJACENT_DELTAS[(face & 7) as usize];
    CellCoord::new(c.x + dx, c.y + dy)
}

/// The `CellClass::Incoming` scatter gate (`cell.cpp:2025`): a techno object
/// scatters from an incoming threat/blocker only when the call is `nokidding`,
/// or the global `Rule.IsScatter` (player-scatter) is on, or the object's house
/// has enough IQ (`House->IQ >= Rule.IQScatter`). Computer houses run at
/// `Rule.MaxIQ` (≥ `IQScatter`) so they auto-scatter; a human (IQ 0) does not,
/// **unless** the call forces it with `nokidding` (M7.14 P0).
///
/// **Where this bites.** The movement-deadlock reaction (mover committed to its
/// only landing cell, a friendly stationary blocker there) is the original's
/// `nokidding == true` site (`drive.cpp:1090/1214`, `Incoming(0,true,true)`) — it
/// forces the blocker out **regardless of IQ**, which is exactly why a *human*
/// harvester nudges a parked ally aside and reaches its dock (the Q5 complaint),
/// with no player-scatter deviation needed. The IQ-gated (`nokidding == false`)
/// path is the *combat* threat-scatter (artillery dodge), where a human's units
/// deliberately stand their ground and only computer units dodge.
fn scatter_gate(world: &World, house: u8, nokidding: bool) -> bool {
    if nokidding {
        return true;
    }
    let iq = world.houses.get(house as usize).map(|h| h.iq).unwrap_or(0);
    iq >= world.catalog.econ.iq.scatter
}

/// Ask every friendly, stationary unit occupying `cell` to scatter out of the
/// way of the mover — the sim's port of `Start_Of_Move`'s MOVE_TEMP reaction.
/// The mover has committed to entering `cell` (its sole landing cell, no detour),
/// so this is the `nokidding == true` variant (`drive.cpp:1090/1214`,
/// `CellClass::Incoming(0, true, true)`): the friendly temp blocker is forced out
/// **regardless of IQ** (each occupier scattered, `cell.cpp:2013`). Enemy
/// occupiers and *moving* allies never reach here (they aren't MOVE_TEMP),
/// matching the original.
fn scatter_friendly_blockers(world: &mut World, mover: Handle, cell: CellCoord, grid: &UnitGrid) {
    let Some(mover_house) = world.units.get(mover).map(|u| u.house) else {
        return;
    };
    // Collect the cell's occupiers in slot order. Vehicles and infantry never
    // co-occupy a cell (Q5.3), so it is one-or-the-other; scan infantry only when
    // no vehicle sits there (avoids an O(n) scan on the common vehicle case).
    let mut blockers: Vec<Handle> = Vec::new();
    if let Some(v) = grid.vehicle_at(cell) {
        if v != mover {
            blockers.push(v);
        }
    } else {
        for h in world.units.handles() {
            if let Some(u) = world.units.get(h) {
                if h != mover && u.is_infantry() && u.cell() == cell {
                    blockers.push(h);
                }
            }
        }
    }
    // Visited guard shared across the whole request. `scatter_blocker` cascades
    // the scatter to a boxed blocker's own friendly, stationary neighbours (the
    // multi-blocker chain fix, P0), so a unit could otherwise be reached twice
    // in one tick. Seeding with the mover keeps the cascade from looping back
    // onto it, and the guard makes the recursion terminate (each unit asked at
    // most once per tick) — which also keeps the per-tick scatter RNG draw count
    // bounded by the number of distinct units in the scene.
    let mut visited: Vec<Handle> = vec![mover];
    for b in blockers {
        // `Can_Enter_Cell` returns MOVE_TEMP only for a *friendly*, *stationary*
        // occupier (`unit.cpp:3336`: `is_moving == false`, i.e. no NavCom / not
        // rotating / not driving — our "empty path"). A busy ally is
        // MOVE_MOVING_BLOCK, not scattered. The `CellClass::Incoming` gate
        // (`scatter_gate`, cell.cpp:2025) is applied per-occupier with
        // `nokidding = true` — the forced move-out — so it always fires here,
        // regardless of the blocker's house IQ (the human-harvester case, Q5).
        let ask = world.units.get(b).is_some_and(|u| {
            world.are_allies(mover_house, u.house)
                && u.path.is_empty()
                && scatter_gate(world, u.house, true)
        });
        if ask && !visited.contains(&b) {
            visited.push(b);
            scatter_blocker(world, b, grid, &mut visited, None);
        }
    }
}

/// Build a fresh [`UnitGrid`] from every unit's current cell/sub-cell, in slot
/// order — the per-tick occupancy snapshot [`move_units`] also constructs. Used
/// by the combat threat-scatter ([`incoming_scatter`]) to pick free flee cells.
fn build_unit_grid(world: &World) -> UnitGrid {
    let (gw, gh) = (world.passable.width(), world.passable.height());
    let mut grid = UnitGrid::new(gw, gh);
    for h in world.units.handles() {
        if let Some(u) = world.units.get(h) {
            if u.is_aircraft() {
                continue; // aircraft fly over cells — no ground occupancy
            }
            if u.is_infantry() {
                grid.claim_spot(u.cell(), u.sub_cell);
            } else {
                grid.claim_vehicle(u.cell(), h);
            }
        }
    }
    grid
}

/// Artillery/grenade-dodge (M7.14 audit P2b) — the IQ-gated **combat** threat
/// scatter, the classic "run away from a slow incoming shell" trick. When a unit
/// fires a slow projectile (`weapon.proj_speed < Rule.Incoming`), the engine lets
/// the occupiers of the *target* cell run away, at fire time: `Fire_At` →
/// `Map[As_Cell(TarCom)].Incoming(Coord, true)` (infantry.cpp:3841 and the
/// `DriveClass`/`UnitClass` fire sites), then `CellClass::Incoming` (cell.cpp:2013)
/// scatters *each* occupier — but only through the IQ gate
/// `nokidding || Rule.IsScatter || House->IQ >= Rule.IQScatter`. Here `nokidding ==
/// false`, so a **computer** occupier (IQ ≥ IQScatter) dodges and a **human** (IQ 0)
/// stands its ground — the human/computer differentiation the M7.14 scatter arm was
/// built for. `threat` is the firer's muzzle; each dodger flees away from it
/// (`Scatter(threat, forced=true, nokidding=false)`, drive.cpp:181), drawing one
/// sync-RNG jitter per dodger. Inert when `Rule.Incoming == 0` (synthetic catalogs),
/// so no golden without a real `Incoming=` moves.
fn incoming_scatter(world: &mut World, target_cell: CellCoord, threat: WorldCoord) {
    let grid = build_unit_grid(world);
    // Occupiers of the target cell in slot order: a vehicle, else infantry.
    let mut occupiers: Vec<Handle> = Vec::new();
    if let Some(v) = grid.vehicle_at(target_cell) {
        occupiers.push(v);
    } else {
        for h in world.units.handles() {
            if let Some(u) = world.units.get(h) {
                if u.is_infantry() && u.cell() == target_cell {
                    occupiers.push(h);
                }
            }
        }
    }
    let mut visited: Vec<Handle> = Vec::new();
    for occ in occupiers {
        // Per-occupier IQ gate (`nokidding = false`) + the reference's own-idle
        // guard: `Scatter` under `nokidding == false` only fires when the unit has
        // no legal NavCom (`!Target_Legal(NavCom)`, drive.cpp:192) — i.e. it isn't
        // already driving somewhere. Our empty-path stands in for "no NavCom", so a
        // unit mid-move is left to its order and an idle guard/attacker dodges.
        let eligible = world
            .units
            .get(occ)
            .is_some_and(|u| u.path.is_empty() && scatter_gate(world, u.house, false));
        if eligible && !visited.contains(&occ) {
            visited.push(occ);
            scatter_blocker(world, occ, &grid, &mut visited, Some(threat));
        }
    }
}

/// Port of `DriveClass::Scatter(threat=0, forced=true, nokidding=false)` as
/// issued by `CellClass::Incoming` from `Start_Of_Move` (drive.cpp:181). The
/// blocker is nudged one cell in a direction seeded by its **current facing**
/// (the call sites pass `threat == 0`, so the bias is the unit's own facing, not
/// a threat direction) plus a single SYNC-RNG jitter draw (`Random_Pick(0, 2) -
/// 1` → −1/0/+1 facings). It then scans all eight adjacent cells in rotation and
/// assigns the **last** one that is `MOVE_OK` (clear terrain + unoccupied) — the
/// original loops without breaking, so the last legal facing wins (drive.cpp:207-213).
/// Returns the chosen cell (for tests/tracing), or `None` if nothing is clear or
/// the unit is exempt.
///
/// **RNG discipline:** exactly one sync draw per scattered blocker (the toface
/// jitter). The `Random_Pick(1, 4)` "1-in-4" gate is skipped because `forced`
/// short-circuits it (`drive.cpp:194`), matching the original at these call sites.
fn scatter_blocker(
    world: &mut World,
    blocker: Handle,
    grid: &UnitGrid,
    visited: &mut Vec<Handle>,
    threat: Option<WorldCoord>,
) -> Option<CellCoord> {
    let (start, facing, loco, is_inf, dumping, house) = {
        let u = world.units.get(blocker)?;
        // A harvester actively dumping at the dock never scatters
        // (`IsDumping`, drive.cpp:191).
        let dumping = u.is_harvester && u.harvest.status == HarvStatus::Unloading;
        (
            u.cell(),
            u.facing,
            u.locomotor,
            u.is_infantry(),
            dumping,
            u.house,
        )
    };
    if dumping {
        return None;
    }
    // toface base = Dir_Facing(...) (drive.cpp:201-206). When a `threat` coord is
    // given (the combat threat-scatter — an incoming projectile's firer), flee
    // *away* from it: `Dir_Facing(Direction8(threat, Coord))`, the direction from
    // the threat toward us. Otherwise (the friendly-blocker push, `threat == 0`),
    // seed from our own facing (`Dir_Facing(PrimaryFacing.Current())`).
    // Dir_Facing: `((dir + 0x10) & 0xFF) >> 5` (inline.h:611), giving 0..7.
    let base = match threat {
        Some(t) => {
            let f = Facing::toward(t, start.center()).unwrap_or(facing);
            (((f.0 as u16 + 0x10) & 0xFF) >> 5) as i32
        }
        None => (((facing.0 as u16 + 0x10) & 0xFF) >> 5) as i32,
    };
    let jitter = world.rng.range(0, 2) - 1; // -1, 0, +1
    let toface = (base + jitter).rem_euclid(8);

    // Iterate all eight facings; the LAST MOVE_OK cell wins (the original's
    // non-breaking `Assign_Destination` loop, drive.cpp:207-213). MOVE_OK = clear
    // terrain + unoccupied (no vehicle, and for a vehicle blocker no infantry).
    let mut chosen: Option<CellCoord> = None;
    for face in 0..8i32 {
        let nf = ((toface + face) & 7) as u8;
        let cell = adjacent_cell(start, nf);
        if !cell.on_map() || !world.passable.is_passable_loco(cell, loco) {
            continue;
        }
        let veh = grid.vehicle_at(cell).filter(|&h| h != blocker);
        let occupied = if is_inf {
            !grid.has_free_spot(cell) || veh.is_some()
        } else {
            veh.is_some() || (grid.spot_bits(cell) & 0x1F) != 0
        };
        if !occupied {
            chosen = Some(cell);
        }
    }
    if let Some(cell) = chosen {
        if let Some(u) = world.units.get_mut(blocker) {
            u.dest = Some(cell);
            u.path = vec![cell];
        }
        return Some(cell);
    }

    // Boxed: no free adjacent cell. Propagate the scatter request to any
    // friendly, stationary neighbour that is itself boxing us in — the
    // original's `CellClass::Incoming` cascade (`cell.cpp:2013`). In the
    // original, `Incoming` scatters *each* occupier of the cell, and a unit
    // that is asked to scatter but cannot move because a friendly blocks it is,
    // on the *next* tick, itself `Incoming`'d by whoever is now trying to enter
    // its cell — so a file of parked allies unclogs from the far end. We port
    // that propagation eagerly within the same request (chaining to the boxing
    // friendly now, rather than waiting a tick per link) so a two-or-more
    // blocker chain in a one-wide corridor resolves instead of deadlocking
    // (the old single-link scatter could *create* a permanent multi-blocker
    // gridlock — the M7.12 P0 regression this closes). Deterministic: faces in
    // rotation order, slot order within a cell, `visited`-guarded so each unit
    // is scattered at most once per tick and the recursion always terminates.
    for face in 0..8i32 {
        let ncell = adjacent_cell(start, (face & 7) as u8);
        if !ncell.on_map() {
            continue;
        }
        // Neighbours occupying `ncell`: a vehicle, or (if none) infantry.
        let mut neighbours: Vec<Handle> = Vec::new();
        if let Some(v) = grid.vehicle_at(ncell).filter(|&h| h != blocker) {
            neighbours.push(v);
        } else {
            for h in world.units.handles() {
                if let Some(u) = world.units.get(h) {
                    if h != blocker && u.is_infantry() && u.cell() == ncell {
                        neighbours.push(h);
                    }
                }
            }
        }
        for nb in neighbours {
            if visited.contains(&nb) {
                continue;
            }
            // Only a *friendly, stationary* (non-dumping) neighbour is MOVE_TEMP
            // and thus scatter-able — the same gate the mover applied to us.
            let eligible = world.units.get(nb).is_some_and(|u| {
                world.are_allies(house, u.house)
                    && u.path.is_empty()
                    && !(u.is_harvester && u.harvest.status == HarvStatus::Unloading)
                    && scatter_gate(world, u.house, true)
            });
            if eligible {
                visited.push(nb);
                scatter_blocker(world, nb, grid, visited, threat);
            }
        }
    }
    None
}

/// Advance `coord` along the cell-centre waypoints of `path` by up to `budget`
/// leptons, returning the new coordinate and how many waypoints were fully
/// reached (consumed). A pure copy of the original in-place stepping loop — same
/// `isqrt` straight-line metric, same partial-step formula, same `dist.max(0)`
/// budget decrement — so non-colliding movement is byte-identical to pre-M7.6.
fn advance_along_path(
    coord: WorldCoord,
    path: &[CellCoord],
    mut budget: i32,
) -> (WorldCoord, usize) {
    let mut c = coord;
    let mut consumed = 0usize;
    while budget > 0 && consumed < path.len() {
        let target = path[consumed].center();
        let dx = (target.x.0 - c.x.0) as i64;
        let dy = (target.y.0 - c.y.0) as i64;
        let dist = isqrt(dx * dx + dy * dy) as i32;
        if dist <= budget {
            c = target;
            budget -= dist.max(0);
            consumed += 1;
        } else {
            let nx = c.x.0 + (dx * budget as i64 / dist as i64) as i32;
            let ny = c.y.0 + (dy * budget as i64 / dist as i64) as i32;
            c = WorldCoord::new(nx, ny);
            budget = 0;
        }
    }
    (c, consumed)
}

/// The number of "excess" vehicles sharing cells — `sum over cells of
/// max(0, vehicles_in_cell - 1)`. Zero in a well-formed game; movement must
/// never increase it (the one-vehicle-per-cell rule).
fn vehicle_excess(world: &World) -> u32 {
    let mut counts: std::collections::BTreeMap<i64, u32> = std::collections::BTreeMap::new();
    for (_, u) in world.units.iter() {
        // Aircraft occupy the air, not a ground cell, so they never count toward
        // the one-vehicle-per-cell invariant (multiple may overfly a cell).
        if u.is_infantry() || u.is_aircraft() {
            continue;
        }
        let c = u.cell();
        *counts
            .entry((c.y as i64) * 100_000 + c.x as i64)
            .or_insert(0) += 1;
    }
    counts.values().map(|&n| n.saturating_sub(1)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> MoveStats {
        // JEEP-like: Speed=10 -> 25 leptons/tick, ROT=10.
        MoveStats {
            max_speed: 25,
            rot: 10,
        }
    }

    fn world() -> World {
        World::new(Passability::all_passable(), 0x1234)
    }

    // --- Combat test helpers (real rules.ini values for the starter weapons) ---
    use crate::combat::{Target, WarheadProfile, WeaponProfile};

    fn pct5(p: [i32; 5]) -> [i32; 5] {
        let mut o = [0i32; 5];
        for (d, v) in o.iter_mut().zip(p) {
            *d = v * 65536 / 100;
        }
        o
    }

    /// 2TNK's 90mm cannon (AP, Damage 30, ROF 50, Range 4.75 cells, Speed 40).
    fn ninety_mm() -> WeaponProfile {
        WeaponProfile {
            damage: 30,
            rof: 50,
            range: 1216, // 4.75 * 256
            proj_speed: 102,
            proj_rot: 0,
            invisible: false,
            instant: false,
            warhead: WarheadProfile {
                spread: 3,
                verses: pct5([30, 75, 75, 100, 50]),
            },
            warhead_ap: true,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// JEEP's M60mg (SA, Damage 15, Range 4, invisible + light-speed = instant).
    fn m60mg() -> WeaponProfile {
        WeaponProfile {
            damage: 15,
            rof: 20,
            range: 1024,
            proj_speed: 255,
            proj_rot: 0,
            invisible: true,
            instant: true,
            warhead: WarheadProfile {
                spread: 3,
                verses: pct5([100, 50, 60, 25, 25]),
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// A MammothTusk-style HE missile: high vs. unarmored/wood, low vs. heavy —
    /// the opposite armor profile to the AP `ninety_mm` cannon.
    fn he_missile() -> WeaponProfile {
        WeaponProfile {
            damage: 75,
            rof: 80,
            range: 1280,
            proj_speed: 76,
            proj_rot: 5,
            invisible: false,
            instant: false,
            warhead: WarheadProfile {
                spread: 6,
                verses: pct5([90, 75, 60, 25, 25]),
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// A MEDI-style heal weapon (negative damage, Organic warhead — only vs
    /// unarmored). Point-blank on an infantryman it heals; used by the smoke test.
    fn heal_weapon() -> WeaponProfile {
        WeaponProfile {
            damage: -50,
            rof: 80,
            range: 468, // 1.83 cells
            proj_speed: 255,
            proj_rot: 0,
            invisible: true,
            instant: true,
            warhead: WarheadProfile {
                spread: 0,
                verses: pct5([100, 0, 0, 0, 0]),
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// M7.7 Chunk C smoke: a medic auto-acquires a wounded friendly infantryman
    /// and heals it (negative damage through `modify_damage`'s point-blank heal).
    #[test]
    fn medic_heals_a_wounded_friendly_infantryman() {
        let mut world = World::new(Passability::all_passable(), 0x1EA1_0001);
        // A medic and a wounded friendly, both infantry, in the same cell area.
        let medic = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 80, stats());
        world.set_unit_combat(medic, 0, Some(heal_weapon()), false);
        if let Some(u) = world.units.get_mut(medic) {
            u.make_infantry(0);
        }
        let patient = world.spawn_unit(0, 1, CellCoord::new(10, 11), Facing(0), 50, stats());
        world.set_unit_combat(patient, 0, None, false);
        if let Some(u) = world.units.get_mut(patient) {
            u.make_infantry(0);
            u.health = 10; // wounded (max 50)
        }
        let hp0 = world.units.get(patient).unwrap().health;
        for _ in 0..40 {
            world.tick(&[]);
        }
        let hp1 = world.units.get(patient).unwrap().health;
        assert!(
            hp1 > hp0,
            "medic should have healed the wounded friendly ({hp0} -> {hp1})"
        );
        assert!(
            hp1 <= world.units.get(patient).unwrap().max_health,
            "heal must not exceed max health"
        );
    }

    /// M7.7 Chunk C smoke: an engineer ordered onto a nearly-dead enemy building
    /// captures it (ownership flips) and is consumed.
    #[test]
    fn engineer_captures_a_weak_enemy_building_and_is_consumed() {
        use crate::catalog::{BuildingProto, Catalog, EconRules};
        let mut world = World::new(Passability::all_passable(), 0xE6E6_0001);
        let proto = BuildingProto {
            name: "POWR".into(),
            foot_w: 2,
            foot_h: 2,
            max_health: 400,
            armor: 1,
            power: 100,
            cost: 300,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 4,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        };
        world.set_catalog(Catalog {
            buildings: vec![proto],
            units: vec![],
            econ: EconRules::default(),
        });
        world.init_houses(3, 0);
        // Enemy (house 2) building, damaged to below the 1/4 capture level.
        let bldg = world.spawn_building(0, 2, CellCoord::new(20, 20)).unwrap();
        world.buildings.get_mut(bldg).unwrap().health = 80; // 20% of 400 (<= 25%)
                                                            // Engineer (house 1, unarmed infantry) adjacent to the footprint.
        let eng = world.spawn_unit(0, 1, CellCoord::new(19, 20), Facing(0), 25, stats());
        world.set_unit_combat(eng, 0, None, false);
        if let Some(u) = world.units.get_mut(eng) {
            u.make_infantry(0);
        }
        world.tick(&[Command::Attack {
            unit: eng,
            target: Target::Building(bldg),
            house: 1,
        }]);
        for _ in 0..20 {
            world.tick(&[]);
        }
        assert_eq!(
            world.buildings.get(bldg).map(|b| b.house),
            Some(1),
            "the building should now belong to house 1 (captured)"
        );
        assert!(
            !world.units.contains(eng),
            "the engineer should be consumed on capture"
        );
    }

    /// M7.7 Chunk B smoke: a defense building auto-acquires the nearest enemy
    /// unit in range and damages it through the shared bullet path — no player
    /// order needed (`BuildingClass::Mission_Guard`).
    #[test]
    fn defense_building_auto_acquires_and_fires_on_an_enemy_in_range() {
        use crate::catalog::{BuildingProto, Catalog, EconRules};
        let mut world = World::new(Passability::all_passable(), 0xDEF0_0001);
        let pbox = BuildingProto {
            name: "PBOX".into(),
            foot_w: 1,
            foot_h: 1,
            max_health: 400,
            armor: 1,
            power: -15,
            cost: 400,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 5,
            sprite_id: 0,
            weapon: Some(m60mg()), // instant hitscan, range 1024 leptons (4 cells)
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        };
        world.set_catalog(Catalog {
            buildings: vec![pbox],
            units: vec![],
            econ: EconRules::default(),
        });
        world.init_houses(3, 0);
        world.spawn_building(0, 1, CellCoord::new(20, 20)).unwrap();
        // Enemy (house 2), unarmored, 2 cells away — inside the pillbox's range.
        let enemy = world.spawn_unit(0, 2, CellCoord::new(22, 20), Facing(0), 50, stats());
        world.set_unit_combat(enemy, 0, None, false); // armor none → full SA damage
        let hp0 = world.units.get(enemy).unwrap().health;
        for _ in 0..40 {
            world.tick(&[]);
        }
        // The pillbox should have acquired and hurt (or killed) the enemy.
        let hurt = world
            .units
            .get(enemy)
            .map(|u| u.health < hp0)
            .unwrap_or(true); // removed = dead = definitely hurt
        assert!(hurt, "the pillbox should auto-acquire and damage the enemy");
    }

    /// The mammoth-tank weapon pick (`What_Weapon_Should_I_Use`): the AP cannon
    /// against armored targets, the HE missiles against unarmored ones — chosen
    /// purely from the `Verses` table, no per-unit special-casing.
    #[test]
    fn secondary_weapon_selected_by_target_armor() {
        let primary = ninety_mm(); // AP: verses none=30%, heavy=100%
        let secondary = he_missile(); // HE: verses none=90%, heavy=25%
        let dist = 500; // both weapons in range

        // vs. unarmored infantry (armor 0): HE outscores AP → secondary.
        let w = super::select_weapon(primary, Some(secondary), 0, dist);
        assert_eq!(
            w.warhead.verses, secondary.warhead.verses,
            "vs none → HE secondary"
        );

        // vs. heavy steel (armor 3): AP outscores HE → primary.
        let w = super::select_weapon(primary, Some(secondary), 3, dist);
        assert_eq!(
            w.warhead.verses, primary.warhead.verses,
            "vs heavy → AP primary"
        );

        // No secondary → always the primary (single-weapon units unchanged).
        let w = super::select_weapon(primary, None, 0, dist);
        assert_eq!(w.warhead.verses, primary.warhead.verses);
    }

    fn spawn_tank(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
        let h = w.spawn_unit(0, house, cell, Facing(0), hp, stats());
        w.set_unit_combat(h, 3 /*heavy=steel*/, Some(ninety_mm()), true);
        h
    }

    #[test]
    fn tank_kills_adjacent_enemy_with_expected_shot_count() {
        // 2TNK (90mm, 30 dmg vs steel) vs a 600-hp heavy target one cell away.
        let mut w = world();
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false); // unarmed heavy (HARV-like)

        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        // Run until the target dies or a generous timeout.
        let mut ticks = 0;
        while w.units.contains(tgt) && ticks < 2000 {
            w.tick(&[]);
            ticks += 1;
        }
        assert!(!w.units.contains(tgt), "target should have been destroyed");
        // 600 hp / 30 dmg = 20 shots. Rearm is ROF=50 ticks between shots, so
        // the kill lands on the 20th shot ~ 19*50 ticks after the first.
        // Sanity-bound the timing rather than pin it exactly.
        assert!(
            (900..1100).contains(&ticks),
            "unexpected time-to-kill: {ticks} ticks"
        );
    }

    #[test]
    fn splash_damage_hits_a_bystander_near_a_force_fire_impact() {
        // Force-fire the 90mm (AP, base 30, spread 3) at empty ground cell
        // (20,20); a heavy-armor bystander sits one cell away at (21,20).
        // Hand-check: impact centre is (20,20).center() = (20*256+128, ...).
        // The bystander centre is one cell (256 leptons) east, so
        // leptons_distance(impact, bystander) = 256.
        // modify_damage(30, AP, armor=3 (100%), 256, 1, 1000):
        //   30*100% = 30; spread 3 -> 256 / (3*5=15) = 17 -> clamp 16 -> 30/16 = 1.
        // So the bystander loses exactly 1 hp per shot from splash alone.
        let mut w = world();
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 20), 400);
        let bystander = w.spawn_unit(0, 2, CellCoord::new(21, 20), Facing(0), 600, stats());
        w.set_unit_combat(bystander, 3, None, false); // unarmed heavy

        let before = w.units.get(bystander).unwrap().health;
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Cell(CellCoord::new(20, 20)),
            house: 1,
        }]);
        // Let the bullet fly to the ground and detonate.
        let mut fired = false;
        for _ in 0..200 {
            w.tick(&[]);
            let now = w.units.get(bystander).unwrap().health;
            if now < before {
                assert_eq!(before - now, 1, "one 90mm splash hit at 1 cell = 1 hp");
                fired = true;
                break;
            }
        }
        assert!(fired, "force-fire at ground never splashed the bystander");
    }

    #[test]
    fn idle_unit_retaliates_against_its_attacker() {
        // An idle, armed guard tank (house 2) that gets shot by house 1 turns and
        // targets the attacker (guard-mission return fire, foot.cpp:1189).
        let mut w = world();
        let attacker = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let guard = spawn_tank(&mut w, 2, CellCoord::new(11, 10), 400);
        // Guard starts idle: no target, no path.
        assert!(!w.units.get(guard).unwrap().has_target());

        w.tick(&[Command::Attack {
            unit: attacker,
            target: Target::Unit(guard),
            house: 1,
        }]);
        // Within a few shots the guard should have taken a hit and retaliated.
        let mut retaliated = false;
        for _ in 0..300 {
            w.tick(&[]);
            if let Some(g) = w.units.get(guard) {
                if g.target == Some(Target::Unit(attacker)) {
                    retaliated = true;
                    break;
                }
            } else {
                break; // guard died before we observed retaliation
            }
        }
        assert!(retaliated, "idle guard never returned fire on its attacker");
    }

    #[test]
    fn retaliation_never_overrides_an_explicit_order() {
        // A unit under a live move order that gets shot must NOT drop its order to
        // retaliate (we only wake truly idle units).
        let mut w = world();
        let attacker = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let mover = spawn_tank(&mut w, 2, CellCoord::new(11, 10), 400);
        w.tick(&[
            Command::Attack {
                unit: attacker,
                target: Target::Unit(mover),
                house: 1,
            },
            Command::Move {
                unit: mover,
                dest: CellCoord::new(30, 30),
                house: 2,
            },
        ]);
        // While the mover still has a path, its target stays None (no hijack).
        for _ in 0..40 {
            w.tick(&[]);
            let Some(m) = w.units.get(mover) else { break };
            if !m.path.is_empty() {
                assert_eq!(
                    m.target, None,
                    "a moving unit must not be hijacked to retaliate"
                );
            }
        }
    }

    #[test]
    fn attack_needs_ownership_and_a_weapon() {
        let mut w = world();
        let armed = spawn_tank(&mut w, 1, CellCoord::new(5, 5), 400);
        // Place the enemy FAR out of weapon range: since M7.11 an idle armed
        // default-Guard unit proactively auto-acquires any enemy within weapon
        // range (universal guard acquisition), which would mask the "command
        // ignored" behaviour this test isolates. Out of range, the only way
        // `armed` could gain a target is the (rejected) Attack command.
        let tgt = w.spawn_unit(0, 2, CellCoord::new(60, 60), Facing(0), 100, stats());
        // Wrong house: ignored.
        w.tick(&[Command::Attack {
            unit: armed,
            target: Target::Unit(tgt),
            house: 99,
        }]);
        assert!(!w.units.get(armed).unwrap().has_target());
        // Unarmed attacker: ignored.
        let unarmed = w.spawn_unit(0, 1, CellCoord::new(5, 6), Facing(0), 400, stats());
        w.tick(&[Command::Attack {
            unit: unarmed,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        assert!(!w.units.get(unarmed).unwrap().has_target());
    }

    #[test]
    fn force_fire_at_cell_consumes_sim_rng_when_scattering() {
        // A tank force-firing an AP shot at a distant ground cell scatters, which
        // draws the sync RNG (the one genuine combat RNG path). The seed must
        // therefore advance across the shot.
        let mut w = world();
        // Place attacker and force-fire target ~4.5 cells apart (in range, and
        // far enough that scatterdist > 0 so a draw actually happens).
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let cell = CellCoord::new(14, 12); // ~4.5 cells => distance > 1024 leptons
        let seed_before = w.rng_seed();
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Cell(cell),
            house: 1,
        }]);
        // Give the turret time to align and fire once.
        let mut fired = false;
        for _ in 0..80 {
            let seed = w.rng_seed();
            w.tick(&[]);
            if w.rng_seed() != seed {
                fired = true;
                break;
            }
        }
        assert!(fired, "force-fire never drew the sim RNG (no scatter)");
        assert_ne!(seed_before, w.rng_seed(), "sim RNG did not advance");
    }

    #[test]
    fn unit_target_shot_is_accurate_no_rng() {
        // A shot at a *vehicle* is accurate — it must NOT draw the sim RNG.
        let mut w = world();
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false);
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        let seed = w.rng_seed();
        // Step through at least one full shot (ROF 50) — target loses health.
        for _ in 0..60 {
            w.tick(&[]);
        }
        assert!(
            w.units.get(tgt).unwrap().health < 600,
            "target took no damage"
        );
        assert_eq!(
            seed,
            w.rng_seed(),
            "accurate vehicle shot must not draw RNG"
        );
    }

    #[test]
    fn instant_weapon_hits_same_tick_as_fire() {
        // M60mg is a hitscan weapon: the bullet detonates the tick it is created.
        let mut w = world();
        let jeep = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 150, stats());
        w.set_unit_combat(jeep, 2, Some(m60mg()), true);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false);
        w.tick(&[Command::Attack {
            unit: jeep,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        let start_hp = w.units.get(tgt).unwrap().health;
        for _ in 0..40 {
            w.tick(&[]);
            // No bullet should ever linger for a hitscan weapon.
            assert!(
                w.bullets.is_empty(),
                "instant weapon left a bullet in flight"
            );
        }
        // SA vs steel = 25% of 15 => 4 dmg/shot; several shots landed.
        assert!(w.units.get(tgt).unwrap().health < start_hp);
    }

    #[test]
    fn attack_is_deterministic_hash_chain() {
        let script = |w: &mut World| -> Vec<u64> {
            let atk = spawn_tank(w, 1, CellCoord::new(8, 8), 400);
            let tgt = w.spawn_unit(0, 2, CellCoord::new(13, 10), Facing(0), 600, stats());
            w.set_unit_combat(tgt, 3, None, false);
            let mut hs = Vec::new();
            hs.push(w.tick(&[Command::Attack {
                unit: atk,
                target: Target::Cell(CellCoord::new(13, 10)),
                house: 1,
            }]));
            for _ in 0..120 {
                hs.push(w.tick(&[]));
            }
            hs
        };
        let mut a = world();
        let mut b = world();
        assert_eq!(script(&mut a), script(&mut b));
    }

    #[test]
    fn move_command_paths_and_advances() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 10),
            house: 1,
        }]);
        let u = w.units.get(h).unwrap();
        assert!(u.is_moving(), "unit should have a path");
        assert!(u.dest.is_some());
        // It should have advanced south (larger y) toward the goal.
        assert!(u.coord.y.0 > CellCoord::new(5, 5).center().y.0);
    }

    #[test]
    fn unit_eventually_arrives() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(8, 9),
            house: 1,
        }]);
        for _ in 0..500 {
            if !w.units.get(h).unwrap().is_moving() {
                break;
            }
            w.tick(&[]);
        }
        let u = w.units.get(h).unwrap();
        assert!(!u.is_moving(), "unit never finished its path");
        assert_eq!(u.cell(), CellCoord::new(8, 9));
        assert!(u.dest.is_none());
    }

    #[test]
    fn wrong_house_command_is_ignored() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 10),
            house: 2, // not the owner
        }]);
        assert!(!w.units.get(h).unwrap().is_moving());
    }

    #[test]
    fn stop_clears_path() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 20),
            house: 1,
        }]);
        assert!(w.units.get(h).unwrap().is_moving());
        w.tick(&[Command::Stop { unit: h, house: 1 }]);
        assert!(!w.units.get(h).unwrap().is_moving());
    }

    #[test]
    fn same_seed_and_commands_give_same_hash_chain() {
        let script = |w: &mut World| -> Vec<u64> {
            let h = w.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats());
            let mut hashes = Vec::new();
            hashes.push(w.tick(&[Command::Move {
                unit: h,
                dest: CellCoord::new(20, 15),
                house: 1,
            }]));
            for _ in 0..60 {
                hashes.push(w.tick(&[]));
            }
            hashes
        };
        let mut a = world();
        let mut b = world();
        assert_eq!(script(&mut a), script(&mut b));
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn hash_changes_when_state_changes() {
        let mut w = world();
        let empty = w.state_hash();
        w.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats());
        assert_ne!(empty, w.state_hash());
    }
}

/// M5 economy smoke tests — asset-free coverage that the new systems (deploy,
/// production, placement, harvesting, power) actually run and stay deterministic.
/// Full-fidelity coverage against real rules.ini is ra-tester's domain; these
/// are the minimal "does it run + is it deterministic" checks the coder ships.
#[cfg(test)]
mod m5_tests {
    use super::*;
    use crate::catalog::{BuildingProto, Catalog, EconRules, UnitProto};
    use crate::coords::{Facing, MAP_CELL_H, MAP_CELL_W};
    use crate::house::{BuildItem, ProdKind};
    use crate::ore::{OreField, OVERLAY_GOLD_FIRST};
    use crate::unit::{HarvStatus, MoveStats};

    // Building ids / unit-proto ids used by the test catalog.
    const B_FACT: u32 = 0;
    const B_POWR: u32 = 1;
    const B_PROC: u32 = 2;
    const B_WEAP: u32 = 3;
    const B_PAD: u32 = 4; // 1x1 filler, for occupancy-blocking tests only
    const U_HARV: u32 = 1;
    const U_TANK: u32 = 2;

    fn stats() -> MoveStats {
        MoveStats {
            max_speed: 40,
            rot: 10,
        }
    }

    /// A tiny catalog: FACT (construction yard), POWR (100 output), PROC
    /// (refinery, −30 drain, spawns a free harvester) + MCV/HARV units. Costs are
    /// small so build loops finish quickly.
    fn catalog() -> Catalog {
        let bproto = |name: &str,
                      w: u8,
                      h: u8,
                      power: i32,
                      cost: i32,
                      prereq: Vec<u32>,
                      cy: bool,
                      refin: bool,
                      wf: bool| BuildingProto {
            is_barracks: false,
            name: name.to_string(),
            foot_w: w,
            foot_h: h,
            max_health: 500,
            armor: 0,
            power,
            cost,
            prereq,
            is_refinery: refin,
            is_construction_yard: cy,
            is_war_factory: wf,
            free_harvester_unit: if refin { Some(U_HARV) } else { None },
            sight: 4,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        };
        let uproto =
            |name: &str, harv: bool, deploys: Option<u32>, cost: i32, prereq: Vec<u32>| UnitProto {
                is_infantry: false,
                locomotor: 1,
                name: name.to_string(),
                sprite_id: if harv { 1 } else { 0 },
                max_health: 400,
                stats: stats(),
                armor: 0,
                weapon: None,
                secondary: None,
                has_turret: false,
                is_harvester: harv,
                deploys_to: deploys,
                cost,
                prereq,
                sight: 2,
                passengers: 0,
                ammo: 0,
            };
        Catalog {
            buildings: vec![
                bproto("FACT", 3, 3, 0, 100, vec![], true, false, false),
                bproto("POWR", 2, 2, 100, 30, vec![B_FACT], false, false, false),
                bproto("PROC", 3, 3, -30, 50, vec![B_POWR], false, true, false),
                bproto("WEAP", 3, 3, -20, 60, vec![B_POWR], false, false, true),
                bproto("PAD", 1, 1, 0, 10, vec![], false, false, false),
            ],
            units: vec![
                uproto("MCV", false, Some(B_FACT), 100, vec![]),
                uproto("HARV", true, None, 140, vec![]),
                uproto("TANK", false, None, 80, vec![B_WEAP]),
            ],
            econ: EconRules::default(),
        }
    }

    fn econ_world(credits: i32) -> World {
        let mut w = World::new(Passability::all_passable(), 0x51ee_d123);
        w.set_catalog(catalog());
        w.init_houses(3, credits);
        w
    }

    #[test]
    fn deploy_mcv_creates_construction_yard_and_occupies_cells() {
        let mut w = econ_world(1000);
        let mcv = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
        assert!(w.passability().is_passable(CellCoord::new(20, 20)));
        w.tick(&[Command::Deploy {
            unit: mcv,
            house: 1,
        }]);
        // The unit is gone; a 3x3 construction yard now stands centred on it.
        assert!(!w.units.contains(mcv));
        assert_eq!(w.buildings.len(), 1);
        let (_, b) = w.buildings.iter().next().unwrap();
        assert!(b.is_construction_yard);
        assert_eq!(b.cell, CellCoord::new(19, 19)); // top-left = mcv - (1,1)
                                                    // Footprint cells are now occupied (impassable to movers).
        assert!(!w.passability().is_passable(CellCoord::new(20, 20)));
        assert!(!w.passability().is_passable(CellCoord::new(21, 21)));
        assert!(w.passability().is_passable(CellCoord::new(23, 23))); // outside
                                                                      // The house owns one construction yard.
        assert!(w.house(1).unwrap().owns_building(B_FACT));
    }

    #[test]
    fn wrong_house_cannot_deploy() {
        let mut w = econ_world(1000);
        let mcv = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
        w.tick(&[Command::Deploy {
            unit: mcv,
            house: 2,
        }]); // not the owner
        assert!(w.units.contains(mcv));
        assert_eq!(w.buildings.len(), 0);
    }

    #[test]
    fn build_and_place_power_plant_updates_power_and_prereqs() {
        let mut w = econ_world(1000);
        let mcv = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
        w.tick(&[Command::Deploy {
            unit: mcv,
            house: 1,
        }]);

        // Prereq gate: PROC needs POWR, so it must be rejected up front.
        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_PROC),
        }]);
        assert!(w.house(1).unwrap().building_prod.is_none());

        // POWR is buildable (prereq FACT owned). Start it and run to completion.
        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_POWR),
        }]);
        assert!(w.house(1).unwrap().building_prod.is_some());
        let mut ready = false;
        for _ in 0..500 {
            w.tick(&[]);
            if w.house(1).unwrap().ready_building == Some(B_POWR) {
                ready = true;
                break;
            }
        }
        assert!(ready, "POWR never completed");
        // Cost 30 was paid in installments.
        assert_eq!(w.house_credits(1), 1000 - 30);

        // Place it adjacent to the yard (proximity ok) and confirm power output.
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(22, 19),
        }]);
        assert!(w.house(1).unwrap().ready_building.is_none());
        assert_eq!(w.house(1).unwrap().power_output, 100);
        assert!(w.house(1).unwrap().owns_building(B_POWR));
    }

    #[test]
    fn refinery_spawns_free_harvester_that_mines_and_banks_credits() {
        let mut w = econ_world(0); // start broke — all credits must come from ore
                                   // A patch of gold ore.
        let total = 128 * 128;
        let mut ov = vec![0xFFu8; total];
        for y in 24..28 {
            for x in 24..28 {
                ov[y * 128 + x] = OVERLAY_GOLD_FIRST;
            }
        }
        w.set_ore(OreField::from_overlay(128, 128, &ov));
        // Place a refinery directly (skips production); it spawns nothing on its
        // own, so also drop in a harvester next to it.
        let refinery = w.spawn_building(B_PROC, 1, CellCoord::new(18, 18)).unwrap();
        assert!(w.buildings.get(refinery).unwrap().is_refinery);
        let harv = w.spawn_unit(1, 1, CellCoord::new(21, 22), Facing(0), 400, stats());
        w.set_unit_harvester(harv, true);

        // Run: the harvester should scan ore, mine, dock, unload, and bank credits.
        let mut banked = false;
        for _ in 0..4000 {
            w.tick(&[]);
            if w.house_credits(1) > 0 {
                banked = true;
                break;
            }
        }
        assert!(banked, "harvester never banked any credits");
        // Credits are whole gold bails × GoldValue.
        assert_eq!(w.house_credits(1) % w.catalog.econ.gold_value, 0);
    }

    #[test]
    fn economy_script_is_deterministic() {
        // Runs a deploy → build-POWR → place-POWR script and asserts BOTH that it
        // is deterministic AND that every step actually took effect (the M5
        // version only compared hash chains, so it would have passed even if the
        // deploy/production/placement had all silently no-op'd identically —
        // ra-tester flagged that as a weak smoke test; strengthened here).
        // Adjacent to the construction yard (top-left 29,29 after deploying the
        // MCV at 30,30) so the proximity rule accepts it — the M5 test used
        // (33,29), which is NOT adjacent, so its placement silently no-op'd and
        // the hash-only comparison passed anyway. That is the weakness fixed here.
        let placed_cell = CellCoord::new(32, 29);
        let script = |w: &mut World| -> Vec<u64> {
            let mcv = w.spawn_unit(0, 1, CellCoord::new(30, 30), Facing(0), 400, stats());
            let mut hs = vec![w.tick(&[Command::Deploy {
                unit: mcv,
                house: 1,
            }])];
            // The MCV must have become a construction yard.
            assert!(!w.units.contains(mcv), "MCV was not consumed by deploy");
            assert!(
                w.buildings
                    .iter()
                    .any(|(_, b)| b.house == 1 && b.is_construction_yard),
                "deploy did not create a construction yard"
            );
            hs.push(w.tick(&[Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_POWR),
            }]));
            assert!(
                w.house(1).unwrap().building_prod.is_some(),
                "StartProduction(POWR) was rejected"
            );
            let mut placed = false;
            for _ in 0..200 {
                // Place POWR as soon as it is ready (deterministic tick).
                if w.house(1).unwrap().ready_building == Some(B_POWR) {
                    let before = w.buildings.len();
                    hs.push(w.tick(&[Command::PlaceBuilding {
                        house: 1,
                        building: B_POWR,
                        cell: placed_cell,
                    }]));
                    // The placement must actually have added the building.
                    assert_eq!(
                        w.buildings.len(),
                        before + 1,
                        "PlaceBuilding did not add a building"
                    );
                    placed = true;
                } else {
                    hs.push(w.tick(&[]));
                }
            }
            assert!(placed, "POWR never completed / was never placed");
            hs
        };
        let mut a = econ_world(1000);
        let mut b = econ_world(1000);
        assert_eq!(script(&mut a), script(&mut b));
        assert_eq!(a.state_hash(), b.state_hash());

        // Concrete end-state assertions (identical for both runs): the house owns
        // a POWR standing on the placement cell, its power output is live, and the
        // footprint is occupied.
        let hs = a.house(1).unwrap();
        assert!(
            hs.owns_building(B_POWR),
            "house does not own the placed POWR"
        );
        assert_eq!(hs.power_output, 100, "placed POWR's power is not accounted");
        assert!(
            a.buildings
                .iter()
                .any(|(_, b)| b.house == 1 && b.type_id == B_POWR && b.cell == placed_cell),
            "no POWR building stands at the placement cell"
        );
        assert!(
            !a.passability().is_passable(placed_cell),
            "placed POWR did not stamp footprint occupancy"
        );
    }

    #[test]
    fn low_power_throttles_production_but_still_completes() {
        // A house whose only building is a refinery (drain 30, no power source):
        // Power_Fraction == 0, so `build_time_scale` snapshots the ×4 multiplier
        // at production start (techno.cpp:6819) — production runs at a quarter
        // speed but still finishes. Compare its build time against a full-power
        // house. (M6 replaced M5's continuous ≤½-power throttle with the
        // original's discrete ×4/×2.5/×1.5 snapshot; this bound updated to match.)
        let build_ticks = |low_power: bool| -> i32 {
            let mut w = econ_world(1000);
            // Give both a construction yard so POWR is buildable.
            w.spawn_building(B_FACT, 1, CellCoord::new(40, 40)).unwrap();
            if low_power {
                // A refinery with no power plant => output 0 < drain 30.
                w.spawn_building(B_PROC, 1, CellCoord::new(50, 50)).unwrap();
                assert!(w.house(1).unwrap().low_power());
            }
            w.tick(&[Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_POWR),
            }]);
            let mut ticks = 1;
            for _ in 0..2000 {
                if w.house(1).unwrap().ready_building == Some(B_POWR) {
                    break;
                }
                w.tick(&[]);
                ticks += 1;
            }
            ticks
        };
        let full = build_ticks(false);
        let low = build_ticks(true);
        assert!(
            low > full,
            "low power ({low} ticks) should be slower than full power ({full} ticks)"
        );
        // Exact numbers (M7.9 P0 STEP_COUNT conversion). POWR costs 30 with the
        // synthetic catalog's default `BuildSpeedBias = 1.0`, so the raw build
        // time is `T = round(30 × 0.9) = 27` ticks — *below* one STEP_COUNT.
        //   full power: rate = Bound(27/54, 1, 255) = 1  => 1 × 54 =  54 ticks
        //   zero power: T×4 = 108, rate = Bound(108/54,1,255) = 2 => 2 × 54 = 108
        // The ×4 power penalty does NOT quadruple the wall-clock here because the
        // full-power time floors up to a single STEP_COUNT (rate clamped 0→1),
        // while the ×4 time clears the 54-tick floor — the authentic factory
        // quirk (`FactoryClass::Start` Bound(.., 1, 255), factory.cpp:439).
        assert_eq!(full, 54, "full-power POWR floors to one STEP_COUNT");
        assert_eq!(low, 108, "zero-power POWR: T×4 then STEP_COUNT rate");
    }

    // -----------------------------------------------------------------
    // ra-tester additions below: harvester FSM edges, production edges,
    // full-loop economy determinism (M5 coverage build-out).
    // -----------------------------------------------------------------

    /// A `World` with a custom passability grid instead of the default
    /// all-passable 128×128 (for tests that need to control what's reachable).
    fn econ_world_on(passable: Passability, credits: i32) -> World {
        let mut w = World::new(passable, 0x51ee_d123);
        w.set_catalog(catalog());
        w.init_houses(3, credits);
        w
    }

    // === Harvester FSM edges ===========================================

    #[test]
    fn harvester_with_no_refinery_stays_idle_and_never_panics() {
        let mut w = econ_world(1000);
        let h = w.spawn_unit(1, 1, CellCoord::new(40, 40), Facing(0), 400, stats());
        w.set_unit_harvester(h, true);
        for _ in 0..50 {
            w.tick(&[]);
            assert_eq!(
                w.units.get(h).unwrap().harvest.status,
                HarvStatus::Idle,
                "no refinery anywhere -> the FSM must guard-idle forever, never panic"
            );
        }
    }

    #[test]
    fn ore_exhausted_mid_harvest_falls_back_to_looking_then_completes_the_cycle() {
        let mut w = econ_world(1000);
        w.spawn_building(B_PROC, 1, CellCoord::new(10, 10)).unwrap();
        // A single isolated ore bail, directly on the harvester's spawn cell.
        let total = 128 * 128;
        let mut ov = vec![0xFFu8; total];
        let ore_cell = CellCoord::new(40, 40);
        ov[(ore_cell.y * 128 + ore_cell.x) as usize] = OVERLAY_GOLD_FIRST;
        w.set_ore(OreField::from_overlay(128, 128, &ov));
        assert_eq!(w.ore.at(ore_cell).bails, 1); // isolated cell -> _adj[0] -> 1 bail

        let h = w.spawn_unit(1, 1, ore_cell, Facing(0), 400, stats());
        w.set_unit_harvester(h, true);

        // Run until the single bail is lifted and the cell empties out from
        // under the harvester mid-`Harvesting`.
        let mut left_harvesting = false;
        for _ in 0..50 {
            w.tick(&[]);
            let hs = w.units.get(h).unwrap().harvest;
            if hs.status != HarvStatus::Harvesting && hs.cargo > 0 {
                left_harvesting = true;
                break;
            }
        }
        assert!(
            left_harvesting,
            "harvester should leave Harvesting the instant its cell's ore hits zero"
        );
        assert!(!w.ore.has_ore(ore_cell));
        assert_eq!(w.units.get(h).unwrap().harvest.cargo, 1);

        // With nowhere else to mine (cargo < capacity), the FSM must still
        // route home on what it already has, not get stuck re-scanning for
        // ore that no longer exists anywhere on the map.
        let mut banked = false;
        for _ in 0..2000 {
            w.tick(&[]);
            if w.house_credits(1) > 1000 {
                banked = true;
                break;
            }
        }
        assert!(
            banked,
            "a partially-loaded harvester with no ore left anywhere should still complete a home cycle"
        );
    }

    #[test]
    fn refinery_removed_while_en_route_forces_idle_without_panic() {
        // Buildings have no despawn/occupancy-clear path in this milestone
        // (confirmed: nothing outside `World::spawn_building` ever calls
        // `Passability::set_occupied`, and there is no `Command` or public
        // `World` method that removes a building) — reported to ra-coder as
        // a structural gap. This test simulates "destroyed" the only way
        // available: removing the arena entry directly, which is exactly
        // what `house_has_refinery`/`nearest_refinery` check (they iterate
        // `world.buildings`, not the occupancy grid), so it is a faithful
        // probe of the FSM's reaction even though no in-game action can
        // trigger it yet.
        let mut w = econ_world(1000);
        let refinery = w.spawn_building(B_PROC, 1, CellCoord::new(60, 60)).unwrap();
        let h = w.spawn_unit(1, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
        w.set_unit_harvester(h, true);
        w.units.get_mut(h).unwrap().harvest.cargo = w.catalog.econ.bail_count; // already "full"

        w.tick(&[]); // Looking(full) -> FindHome
        w.tick(&[]); // FindHome -> HeadingHome, path assigned
        w.tick(&[]); // genuinely en route
        assert_eq!(
            w.units.get(h).unwrap().harvest.status,
            HarvStatus::HeadingHome
        );
        assert!(
            w.units.get(h).unwrap().is_moving(),
            "test setup: should be genuinely en route before the refinery vanishes"
        );

        w.buildings.remove(refinery);

        for _ in 0..30 {
            w.tick(&[]);
            assert_eq!(
                w.units.get(h).unwrap().harvest.status,
                HarvStatus::Idle,
                "with the refinery gone mid-route, the FSM must guard-idle, not panic or chase a stale home"
            );
        }
    }

    #[test]
    fn refinery_removed_while_unloading_drops_the_pending_credit_and_goes_idle() {
        // Same caveat as the en-route test above about how "destroyed" is
        // simulated here. This one surfaces a real, documentable behavior:
        // the no-refinery guard clause runs *before* the state match every
        // tick, so it preempts `Unloading`'s own payout even one tick before
        // it would have fired — the cargo's credit value is silently lost,
        // and the harvester is left idle holding cargo it can never bank
        // (there is no other refinery to re-home to). Worth a design call
        // from ra-coder: original RA's harvester behavior on a
        // destroyed-while-docked refinery is a `QUIRKS.md` candidate either
        // way (DESIGN.md §5).
        let mut w = econ_world(1000);
        let refinery = w.spawn_building(B_PROC, 1, CellCoord::new(60, 60)).unwrap();
        let dock = CellCoord::new(61, 63); // refinery's preferred south dock (centre x, tl.y+h)
        let h = w.spawn_unit(1, 1, dock, Facing(0), 400, stats());
        w.set_unit_harvester(h, true);
        {
            let u = w.units.get_mut(h).unwrap();
            u.harvest.cargo = 10;
            u.harvest.gold = 10;
            u.harvest.status = HarvStatus::Unloading;
            u.harvest.timer = 5; // mid-countdown: hasn't paid out yet
            u.harvest.home = Some(refinery);
        }
        let credits_before = w.house_credits(1);

        w.buildings.remove(refinery);
        w.tick(&[]);

        assert_eq!(
            w.units.get(h).unwrap().harvest.status,
            HarvStatus::Idle,
            "the no-refinery guard fires before the Unloading arm even runs"
        );
        assert_eq!(
            w.house_credits(1),
            credits_before,
            "documented edge case: cargo pending payout at the instant of destruction is lost, \
             not credited"
        );
        assert_eq!(
            w.units.get(h).unwrap().harvest.cargo,
            10,
            "cargo is not cleared either -- the unit sits idle holding it forever (no other refinery)"
        );
    }

    #[test]
    fn two_harvesters_share_one_refinery_without_deadlock() {
        let mut w = econ_world(1000);
        w.spawn_building(B_PROC, 1, CellCoord::new(40, 40)).unwrap();
        // A generous ore patch so both harvesters always have somewhere to mine.
        let total = 128 * 128;
        let mut ov = vec![0xFFu8; total];
        for y in 44..50 {
            for x in 44..50 {
                ov[y * 128 + x] = OVERLAY_GOLD_FIRST;
            }
        }
        w.set_ore(OreField::from_overlay(128, 128, &ov));

        let h1 = w.spawn_unit(1, 1, CellCoord::new(36, 44), Facing(0), 400, stats());
        w.set_unit_harvester(h1, true);
        let h2 = w.spawn_unit(1, 1, CellCoord::new(36, 46), Facing(0), 400, stats());
        w.set_unit_harvester(h2, true);

        // A dead-locked pair would show one or both harvesters permanently
        // stuck carrying cargo that never reaches zero again; track each
        // one's cargo independently and count every drop-to-zero (an
        // unload completing) over a generous tick budget.
        let mut unloads = [0u32; 2];
        let mut prev_cargo = [0u16; 2];
        for _ in 0..6000 {
            w.tick(&[]);
            for (i, h) in [h1, h2].iter().enumerate() {
                let cargo = w.units.get(*h).unwrap().harvest.cargo;
                if prev_cargo[i] > 0 && cargo == 0 {
                    unloads[i] += 1;
                }
                prev_cargo[i] = cargo;
            }
        }
        assert!(
            unloads[0] >= 1,
            "harvester 1 should have completed at least one full unload cycle"
        );
        assert!(
            unloads[1] >= 1,
            "harvester 2 should have completed at least one full unload cycle"
        );
    }

    #[test]
    fn harvester_full_with_zero_ore_on_map_still_finds_home_and_unloads() {
        let mut w = econ_world(1000); // default OreField is empty -- no ore anywhere
        w.spawn_building(B_PROC, 1, CellCoord::new(50, 50)).unwrap();
        let h = w.spawn_unit(1, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
        w.set_unit_harvester(h, true);
        {
            let u = w.units.get_mut(h).unwrap();
            u.harvest.cargo = w.catalog.econ.bail_count;
            u.harvest.gold = w.catalog.econ.bail_count;
        }
        let mut banked = false;
        for _ in 0..3000 {
            w.tick(&[]);
            if w.house_credits(1) > 1000 {
                banked = true;
                break;
            }
        }
        assert!(
            banked,
            "a full harvester with no ore left anywhere must still route home and unload -- \
             `Looking`'s cargo>=capacity check must win over its ore-scan, not the other way round"
        );
    }

    // === Production edges ==============================================

    #[test]
    fn insufficient_funds_stalls_progress_and_credits_never_go_negative() {
        let mut w = econ_world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(30, 30)).unwrap();
        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_POWR),
        }]);
        for _ in 0..3 {
            w.tick(&[]);
        }
        let progress_before_broke = w.house(1).unwrap().building_prod.unwrap().progress;
        assert!(
            progress_before_broke > 0,
            "test setup: expected some progress first"
        );

        w.set_house_credits(1, 0);
        // Run broke for a good while, recording progress each tick.
        let mut last = progress_before_broke;
        for _ in 0..80 {
            w.tick(&[]);
            assert!(
                w.house_credits(1) >= 0,
                "credits must never go negative under a stalled installment"
            );
            last = w.house(1).unwrap().building_prod.unwrap().progress;
        }
        // Production must NOT complete while broke, and must have plateaued (a
        // stall). Note: with the M7.9 P0 STEP_COUNT conversion POWR is a 54-step
        // build costing only 30 credits, so the earliest installments round to
        // **0** (`30·(p+1)/54 == 0` for the first steps) and those free steps DO
        // advance at zero credits — exactly as the original proceeds when
        // `Cost_Per_Tick()` rounds to 0 (factory.cpp:207). Once the running
        // installment reaches 1 credit the lane truly stalls. So we assert the
        // real invariants: (a) it never finished, (b) it froze (last two ticks
        // equal), (c) credits stayed non-negative.
        assert!(
            w.house(1).unwrap().ready_building.is_none(),
            "production must not complete while credits are exhausted"
        );
        let progress_after_broke = w.house(1).unwrap().building_prod.unwrap().progress;
        assert_eq!(
            progress_after_broke, last,
            "production must have stalled (progress frozen) while broke"
        );
        assert!(
            progress_after_broke < w.house(1).unwrap().building_prod.unwrap().total_ticks,
            "a stalled build must sit below completion"
        );

        // Refund the treasury: production must resume and finish.
        w.set_house_credits(1, 1000);
        let mut ready = false;
        for _ in 0..500 {
            w.tick(&[]);
            if w.house(1).unwrap().ready_building == Some(B_POWR) {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "production should resume and finish once funded again"
        );
    }

    #[test]
    fn cancel_mid_build_refunds_exactly_what_was_spent() {
        let mut w = econ_world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(30, 30)).unwrap();
        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_POWR),
        }]);
        for _ in 0..5 {
            w.tick(&[]);
        }
        let spent = w.house(1).unwrap().building_prod.unwrap().spent;
        assert!(
            spent > 0,
            "test setup: expected some spend before cancelling"
        );
        let credits_before_cancel = w.house_credits(1);

        w.tick(&[Command::CancelProduction {
            house: 1,
            kind: ProdKind::Building,
        }]);

        assert!(w.house(1).unwrap().building_prod.is_none());
        assert_eq!(
            w.house_credits(1),
            credits_before_cancel + spent,
            "cancel must refund exactly the installments spent so far -- no more, no less"
        );
    }

    #[test]
    fn double_start_same_lane_is_rejected() {
        // Two identical houses: one gets a hijack attempt on its busy lane
        // every tick, the other never does. If the hijack were accepted (or
        // silently spent anything extra), the two would diverge.
        let mut w = econ_world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(30, 30)).unwrap();
        w.spawn_building(B_FACT, 2, CellCoord::new(60, 60)).unwrap();
        w.tick(&[
            Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_POWR),
            },
            Command::StartProduction {
                house: 2,
                item: BuildItem::Building(B_POWR),
            },
        ]);
        assert_eq!(
            w.house(1).unwrap().building_prod.unwrap().item,
            BuildItem::Building(B_POWR)
        );

        // POWR (cost 30) takes ~27 ticks to finish; stay well short of that so
        // the lane is still busy (not yet moved to `ready_building`) for
        // every iteration below.
        for _ in 0..20 {
            w.tick(&[Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_FACT), // hijack attempt, every tick
            }]);
            // House 2 is the control: same production, no hijack attempts.
            let h1 = w.house(1).unwrap().building_prod.unwrap();
            assert_eq!(
                h1.item,
                BuildItem::Building(B_POWR),
                "a busy lane must reject a second StartProduction outright"
            );
        }
        let h1_final = w.house(1).unwrap();
        let h2_final = w.house(2).unwrap();
        assert_eq!(
            (h1_final.credits, h1_final.building_prod.map(|p| p.spent)),
            (h2_final.credits, h2_final.building_prod.map(|p| p.spent)),
            "the hijacked house must track the control house exactly -- \
             repeated rejected StartProductions must never spend anything extra"
        );
    }

    #[test]
    fn war_factory_exit_fully_blocked_completes_but_never_spawns_without_panic() {
        // A 3x3 grid exactly matching WEAP's 3x3 footprint: every ring cell
        // `find_factory_exit` scans (the 1-cell border around the footprint)
        // is off-grid, so no exit ever exists -- permanently. (A genuine
        // dynamic block-then-unblock isn't constructible with today's API:
        // see the "no despawn/occupancy-clear path" note on the refinery
        // removal tests above; `war_factory_exit_partially_blocked_...`
        // below covers the "still finds a free cell" half instead.)
        let grid = Passability::new(3, 3, vec![true; 9]);
        let mut w = econ_world_on(grid, 1000);
        let weap = w.spawn_building(B_WEAP, 1, CellCoord::new(0, 0)).unwrap();
        assert!(w.buildings.get(weap).unwrap().is_war_factory);

        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Unit(U_TANK),
        }]);
        assert!(w.house(1).unwrap().unit_prod.is_some());

        for _ in 0..3000 {
            w.tick(&[]);
        }
        let prod = w.house(1).unwrap().unit_prod;
        assert!(
            prod.is_some(),
            "the lane should still hold the completed-but-stuck production, not vanish"
        );
        assert!(
            prod.unwrap().done,
            "production should have finished paying for itself"
        );
        assert_eq!(
            prod.unwrap().spent,
            prod.unwrap().cost,
            "cost is fully paid even though the unit never spawns"
        );
        assert_eq!(
            w.units.len(),
            0,
            "a permanently walled-in exit must never spawn the unit"
        );
    }

    #[test]
    fn war_factory_exit_partially_blocked_still_finds_the_free_ring_cell() {
        let mut w = econ_world(1000);
        let weap = w.spawn_building(B_WEAP, 1, CellCoord::new(50, 50)).unwrap();
        let (tl, fw, fh) = {
            let b = w.buildings.get(weap).unwrap();
            (b.cell, b.foot_w as i32, b.foot_h as i32)
        };
        // Block every ring cell except one corner, (tl.x-1, tl.y-1), with 1x1
        // filler buildings placed directly (bypassing production).
        let free = CellCoord::new(tl.x - 1, tl.y - 1);
        for x in (tl.x - 1)..=(tl.x + fw) {
            for y in (tl.y - 1)..=(tl.y + fh) {
                let c = CellCoord::new(x, y);
                let on_ring = x == tl.x - 1 || x == tl.x + fw || y == tl.y - 1 || y == tl.y + fh;
                if on_ring && c != free {
                    w.spawn_building(B_PAD, 1, c).unwrap();
                }
            }
        }

        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Unit(U_TANK),
        }]);
        let mut spawned = false;
        for _ in 0..3000 {
            w.tick(&[]);
            if !w.units.is_empty() {
                spawned = true;
                break;
            }
        }
        assert!(
            spawned,
            "the one remaining free ring cell should still be found and used"
        );
        let (_, u) = w.units.iter().next().unwrap();
        assert_eq!(
            u.cell(),
            free,
            "should exit at the sole unblocked ring cell"
        );
    }

    #[test]
    fn place_building_validation_rejects_bad_spots_and_accepts_valid_ones() {
        let mut w = econ_world(1000);
        let mcv = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
        w.tick(&[Command::Deploy {
            unit: mcv,
            house: 1,
        }]);
        assert_eq!(w.buildings.len(), 1); // FACT at (19,19)-(21,21)

        // Fabricate a ready POWR directly, isolating placement validation
        // from the production system (already covered elsewhere).
        w.houses[1].ready_building = Some(B_POWR);
        let before = w.house_credits(1);

        // Off-map: negative, and past the grid's far edge.
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(-1, -1),
        }]);
        assert_eq!(
            w.house(1).unwrap().ready_building,
            Some(B_POWR),
            "negative-coordinate placement must be rejected"
        );
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(MAP_CELL_W, MAP_CELL_H),
        }]);
        assert_eq!(
            w.house(1).unwrap().ready_building,
            Some(B_POWR),
            "past-the-far-edge placement must be rejected"
        );
        assert_eq!(w.buildings.len(), 1);

        // On occupied ground: right on top of the construction yard.
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(19, 19),
        }]);
        assert_eq!(
            w.house(1).unwrap().ready_building,
            Some(B_POWR),
            "overlapping an existing footprint must be rejected"
        );
        assert_eq!(w.buildings.len(), 1);

        // Non-adjacent: clear, on-map ground far from any owned building.
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(90, 90),
        }]);
        assert_eq!(
            w.house(1).unwrap().ready_building,
            Some(B_POWR),
            "non-adjacent placement must be rejected by the proximity rule"
        );
        assert_eq!(w.buildings.len(), 1);

        // Valid: adjacent to the yard, on clear ground.
        w.tick(&[Command::PlaceBuilding {
            house: 1,
            building: B_POWR,
            cell: CellCoord::new(22, 19),
        }]);
        assert_eq!(
            w.house(1).unwrap().ready_building,
            None,
            "a valid adjacent placement must be accepted"
        );
        assert_eq!(w.buildings.len(), 2);
        assert_eq!(
            w.house_credits(1),
            before,
            "placement itself charges nothing extra (cost was already paid in installments)"
        );
    }

    // === Full-loop economy determinism (item 3) ========================

    /// Deploy an MCV, build+place POWR then PROC (spawns a free harvester
    /// that mines and banks at least once), build+place WEAP, then produce a
    /// TANK -- recording every tick's hash plus a sparse `(tick, commands)`
    /// log of only the ticks that actually issued a command. All decisions
    /// (when to place, when to start the next item) are made from
    /// deterministic world state alone, so this is safe to call repeatedly
    /// and expect byte-identical results.
    fn run_full_econ_script(mut w: World) -> (Vec<u64>, Vec<(u32, Vec<Command>)>) {
        let mcv = w.spawn_unit(0, 1, CellCoord::new(30, 30), Facing(0), 400, stats());
        let mut hashes = Vec::new();
        let mut log: Vec<(u32, Vec<Command>)> = Vec::new();
        let mut step = |w: &mut World, cmds: Vec<Command>| {
            let t = w.tick_count();
            let h = w.tick(&cmds);
            hashes.push(h);
            if !cmds.is_empty() {
                log.push((t, cmds));
            }
        };

        step(
            &mut w,
            vec![Command::Deploy {
                unit: mcv,
                house: 1,
            }],
        );
        step(
            &mut w,
            vec![Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_POWR),
            }],
        );
        for _ in 0..300 {
            if w.house(1).unwrap().ready_building == Some(B_POWR) {
                step(
                    &mut w,
                    vec![Command::PlaceBuilding {
                        house: 1,
                        building: B_POWR,
                        cell: CellCoord::new(32, 29),
                    }],
                );
                break;
            }
            step(&mut w, vec![]);
        }
        assert!(
            w.house(1).unwrap().owns_building(B_POWR),
            "setup: POWR should be placed"
        );

        step(
            &mut w,
            vec![Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_PROC),
            }],
        );
        for _ in 0..300 {
            if w.house(1).unwrap().ready_building == Some(B_PROC) {
                step(
                    &mut w,
                    vec![Command::PlaceBuilding {
                        house: 1,
                        building: B_PROC,
                        cell: CellCoord::new(29, 32),
                    }],
                );
                break;
            }
            step(&mut w, vec![]);
        }
        assert!(
            w.house(1).unwrap().owns_building(B_PROC),
            "setup: PROC should be placed"
        );

        let credits_before_harvest = w.house_credits(1);
        for _ in 0..3000 {
            if w.house_credits(1) > credits_before_harvest {
                break;
            }
            step(&mut w, vec![]);
        }
        assert!(
            w.house_credits(1) > credits_before_harvest,
            "setup: the free harvester should have banked something"
        );

        step(
            &mut w,
            vec![Command::StartProduction {
                house: 1,
                item: BuildItem::Building(B_WEAP),
            }],
        );
        for _ in 0..300 {
            if w.house(1).unwrap().ready_building == Some(B_WEAP) {
                step(
                    &mut w,
                    vec![Command::PlaceBuilding {
                        house: 1,
                        building: B_WEAP,
                        cell: CellCoord::new(34, 29),
                    }],
                );
                break;
            }
            step(&mut w, vec![]);
        }
        assert!(
            w.house(1).unwrap().owns_building(B_WEAP),
            "setup: WEAP should be placed"
        );

        step(
            &mut w,
            vec![Command::StartProduction {
                house: 1,
                item: BuildItem::Unit(U_TANK),
            }],
        );
        let units_before = w.units.len();
        for _ in 0..300 {
            if w.units.len() > units_before {
                break;
            }
            step(&mut w, vec![]);
        }
        assert!(
            w.units.len() > units_before,
            "setup: TANK should have spawned"
        );

        // Economy-only, no combat: the sim RNG must never be consumed. This
        // guards the invariant M6's ore-growth work (which the ore module's
        // own docs flag as RNG-consuming) will change on purpose.
        assert_eq!(
            w.rng_seed(),
            0x51ee_d123,
            "an economy-only script must never draw the sim RNG"
        );

        (hashes, log)
    }

    fn econ_script_world() -> World {
        let mut w = econ_world(2000);
        let total = 128 * 128;
        let mut ov = vec![0xFFu8; total];
        for y in 34..38 {
            for x in 34..38 {
                ov[y * 128 + x] = OVERLAY_GOLD_FIRST;
            }
        }
        w.set_ore(OreField::from_overlay(128, 128, &ov));
        w
    }

    #[test]
    fn full_economy_loop_same_seed_twice_gives_identical_hash_chains() {
        let (hashes_a, log_a) = run_full_econ_script(econ_script_world());
        let (hashes_b, log_b) = run_full_econ_script(econ_script_world());
        assert_eq!(
            hashes_a, hashes_b,
            "two independent runs of the identical full economy script must match tick-for-tick"
        );
        assert_eq!(log_a, log_b, "the same decisions must be made both times");
    }

    #[test]
    fn full_economy_loop_command_log_replay_matches_live_run() {
        let (live_hashes, log) = run_full_econ_script(econ_script_world());

        // Replay: feed exactly the recorded (tick, commands) pairs -- no
        // re-deciding anything from live state -- to a fresh world and
        // confirm the hash chain matches tick-for-tick.
        let mut replay = econ_script_world();
        let _mcv = replay.spawn_unit(0, 1, CellCoord::new(30, 30), Facing(0), 400, stats());
        let mut replay_hashes = Vec::with_capacity(live_hashes.len());
        let mut li = 0usize;
        for t in 0..live_hashes.len() as u32 {
            let cmds: Vec<Command> = if li < log.len() && log[li].0 == t {
                let c = log[li].1.clone();
                li += 1;
                c
            } else {
                Vec::new()
            };
            replay_hashes.push(replay.tick(&cmds));
        }
        assert_eq!(
            li,
            log.len(),
            "replay should have consumed the whole recorded log"
        );
        assert_eq!(
            live_hashes, replay_hashes,
            "command-log replay must reproduce the live run's hash chain exactly"
        );
    }

    // -----------------------------------------------------------------
    // M6 coder smoke tests: building destruction, sell, win/lose, ore growth.
    // (Minimal "does it run + stays deterministic" checks; full fidelity
    // coverage is ra-tester's domain.)
    // -----------------------------------------------------------------

    /// A generous hitscan weapon that damages any armor (for fast building kills).
    fn any_weapon() -> crate::combat::WeaponProfile {
        crate::combat::WeaponProfile {
            damage: 30,
            rof: 10,
            range: 2000,
            proj_speed: 255,
            proj_rot: 0,
            invisible: true,
            instant: true,
            warhead: crate::combat::WarheadProfile {
                spread: 3,
                verses: [65536; 5],
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    #[test]
    fn building_destroyed_clears_occupancy_power_and_count() {
        let mut w = econ_world(1000);
        let bh = w.spawn_building(B_POWR, 1, CellCoord::new(40, 40)).unwrap();
        assert_eq!(w.house(1).unwrap().power_output, 100);
        assert!(!w.passability().is_passable(CellCoord::new(40, 40)));
        // Weaken it so a couple of shots finish it.
        w.buildings.get_mut(bh).unwrap().health = 20;

        let atk = w.spawn_unit(0, 2, CellCoord::new(43, 40), Facing(0), 400, stats());
        w.set_unit_combat(atk, 0, Some(any_weapon()), true);
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Building(bh),
            house: 2,
        }]);
        for _ in 0..400 {
            if !w.buildings.contains(bh) {
                break;
            }
            w.tick(&[]);
        }
        assert!(!w.buildings.contains(bh), "building was not destroyed");
        assert!(
            w.passability().is_passable(CellCoord::new(40, 40)),
            "destroyed building did not free its footprint occupancy"
        );
        assert_eq!(
            w.house(1).unwrap().power_output,
            0,
            "destroyed building's power was not reversed"
        );
        assert!(
            !w.house(1).unwrap().owns_building(B_POWR),
            "destroyed building's type count was not decremented"
        );
    }

    #[test]
    fn sell_refunds_fraction_and_frees_footprint() {
        let mut w = econ_world(1000);
        let bh = w.spawn_building(B_POWR, 1, CellCoord::new(40, 40)).unwrap();
        let before = w.house_credits(1);
        w.tick(&[Command::Sell {
            house: 1,
            building: bh,
        }]);
        assert!(!w.buildings.contains(bh), "sold building not removed");
        assert!(
            w.passability().is_passable(CellCoord::new(40, 40)),
            "sold building did not free its footprint"
        );
        // POWR cost 30 × RefundPercent 50% = 15 credits refunded.
        assert_eq!(w.house_credits(1), before + 15);
        assert_eq!(w.house(1).unwrap().power_output, 0);

        // Selling a building you do not own is rejected.
        let bh2 = w.spawn_building(B_POWR, 1, CellCoord::new(50, 50)).unwrap();
        w.tick(&[Command::Sell {
            house: 2,
            building: bh2,
        }]);
        assert!(
            w.buildings.contains(bh2),
            "a non-owner must not be able to sell a building"
        );
    }

    #[test]
    fn house_elimination_yields_victory_and_defeat() {
        use crate::ai::{AiPlayer, Difficulty};

        // Victory: the player (house 1) is alive; the sole AI (house 2) owns
        // nothing (no buildings, no units) → eliminated.
        let mut w = econ_world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(40, 40)).unwrap();
        w.set_player_house(1);
        w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
        assert_eq!(w.game_over(), GameOver::Ongoing);
        w.tick(&[]);
        assert_eq!(
            w.game_over(),
            GameOver::Victory,
            "all AI houses eliminated should be a player Victory"
        );

        // Defeat: the player (house 1) owns nothing; the AI (house 2) is alive.
        let mut w = econ_world(1000);
        w.spawn_building(B_FACT, 2, CellCoord::new(40, 40)).unwrap();
        w.set_player_house(1);
        w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
        w.tick(&[]);
        assert_eq!(
            w.game_over(),
            GameOver::Defeat,
            "the player owning no buildings and no units should be a Defeat"
        );
    }

    #[test]
    fn ore_growth_draws_sim_rng_and_replays_identically() {
        let make = || -> World {
            let mut w = econ_world(1000);
            let total = 128 * 128;
            let mut ov = vec![0xFFu8; total];
            for y in 30..40 {
                for x in 30..40 {
                    ov[y * 128 + x] = OVERLAY_GOLD_FIRST;
                }
            }
            w.set_ore(OreField::from_overlay(128, 128, &ov));
            w.set_ore_growth(true, true);
            w
        };
        let mut a = make();
        let seed0 = a.rng_seed();
        let bails0 = a.ore.total_bails();
        // ~3 full map sweeps at the default GrowthRate=2 (subcount≈9 cells/tick).
        for _ in 0..2500 {
            a.tick(&[]);
        }
        assert_ne!(
            a.rng_seed(),
            seed0,
            "ore growth/spread must consume the sync RNG (the updated M5 pin)"
        );
        assert_ne!(
            a.ore.total_bails(),
            bails0,
            "ore should have grown and/or spread"
        );

        // Same seed twice → identical outcome (determinism preserved).
        let mut b = make();
        for _ in 0..2500 {
            b.tick(&[]);
        }
        assert_eq!(a.state_hash(), b.state_hash());
        assert_eq!(a.rng_seed(), b.rng_seed());
    }
}
