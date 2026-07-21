//! Static build data — the sim's `TypeClass` layer expressed as plain data
//! (DESIGN.md §3.8, §4.9 M5). This is the immutable catalog of what a house can
//! build: building and unit *prototypes* with their cost, footprint, power,
//! prerequisites, and the runtime stats a completed object is spawned with.
//!
//! Like [`crate::unit::MoveStats`] and [`crate::combat::WeaponProfile`], these
//! prototypes are **lifted from `ra-data`** (rules.ini + the code-defined
//! footprint table) by the client at load time and handed to the sim, so
//! `ra-sim` stays off the INI layer (§4.1). The catalog is immutable, so it is
//! not folded into the per-tick state hash.

use crate::combat::WeaponProfile;
use crate::unit::MoveStats;

/// The rules.ini `[IQ]` control table (`RulesClass::IQ`, `rules.cpp:IQ()`), which
/// gates *which* automatic behaviours a house is allowed to perform based on its
/// `IQ` rating. Each field is the **minimum IQ** at which the named ability turns
/// on; a house acts on ability `X` iff `house.iq >= iq.X`. Computer houses run at
/// `max_iq`; the human runs at `0` (`scenario.cpp:2890`, `house.cpp:7452`).
/// Defaults are the reference compile-time values (`rules.cpp:137-147`); the
/// real-asset loader overrides them from rules.ini `[IQ]` (where stock RA ships
/// `Harvester=2`, `RepairSell=1`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IqRules {
    /// Number of discrete IQ levels; computer houses run at this (`MaxIQLevels`).
    pub max_iq: i32,
    /// IQ to auto-fire super weapons (`SuperWeapons`).
    pub super_weapons: i32,
    /// IQ to auto-control building/unit production (`Production`).
    pub production: i32,
    /// IQ at which newly produced units start in **Guard Area** mode (`GuardArea`).
    pub guard_area: i32,
    /// IQ to auto repair/sell damaged buildings (`RepairSell`).
    pub repair_sell: i32,
    /// IQ to auto-crush crushable antagonists (`AutoCrush`).
    pub auto_crush: i32,
    /// IQ to **scatter from incoming threats** (grenades/artillery) — the gate on
    /// `CellClass::Incoming` (`Scatter`, `cell.cpp:2025`).
    pub scatter: i32,
    /// IQ to consider transport contents when target-picking (`ContentScan`).
    pub content_scan: i32,
    /// IQ to auto-replace aircraft (`Aircraft`).
    pub aircraft: i32,
    /// IQ to **auto-replace lost harvesters** (`Harvester`, `house.cpp:6075`).
    pub harvester: i32,
    /// IQ to sell buildings back (`SellBack`).
    pub sell_back: i32,
}

impl Default for IqRules {
    fn default() -> IqRules {
        // Reference compile-time defaults (rules.cpp:137-147).
        IqRules {
            max_iq: 5,
            super_weapons: 4,
            production: 5,
            guard_area: 4,
            repair_sell: 3,
            auto_crush: 2,
            scatter: 3,
            content_scan: 4,
            aircraft: 4,
            harvester: 3,
            sell_back: 2,
        }
    }
}

