//! Deterministic skirmish AI (DESIGN.md §3.10, §4.9 M6, item 3).
//!
//! The AI runs *inside* the sim and issues the same [`Command`]s a player would
//! (§3.10: "the AI issues the same Commands a player would … so replays show AI
//! decisions"). It is a deliberately simplified but faithful port of the two
//! systems that do the real work in the shipped RA1 house AI:
//!
//! - **`HouseClass::AI_Building`** (`house.cpp:5696`) — a max-urgency build
//!   selector. The elaborate `Expert_AI` strategy table mostly no-ops in the
//!   shipped game (`AI_Build_Power/Defense/Offense/Income` all `return false`),
//!   so all real base building happens here. We reproduce its effective
//!   precedence: keep power non-negative → guarantee a refinery+harvester →
//!   war factory → (defences/tech, which the starter catalog does not model).
//! - **Attack waves** — `HouseClass::AI` spawns teams on the `AlertTime` timer
//!   (`house.cpp:1042`) that head for the designated `Enemy` (chosen by the
//!   distance-dominant scoring at `house.cpp:4941`). We gather this house's idle
//!   armed units on a timer and send them at the nearest enemy structure.
//!
//! Difficulty scales the attack cadence and wave size (the `easy/normal/hard`
//! rules.ini handicaps). The full FirePower/Armor/BuildTime stat biases
//! (`house.cpp:278` `Assign_Handicap`) are a documented simplification — combat
//! and production run at uniform rates for every house in this milestone.
//!
//! **Sync RNG.** Where the original draws `Scen.RandomNumber` we draw the sim
//! RNG, in a fixed order (`step` is called per AI house in house-index order):
//! the weighted-random vehicle pick (`house.cpp:6186`) and the attack-cadence
//! jitter (`house.cpp:1056`).

use crate::catalog::Catalog;
use crate::coords::CellCoord;
use crate::hash::Fnv1a;
use crate::house::{BuildItem, House};
use crate::rng::RandomLcg;
use crate::world::{Command, World};
use crate::Target;

/// AI difficulty (the three rules.ini `[Easy]`/`[Normal]`/`[Difficult]` sets,
/// `rules.cpp:1026`). Here it tunes attack aggressiveness and wave size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Difficulty {
    /// Slower, smaller attack waves.
    Easy,
    /// Baseline.
    #[default]
    Normal,
    /// Faster, larger attack waves.
    Hard,
}

impl Difficulty {
    /// Base ticks between attack waves (before RNG jitter). Harder = sooner.
    fn attack_interval(self) -> u32 {
        match self {
            Difficulty::Easy => 90 * 15,   // 90 s
            Difficulty::Normal => 60 * 15, // 60 s
            Difficulty::Hard => 35 * 15,   // 35 s
        }
    }

    /// Minimum armed units gathered before an attack wave launches.
    fn min_force(self) -> usize {
        match self {
            Difficulty::Easy => 4,
            Difficulty::Normal => 3,
            Difficulty::Hard => 2,
        }
    }
}

/// One AI-controlled house. Holds only small, hashable decision state; all world
/// facts are read fresh from [`World`] each tick.
#[derive(Clone, Debug)]
pub struct AiPlayer {
    /// The house this controller plays.
    pub house: u8,
    /// Difficulty handicap.
    pub difficulty: Difficulty,
    /// Ticks until the next economy/build decision pass (cadence throttle).
    decide_timer: u32,
    /// Ticks until the next attack wave.
    attack_timer: u32,
    /// Whether the starting MCV has been deployed into a construction yard.
    deployed: bool,
}

/// Economy/build decisions are re-evaluated on this cadence (~1 s), matching the
/// original's `AI_Building` returning `TICKS_PER_SECOND` (`house.cpp` return).
const DECIDE_PERIOD: u32 = 15;

impl AiPlayer {
    /// A fresh controller for `house` at `difficulty`. The first attack wave is
    /// delayed by roughly one interval.
    pub fn new(house: u8, difficulty: Difficulty) -> AiPlayer {
        AiPlayer {
            house,
            difficulty,
            decide_timer: 0,
            attack_timer: difficulty.attack_interval(),
            deployed: false,
        }
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(self.house);
        h.write_u8(self.difficulty as u8);
        h.write_u32(self.decide_timer);
        h.write_u32(self.attack_timer);
        h.write_u8(self.deployed as u8);
    }

