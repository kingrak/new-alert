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
//! - **`HouseClass::Expert_AI`** (`house.cpp:4877`, M7.10) — on the ~10 s AITimer:
//!   pick the designated **enemy** by the weighted score (distance-dominant +
//!   kills-against-me + relative base size + last-attacker, house.cpp:4941), and
//!   raise the **rubber-band** unit/building caps to the average enemy's size + 10
//!   (`Control.MaxUnit/MaxBuilding`, house.cpp:5010).
//! - **Composed attack teams** (M7.10, standing in for `TeamTypeClass`/`TeamClass`
//!   mission scripts, teamtype.h) — on the `AlertTime` cadence (house.cpp:1042),
//!   gather a weighted vehicle+infantry mix that can reach a **staging cell** on
//!   the base edge toward the enemy, rally there, then attack-move the objective;
//!   dissolve (survivors retreat) when decimated, with an occasional
//!   harvester-harassment mission.
//! - **Economic reflexes** (M7.10) — repair damaged buildings (`Repair_AI`,
//!   building.cpp:5834, via `Command::Repair`), sell a non-essential building when
//!   broke (`AI_Raise_Money`, house.cpp:5552), and fire-sale + all-out attack in
//!   the lost-cause endgame (`Fire_Sale`/`Do_All_To_Hunt`, house.cpp:7622/7651).
//!
//! Difficulty (M7.9 P2a) applies the full FirePower/Armor/ROF/Groundspeed/Cost/
//! BuildTime stat handicaps (`Assign_Handicap`, house.cpp:278) house-scoped, and
//! also scales the attack cadence + wave size here.
//!
//! **Sync RNG.** Where the original draws `Scen.RandomNumber` we draw the sim
//! RNG, in a fixed order (`step` runs per AI house in house-index order): the
//! weighted vehicle/infantry production picks (`house.cpp:6186`), the attack
//! jitter (`house.cpp:1056`), and the team composition draws (harass roll, vehicle
//! count, infantry count). Expert_AI scoring is deterministic (no draw).

use crate::catalog::{round_up_fixed, BuildingProto, Catalog};
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

    /// Target number of **vehicles** in a composed attack team (M7.10 d).
    fn team_vehicles(self) -> i32 {
        match self {
            Difficulty::Easy => 2,
            Difficulty::Normal => 3,
            Difficulty::Hard => 4,
        }
    }
}

/// Which base-building/economy *policy* an [`AiPlayer`] runs — a
/// **test/measurement** knob (M7.14 audit P0), NOT a gameplay setting. `Expert`
/// (the default) is the current, faithful ratio-driven policy; `Legacy` restores
/// the exact pre-M7.14 fixed-priority ladder so the two can be raced head-to-head
/// in an honest A/B ("does the new AI actually beat the old one?"). Real play
/// **always** uses `Expert` — `Legacy` is only ever installed by the acceptance
/// harness (`ai_profile` / `ui_ai_vs_ai`'s Expert-vs-Legacy record).
///
/// The two policies differ in exactly the M7.14 building/economy delta, and share
/// **everything else** (Expert_AI enemy scoring, rubber-band caps, composed teams,
/// economic reflexes, combat, movement) — so a race isolates the value of the
/// ratio-driven base composition, not incidental differences:
///
/// - **`next_structure`** — `Expert` uses the ratio×limit self-limiting composition
///   ([`AiPlayer::next_structure`], `AI_Building`, house.cpp:5696); `Legacy` uses the
///   fixed power→refinery→war→barracks→radar→defense→expand ladder
///   ([`AiPlayer::next_structure_legacy`], the verbatim pre-M7.14 snapshot at
///   git `9155fce^`).
/// - **auto-harvester replacement** — `Expert` gates it on `IQ >= IQHarvester`
///   (M7.14); `Legacy` replaces a lost harvester unconditionally (the pre-M7.14
///   `refineries > harvesters` test — which the git archaeology shows *did* exist,
///   a correction to the brief's "Legacy has no replacement": stripping it would
///   only rig the A/B in Expert's favour, so the faithful snapshot keeps it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AiProfile {
    /// The pre-M7.14 fixed-ladder policy (baseline for the A/B). Never used in real play.
    Legacy,
    /// The current faithful ratio-driven policy (the shipping default).
    #[default]
    Expert,
}

/// Expert_AI re-evaluation cadence (`HouseClass::Expert_AI` returns
/// `TICKS_PER_SECOND * 10`, house.cpp:4877 — "relatively time consuming, call
/// periodically"). Enemy re-selection and rubber-band caps run on this timer, not
/// every tick.
const EXPERT_PERIOD: u32 = 150;

/// A composed attack team (M7.10 d) — a subset of the house's forces gathered
/// with a target composition, moved to a staging cell near the base edge toward
/// the enemy, then sent in as an attack-move. Dissolves (survivors retreat) when
/// decimated. Stands in for the original's `TeamTypeClass`/`TeamClass` mission
/// scripts (teamtype.h) with a single ad-hoc team.
#[derive(Clone, Debug)]
struct Team {
    /// Live member unit handles.
    members: Vec<crate::Handle>,
    /// Where the team is in its lifecycle.
    phase: TeamPhase,
    /// What it is going for (an enemy building/base, or an enemy harvester).
    target: Target,
    /// The rally cell near the base edge toward the enemy.
    staging: CellCoord,
    /// Member count at formation — the denominator for the retreat threshold.
    initial_size: usize,
    /// Countdown while `Staging` before we give up waiting and attack anyway.
    stage_timer: u32,
    /// A harvester-harassment team (targets an enemy harvester) vs. a base assault.
    is_harass: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TeamPhase {
    /// Moving to the staging cell (gathering).
    Staging,
    /// Attack-moving the target.
    Attacking,
}

impl Team {
    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u32(self.members.len() as u32);
        for m in &self.members {
            h.write_u32(m.index);
            h.write_u32(m.gen);
        }
        h.write_u8(self.phase as u8);
        match self.target {
            Target::Unit(t) => {
                h.write_u8(0);
                h.write_u32(t.index);
                h.write_u32(t.gen);
            }
            Target::Building(t) => {
                h.write_u8(1);
                h.write_u32(t.index);
                h.write_u32(t.gen);
            }
            Target::Cell(c) => {
                h.write_u8(2);
                h.write_i32(c.x);
                h.write_i32(c.y);
            }
        }
        h.write_i32(self.staging.x);
        h.write_i32(self.staging.y);
        h.write_u32(self.initial_size as u32);
        h.write_u32(self.stage_timer);
        h.write_u8(self.is_harass as u8);
    }
}

/// How long a team waits at staging before committing to the attack anyway
/// (~13 s at 15 Hz), so a team never stalls forever if a straggler can't reach
/// the rally cell.
const STAGE_TIMEOUT: u32 = 200;

/// `RepairDelay` in hundredths (`Rule.Diff[].RepairDelay`, rules.cpp:316 = `.02`
/// fixed default) — the difficulty handicap that scales the repair-throttle re-arm
/// window (`RepairTimer`, building.cpp:5842). Kept in integer hundredths so the
/// re-arm math is deterministic (matching the reference `(int)(fixed × int)`).
const REPAIR_DELAY_HUNDREDTHS: i32 = 2; // 0.02

/// `TICKS_PER_MINUTE` (defines.h:3122 = `TICKS_PER_SECOND(15) × 60`), used for the
/// repair-throttle re-arm window.
const TICKS_PER_MINUTE: i32 = 900;