/// The rules.ini `[AI]` section (`RulesClass::AI`, `rules.cpp:AI()`) — the
/// base-composition ratio table and attack cadence that drive `HouseClass::
/// AI_Building`/`AI_Unit`. Ratios are stored as **raw 16.16 fixed** (a rules.ini
/// `.16` becomes `0.16 × 65536`); a category's desired count is
/// `Round_Up(ratio × CurBuildings)`, clamped to its `*_limit`. Defaults are the
/// reference compile-time values (`rules.cpp:94-115`); the loader overrides from
/// rules.ini.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiRules {
    /// Average minutes between computer attacks (`AttackInterval`, fixed raw).
    pub attack_interval: i32,
    /// Average delay before the first attack (`AttackDelay`, fixed raw).
    pub attack_delay: i32,
    /// Cash floor below which repair won't begin (`CreditReserve`; overrides
    /// `RepairThreshhold`).
    pub credit_reserve: i32,
    /// Build power plants until surplus ≥ this (`PowerSurplus`).
    pub power_surplus: i32,
    /// Computer base may be at most `largest human base + this` (`BaseSizeAdd`).
    pub base_size_add: i32,
    /// Ratio of base that should be refineries (`RefineryRatio`, fixed raw).
    pub refinery_ratio: i32,
    /// Hard cap on refineries (`RefineryLimit`).
    pub refinery_limit: i32,
    /// Ratio of base that should be barracks (`BarracksRatio`, fixed raw).
    pub barracks_ratio: i32,
    /// Hard cap on barracks (`BarracksLimit`).
    pub barracks_limit: i32,
    /// Ratio of base that should be war factories (`WarRatio`, fixed raw).
    pub war_ratio: i32,
    /// Hard cap on war factories (`WarLimit`).
    pub war_limit: i32,
    /// Ratio of base that should be defensive structures (`DefenseRatio`, fixed raw).
    pub defense_ratio: i32,
    /// Hard cap on defensive buildings (`DefenseLimit`).
    pub defense_limit: i32,
    /// Ratio of base that should be anti-air (`AARatio`, fixed raw). Folded into
    /// the defense category here (no aircraft threat modelled).
    pub aa_ratio: i32,
    /// Hard cap on anti-air (`AALimit`).
    pub aa_limit: i32,
    /// Ratio of base that should be tesla coils (`TeslaRatio`, fixed raw). Folded
    /// into the defense category (we pick the strongest buildable defense).
    pub tesla_ratio: i32,
    /// Hard cap on tesla coils (`TeslaLimit`).
    pub tesla_limit: i32,
    /// Ratio of base that should be helipads (`HelipadRatio`, fixed raw). Unused
    /// (no aircraft), parsed for completeness.
    pub helipad_ratio: i32,
    /// Hard cap on helipads (`HelipadLimit`).
    pub helipad_limit: i32,
    /// Power fraction below which the AI sells to raise power (`PowerEmergency`,
    /// fixed raw).
    pub power_emergency: i32,
}

impl Default for AiRules {
    fn default() -> AiRules {
        // Reference compile-time defaults (rules.cpp:94-115, 263).
        AiRules {
            attack_interval: 3 << 16,
            attack_delay: 5 << 16,
            // Reference RepairThreshhold default (rules.cpp:263). Stock rules.ini
            // overrides this to 100 via `CreditReserve`; the default is kept at
            // 1000 so synthetic catalogs keep the pre-M7.14 AI repair behaviour.
            credit_reserve: 1000,
            power_surplus: 50,
            base_size_add: 3,
            refinery_ratio: fixed_raw_hundredths(16), // .16
            refinery_limit: 4,
            barracks_ratio: fixed_raw_hundredths(16), // .16
            barracks_limit: 2,
            war_ratio: fixed_raw_hundredths(10), // .1
            war_limit: 2,
            defense_ratio: fixed_raw_hundredths(50), // .5
            defense_limit: 40,
            aa_ratio: fixed_raw_hundredths(14), // .14
            aa_limit: 10,
            tesla_ratio: fixed_raw_hundredths(16), // .16
            tesla_limit: 10,
            helipad_ratio: fixed_raw_hundredths(12), // .12
            helipad_limit: 5,
            power_emergency: (3 << 16) / 4, // .75
        }
    }
}

/// A rules.ini fixed value expressed in hundredths (e.g. `.16` → `16`) as raw
/// 16.16 fixed, for the [`AiRules`] ratio defaults.
const fn fixed_raw_hundredths(h: i32) -> i32 {
    (h * (1 << 16)) / 100
}

/// `Round_Up(ratio_raw × n)` (`common/fixed.h Round_Up`): the ceiling of a raw
/// 16.16 ratio times an integer count, used for the `[AI]` base-composition
/// desired counts (`Round_Up(Rule.*Ratio * fixed(CurBuildings))`, house.cpp:5765+).
pub fn round_up_fixed(ratio_raw: i32, n: i32) -> i32 {
    (((ratio_raw as i64 * n as i64) + 0xFFFF) >> 16) as i32
}

