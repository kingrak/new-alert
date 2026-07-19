//! `World` — the single explicit game-state value (DESIGN.md §3.2, §4.2) and
//! the command pipeline that is the *only* way to mutate it (§4.4).
//!
//! Each tick runs a fixed, explicit sequence of systems — **commands, then
//! movement** — over arenas iterated in slot order, with one seeded RNG owned
//! here. At the end of a tick the whole mutable state is folded into a 64-bit
//! FNV-1a hash (§4.2): the hash chain is the determinism backbone, asserted in
//! replays and multiplayer alike.

use crate::ai::AiPlayer;
use crate::arena::{Arena, Handle};
use crate::building::Building;
use crate::bullet::Bullet;
use crate::catalog::Catalog;
use crate::combat::{aligned_to_fire, modify_damage, Target};
use crate::coords::{
    coord_move, isqrt, leptons_distance, spot_index, CellCoord, Facing, Locomotor, WorldCoord,
    SUBCELL_COUNT,
};
use crate::hash::Fnv1a;
use crate::house::{BuildItem, House, ProdKind, Production};
use crate::occupancy::UnitGrid;
use crate::ore::OreField;
use crate::path::{find_path, find_path_avoiding, Passability};
use crate::rng::RandomLcg;
use crate::shroud::Shroud;
use crate::unit::{HarvStatus, MoveStats, Unit};

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
        }
    }

    /// Install the build catalog (footprints, costs, prerequisites, protos).
    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
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
    pub fn set_ai(&mut self, ai: Vec<AiPlayer>) {
        self.ai = ai;
    }

    /// Designate the tracked player house for win/lose resolution (skirmish).
    pub fn set_player_house(&mut self, house: u8) {
        self.player_house = Some(house);
    }

    /// The current terminal game state (`Ongoing` until a house is eliminated).
    pub fn game_over(&self) -> GameOver {
        self.game_over
    }

    /// Whether `house` is still alive — it owns at least one live building **or**
    /// one live unit. Elimination is "all buildings AND all units destroyed"
    /// (the classic MP defeat check, `house.cpp:1290`).
    pub fn house_alive(&self, house: u8) -> bool {
        self.buildings
            .iter()
            .any(|(_, b)| b.house == house && b.is_alive())
            || self.units.iter().any(|(_, u)| u.house == house)
    }

    /// Read a house's credits (0 if the house index is out of range).
    pub fn house_credits(&self, house: u8) -> i32 {
        self.houses
            .get(house as usize)
            .map(|h| h.credits)
            .unwrap_or(0)
    }

    /// Set a house's credits (no-op if out of range). For the loader / tests.
    pub fn set_house_credits(&mut self, house: u8, credits: i32) {
        if let Some(h) = self.houses.get_mut(house as usize) {
            h.credits = credits;
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

    // System 4: combat (targeting + rotation + firing).
    run_combat(world);

    // System 5: movement.
    move_units(world);

    // System 6: bullets (flight + detonation + damage + death).
    run_bullets(world);

    // System 7: ore growth/spread (draws sync RNG when enabled) — deferred M5 item.
    run_ore_growth(world);

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
    // Victory once every AI-controlled house has been eliminated.
    if !world.ai.is_empty() && world.ai.iter().all(|a| !world.house_alive(a.house)) {
        world.game_over = GameOver::Victory;
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
            let (start, loco, is_inf) = match world.units.get(unit) {
                Some(u) if u.house == house => (u.cell(), u.locomotor, u.is_infantry()),
                _ => return,
            };
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
                }
            }
        }
        Command::Attack {
            unit,
            target,
            house,
        } => {
            // Reject the order up front for unowned/stale/unarmed units, and for
            // targeting oneself. Otherwise store the TarCom; `run_combat` drives
            // the approach/aim/fire each tick.
            let ok = match world.units.get(unit) {
                Some(u) => u.house == house && u.weapon.is_some(),
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
            if let Some(u) = world.units.get_mut(unit) {
                u.target = Some(target);
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
    let (cost, prereq, need_yard, need_factory, need_barracks, kind) = match item {
        BuildItem::Building(id) => match world.catalog.building(id) {
            Some(p) => (
                p.cost,
                p.prereq.clone(),
                true,
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
                ProdKind::Infantry,
            ),
            Some(p) => (p.cost, p.prereq.clone(), false, true, false, ProdKind::Unit),
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
    if (need_yard && !has_yard)
        || (need_factory && !has_factory)
        || (need_barracks && !has_barracks)
    {
        return;
    }
    // Must be able to afford at least the first installment (any credits).
    if world.house_credits(house) <= 0 {
        return;
    }

    // Build time = base (Cost·TICKS_PER_MINUTE/1000) × the discrete low-power
    // multiplier, **snapshotted here at production START** (techno.cpp:6819 +
    // factory.cpp:432 — the original bakes it into the factory Rate once and
    // never recomputes it while the build runs). This replaces M5's per-tick
    // continuous throttle (which slowed at ≤½ power); the multiplier is now the
    // original's exact ×4/×2.5/×1.5 snapshot.
    let base_ticks = world.catalog.time_to_build(cost);
    let (scale_n, scale_d) = world
        .houses
        .get(house as usize)
        .map(|h| h.build_time_scale())
        .unwrap_or((1, 1));
    let total_ticks = ((base_ticks as i64 * scale_n as i64 / scale_d as i64).max(1)) as i32;
    let prod = Production {
        item,
        cost,
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
    if !passes_proximity(world, house, building, cell) {
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
}

/// Whether `house` still owns a live barracks (infantry factory).
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
    world.set_unit_harvester(handle, proto.is_harvester);
    if let Some(u) = world.units.get_mut(handle) {
        u.set_locomotor(loco_from_index(proto.locomotor));
    }
    world.set_unit_sight(handle, proto.sight);
}

/// Combat system: for each unit (in slot order) decrement its rearm timer,
/// rotate its turret/body toward its target, approach if out of range, and fire
/// when aimed and rearmed. Ported from `UnitClass::Rotation_AI` +
/// `Firing_AI` + `Can_Fire` (`unit.cpp`). Iterating in slot order keeps the
/// sim-RNG draw sequence (bullet scatter) deterministic.
fn run_combat(world: &mut World) {
    for handle in world.units.handles() {
        // Decrement the rearm countdown regardless of whether we fire.
        if let Some(u) = world.units.get_mut(handle) {
            if u.arm > 0 {
                u.arm -= 1;
            }
        }

        // Snapshot what we need without holding a borrow across the RNG draw.
        let (weapon, coord, turret, body, has_turret, rot, target) = match world.units.get(handle) {
            Some(u) => match (u.target, u.weapon) {
                (Some(t), Some(w)) => (
                    w,
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

        // Resolve the target's current aim point; drop stale/dead targets.
        let drop_target = |world: &mut World| {
            if let Some(u) = world.units.get_mut(handle) {
                u.target = None;
            }
        };
        let target_coord = match target {
            Target::Unit(t) => match world.units.get(t) {
                Some(tu) if tu.is_alive() => tu.coord,
                _ => {
                    drop_target(world);
                    continue;
                }
            },
            Target::Building(t) => match world.buildings.get(t) {
                Some(tb) if tb.is_alive() => tb.center_cell().center(),
                _ => {
                    drop_target(world);
                    continue;
                }
            },
            Target::Cell(c) => c.center(),
        };

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
            // Approach: path toward the target. For a *building* the target cell
            // sits inside an impassable footprint, so path to the nearest passable
            // footprint-adjacent cell instead (else `find_path` to the occupied
            // centre returns `None` and the attacker never closes in).
            let goal = match target {
                Target::Building(t) => world
                    .buildings
                    .get(t)
                    .and_then(|b| nearest_adjacent_passable(&world.passable, b, coord.cell()))
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
            fire(world, handle, coord, aim, target, target_coord, &weapon);
            if let Some(u) = world.units.get_mut(handle) {
                u.arm = weapon.rof;
            }
        }
    }
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
    muzzle: WorldCoord,
    aim: Facing,
    target: Target,
    target_coord: WorldCoord,
    weapon: &crate::combat::WeaponProfile,
) {
    let source_house = world.units.get(shooter).map(|u| u.house).unwrap_or(0);

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

    let bullet = Bullet {
        pos: if weapon.instant { impact } else { muzzle },
        impact,
        target,
        speed: weapon.proj_speed,
        facing: dir,
        damage: weapon.damage,
        warhead: weapon.warhead,
        min_damage: weapon.min_damage,
        max_damage: weapon.max_damage,
        source_house,
        source_unit: shooter,
        instant: weapon.instant,
        invisible: weapon.invisible,
    };
    world.bullets.insert(bullet);
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

    // --- Units in the 3×3 neighbourhood ---
    for h in world.units.handles() {
        if h == source {
            continue;
        }
        let (coord, armor) = match world.units.get(h) {
            Some(u) if u.is_alive() => (u.coord, u.armor),
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
        let dmg = modify_damage(damage, warhead, armor, distance, min_damage, max_damage);
        if dmg <= 0 {
            continue;
        }
        if let Some(u) = world.units.get_mut(h) {
            u.health = u.health.saturating_sub(dmg as u16);
            if u.health == 0 {
                if !dead_units.contains(&h) {
                    dead_units.push(h);
                }
            } else if source_house.is_some_and(|sh| sh != u.house) {
                // Auto-retaliation (guard-mission return fire, item 2):
                // `FootClass::Take_Damage` assigns the attacker as TarCom when
                // the unit survives, is allowed to retaliate, and is idle.
                assign_retaliation(u, source);
            }
        }
    }

    // --- Buildings covering the 3×3 neighbourhood ---
    for h in world.buildings.handles() {
        let (covers_impact, near, center, armor) = match world.buildings.get(h) {
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
        if dmg <= 0 {
            continue;
        }
        if let Some(b) = world.buildings.get_mut(h) {
            b.health = b.health.saturating_sub(dmg as u16);
            if b.health == 0 && !dead_buildings.contains(&h) {
                dead_buildings.push(h);
            }
        }
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
        h.credits -= installment;
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
        .any(|(h, u)| !u.is_infantry() && u.cell() == cell && Some(h) != except)
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
    world
        .units
        .iter()
        .any(|(h, u)| !u.is_infantry() && Some(h) != except && u.dest == Some(cell))
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
        _ => Locomotor::Track,
    }
}

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
                if let Some(hh) = world.houses.get_mut(house as usize) {
                    hh.credits += credits;
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
            Some(u) if !u.path.is_empty() => (u.cell(), u.is_infantry(), u.sub_cell, u.locomotor),
            _ => continue,
        };

        // Rotate toward the next waypoint before translating.
        if let Some(u) = world.units.get_mut(handle) {
            let target = u.path[0].center();
            if let Some(desired) = Facing::toward(u.coord, target) {
                u.facing = u.facing.rotate_toward(desired, u.stats.rot.wrapping_add(1));
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
                .map(|u| u.stats.max_speed)
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
                continue; // hold position this tick
            }
        }

        let Some(unit) = world.units.get_mut(handle) else {
            continue;
        };
        // Consume the waypoints the (final) advance fully reached.
        let (applied_coord, consumed) =
            advance_along_path(unit.coord, &unit.path, unit.stats.max_speed);
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
        if u.is_infantry() {
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
        let tgt = w.spawn_unit(0, 2, CellCoord::new(6, 5), Facing(0), 100, stats());
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
                has_turret: false,
                is_harvester: harv,
                deploys_to: deploys,
                cost,
                prereq,
                sight: 2,
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
        // Zero power => ×4 build-time multiplier (techno.cpp:6819), snapshotted at
        // start; allow a small slack for the ±1-tick loop/rounding boundaries.
        assert!(
            (full * 4 - 4..=full * 4 + 4).contains(&low),
            "zero-power build should take ~4× as long (full={full}, low={low})"
        );
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
        for _ in 0..20 {
            w.tick(&[]);
            assert!(
                w.house_credits(1) >= 0,
                "credits must never go negative under a stalled installment"
            );
        }
        let progress_after_broke = w.house(1).unwrap().building_prod.unwrap().progress;
        assert_eq!(
            progress_after_broke, progress_before_broke,
            "production must stall (zero further progress) while credits are exhausted"
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