/// Money floor below which a moneyless AI sells a non-essential building to raise
/// cash (`Check_Raise_Money` `< 100`, house.cpp:5288, taken as the emergency
/// floor for the "can't make money" case).
const RAISE_MONEY_FLOOR: i32 = 100;

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
    /// Ticks until the next Expert_AI pass (enemy re-pick + rubber-band caps).
    expert_timer: u32,
    /// The designated enemy house (`HouseClass::Enemy`, house.cpp:4989), chosen by
    /// the weighted Expert_AI score. `None` until one is picked.
    enemy: Option<u8>,
    /// Rubber-band unit cap (`Control.MaxUnit`, house.cpp:5010): the AI builds
    /// vehicles up to this, scaled to the average enemy's army size. `0` until the
    /// first Expert_AI pass sets it.
    max_units: u32,
    /// Rubber-band building cap (`Control.MaxBuilding`, house.cpp:5010): caps the
    /// AI's discretionary base expansion so it doesn't spam power plants and wall
    /// itself in. `0` until the first Expert_AI pass sets it (= uncapped).
    max_buildings: u32,
    /// The one active composed attack team, if any (M7.10 d).
    team: Option<Team>,
    /// Consecutive attack teams dissolved by decimation (M7.11 P1a — escalating
    /// waves). Each decimated team bumps this; it scales the next wave's size so
    /// repeated failed attacks eventually commit an overwhelming force, keeping
    /// AI-vs-AI decisive even against active defenders (the M7.11 skirmish parity
    /// change). Capped at [`MAX_ESCALATION`]. Stands in for the original's rising
    /// attack urgency (`Check_Attack`/`Attack` counters, house.cpp:5226) — no
    /// single mechanism maps 1:1, so this is documented as tuning.
    failed_attacks: u32,
    /// Base-building/economy policy (M7.14 audit P0). [`AiProfile::Expert`] (the
    /// default) is the shipping ratio-driven policy; [`AiProfile::Legacy`] restores
    /// the pre-M7.14 fixed ladder for the honest A/B. Real play is always Expert.
    profile: AiProfile,
    /// Repair-throttle cooldown (M7.14 audit P1 — `HouseClass::RepairTimer`,
    /// house.h:354). Counts down each tick; the AI may only *begin* a repair when it
    /// reaches 0, and it re-arms to a random multi-tick delay after each repair
    /// (`Repair_AI`, building.cpp:5842). This is a faithful port of the original's
    /// repair pacing, and it lets us use the stock `[AI] CreditReserve=100` floor
    /// instead of the earlier hardcoded 1000. NOTE (M7.15 audit): the throttle is
    /// *fidelity*, not load-bearing for decisiveness — the M7.15 audit disabled it
    /// (repair every pass at CreditReserve=100) and no starvation deadlock returned
    /// on the current M7.10/M7.11-tuned economy, so the earlier "prevents a deadlock"
    /// justification is unsubstantiated by the suite; kept purely for faithfulness.
    /// `0` (armed) until the first repair fires. Folded into the hash only when
    /// non-zero (like the other AI decision state).
    repair_timer: u32,
}

/// Cap on the consecutive-failure escalation counter (M7.11 P1a). Once reached,
/// a wave already commits effectively the whole reachable army, so further
/// growth is pointless — the cap keeps the value bounded (and its hash stable).
const MAX_ESCALATION: u32 = 8;