/// Economy constants, from rules.ini `[General]` (defaults are the RA stock
/// values, `redalert/rules.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EconRules {
    /// Credits per gold bail (`GoldValue`, rules.cpp:231, default 35).
    pub gold_value: i32,
    /// Credits per gem bail (`GemValue`, rules.cpp:232, default 110).
    pub gem_value: i32,
    /// Harvester capacity in bails (`BailCount`, rules.cpp:230, default 28).
    pub bail_count: u16,
    /// Ticks between harvest/dump steps (`OreDumpRate`, rules.cpp:175, default 2).
    pub ore_dump_rate: u16,
    /// Ticks per game minute (`TICKS_PER_MINUTE`, defines.h:3122 = 15*60 = 900).
    pub ticks_per_minute: i32,
    /// Production installment steps (`STEP_COUNT`, factory.h:118 = 54).
    pub step_count: i32,
    /// Global build-speed bias as a raw 16.16 fixed value (`Rule.BuildSpeedBias`,
    /// from `[General] BuildSpeed`, rules.cpp:464 — stock RA ships `.8`, so
    /// `0.8 × 65536 = 52428`). This scales *every* item's build time (a value
    /// below 1 makes everything build faster). The code default here is `1.0`
    /// (`65536`), matching the reference's compile-time default (`rules.cpp:261
    /// BuildSpeedBias(1)`) before rules.ini overrides it; the real-asset loader
    /// reads the `.8` from rules.ini. **This was the missing multiplier that made
    /// our builds run 25% too slow (M7.9 P0).**
    pub build_speed_bias_raw: i32,
    /// Long ore-scan radius in cells (`TiberiumLongScan/CELL`, rules.cpp:267 =
    /// 0x2000 leptons = 32 cells).
    pub long_scan_cells: i32,
    /// Short ore-scan radius in cells (`TiberiumShortScan/CELL`, rules.cpp:266 =
    /// 0x0600 leptons = 6 cells).
    pub short_scan_cells: i32,
    /// Sell refund as a percentage of build cost (`RefundPercent`, rules.cpp:258,
    /// default 50%). The refund is a flat fraction of cost, independent of the
    /// building's current health (`techno.cpp:6417`).
    pub refund_percent: i32,
    /// Ore growth/spread map-sweep period in minutes (`GrowthRate`, rules.cpp:198,
    /// default 2). One grow+spread wave fires per full 128×128 sweep, so the scan
    /// processes `MAP_CELL_TOTAL / (growth_rate · TICKS_PER_MINUTE)` cells/tick.
    pub growth_rate: i32,
    /// Difficulty stat-handicap table (M7.9 P2a), indexed by
    /// [`crate::ai::Difficulty`] `as usize` (`Easy=0, Normal=1, Hard=2`). Loaded
    /// from rules.ini's `[Easy]/[Normal]/[Difficult]` sections by the client;
    /// defaults to three **neutral** (all-`1.0`) handicaps, so every synthetic
    /// catalog and its goldens are unaffected. (Kept here — flowing through
    /// [`EconRules::default`] — so adding it did not disturb the ~20 hand-built
    /// `Catalog { … }` literals across the test suites.) The label→section mapping
    /// is in [`Catalog::difficulty_handicap`] (a "Hard" AI is the *strong* one).
    pub difficulty: [crate::house::Handicap; 3],
    /// Building self-repair HP restored per step (`Rule.RepairStep`, from
    /// `[General] RepairStep`). Reference **compile-time** default is `5`
    /// (`rules.cpp:221`), but the stock `redalert.mix` rules.ini overrides it to
    /// `7`; we keep the *stock-rules.ini* value as our default (matching the
    /// module constant this replaced — M7.9.1 audit / Q14) so every synthetic
    /// catalog's repair behaviour stays byte-identical, and the real-asset loader
    /// re-reads it from rules.ini. Promoted here (M7.5 P0) so the four repair
    /// numbers can't silently drift in code the way they did once already.
    pub brepair_step: i32,
    /// Building repair cost fraction (`Rule.RepairPercent`) as `num/den`. Stock
    /// rules.ini `20%` (= `1/5`); reference compile-time default `1/4`.
    pub brepair_percent_num: i32,
    /// Denominator for [`EconRules::brepair_percent_num`].
    pub brepair_percent_den: i32,
    /// Service-depot (FIX) **unit** repair HP per step (`Rule.URepairStep`, from
    /// `[General] URepairStep`). Stock rules.ini `10`; reference compile-time
    /// default `5`.
    pub urepair_step: i32,
    /// Unit repair cost fraction (`Rule.URepairPercent`) as `num/den`. Stock
    /// rules.ini `20%` (= `20/100`); reference compile-time default `1/4`.
    pub urepair_percent_num: i32,
    /// Denominator for [`EconRules::urepair_percent_num`].
    pub urepair_percent_den: i32,
    /// The `[IQ]` control table (M7.14 P0) — the minimum-IQ thresholds gating each
    /// automatic house behaviour (scatter, harvester replacement, guard-area, …).
    pub iq: IqRules,
    /// The `[AI]` base-composition + cadence table (M7.14 P1) — the ratios/limits
    /// that drive ratio-based `AI_Building`.
    pub ai: AiRules,
    /// The **incoming-projectile scatter threshold** (`Rule.Incoming`, from
    /// `[General] Incoming`, rules.cpp:477 `Get_MPHType`, default `MPH_IMMOBILE=0`).
    /// A weapon whose projectile speed is *below* this value lets the units in its
    /// target cell run away when it fires — the artillery/grenade-dodge reflex
    /// (`Fire_At` → `Map[As_Cell(TarCom)].Incoming(Coord,true)`, infantry.cpp:3841;
    /// cell.cpp:2013, IQ-gated). Stored on the **same 0..255 `scale_to_256` scale**
    /// as [`crate::combat::WeaponProfile::proj_speed`] so the two compare directly.
    /// The compile-time default `0` disables the reflex entirely (no weapon is
    /// slower than 0), so every synthetic catalog is byte-identical; the real-asset
    /// loader reads the stock `Incoming=10`. **Not hashed** (a `Catalog` constant).
    pub incoming_speed: i32,
}