    /// One AI tick: read `world`, draw from `rng` where the original draws the
    /// sync RNG, and push the [`Command`]s it wants applied this tick into `out`.
    /// The caller applies them through the normal command pipeline.
    pub fn step(&mut self, world: &World, rng: &mut RandomLcg, out: &mut Vec<Command>) {
        // Dead house: nothing to do.
        if !world.house_alive(self.house) {
            return;
        }

        self.decide_timer = self.decide_timer.saturating_sub(1);
        self.attack_timer = self.attack_timer.saturating_sub(1);

        // 1) Deploy the starting MCV into a construction yard once. Building
        // and unit production genuinely need a live construction yard (their
        // own prereq checks already gate on it via `next_structure`), but a
        // wave attack does not — it only needs idle armed units and a live
        // enemy (`launch_attack`/`base_center` already handle "no buildings
        // of our own" gracefully). So base-less/base-lost houses fall through
        // to the attack check below instead of returning early: an AI whose
        // base has been destroyed should still fight with whatever forces
        // survive it, exactly as a player would, rather than going
        // permanently inert.
        //
        // *** ra-tester fix (M6 AI coverage task), flagged for ra-coder
        // review. *** Found via `ra-client/tests/ui_ai_vs_ai.rs`'s real-map
        // AI-vs-AI suite: on `scm01ea.ini`, house A's base was fully
        // destroyed (construction yard + everything else) while 9 armed
        // units survived; with the old `return` here, `step` exited before
        // ever reaching the attack-timer check again, so those 9 units sat
        // idle forever and the game never resolved even after 40+ simulated
        // minutes. The `AiPlayer::deployed` field's doc comment ("whether the
        // starting MCV has been deployed") and its dead-until-now
        // `self.deployed = true` write suggest this distinction (has ever
        // had a base vs. never had one) was intended but never wired up —
        // this restores it structurally instead, without needing the field.
        if !self.has_construction_yard(world) {
            if let Some(mcv) = self.own_mcv(world) {
                out.push(Command::Deploy {
                    unit: mcv,
                    house: self.house,
                });
            }
        } else {
            self.deployed = true;
            if self.decide_timer == 0 {
                self.decide_timer = DECIDE_PERIOD;
                self.build_base(world, out);
                self.produce_units(world, rng, out);
            }
        }

        if self.attack_timer == 0 {
            // Reset with a jittered interval (house.cpp:1056: AlertTime reset is
            // `interval * Random_Pick(TICKS_PER_MINUTE/2, TICKS_PER_MINUTE*2)`;
            // simplified to a ±50% jitter around the base interval).
            let base = self.difficulty.attack_interval();
            let jitter = rng.range(-(base as i32) / 2, (base as i32) / 2);
            self.attack_timer = (base as i32 + jitter).max(1) as u32;
            self.launch_attack(world, out);
        }
    }

    // ---- Base building (AI_Building, house.cpp:5696) -----------------------

    /// Place a ready building, else start the next-priority structure. Only one
    /// structure lane runs at a time (matching our house model), so this issues
    /// at most one production/placement command per pass.
    fn build_base(&self, world: &World, out: &mut Vec<Command>) {
        let Some(hs) = world.house(self.house) else {
            return;
        };
        let cat = &world.catalog;

        // A completed structure waiting for a spot -> place it near the base.
        if let Some(ready) = hs.ready_building {
            if let Some(cell) = self.placement_cell(world, ready) {
                out.push(Command::PlaceBuilding {
                    house: self.house,
                    building: ready,
                    cell,
                });
            }
            return;
        }

        // A structure already building -> wait for it.
        if hs.building_prod.is_some() {
            return;
        }

        // Choose the next structure by the original's effective precedence.
        if let Some(id) = self.next_structure(world, hs, cat) {
            out.push(Command::StartProduction {
                house: self.house,
                item: BuildItem::Building(id),
            });
        }
    }

    /// The next structure to build, by role, mirroring `AI_Building`'s urgency
    /// ordering for the starter catalog: power when low → refinery when none →
    /// war factory → a second power/refinery to keep growing.
    fn next_structure(&self, world: &World, hs: &House, cat: &Catalog) -> Option<u32> {
        let owns = |id: u32| hs.owns_building(id);
        let can_afford = world.house_credits(self.house) > 0;
        if !can_afford {
            return None;
        }
        let power_id = self.role_building(cat, Role::Power);
        let refinery_id = self.role_building(cat, Role::Refinery);
        let factory_id = self.role_building(cat, Role::WarFactory);

        let has_power_building = power_id.map(owns).unwrap_or(false);
        let has_refinery = refinery_id.map(owns).unwrap_or(false);
        let has_factory = factory_id.map(owns).unwrap_or(false);
        let low_power = hs.low_power();

        // 1) Power: build the first plant, or another when running a deficit.
        if let Some(p) = power_id {
            if (!has_power_building || low_power) && self.buildable(world, hs, p) {
                return Some(p);
            }
        }
        // 2) Refinery (economy) — HIGH urgency when none yet (house.cpp:5765).
        if let Some(r) = refinery_id {
            if !has_refinery && self.buildable(world, hs, r) {
                return Some(r);
            }
        }
        // 3) War factory.
        if let Some(f) = factory_id {
            if !has_factory && self.buildable(world, hs, f) {
                return Some(f);
            }
        }
        // 3b) Barracks (cheap infantry factory) once the war factory is up.
        if has_factory {
            if let Some(bar) = self.role_building(cat, Role::Barracks) {
                if !owns(bar) && self.buildable(world, hs, bar) {
                    return Some(bar);
                }
            }
        }
        // 4) Keep expanding: a second refinery, then a spare power plant.
        if let Some(r) = refinery_id {
            if self.count_owned(world, r) < 2 && self.buildable(world, hs, r) {
                return Some(r);
            }
        }
        if let Some(p) = power_id {
            if self.buildable(world, hs, p) {
                return Some(p);
            }
        }
        None
    }