/// Consecutive decimated waves after which the AI abandons staged waves and goes
/// **all-out** (M7.11 P1d). Below this, escalating staged waves (P1a) apply
/// graduated pressure; at/above it, the whole army assaults enemy production
/// continuously. Chosen so a few genuine failures still play out as normal waves
/// (fidelity) before committing everything (decisiveness).
const ALL_OUT_ESCALATION: u32 = 4;

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
            expert_timer: 0,
            enemy: None,
            max_units: 0,
            max_buildings: 0,
            team: None,
            failed_attacks: 0,
            profile: AiProfile::Expert,
            repair_timer: 0,
        }
    }

    /// Set this controller's base-building/economy [`AiProfile`] (M7.14 audit P0).
    /// Only the acceptance harness calls this with `Legacy`; real play never does.
    pub fn with_profile(mut self, profile: AiProfile) -> AiPlayer {
        self.profile = profile;
        self
    }

    /// The base-building/economy policy this controller runs.
    pub fn profile(&self) -> AiProfile {
        self.profile
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_u8(self.house);
        h.write_u8(self.difficulty as u8);
        h.write_u32(self.decide_timer);
        h.write_u32(self.attack_timer);
        h.write_u8(self.deployed as u8);
        h.write_u32(self.expert_timer);
        // Fold new decision state in ONLY when set/present, so any pre-M7.10 AI
        // golden (which never sets an enemy/cap/team) hashes byte-identically and
        // the churn is scoped to games that actually exercise the new AI.
        if let Some(e) = self.enemy {
            h.write_u8(0xE1);
            h.write_u8(e);
        }
        if self.max_units != 0 {
            h.write_u8(0xCA);
            h.write_u32(self.max_units);
        }
        if self.max_buildings != 0 {
            h.write_u8(0xCB);
            h.write_u32(self.max_buildings);
        }
        if let Some(t) = &self.team {
            h.write_u8(0x7E);
            t.hash_into(h);
        }
        if self.failed_attacks != 0 {
            h.write_u8(0xFA);
            h.write_u32(self.failed_attacks);
        }
        // Profile is folded in ONLY when it is not the shipping default (Expert),
        // so every real game / pre-M7.14-audit AI golden (all Expert) appends no
        // bytes and hashes byte-identically — the flag is pure test infrastructure.
        if self.profile != AiProfile::Expert {
            h.write_u8(0x9F);
            h.write_u8(self.profile as u8);
        }
        // Repair throttle timer — folded only when armed (non-zero), so an AI that
        // has never repaired (and every pre-M7.14-audit golden) is unchanged.
        if self.repair_timer != 0 {
            h.write_u8(0x8D);
            h.write_u32(self.repair_timer);
        }
    }

    /// The house this AI plays.
    pub fn house(&self) -> u8 {
        self.house
    }

    /// The designated enemy house, if any (Expert_AI `Enemy`).
    pub fn enemy(&self) -> Option<u8> {
        self.enemy
    }

    /// The rubber-band unit / building caps (`Control.MaxUnit`/`MaxBuilding`).
    pub fn caps(&self) -> (u32, u32) {
        (self.max_units, self.max_buildings)
    }

    /// A read-only snapshot of the active composed team, for showcase/inspection:
    /// `(member_count, initial_size, is_staging, is_harass)`. `None` when no team.
    pub fn team_summary(&self) -> Option<(usize, usize, bool, bool)> {
        self.team.as_ref().map(|t| {
            (
                t.members.len(),
                t.initial_size,
                t.phase == TeamPhase::Staging,
                t.is_harass,
            )
        })
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
        self.expert_timer = self.expert_timer.saturating_sub(1);
        // Repair-throttle cooldown (`RepairTimer` CDTimerClass counts down each
        // frame; `DidRepair` is cleared only once it hits 0, house.cpp:1433).
        self.repair_timer = self.repair_timer.saturating_sub(1);

        // 0) Expert_AI pass (M7.10 b+c): re-pick the designated enemy by the
        // weighted score and raise the rubber-band unit cap to match the enemies'
        // sizes. On the AITimer cadence, not every tick. No commands, no RNG.
        if self.expert_timer == 0 {
            self.expert_timer = EXPERT_PERIOD;
            self.expert_ai(world);
        }

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

        // 2) Economic reflexes (M7.10 e): repair damaged buildings, sell when
        // broke, and fire-sale + all-out attack in the endgame. Runs on the
        // decide cadence to bound work and command volume.
        if (self.decide_timer == DECIDE_PERIOD || self.decide_timer == 0)
            && (self.has_construction_yard(world) || self.has_any_building(world))
        {
            self.economic_reflexes(world, rng, out);
        }

        // 3) Composed attack teams (M7.10 d): advance the active team's lifecycle
        // (stage → attack → dissolve) every tick, and on the attack cadence form a
        // new one from idle forces.
        self.manage_team(world, rng, out);
    }

    // ---- Expert_AI: enemy selection + rubber-band caps (house.cpp:4877) -----

    /// Re-evaluate the designated enemy and the rubber-band unit cap
    /// (`HouseClass::Expert_AI`, house.cpp:4900-5020). Pure decision update — reads
    /// `world`, writes only `self.enemy` / `self.max_units`, emits nothing, draws
    /// no RNG (the scoring is fully deterministic).
    fn expert_ai(&mut self, world: &World) {
        // Drop a dead/defeated designated enemy (house.cpp:4888).
        if let Some(e) = self.enemy {
            if !world.house_alive(e) {
                self.enemy = None;
            }
        }

        let center = self.base_center(world);
        // MAP_CELL_W in the original (128). Distance is in cells here.
        const MAP_W: i32 = 128;
        let my_units = self.army_size(world, self.house);
        let my_buildings = self.building_size(world, self.house);
        let my_infantry = self.infantry_size(world, self.house);

        let mut best: Option<(i32, u8)> = None;
        let mut enemy_units_sum = 0i32;
        let mut enemy_buildings_sum = 0i32;
        let mut enemy_count = 0i32;
        // Houses in index order (deterministic), skipping ourself + dead houses.
        for h in 0..world.houses.len() as u8 {
            if h == self.house || !world.house_alive(h) {
                continue;
            }
            // Only enemy houses that have a base established (a `Center`) count —
            // the original refuses to pick until every enemy has started
            // (house.cpp:4922); we relax to "has any building or unit".
            let ecenter = self.base_center_of(world, h);
            let eu = self.army_size(world, h);
            let eb = self.building_size(world, h);
            let ei = self.infantry_size(world, h);
            enemy_units_sum += eu;
            enemy_buildings_sum += eb;
            enemy_count += 1;

            // house.cpp:4941 weighted score (all terms cited):
            //   ((MAP_CELL_W*2) - Distance) * 2                  (distance-dominant)
            //   + BuildingsKilled[me]*5 + UnitsKilled[me]        (kills against me)
            //   + (enemy.CurUnits - CurUnits)                    (relative base size)
            //   + (enemy.CurBuildings - CurBuildings)
            //   + (enemy.CurInfantry - CurInfantry)/4
            //   + (enemy == LAEnemy ? 100 : 0)                   (last attacker)
            let dist = cell_distance(center, ecenter);
            let mut value = ((MAP_W * 2) - dist) * 2;
            if let Some(eh) = world.house(h) {
                value += eh.buildings_killed_by(self.house) as i32 * 5;
                value += eh.units_killed_by(self.house) as i32;
            }
            value += eu - my_units;
            value += eb - my_buildings;
            value += (ei - my_infantry) / 4;
            if world.house(self.house).and_then(|h| h.last_attacker) == Some(h) {
                value += 100;
            }
            if best.map(|(bv, _)| value > bv).unwrap_or(true) {
                best = Some((value, h));
            }
        }
        self.enemy = best.map(|(_, h)| h);

        // Rubber-band caps (house.cpp:5010): raise our unit + building appetite to
        // the average enemy's size + 10, never shrinking (max with the current
        // cap), with sane early-game floors so a base with no visible enemies still
        // builds a starting force and a full base.
        let (avg_u, avg_b) = if enemy_count > 0 {
            (
                enemy_units_sum / enemy_count,
                enemy_buildings_sum / enemy_count,
            )
        } else {
            (0, 0)
        };
        self.max_units = self.max_units.max((avg_u + 10).max(10) as u32);
        self.max_buildings = self.max_buildings.max((avg_b + 10).max(10) as u32);
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

    /// The next structure to build — a faithful port of `HouseClass::AI_Building`'s
    /// **ratio-driven base composition** (house.cpp:5696). For each category the AI
    /// wants `Round_Up(Rule.<Cat>Ratio × CurBuildings)` structures (clamped to the
    /// category `Limit`); if it holds fewer, that category becomes a build *choice*
    /// with an urgency, and the AI builds the **most urgent** choice this pass.
    ///
    /// **Reference note.** The desired count multiplies `CurBuildings` (this house's
    /// live building count), *not* `CurBuildings + BaseSizeAdd` — the `BaseSizeAdd`
    /// cap in the original is present but commented out (house.cpp:5716), so the
    /// shipped game uses the raw ratio (rule 3: reference is ground truth). This is
    /// what replaces the old fixed power→refinery→factory priority ladder.
    ///
    /// **Taxonomy adaptation.** We fold AA/Tesla into the single "defense" category
    /// (we model no aircraft, and pick the strongest buildable armed building), and
    /// skip Helipad/Airstrip (no aircraft sim). Ratios/limits are 100% rules.ini.
    fn next_structure(&self, world: &World, hs: &House, cat: &Catalog) -> Option<u32> {
        // A/B baseline (M7.14 audit P0): the pre-M7.14 fixed-priority ladder.
        if self.profile == AiProfile::Legacy {
            return self.next_structure_legacy(world, hs, cat);
        }
        let ai = &cat.econ.ai;
        // `Available_Money` (house.cpp): the spendable pool. `hasincome` is the
        // original's refinery+harvester+not-short gate that lets a category build
        // even when cash is momentarily below its price (income is coming).
        let money = world.house_credits(self.house);
        let refinery_id = self.role_building(cat, Role::Refinery);
        let has_income = refinery_id
            .map(|r| self.count_owned(world, r) > 0)
            .unwrap_or(false)
            && self.has_harvester(world);
        // CurBuildings — this house's live, non-wall buildings.
        let cur = self.building_size(world, self.house);

        let cost_of = |id: u32| cat.building(id).map(|b| b.cost).unwrap_or(0);

        // Build choices in the original's declaration order (so ties resolve to the
        // earlier-declared category, matching the `Urgency > best` scan,
        // house.cpp:5990): power, refinery, barracks, war factory, radar, defense.
        let mut choices: Vec<(Urgency, u32)> = Vec::new();

        // Power — build until `Power <= Drain + PowerSurplus` (house.cpp:5744).
        if let Some(p) = self.role_building(cat, Role::Power) {
            let short_power = hs.power_output <= hs.power_drain + ai.power_surplus;
            if short_power && cost_of(p) < money && self.buildable(world, hs, p) {
                let refineries = refinery_id.map(|r| self.count_owned(world, r)).unwrap_or(0);
                let urg = if refineries == 0 {
                    Urgency::Low
                } else {
                    Urgency::Medium
                };
                choices.push((urg, p));
            }
        }

        // Refinery — `current < Round_Up(RefineryRatio × cur) && < RefineryLimit`
        // (house.cpp:5762). HIGH when the house has none yet.
        if let Some(r) = refinery_id {
            let current = self.count_owned(world, r);
            let desired = round_up_fixed(ai.refinery_ratio, cur);
            if current < desired
                && current < ai.refinery_limit
                && (money > cost_of(r) || has_income)
                && self.buildable(world, hs, r)
            {
                let urg = if current == 0 {
                    Urgency::High
                } else {
                    Urgency::Medium
                };
                choices.push((urg, r));
            }
        }

        // Barracks — `current < Round_Up(BarracksRatio × cur) && < BarracksLimit`
        // and `(money > 300 || hasincome)` (house.cpp:5787).
        if let Some(b) = self.role_building(cat, Role::Barracks) {
            let current = self.count_owned(world, b);
            let desired = round_up_fixed(ai.barracks_ratio, cur);
            if current < desired
                && current < ai.barracks_limit
                && (money > 300 || has_income)
                && (cost_of(b) < money || has_income)
                && self.buildable(world, hs, b)
            {
                let urg = if current > 0 {
                    Urgency::Low
                } else {
                    Urgency::Medium
                };
                choices.push((urg, b));
            }
        }

        // War factory — `current < Round_Up(WarRatio × cur) && < WarLimit` and
        // `(money > 2000 || hasincome)` (house.cpp:5831).
        if let Some(f) = self.role_building(cat, Role::WarFactory) {
            let current = self.count_owned(world, f);
            let desired = round_up_fixed(ai.war_ratio, cur);
            if current < desired
                && current < ai.war_limit
                && (money > 2000 || has_income)
                && (cost_of(f) < money || has_income)
                && self.buildable(world, hs, f)
            {
                let urg = if current > 0 {
                    Urgency::Low
                } else {
                    Urgency::Medium
                };
                choices.push((urg, f));
            }
        }

        // Radar dome — the original builds a radar as part of AA/tech
        // (house.cpp:5900); we build one for the minimap/tech gate once the economy
        // runs. Matched by catalog name (no new role enum). MEDIUM, one is enough.
        if has_income {
            if let Some(dome) = cat
                .buildings
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case("DOME"))
                .map(|i| i as u32)
            {
                if !hs.owns_building(dome)
                    && (cost_of(dome) < money || has_income)
                    && self.buildable(world, hs, dome)
                {
                    choices.push((Urgency::Medium, dome));
                }
            }
        }

        // Defense — `owned < Round_Up(DefenseRatio × cur) && < DefenseLimit`
        // (house.cpp:5851). AA/Tesla ratios are folded in here (max of the three,
        // since we pick one strongest buildable armed building). We prefer the
        // strongest (reverse catalog order → tesla/gun before pillbox).
        {
            let mut owned_def = 0i32;
            let mut strongest: Option<u32> = None;
            for (id, p) in cat.buildings.iter().enumerate().rev() {
                if p.weapon.is_some() && !p.is_wall && !is_air_only_defense(p) {
                    owned_def += self.count_owned(world, id as u32);
                    if strongest.is_none()
                        && (p.cost < money || has_income)
                        && self.buildable(world, hs, id as u32)
                    {
                        strongest = Some(id as u32);
                    }
                }
            }
            let desired = round_up_fixed(ai.defense_ratio, cur)
                .max(round_up_fixed(ai.aa_ratio, cur))
                .max(round_up_fixed(ai.tesla_ratio, cur));
            if owned_def < desired && owned_def < ai.defense_limit {
                if let Some(d) = strongest {
                    choices.push((Urgency::Medium, d));
                }
            }
        }

        // Pick the most urgent choice; the FIRST at the top urgency wins (the
        // original's strict `Urgency > best` scan keeps the earlier-declared
        // category on a tie, house.cpp:5990).
        let best = choices.iter().map(|(u, _)| *u).max()?;
        choices
            .into_iter()
            .find(|(u, _)| *u == best)
            .map(|(_, id)| id)
    }

    /// The **pre-M7.14 fixed-priority ladder** — a verbatim snapshot of
    /// `next_structure` at git `9155fce^` (M7.11 tip), used ONLY by
    /// [`AiProfile::Legacy`] as the A/B baseline (M7.14 audit P0). It builds in a
    /// fixed order (power → refinery → war factory → barracks → radar → defense →
    /// discretionary expand), with the M7.10/M7.11 rubber-band **building** cap and
    /// the low-power-gated spare-power tail. It is deliberately *not* kept in sync
    /// with `next_structure` — it is a frozen historical policy for measurement.
    fn next_structure_legacy(&self, world: &World, hs: &House, cat: &Catalog) -> Option<u32> {
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
        // 2) Refinery (economy).
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
        // 3b) Barracks once the war factory is up.
        if has_factory {
            if let Some(bar) = self.role_building(cat, Role::Barracks) {
                if !owns(bar) && self.buildable(world, hs, bar) {
                    return Some(bar);
                }
            }
        }
        // 3b2) Radar dome once the economy runs. Matched by catalog name.
        if has_factory {
            if let Some(dome) = cat
                .buildings
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case("DOME"))
                .map(|i| i as u32)
            {
                if !owns(dome) && self.buildable(world, hs, dome) {
                    return Some(dome);
                }
            }
        }
        // 3c) Base defense: keep 2 + refineries combat defenses, strongest first.
        if has_factory {
            let refineries = refinery_id.map(|r| self.count_owned(world, r)).unwrap_or(0);
            let mut owned_def = 0i32;
            let mut pick: Option<u32> = None;
            for (id, p) in cat.buildings.iter().enumerate().rev() {
                if p.weapon.is_some() && !p.is_wall && !is_air_only_defense(p) {
                    owned_def += self.count_owned(world, id as u32);
                    if pick.is_none() && self.buildable(world, hs, id as u32) {
                        pick = Some(id as u32);
                    }
                }
            }
            if owned_def < 2 + refineries {
                if let Some(d) = pick {
                    return Some(d);
                }
            }
        }
        // 4) Discretionary expand: a second refinery, then a spare power plant —
        // up to the rubber-band building cap, spare-power gated on a real deficit
        // (the M7.11 runaway fix). `0` cap means uncapped.
        let under_bcap = self.max_buildings == 0
            || self.building_size(world, self.house) < self.max_buildings as i32;
        if under_bcap {
            if let Some(r) = refinery_id {
                if self.count_owned(world, r) < 2 && self.buildable(world, hs, r) {
                    return Some(r);
                }
            }
            if low_power {
                if let Some(p) = power_id {
                    if self.buildable(world, hs, p) {
                        return Some(p);
                    }
                }
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
            // **Auto harvester replacement** (the "mining" trick, house.cpp:6075):
            // `IQ >= Rule.IQHarvester && !IsTiberiumShort && !IsHuman &&
            // BQuantity[REFINERY] > UQuantity[HARVESTER]` → queue a harvester. So a
            // computer house (IQ = MaxIQ ≥ IQHarvester) keeps one harvester per
            // refinery, and when one is destroyed it buys a replacement — the
            // economic reflex our AI otherwise lacked. IQ-gated per the original;
            // the human (IQ 0) never gets a free replacement.
            //
            // **Deviation (cited):** the original also skips this on
            // `Difficulty == DIFF_HARD` (house.cpp:6076). We do *not* replicate that
            // carve-out — our difficulty labels are inverted for AI opponents
            // (Q15), and the acceptance bar requires economic recovery at *every*
            // difficulty, so a killed AI harvester is always replaced.
            let iq = hs.iq;
            // Expert IQ-gates the replacement (M7.14); Legacy replaces
            // unconditionally (the verbatim pre-M7.14 `refineries > harvesters`
            // policy — the honest A/B baseline, git `9155fce^`).
            let iq_ok = match self.profile {
                AiProfile::Expert => iq >= cat.econ.iq.harvester,
                AiProfile::Legacy => true,
            };
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
            if iq_ok && refineries > harvesters {
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
            // Rubber-band cap (M7.10 c): stop building **combat vehicles** once our
            // army reaches the enemy-scaled cap (`Control.MaxUnit`, house.cpp:5010).
            // Harvesters (the economy replacement above) are never capped. `0` cap
            // (before the first Expert_AI pass) means "uncapped".
            let under_cap =
                self.max_units == 0 || self.army_size(world, self.house) < self.max_units as i32;
            if !issued && under_cap {
                // Weighted-random pick among buildable **vehicles** — the original
                // `AI_Unit` table (`house.cpp:6172`): each buildable non-harvester
                // unit weighs **20 if it has a primary weapon, else 1** (so the
                // unarmed support vehicles TRUK/MNLY are built, but rarely). Infantry
                // are excluded here — they build on the barracks strip below.
                let eligible: Vec<(u32, i32)> = cat
                    .units
                    .iter()
                    .enumerate()
                    .filter(|(id, p)| {
                        !p.is_harvester
                            && !p.is_infantry
                            && p.deploys_to.is_none()
                            // Aircraft (P0 aircraft arc): the AI does not build/fly
                            // air yet (AI-air is a documented cut), so exclude them
                            // from the vehicle lane — otherwise it wastes production
                            // slots on helis it has no helipad to build.
                            && p.locomotor != crate::LOCO_AIR_INDEX
                            && self.unit_buildable(world, hs, *id as u32)
                    })
                    .map(|(id, p)| (id as u32, if p.weapon.is_some() { 20 } else { 1 }))
                    .collect();
                let total: i32 = eligible.iter().map(|(_, w)| *w).sum();
                if total > 0 {
                    // Weighted walk over the counter array (house.cpp:6186).
                    let mut choice = rng.range(0, total - 1);
                    for (id, w) in &eligible {
                        if choice < *w {
                            out.push(Command::StartProduction {
                                house: self.house,
                                item: BuildItem::Unit(*id),
                            });
                            break;
                        }
                        choice -= *w;
                    }
                }
            }
        }

        // --- Infantry lane (barracks) — cheap wave filler ---
        let has_barracks = self
            .role_building(cat, Role::Barracks)
            .map(|b| hs.owns_building(b))
            .unwrap_or(false);
        if hs.infantry_prod.is_none() && world.house_credits(self.house) > 0 && has_barracks {
            // Only **offensive** infantry (a weapon that does positive damage) —
            // this admits the new combat specialists E4 (flamethrower) and DOG but
            // excludes the medic (heal weapon, negative damage) and the engineer
            // (unarmed), which the skirmish AI cannot use without micro. (`AI_Infantry`,
            // `house.cpp:6400`, builds combat infantry for its attack teams.)
            let eligible: Vec<u32> = cat
                .units
                .iter()
                .enumerate()
                .filter(|(id, p)| {
                    p.is_infantry
                        && p.weapon.map(|w| w.damage > 0).unwrap_or(false)
                        && self.unit_buildable(world, hs, *id as u32)
                })
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

    // ---- Composed attack teams (M7.10 d) -----------------------------------

    /// Advance the active team's lifecycle each tick, and form a new one on the
    /// attack cadence. Ports the shape of `TeamClass` mission scripts
    /// (teamtype.h): gather (Staging) → attack-move (Attacking) → dissolve when
    /// decimated (the fear/retreat threshold), with an occasional
    /// harvester-harassment team.
    fn manage_team(&mut self, world: &World, rng: &mut RandomLcg, out: &mut Vec<Command>) {
        // Sustained-failure endgame (M7.11 P1d — `Do_All_To_Hunt`, house.cpp:7651).
        // After `ALL_OUT_ESCALATION` consecutive decimated waves, abandon the
        // cautious stage-and-retreat cadence and commit the whole army to a
        // relentless assault on enemy production, re-pointing any idle or merely
        // auto-guarding armed unit each tick (units already attack-ordered keep
        // their target — `guard_target` cleared, so they aren't re-issued). This is
        // what guarantees a decision against active defenders: dribbled waves that
        // always retreat at 50% losses can stalemate forever (the observed
        // scg05ea/Easy stall), whereas an all-out assault presses until one side's
        // production falls and the loser's own fire-sale/all-hunt finishes it.
        if self.failed_attacks >= ALL_OUT_ESCALATION {
            self.team = None;
            self.all_out_assault(world, out);
            return;
        }
        if self.team.is_some() {
            self.advance_team(world, out);
            return;
        }
        // No team → form one on the attack cadence.
        if self.attack_timer == 0 {
            // Jittered reset (house.cpp:1056: `AlertTime * Random_Pick(...)`;
            // simplified to a ±50% jitter around the base interval).
            let base = self.difficulty.attack_interval();
            let jitter = rng.range(-(base as i32) / 2, (base as i32) / 2);
            self.attack_timer = (base as i32 + jitter).max(1) as u32;
            self.form_team(world, rng, out);
        }
    }

    /// Progress the current team: prune dead members, dissolve+retreat if
    /// decimated, and drive the Staging → Attacking transition. Issues attack/move
    /// orders only on transitions, never every tick (so it doesn't spam commands).
    fn advance_team(&mut self, world: &World, out: &mut Vec<Command>) {
        let Some(mut team) = self.team.take() else {
            return;
        };
        team.members
            .retain(|&h| world.units.get(h).map(|u| u.is_alive()).unwrap_or(false));
        let alive = team.members.len();

        // Dissolve + retreat when decimated (below half the starting size, floored
        // at 2) — our stand-in for the per-unit fear/retreat thresholds
        // (`FootClass` `IsScaredToDeath`/`Fear`, deferred). Survivors fall back to
        // the base and the slot frees for a fresh team. `retreat_floor` is always
        // >= 2, so a total wipeout (`alive == 0`) falls into this branch too —
        // deliberately: a wave that gets wiped out entirely is at least as
        // strong a failure signal as one merely ground below half strength, and
        // must escalate the next wave the same way (ra-tester audit fix,
        // M7.11 — a prior `if alive == 0 { return; }` short-circuited BEFORE
        // this check and skipped the escalation bump on a total wipeout, the
        // one outcome most likely to need it).
        let retreat_floor = (team.initial_size / 2).max(2);
        if alive < retreat_floor {
            // Escalate: a team ground down below half its size failed to break the
            // enemy's defense — bump the failure counter so the *next* wave is
            // bigger (M7.11 P1a). Capped so it stays bounded/hashable. This is the
            // mechanism that keeps AI-vs-AI decisive with active defenders: dribbled
            // small waves would stalemate forever, but each loss makes the next
            // commitment larger until a wave overwhelms the defense.
            self.failed_attacks = (self.failed_attacks + 1).min(MAX_ESCALATION);
            let base = self.base_center(world);
            for &unit in &team.members {
                out.push(Command::Move {
                    unit,
                    dest: base,
                    house: self.house,
                });
            }
            return; // team dissolved
        }

        match team.phase {
            TeamPhase::Staging => {
                team.stage_timer = team.stage_timer.saturating_sub(1);
                // Gathered once most members are within 3 cells of the rally point.
                let gathered = team
                    .members
                    .iter()
                    .filter(|&&h| {
                        world
                            .units
                            .get(h)
                            .map(|u| cell_distance(u.cell(), team.staging) <= 3)
                            .unwrap_or(false)
                    })
                    .count()
                    * 2
                    >= alive;
                if gathered || team.stage_timer == 0 {
                    // Re-validate the target (it may have died while staging).
                    let target = self
                        .validate_target(world, team.target)
                        .or_else(|| self.enemy_target(world));
                    if let Some(target) = target {
                        team.target = target;
                        team.phase = TeamPhase::Attacking;
                        for &unit in &team.members {
                            out.push(Command::Attack {
                                unit,
                                target,
                                house: self.house,
                            });
                        }
                    } else {
                        return; // no enemy left; disband
                    }
                }
            }
            TeamPhase::Attacking => {
                // Re-target if the objective died (chase the next-nearest enemy).
                if self.validate_target(world, team.target).is_none() {
                    if let Some(target) = self.enemy_target(world) {
                        team.target = target;
                        for &unit in &team.members {
                            out.push(Command::Attack {
                                unit,
                                target,
                                house: self.house,
                            });
                        }
                    } else {
                        return; // enemy eliminated; disband
                    }
                }
            }
        }
        self.team = Some(team);
    }

    /// Commit every armed, non-harvester unit to a relentless assault on enemy
    /// production (M7.11 P1d, `Do_All_To_Hunt`). Only idle or auto-guarding units
    /// are (re)ordered, so an already-attacking unit keeps its target and command
    /// volume stays low; as production buildings fall, `enemy_target` re-points the
    /// army at the next-weakest one until the enemy is finished.
    fn all_out_assault(&self, world: &World, out: &mut Vec<Command>) {
        let Some(target) = self.enemy_target(world) else {
            return;
        };
        for h in world.units.handles() {
            if let Some(u) = world.units.get(h) {
                if u.house == self.house
                    && u.weapon.is_some()
                    && !u.is_harvester
                    && (u.target.is_none() || u.guard_target)
                {
                    out.push(Command::Attack {
                        unit: h,
                        target,
                        house: self.house,
                    });
                }
            }
        }
    }

    /// Form a composed team from idle forces: a weighted vehicle+infantry mix
    /// (`team_vehicles` vehicles + 0..2 infantry), a staging cell near the base
    /// edge toward the enemy, and — occasionally — a harvester-harassment mission
    /// instead of a base assault. RNG draws (fixed order): harass roll, then the
    /// vehicle count jitter, then the infantry count.
    fn form_team(&mut self, world: &World, rng: &mut RandomLcg, out: &mut Vec<Command>) {
        // Occasional harvester-harassment mission (1 in 4) when an enemy harvester
        // exists — a small, fast strike at the enemy economy. Draw the roll first
        // (fixed RNG order) regardless, so the sequence is stable.
        let harass_roll = rng.range(0, 3);
        let harvester_target = self.enemy_harvester(world);
        let is_harass = harass_roll == 0 && harvester_target.is_some();

        // Target: an enemy harvester (harass) or the designated enemy's base.
        let target = if is_harass {
            harvester_target
        } else {
            self.enemy_target(world)
        };
        let Some(target) = target else {
            return;
        };

        // Staging cell: a rally point on the base edge **toward the enemy**, pushed
        // far enough out to clear the base's own building ring so the team can
        // actually egress.
        let base = self.base_center(world);
        let tcell = self.target_cell(world, target).unwrap_or(base);
        let staging = self.staging_cell(world, base, tcell);

        // Idle armed units of ours (not harvesters, no current target) that can
        // actually **reach the staging cell** — this excludes units boxed inside
        // the base by our own buildings, so the composed team is one that can
        // egress (mirrors a `TeamClass` only recruiting members that can reach the
        // rally waypoint). Reachability is checked with the real pathfinder.
        let mut vehicles: Vec<crate::Handle> = Vec::new();
        let mut infantry: Vec<crate::Handle> = Vec::new();
        for h in world.units.handles() {
            if let Some(u) = world.units.get(h) {
                if u.house == self.house
                    && u.weapon.is_some()
                    && !u.is_harvester
                    // A unit that is merely auto-guarding (M7.5-B `guard_target`)
                    // counts as idle-and-available: recruiting it into a team
                    // issues a Move that clears the guard target. Without this,
                    // guard auto-acquisition would starve team recruitment (every
                    // idle defender near an enemy holds a target) and the AI would
                    // never mount an offensive — the scg05ea stall.
                    && (u.target.is_none() || u.guard_target)
                    && crate::path::find_path(world.passability(), u.cell(), staging, u.locomotor)
                        .is_some()
                {
                    if u.is_infantry() {
                        infantry.push(h);
                    } else {
                        vehicles.push(h);
                    }
                }
            }
        }

        // Composition: a weighted vehicle+infantry mix. Vehicles: difficulty base
        // ±1; infantry: 0..2. Clamped to what is actually idle + reachable. The
        // RNG draws (infantry count, then vehicle jitter) stay in a fixed order so
        // same-seed runs match; the escalation term is added deterministically.
        let want_i_raw = rng.range(0, 2);
        let jitter = rng.range(-1, 1);
        // M7.11 P1a — escalating waves: each consecutive decimated team adds to the
        // next wave's target size (vehicles ~2x infantry), so a stalled offensive
        // ratchets up until it commits an overwhelming force. `escalation` is 0 for
        // a fresh/successful attacker (unchanged behaviour) and grows to
        // `MAX_ESCALATION`, at which point a wave takes effectively every reachable
        // armed unit.
        let escalation = self.failed_attacks as i32;
        let want_i = (want_i_raw + escalation).clamp(0, infantry.len() as i32) as usize;
        let mut want_v = (self.difficulty.team_vehicles() + jitter + escalation * 2)
            .clamp(0, vehicles.len() as i32) as usize;
        // Top up the vehicle count so the team reaches the difficulty's minimum
        // force when enough units exist (a pure-vehicle team still qualifies) —
        // otherwise a cautious composition below `min_force` would never launch.
        let min_force = self.difficulty.min_force();
        while want_v + want_i < min_force && want_v < vehicles.len() {
            want_v += 1;
        }

        let mut members: Vec<crate::Handle> = Vec::new();
        members.extend(vehicles.iter().take(want_v).copied());
        members.extend(infantry.iter().take(want_i).copied());

        // Need at least the difficulty's minimum force to bother.
        if members.len() < min_force {
            return;
        }

        let initial_size = members.len();
        // Send everyone to the rally point (gather), then attack once staged.
        for &unit in &members {
            out.push(Command::Move {
                unit,
                dest: staging,
                house: self.house,
            });
        }
        self.team = Some(Team {
            members,
            phase: TeamPhase::Staging,
            target,
            staging,
            initial_size,
            stage_timer: STAGE_TIMEOUT,
            is_harass,
        });
    }

    /// A rally cell on the base edge toward `dest` — pushed far enough out
    /// (`STEP` cells) to clear the base's own building ring, so team members can
    /// egress to it. Returns the farthest passable cell along the line toward the
    /// enemy (falls back to `base` if none is passable).
    fn staging_cell(&self, world: &World, base: CellCoord, dest: CellCoord) -> CellCoord {
        // Far enough to sit outside a rubber-band-capped base's building blob.
        const STEP: i32 = 12;
        let dx = (dest.x - base.x).signum();
        let dy = (dest.y - base.y).signum();
        let mut best = base;
        // Prefer the farthest passable cell along the line (walk outward, keep the
        // last passable one so we clear the base rather than stopping at its edge).
        for k in 1..=STEP {
            let c = CellCoord::new(base.x + dx * k, base.y + dy * k);
            if world.passability().is_passable(c) {
                best = c;
            }
        }
        best
    }

    /// Whether a target is still a live objective (used to re-validate a team's
    /// aim after members/objectives may have died).
    fn validate_target(&self, world: &World, target: Target) -> Option<Target> {
        match target {
            Target::Unit(h) => world
                .units
                .get(h)
                .filter(|u| u.is_alive() && u.house != self.house)
                .map(|_| target),
            Target::Building(h) => world
                .buildings
                .get(h)
                .filter(|b| b.is_alive() && b.house != self.house)
                .map(|_| target),
            Target::Cell(_) => Some(target),
        }
    }

    /// The objective a base-assault team heads for. Target selection follows the
    /// original's quarry preference `QUARRY_FACTORIES` (attack production buildings,
    /// `defines.h:2477`): **focus and finish** by going for the enemy's *production*
    /// (war factory / construction yard / barracks) first, so a breakthrough
    /// cripples the enemy's ability to reinforce and drives the game to a decision
    /// (M7.11 P1c). Among candidate production buildings we pick the one in the
    /// **weakest-defended sector** — lowest summed nearby defense strength, a
    /// simplified `HouseClass::Adjust_Threat` region scan (house.cpp:2475) — so the
    /// team attacks the enemy base at its soft point (M7.11 P1b), tie-broken by
    /// nearest to our base. Falls back to the nearest building, then nearest unit.
    fn enemy_target(&self, world: &World) -> Option<Target> {
        let base = self.base_center(world);

        // Candidate production buildings, preferring the designated enemy's; if the
        // designated enemy has none live, consider every enemy's production.
        let want_house = |house: u8| match self.enemy {
            Some(e) => house == e,
            None => house != self.house,
        };
        let mut pick: Option<(i64, i64, crate::Handle)> = None; // (threat, dist², handle)
        for (h, b) in world.buildings.iter() {
            if b.is_alive() && !b.is_wall && want_house(b.house) && is_production(world, b) {
                let cell = b.center_cell();
                let threat = self.sector_threat(world, b.house, cell);
                let dist = sq_dist(cell, base);
                if pick
                    .map(|(pt, pd, _)| (threat, dist) < (pt, pd))
                    .unwrap_or(true)
                {
                    pick = Some((threat, dist, h));
                }
            }
        }
        // If the designated enemy had no production building, retry across ALL
        // enemies before giving up on the production quarry (a designated enemy
        // reduced to non-production buildings still shouldn't make us ignore a
        // reachable enemy factory elsewhere).
        if pick.is_none() && self.enemy.is_some() {
            for (h, b) in world.buildings.iter() {
                if b.is_alive() && !b.is_wall && b.house != self.house && is_production(world, b) {
                    let cell = b.center_cell();
                    let threat = self.sector_threat(world, b.house, cell);
                    let dist = sq_dist(cell, base);
                    if pick
                        .map(|(pt, pd, _)| (threat, dist) < (pt, pd))
                        .unwrap_or(true)
                    {
                        pick = Some((threat, dist, h));
                    }
                }
            }
        }
        if let Some((_, _, h)) = pick {
            return Some(Target::Building(h));
        }

        // No production buildings left anywhere: fall to the designated enemy's
        // nearest building, then the nearest enemy target of any kind.
        if let Some(e) = self.enemy {
            let mut best: Option<(i64, crate::Handle)> = None;
            for (h, b) in world.buildings.iter() {
                if b.house == e && b.is_alive() && !b.is_wall {
                    let d = sq_dist(b.center_cell(), base);
                    if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                        best = Some((d, h));
                    }
                }
            }
            if let Some((_, h)) = best {
                return Some(Target::Building(h));
            }
        }
        self.nearest_enemy_target(world, base)
    }

    /// Summed defense strength of `house`'s armed, non-wall buildings within
    /// [`SECTOR_THREAT_RADIUS`] cells of `at` — a simplified port of
    /// `HouseClass::Adjust_Threat`'s region threat accumulation (house.cpp:2475),
    /// collapsed to a single local sum. Used to route attacks toward the enemy
    /// base's weakest-defended production building (M7.11 P1b).
    fn sector_threat(&self, world: &World, house: u8, at: CellCoord) -> i64 {
        let cat = &world.catalog;
        let mut threat = 0i64;
        for (_, b) in world.buildings.iter() {
            if b.house == house
                && b.is_alive()
                && !b.is_wall
                && cat
                    .building(b.type_id)
                    .map(|p| p.weapon.is_some())
                    .unwrap_or(false)
                && cell_distance(b.center_cell(), at) <= SECTOR_THREAT_RADIUS
            {
                threat += b.health as i64;
            }
        }
        threat
    }

    /// The nearest enemy harvester to our base (harassment target), if any.
    fn enemy_harvester(&self, world: &World) -> Option<Target> {
        let base = self.base_center(world);
        let mut best: Option<(i64, crate::Handle)> = None;
        for (h, u) in world.units.iter() {
            if u.house != self.house && u.is_harvester && u.is_alive() {
                let d = sq_dist(u.cell(), base);
                if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best = Some((d, h));
                }
            }
        }
        best.map(|(_, h)| Target::Unit(h))
    }

    /// The cell a target currently occupies (for staging direction).
    fn target_cell(&self, world: &World, target: Target) -> Option<CellCoord> {
        match target {
            Target::Unit(h) => world.units.get(h).map(|u| u.cell()),
            Target::Building(h) => world.buildings.get(h).map(|b| b.center_cell()),
            Target::Cell(c) => Some(c),
        }
    }

    // ---- Economic reflexes (M7.10 e) ---------------------------------------

    /// Repair damaged buildings, sell when broke, and fire-sale + all-out attack
    /// in the endgame (`HouseClass::Expert_AI` money/state block, house.cpp:5030 +
    /// `Repair_AI`/`AI_Raise_Money`/`Fire_Sale`/`Do_All_To_Hunt`).
    fn economic_reflexes(&mut self, world: &World, rng: &mut RandomLcg, out: &mut Vec<Command>) {
        // Fire-sale endgame first: if we can no longer produce anything (no
        // construction yard, war factory, or barracks) AND no MCV left to redeploy
        // one, but still hold buildings, the game is effectively lost — sell
        // everything and throw all forces at the enemy (`Check_Fire_Sale` →
        // `Fire_Sale` + `Do_All_To_Hunt`, house.cpp:5252/7622/7651). The MCV guard
        // is essential: at game start (before the starting MCV deploys) a house may
        // already hold a building yet have no factory — that is a *buildup* state,
        // not a lost cause, so it must not trigger the fire sale.
        let can_produce = self.has_construction_yard(world)
            || self.owns_role(world, Role::WarFactory)
            || self.owns_role(world, Role::Barracks);
        let recoverable = self.own_mcv(world).is_some();
        // `deployed` gates this to a house that once had a construction yard and
        // has since lost its production — the genuine lost-cause endgame. A house
        // that never established a base (e.g. a scenario/test house holding a lone
        // non-factory building) is a *buildup*, not an endgame, and must not
        // fire-sale itself into elimination.
        if self.deployed && !can_produce && !recoverable && self.has_any_building(world) {
            self.fire_sale_and_hunt(world, out);
            return;
        }

        let money = world.house_credits(self.house);
        let cat = &world.catalog;
        let iq = world.house(self.house).map(|h| h.iq).unwrap_or(0);

        // **RepairSell IQ gate (M7.14 audit P2c).** `Repair_AI` wraps both the
        // repair and the sell-back reflexes in `if (House->IQ >= Rule.IQRepairSell
        // && …)` (building.cpp:5829). A house below the threshold does neither; a
        // computer house (IQ = MaxIQ) is above stock `RepairSell=1`. Gate both our
        // repair and our sell-when-broke reflexes consistently.
        if iq < cat.econ.iq.repair_sell {
            return;
        }

        // Building auto-repair (`Repair_AI`, building.cpp:5834), now **faithfully
        // throttled** (M7.14 audit P1). Two fidelity fixes over the old band-aid:
        //
        // 1. **Real `CreditReserve` floor.** The repair-affordability floor is the
        //    stock `[AI] CreditReserve` (`cat.econ.ai.credit_reserve` — 100 on real
        //    assets, overriding `RepairThreshhold`, rules.cpp:724). The synthetic
        //    default stays 1000, so synthetic AI repair economics are unchanged.
        // 2. **`RepairTimer` cooldown.** We only *begin* a repair when the throttle
        //    is armed (`repair_timer == 0`, the `!DidRepair` gate), then re-arm it
        //    to a random multi-tick delay (`Repair_AI`, building.cpp:5842) drawn
        //    from the sync RNG. This is what makes `CreditReserve=100` safe: without
        //    it, the old reflex repaired the most-damaged building *every* pass,
        //    draining a symmetric Normal AI-vs-AI down to the floor and starving
        //    production (the exact deadlock the hardcoded 1000 papered over). The
        //    throttle repairs at most one building per cooldown window, so the
        //    economy recovers between repairs and stays decisive at the real floor.
        if self.repair_timer == 0 && money >= cat.econ.ai.credit_reserve {
            let mut worst: Option<(i32, crate::Handle)> = None;
            for (h, b) in world.buildings.iter() {
                if b.house == self.house && b.is_alive() && !b.is_wall && !b.is_repairing {
                    let ratio = b.health as i32 * 1000 / b.max_health.max(1) as i32;
                    if ratio < 1000 && worst.map(|(bw, _)| ratio < bw).unwrap_or(true) {
                        worst = Some((ratio, h));
                    }
                }
            }
            if let Some((_, building)) = worst {
                out.push(Command::Repair {
                    house: self.house,
                    building,
                });
                // Re-arm the throttle: `RepairTimer = Random_Pick(RepairDelay *
                // (TICKS_PER_MINUTE/4), RepairDelay * TICKS_PER_MINUTE * 2)`
                // (building.cpp:5842). `RepairDelay` is the difficulty handicap's
                // `.02` default (rules.cpp:316) — expressed here in hundredths for
                // integer/deterministic math, matching the reference `(int)(fixed *
                // int)` truncation.
                let low = REPAIR_DELAY_HUNDREDTHS * (TICKS_PER_MINUTE / 4) / 100;
                let high = REPAIR_DELAY_HUNDREDTHS * (TICKS_PER_MINUTE * 2) / 100;
                self.repair_timer = rng.range(low, high).max(1) as u32;
                return; // one economic action per pass
            }
        }

        // Sell-when-broke (`AI_Raise_Money`, house.cpp:5552 / `Check_Raise_Money`,
        // house.cpp:5283): when money is critically low AND we can't make more,
        // sell one **non-essential** building (defenses/tech/silos before the core
        // economy) to raise cash. Also gated on `IQ >= IQRepairSell` above.
        let can_make_money = self.owns_role(world, Role::Refinery) && self.has_harvester(world);
        if money < RAISE_MONEY_FLOOR && !can_make_money {
            if let Some(building) = self.least_essential_sellable(world) {
                out.push(Command::Sell {
                    house: self.house,
                    building,
                });
            }
        }
    }

    /// Sell every building and send every unit to attack — the lost-cause endgame.
    fn fire_sale_and_hunt(&self, world: &World, out: &mut Vec<Command>) {
        for (h, b) in world.buildings.iter() {
            if b.house == self.house && b.is_alive() {
                out.push(Command::Sell {
                    house: self.house,
                    building: h,
                });
            }
        }
        if let Some(target) = self.enemy_target(world) {
            for h in world.units.handles() {
                if let Some(u) = world.units.get(h) {
                    if u.house == self.house && u.weapon.is_some() && !u.is_harvester {
                        out.push(Command::Attack {
                            unit: h,
                            target,
                            house: self.house,
                        });
                    }
                }
            }
        }
    }

    /// The least-essential sellable building: a non-core structure (not the
    /// construction yard / refinery / war factory / barracks / power / wall),
    /// preferring the highest-index (usually a defense/tech add-on) so the core
    /// economy is kept until last (`AI_Raise_Money` priority order, house.cpp:5560).
    fn least_essential_sellable(&self, world: &World) -> Option<crate::Handle> {
        let cat = &world.catalog;
        let mut pick: Option<(usize, crate::Handle)> = None;
        for (h, b) in world.buildings.iter() {
            if b.house != self.house || !b.is_alive() || b.is_wall {
                continue;
            }
            let essential = b.is_construction_yard
                || b.is_refinery
                || b.is_war_factory
                || b.is_barracks
                || cat
                    .building(b.type_id)
                    .map(|p| p.power > 0)
                    .unwrap_or(false);
            if essential {
                continue;
            }
            let idx = b.type_id as usize;
            if pick.map(|(pi, _)| idx > pi).unwrap_or(true) {
                pick = Some((idx, h));
            }
        }
        pick.map(|(_, h)| h)
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

    /// Count of a house's live combat units (`HouseClass::CurUnits`, house.cpp).
    /// Harvesters are excluded — they are economy, not army (matching the intent
    /// of the rubber-band/enemy-size comparison, which is about fighting strength).
    fn army_size(&self, world: &World, house: u8) -> i32 {
        world
            .units
            .iter()
            .filter(|(_, u)| {
                u.house == house && u.is_alive() && !u.is_harvester && !u.is_infantry()
            })
            .count() as i32
    }

    /// Count of a house's live infantry (`HouseClass::CurInfantry`).
    fn infantry_size(&self, world: &World, house: u8) -> i32 {
        world
            .units
            .iter()
            .filter(|(_, u)| u.house == house && u.is_alive() && u.is_infantry())
            .count() as i32
    }

    /// Count of a house's live non-wall buildings (`HouseClass::CurBuildings`).
    fn building_size(&self, world: &World, house: u8) -> i32 {
        world
            .buildings
            .iter()
            .filter(|(_, b)| b.house == house && b.is_alive() && !b.is_wall)
            .count() as i32
    }

    /// A house's base-centre cell (construction yard, else any building, else its
    /// first unit, else the map centre) — the `Center` used in enemy scoring.
    fn base_center_of(&self, world: &World, house: u8) -> CellCoord {
        if let Some((_, b)) = world
            .buildings
            .iter()
            .find(|(_, b)| b.house == house && b.is_construction_yard && b.is_alive())
            .or_else(|| {
                world
                    .buildings
                    .iter()
                    .find(|(_, b)| b.house == house && b.is_alive())
            })
        {
            return b.center_cell();
        }
        world
            .units
            .iter()
            .find(|(_, u)| u.house == house && u.is_alive())
            .map(|(_, u)| u.cell())
            .unwrap_or(CellCoord::new(64, 64))
    }

    /// Whether the house owns any live building at all.
    fn has_any_building(&self, world: &World) -> bool {
        world
            .buildings
            .iter()
            .any(|(_, b)| b.house == self.house && b.is_alive())
    }

    /// Whether the house owns a live building of the given role.
    ///
    /// **Cache invariant.** This resolves through [`crate::House::owns_building`],
    /// i.e. the `building_counts` cache — see that method's warning. The cache is
    /// only correct when every building add/remove goes through the command/sim
    /// paths (`spawn_building` / `remove_building` / `capture_building`), which
    /// keep [`crate::House::adjust_building_count`] in step; do not bypass them.
    fn owns_role(&self, world: &World, role: Role) -> bool {
        self.role_building(&world.catalog, role)
            .map(|id| {
                world
                    .house(self.house)
                    .map(|h| h.owns_building(id))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Whether the house owns a live harvester (part of "can make money").
    fn has_harvester(&self, world: &World) -> bool {
        world
            .units
            .iter()
            .any(|(_, u)| u.house == self.house && u.is_harvester && u.is_alive())
    }

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

/// Whether a building is an **anti-air-only** emplacement (AGUN/SAM) the AI must
/// not pick as a *ground* defense — its projectile only hits aircraft, so against
/// the ground-only AI-vs-AI game it is a useless "defense". Identified by catalog
/// name (the §3.8 table-free role pattern), matching `world::building_is_aa`.
/// (P0 aircraft arc: AI-air is a documented cut, so the AI ignores AA buildings.)
fn is_air_only_defense(p: &BuildingProto) -> bool {
    matches!(p.name.as_str(), "AGUN" | "SAM")
}

/// A building role the AI shops for.
#[derive(Clone, Copy)]
enum Role {
    Power,
    Refinery,
    WarFactory,
    Barracks,
}

/// Build-choice urgency (`UrgencyType`, defines.h:663) — the priority a category
/// carries into `AI_Building`'s max-urgency selection. Ordered so `max`/`>`
/// compares correctly (`None < Low < Medium < High < Critical`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Urgency {
    #[allow(dead_code)]
    None,
    Low,
    Medium,
    High,
    #[allow(dead_code)]
    Critical,
}

/// Radius (cells) of the local defense-threat scan used to route attacks toward
/// the enemy base's weakest-defended production building (M7.11 P1b). A modest
/// sector around each candidate — large enough to feel a base-defense cluster,
/// small enough to distinguish the guarded side from the soft side.
const SECTOR_THREAT_RADIUS: i32 = 6;

/// Whether a building is a **production** building (the `QUARRY_FACTORIES` quarry,
/// `defines.h:2477`): a war factory, construction yard, or barracks. These are the
/// "focus and finish" priority targets (M7.11 P1c) — killing them stops the enemy
/// reinforcing.
fn is_production(_world: &World, b: &crate::building::Building) -> bool {
    b.is_war_factory || b.is_construction_yard || b.is_barracks
}

/// Cell distance between two cells (Chebyshev — the max of the axis deltas),
/// matching the original's `Distance` used for cell-space base scoring
/// (`house.cpp:4941`, where `Distance` collapses to the dominant axis).
fn cell_distance(a: CellCoord, b: CellCoord) -> i32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

/// Squared Euclidean cell distance — cheap nearest-target comparisons (no sqrt,
/// order-preserving).
fn sq_dist(a: CellCoord, b: CellCoord) -> i64 {
    let dx = (a.x - b.x) as i64;
    let dy = (a.y - b.y) as i64;
    dx * dx + dy * dy
}