impl Default for EconRules {
    fn default() -> EconRules {
        EconRules {
            gold_value: 35,
            gem_value: 110,
            bail_count: 28,
            ore_dump_rate: 2,
            ticks_per_minute: 900,
            step_count: 54,
            build_speed_bias_raw: 1 << 16, // 1.0 (reference compile-time default)
            long_scan_cells: 32,
            short_scan_cells: 6,
            refund_percent: 50,
            growth_rate: 2,
            difficulty: [crate::house::Handicap::default(); 3],
            // Stock-rules.ini repair values (not the reference compile-time
            // 5/25% defaults) — see the field docs. Kept equal to the module
            // constants this promotion replaced so synthetic-catalog repair is
            // byte-identical.
            brepair_step: 7,
            brepair_percent_num: 1,
            brepair_percent_den: 5,
            urepair_step: 10,
            urepair_percent_num: 20,
            urepair_percent_den: 100,
            iq: IqRules::default(),
            ai: AiRules::default(),
            // MPH_IMMOBILE (defines.h:1104) — the reflex is off until rules.ini
            // sets a real `Incoming=`, so synthetic catalogs never scatter.
            incoming_speed: 0,
        }
    }
}

/// A buildable building type (footprint + stats + prerequisites).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildingProto {
    /// Human-facing short name (rules.ini section), e.g. `"POWR"`.
    pub name: String,
    /// Footprint width in cells (from the code-defined BSIZE table).
    pub foot_w: u8,
    /// Footprint height in cells.
    pub foot_h: u8,
    /// Max strength / hit points (`Strength=`).
    pub max_health: u16,
    /// Armor class index (`Armor=`).
    pub armor: u8,
    /// Net power: positive = output, negative = drain (`Power=`).
    pub power: i32,
    /// Build cost in credits (`Cost=`).
    pub cost: i32,
    /// Prerequisite building type ids (indices into
    /// [`Catalog::buildings`]) that the house must already own.
    pub prereq: Vec<u32>,
    /// This is a Tiberium refinery (PROC): a harvester dock, and it spawns a
    /// free harvester when built (`building.cpp:2640`).
    pub is_refinery: bool,
    /// This is a construction yard (CONST/FACT): produces buildings, and is what
    /// an MCV deploys into.
    pub is_construction_yard: bool,
    /// This is a war factory (WEAP): produces vehicles.
    pub is_war_factory: bool,
    /// This is a barracks (TENT/BARR): produces infantry (a separate strip from
    /// the war factory, M7.6).
    pub is_barracks: bool,
    /// The unit-proto index of the free harvester a refinery spawns (if
    /// `is_refinery`).
    pub free_harvester_unit: Option<u32>,
    /// Sight range in cells (`Sight=`) — reveals the shroud on placement (M6).
    pub sight: u8,
    /// Client sprite index for this building (opaque to the sim).
    pub sprite_id: u32,
    /// Defensive weapon (`Primary=`), if this is a combat building (PBOX/GUN/
    /// TSLA/…). `None` for ordinary structures. (M7.7 Chunk B)
    pub weapon: Option<WeaponProfile>,
    /// Whether the building aims an independently-rotating turret (GUN) vs. a
    /// fixed emplacement that fires along a static facing (PBOX/TSLA/FTUR).
    pub has_turret: bool,
    /// Whether the weapon "charges up" before firing (`Charges=yes` — the tesla
    /// coil): a fixed delay, then an instant bolt.
    pub charges: bool,
    /// Whether this "building" is a **wall** segment (SBAG/CYCL/BRIK): a 1×1
    /// buildable that blocks movement and is attackable, but does not count as a
    /// base structure (win/lose, AI base). Modeled as a 1×1 building rather than a
    /// separate overlay layer — see QUIRKS Q9.
    pub is_wall: bool,
    /// Credit **storage** this structure provides (`Storage=`) — refineries and
    /// silos. A house's spendable-credit cap is the sum over its live buildings;
    /// harvest income beyond the cap is wasted (`HouseClass::Harvested`,
    /// `house.cpp:80`). `0` for non-storage buildings (M7.7 Chunk C).
    pub storage: i32,
}