    // ---- Unit production (AI_Unit skirmish mode, house.cpp:6166) ------------

    fn produce_units(&self, world: &World, rng: &mut RandomLcg, out: &mut Vec<Command>) {
        let Some(hs) = world.house(self.house) else {
            return;
        };
        let cat = &world.catalog;

        // --- Vehicle lane (war factory) ---
        let has_factory = self
            .role_building(cat, Role::WarFactory)
            .map(|f| hs.owns_building(f))
            .unwrap_or(false);
        if hs.unit_prod.is_none() && world.house_credits(self.house) > 0 && has_factory {
            // Replacement harvester first, if the refinery outnumbers harvesters
            // (house.cpp:6075).
            let refineries = self
                .role_building(cat, Role::Refinery)
                .map(|r| self.count_owned(world, r))
                .unwrap_or(0);
            let harvesters = world
                .units
                .iter()
                .filter(|(_, u)| u.house == self.house && u.is_harvester)
                .count() as i32;
            let mut issued = false;
            if refineries > harvesters {
                if let Some((id, _)) = cat.units.iter().enumerate().find(|(_, p)| p.is_harvester) {
                    if self.unit_buildable(world, hs, id as u32) {
                        out.push(Command::StartProduction {
                            house: self.house,
                            item: BuildItem::Unit(id as u32),
                        });
                        issued = true;
                    }
                }
            }
            if !issued {
                // Weighted-random pick among buildable armed **vehicles**
                // (house.cpp:6186; uniform weights). Infantry are excluded here —
                // they build on the barracks strip below.
                let eligible: Vec<u32> = cat
                    .units
                    .iter()
                    .enumerate()
                    .filter(|(id, p)| {
                        p.weapon.is_some()
                            && !p.is_harvester
                            && !p.is_infantry
                            && p.deploys_to.is_none()
                            && self.unit_buildable(world, hs, *id as u32)
                    })
                    .map(|(id, _)| id as u32)
                    .collect();
                if !eligible.is_empty() {
                    let pick = rng.range(0, eligible.len() as i32 - 1) as usize;
                    out.push(Command::StartProduction {
                        house: self.house,
                        item: BuildItem::Unit(eligible[pick]),
                    });
                }
            }
        }

        // --- Infantry lane (barracks) — cheap wave filler ---
        let has_barracks = self
            .role_building(cat, Role::Barracks)
            .map(|b| hs.owns_building(b))
            .unwrap_or(false);
        if hs.infantry_prod.is_none() && world.house_credits(self.house) > 0 && has_barracks {
            let eligible: Vec<u32> = cat
                .units
                .iter()
                .enumerate()
                .filter(|(id, p)| p.is_infantry && self.unit_buildable(world, hs, *id as u32))
                .map(|(id, _)| id as u32)
                .collect();
            // RNG is drawn ONLY when infantry are actually producible, so catalogs
            // with no infantry (every pre-M7.6 test) draw no extra RNG and keep
            // their AI hash chains unchanged.
            if !eligible.is_empty() {
                let pick = rng.range(0, eligible.len() as i32 - 1) as usize;
                out.push(Command::StartProduction {
                    house: self.house,
                    item: BuildItem::Unit(eligible[pick]),
                });
            }
        }
    }

    // ---- Attack waves (house.cpp:1042 team spawn + Greatest_Threat) ---------

    /// Gather this house's idle armed units and send them at the nearest enemy
    /// structure (its base), once enough force has accumulated.
    fn launch_attack(&self, world: &World, out: &mut Vec<Command>) {
        // Idle armed vehicles (not harvesters, not already attacking).
        let force: Vec<_> = world
            .units
            .handles()
            .into_iter()
            .filter(|&h| {
                world
                    .units
                    .get(h)
                    .map(|u| {
                        u.house == self.house
                            && u.weapon.is_some()
                            && !u.is_harvester
                            && u.target.is_none()
                    })
                    .unwrap_or(false)
            })
            .collect();
        if force.len() < self.difficulty.min_force() {
            return;
        }

        // Pick a target: the nearest enemy building to our base centre, else the
        // nearest enemy unit. Enemy = any live house other than ours (the
        // original scores by distance-dominant value, house.cpp:4941; nearest is
        // the dominant term).
        let base = self.base_center(world);
        let target = self.nearest_enemy_target(world, base);
        let Some(target) = target else {
            return;
        };
        for unit in force {
            out.push(Command::Attack {
                unit,
                target,
                house: self.house,
            });
        }
    }