/// A buildable unit type (vehicle) with the runtime stats it spawns with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitProto {
    /// Human-facing short name (rules.ini section), e.g. `"2TNK"`.
    pub name: String,
    /// Client sprite index (becomes `Unit::type_id`).
    pub sprite_id: u32,
    /// Max strength (`Strength=`).
    pub max_health: u16,
    /// Movement stats.
    pub stats: MoveStats,
    /// Armor class index.
    pub armor: u8,
    /// Primary weapon (None = unarmed, e.g. HARV/MCV).
    pub weapon: Option<WeaponProfile>,
    /// Secondary weapon (`Secondary=`), if any — e.g. the mammoth tank's
    /// anti-infantry/air missiles. The sim selects primary vs. secondary per
    /// target armor at fire time (`What_Weapon_Should_I_Use`).
    pub secondary: Option<WeaponProfile>,
    /// Whether it aims an independent turret.
    pub has_turret: bool,
    /// Whether this unit is a harvester (drives the harvest FSM).
    pub is_harvester: bool,
    /// Whether this unit is infantry (occupies a sub-cell spot, built by the
    /// barracks, M7.6). Vehicles are `false`.
    pub is_infantry: bool,
    /// Ground locomotor index (`SPEED_FOOT`=0/`TRACK`=1/`WHEEL`=2) for
    /// per-land passability. Defaults to Track (0 would be Foot, so this is a
    /// meaningful default only paired with `is_infantry`).
    pub locomotor: u8,
    /// The building-proto index this unit deploys into (MCV → CONST), if any.
    pub deploys_to: Option<u32>,
    /// Build cost in credits (`Cost=`).
    pub cost: i32,
    /// Prerequisite building type ids the house must own to build it.
    pub prereq: Vec<u32>,
    /// Sight range in cells (`Sight=`) — reveals the shroud as the unit moves (M6).
    pub sight: u8,
    /// Passenger capacity (`Passengers=`, `udata.cpp`). Non-zero makes this a
    /// transport (APC); 0 for everything else. Drives the Load/Unload commands
    /// (M7.5-B P1).
    pub passengers: u8,
    /// Ammunition capacity for **aircraft** (`Ammo=`, `Class->MaxAmmo`,
    /// `techno.cpp:7066`). Combined with `locomotor == Air`, a produced/placed
    /// aircraft spawns with this many rounds and rearms at a helipad when empty. `0`
    /// for ground units (no magazine). (P0 aircraft arc.)
    pub ammo: u16,
}

/// The immutable catalog handed to [`crate::World`] at construction.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// Building prototypes, indexed by building type id.
    pub buildings: Vec<BuildingProto>,
    /// Unit prototypes, indexed by unit-proto id.
    pub units: Vec<UnitProto>,
    /// Economy constants.
    pub econ: EconRules,
}

impl Catalog {
    /// An empty catalog with default economy rules (movement/combat-only worlds).
    pub fn new() -> Catalog {
        Catalog {
            buildings: Vec::new(),
            units: Vec::new(),
            econ: EconRules::default(),
        }
    }

    /// Borrow a building prototype by id.
    pub fn building(&self, id: u32) -> Option<&BuildingProto> {
        self.buildings.get(id as usize)
    }

    /// Borrow a unit prototype by id.
    pub fn unit(&self, id: u32) -> Option<&UnitProto> {
        self.units.get(id as usize)
    }

    /// The stat handicap for an AI house at difficulty `d` (M7.9 P2a). The table
    /// is indexed by our [`crate::ai::Difficulty`] and the client loads it so that
    /// a **stronger label gets the buffs**: an AI at `Hard` receives rules.ini's
    /// `[Easy]` biases (FirePower 1.2, ROF .8, Cost/BuildTime .8 — the "buffed"
    /// handicap), while `Easy` receives `[Difficult]` (FirePower .8, ROF 1.2). See
    /// QUIRKS for why the RA sections invert vs. their names for AI opponents.
    pub fn difficulty_handicap(&self, d: crate::ai::Difficulty) -> crate::house::Handicap {
        self.econ.difficulty[d as usize]
    }

    /// Raw `TechnoTypeClass::Time_To_Build` result **T** in ticks, *before* the
    /// factory's STEP_COUNT rate conversion (`techno.cpp:6777`):
    ///
    /// ```text
    /// time = Cost * Rule.BuildSpeedBias * fixed(TICKS_PER_MINUTE, 1000)
    /// ```
    ///
    /// The original evaluates this with the `fixed` class's `int * fixed`
    /// operators, each of which **rounds to nearest** on the way back to `int`
    /// (`common/fixed.h`, `operator unsigned`). We reproduce that exactly with
    /// integer 16.16 math ([`fx_mul_round`]) so there is no float and no drift:
    ///   * `t1 = round(Cost × BuildSpeedBias)`
    ///   * `T  = round(t1 × (TICKS_PER_MINUTE / 1000))`
    ///
    /// The stock `.8` bias makes `T = Cost × 0.72` (0.8 × 0.9). The per-house
    /// `BuildSpeedBias` (line 6790) and difficulty IQ slowdown (line 6815) are
    /// `×1` for a normal human house and handled by the caller when they are not.
    pub fn build_time_base(&self, cost: i32) -> i32 {
        let bias = self.econ.build_speed_bias_raw as i64;
        let t1 = fx_mul_round(cost, bias);
        // fixed(TICKS_PER_MINUTE, 1000) as a raw 16.16 ratio (matches the
        // reference's `fixed(int,int)` ctor: numerator·PRECISION/denominator).
        let tpm = self.econ.ticks_per_minute as i64 * (1i64 << 16) / 1000;
        fx_mul_round(t1, tpm)
    }

    /// Full build time in **sim ticks** for an item costing `cost`, applying the
    /// low-power `scale` (`scale_n/scale_d`, [`crate::house::House::build_time_scale`])
    /// and then the factory's STEP_COUNT rate conversion (`FactoryClass::Start`,
    /// `factory.cpp:432`):
    ///
    /// ```text
    /// time  = build_time_base × power_scale     // techno.cpp:6832 `time *= scale`
    /// rate  = Bound(time / STEP_COUNT, 1, 255)   // factory.cpp:439-440
    /// build = rate × STEP_COUNT
    /// ```
    ///
    /// The original factory advances one of `STEP_COUNT (=54)` stages every
    /// `rate` ticks (`StageClass`, `stage.h`), so it takes `rate × 54` ticks —
    /// which is why even a trivially cheap item never builds in under 54 ticks
    /// (`rate` floors to 1). Our production model advances one step per tick, so
    /// the returned value is the number of ticks the build takes. Passing
    /// `scale = (1, 1)` is the full-power case.
    pub fn time_to_build(&self, cost: i32, scale_n: i32, scale_d: i32) -> i32 {
        let t = self.build_time_base(cost) as i64;
        let d = scale_d.max(1) as i64;
        // `time *= scale`: `int *= fixed` rounds to nearest (fixed.h). For our
        // rational scale that is round(t·n/d) = (t·n + d/2) / d.
        let scaled = ((t * scale_n as i64 + d / 2) / d) as i32;
        let steps = self.econ.step_count.max(1);
        let rate = (scaled / steps).clamp(1, 255);
        rate * steps
    }
}

/// Multiply an integer `val` by a raw 16.16 fixed `fx_raw`, rounding the result
/// to the nearest integer — the exact behaviour of the reference `fixed` class's
/// `int * fixed` operators (`fixed(val) *= fx; (unsigned)result`), which round
/// via `(raw + PRECISION/2) / PRECISION` (`common/fixed.h`). All inputs here are
/// non-negative (costs, times), so this is a plain round-half-up.
fn fx_mul_round(val: i32, fx_raw: i64) -> i32 {
    ((val as i64 * fx_raw + (1i64 << 15)) >> 16) as i32
}