    fn nearest_enemy_target(&self, world: &World, from: CellCoord) -> Option<Target> {
        let key = |c: CellCoord| -> i64 {
            let dx = (c.x - from.x) as i64;
            let dy = (c.y - from.y) as i64;
            dx * dx + dy * dy
        };
        // Prefer enemy buildings (attacking the base ends the game).
        let mut best_b: Option<(i64, crate::Handle)> = None;
        for (h, b) in world.buildings.iter() {
            if b.house != self.house && b.is_alive() {
                let d = key(b.center_cell());
                if best_b.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best_b = Some((d, h));
                }
            }
        }
        if let Some((_, h)) = best_b {
            return Some(Target::Building(h));
        }
        let mut best_u: Option<(i64, crate::Handle)> = None;
        for (h, u) in world.units.iter() {
            if u.house != self.house {
                let d = key(u.cell());
                if best_u.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best_u = Some((d, h));
                }
            }
        }
        best_u.map(|(_, h)| Target::Unit(h))
    }

    // ---- Helpers -----------------------------------------------------------

    fn has_construction_yard(&self, world: &World) -> bool {
        world
            .buildings
            .iter()
            .any(|(_, b)| b.house == self.house && b.is_construction_yard && b.is_alive())
    }

    fn own_mcv(&self, world: &World) -> Option<crate::Handle> {
        world.units.handles().into_iter().find(|&h| {
            world
                .units
                .get(h)
                .map(|u| {
                    u.house == self.house
                        && world
                            .catalog
                            .units
                            .iter()
                            .any(|p| p.sprite_id == u.type_id && p.deploys_to.is_some())
                })
                .unwrap_or(false)
        })
    }

    /// The base anchor cell: the construction yard centre, else any owned building.
    fn base_center(&self, world: &World) -> CellCoord {
        world
            .buildings
            .iter()
            .find(|(_, b)| b.house == self.house && b.is_construction_yard)
            .or_else(|| world.buildings.iter().find(|(_, b)| b.house == self.house))
            .map(|(_, b)| b.center_cell())
            .unwrap_or(CellCoord::new(64, 64))
    }

    /// A legal footprint top-left near the base for building `id`, spiralling out
    /// from the construction yard (deterministic scan order).
    fn placement_cell(&self, world: &World, id: u32) -> Option<CellCoord> {
        let anchor = world
            .buildings
            .iter()
            .find(|(_, b)| b.house == self.house && b.is_construction_yard)
            .or_else(|| world.buildings.iter().find(|(_, b)| b.house == self.house))
            .map(|(_, b)| b.cell)?;
        for r in 1..14 {
            for dy in -r..=r {
                for dx in -r..=r {
                    let c = CellCoord::new(anchor.x + dx, anchor.y + dy);
                    if world.can_place_building(self.house, id, c) {
                        return Some(c);
                    }
                }
            }
        }
        None
    }

    fn count_owned(&self, world: &World, id: u32) -> i32 {
        world
            .house(self.house)
            .map(|h| h.building_counts.get(id as usize).copied().unwrap_or(0) as i32)
            .unwrap_or(0)
    }

    /// Whether the house can start structure `id` right now (prereqs owned + the
    /// construction yard present; funds/lane are re-checked by the sim). Mirrors
    /// the sidebar's buildable test so the AI never spams rejected commands.
    fn buildable(&self, world: &World, hs: &House, id: u32) -> bool {
        let Some(p) = world.catalog.building(id) else {
            return false;
        };
        p.prereq.iter().all(|&pre| hs.owns_building(pre))
    }

    fn unit_buildable(&self, world: &World, hs: &House, id: u32) -> bool {
        let Some(p) = world.catalog.unit(id) else {
            return false;
        };
        p.prereq.iter().all(|&pre| hs.owns_building(pre))
    }

    /// The first building id in the catalog matching a role.
    fn role_building(&self, cat: &Catalog, role: Role) -> Option<u32> {
        cat.buildings
            .iter()
            .position(|b| match role {
                Role::Power => b.power > 0 && !b.is_construction_yard,
                Role::Refinery => b.is_refinery,
                Role::WarFactory => b.is_war_factory,
                Role::Barracks => b.is_barracks,
            })
            .map(|i| i as u32)
    }
}

/// A building role the AI shops for.
#[derive(Clone, Copy)]
enum Role {
    Power,
    Refinery,
    WarFactory,
    Barracks,
}
