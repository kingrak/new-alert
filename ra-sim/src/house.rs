//! Houses in the sim (DESIGN.md §4.9 M5): credits, power, owned-building counts,
//! and the production factories. Ownership of *units* already lives on the unit
//! (`Unit::house`); this is the per-house economic state the original keeps on
//! `HouseClass` (`house.h`).
//!
//! Houses live in a `Vec<House>` indexed by house id, iterated in index order
//! (never hashed) per the determinism contract (§4.2).

use crate::hash::Fnv1a;

/// Which slot a production occupies. A house runs at most one structure build
/// and one vehicle build at a time (M5 simplification of the original's
/// per-factory queues; the sidebar exposes exactly these two lanes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProdKind {
    /// Structures (built by the construction yard, then placed).
    Building,
    /// Vehicles (built by the war factory, spawned at its exit).
    Unit,
    /// Infantry (built by the barracks, spawned at its exit) — a separate strip
    /// from vehicles, exactly as the original builds infantry independently of
    /// the war factory (`factory.cpp` per-`RTTI` factory queues).
    Infantry,
}

/// What is being produced: a building type id or a unit-proto id (indices into
/// the [`crate::catalog::Catalog`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildItem {
    /// A structure, by building type id.
    Building(u32),
    /// A vehicle, by unit-proto id.
    Unit(u32),
}

/// One in-progress production (a `FactoryClass`, `factory.cpp`, expressed as
/// data). Cost is paid in installments as progress advances, so total credits
/// spent equals `cost` exactly (`FactoryClass::AI`, `factory.cpp:203-227`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Production {
    /// The item being built.
    pub item: BuildItem,
    /// Full cost in credits.
    pub cost: i32,
    /// Total build time in ticks (`Time_To_Build`), **including the low-power
    /// multiplier snapshotted at production start** (see `crate::world`). The
    /// original bakes this into the factory Rate once in `FactoryClass::Start`
    /// (`factory.cpp:432-442`) and never recomputes it mid-build.
    pub total_ticks: i32,
    /// Progress in ticks so far (0..=`total_ticks`).
    pub progress: i32,
    /// Credits already spent (installments paid).
    pub spent: i32,
    /// Completed and awaiting placement (buildings) / spawn (units).
    pub done: bool,
    /// On hold: the player suspended this build with a sidebar-cameo
    /// right-click (`FactoryClass::Suspend`, factory.cpp:410 — `IsSuspended`
    /// set, `Set_Rate(0)`). A paused lane makes no progress and pays no
    /// installments until resumed (`FactoryClass::Start`, factory.cpp:439)
    /// or abandoned. Defaults to `false` (M7.21).
    pub paused: bool,
}

impl Production {
    /// Fraction complete in permille (0..=1000), for the sidebar clock.
    pub fn progress_permille(&self) -> i32 {
        if self.done {
            return 1000;
        }
        (self.progress as i64 * 1000 / self.total_ticks.max(1) as i64) as i32
    }

    fn hash_into(&self, h: &mut Fnv1a) {
        match self.item {
            BuildItem::Building(id) => {
                h.write_u8(0);
                h.write_u32(id);
            }
            BuildItem::Unit(id) => {
                h.write_u8(1);
                h.write_u32(id);
            }
        }
        h.write_i32(self.cost);
        h.write_i32(self.total_ticks);
        h.write_i32(self.progress);
        h.write_i32(self.spent);
        h.write_u8(self.done as u8);
        // Hold flag (M7.21 P1): folded in ONLY while paused, appending no
        // bytes for a lane that has never been put on hold — so every prior
        // golden (nothing ever pauses) hashes byte-identically.
        if self.paused {
            h.write_u8(0x50);
        }
    }
}

/// The whole number `1.0` in raw 16.16 fixed (a neutral, no-op bias).
pub(crate) const FX_ONE: i32 = 1 << 16;

/// Round `val × bias` to the nearest integer, `bias` being a raw 16.16 fixed —
/// the reference `int * fixed` rounding (`common/fixed.h`). A `FX_ONE` bias is
/// the exact identity, so a neutral-handicap house computes byte-identically.
#[inline]
pub(crate) fn fx_mul(val: i32, bias: i32) -> i32 {
    ((val as i64 * bias as i64 + (1i64 << 15)) >> 16) as i32
}

/// A house's **difficulty stat handicap** — the `[Easy]/[Normal]/[Difficult]`
/// bias multipliers (`Difficulty_Get`, rules.cpp:307) that
/// `HouseClass::Assign_Handicap` (house.cpp:278) copies onto a house and every
/// object applies at the relevant computation site. Stored as raw 16.16 fixed.
/// All-`1.0` (the [`Default`]) is "no handicap" — a human on Normal and every
/// synthetic catalog — and is a byte-exact no-op, so it never perturbs a golden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handicap {
    /// Firepower bias: damage this house's weapons deal (`techno.cpp:3303`,
    /// `firepower = Attack * House->FirepowerBias`).
    pub firepower: i32,
    /// Armor bias: damage this house's objects *take* (`techno.cpp:4099`,
    /// `damage = damage * House->ArmorBias`) — >1 takes more, <1 takes less.
    pub armor: i32,
    /// Rate-of-fire bias: rearm delay after firing (`techno.cpp:3066`,
    /// `ROF * House->ROFBias`) — <1 fires faster.
    pub rof: i32,
    /// Groundspeed bias: movement speed + turn rate (`drive.cpp:648/1354`,
    /// `MaxSpeed/ROT * House->GroundspeedBias`).
    pub groundspeed: i32,
    /// Cost bias: money charged to build (`cell.cpp:2391`, `Cost * House->CostBias`).
    pub cost: i32,
    /// Build-time bias: the difficulty `BuildTime` factor folded into
    /// `Time_To_Build` (`Assign_Handicap` `BuildSpeedBias`, house.cpp:293).
    pub build_time: i32,
}

impl Default for Handicap {
    fn default() -> Handicap {
        Handicap {
            firepower: FX_ONE,
            armor: FX_ONE,
            rof: FX_ONE,
            groundspeed: FX_ONE,
            cost: FX_ONE,
            build_time: FX_ONE,
        }
    }
}

impl Handicap {
    /// Whether this handicap is the all-`1.0` no-op (so callers can skip both the
    /// arithmetic and the hashing).
    pub fn is_neutral(&self) -> bool {
        *self == Handicap::default()
    }

    fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.firepower);
        h.write_i32(self.armor);
        h.write_i32(self.rof);
        h.write_i32(self.groundspeed);
        h.write_i32(self.cost);
        h.write_i32(self.build_time);
    }
}

/// One house's economic state.
#[derive(Clone, Debug)]
pub struct House {
    /// Given credits (scenario start, sell refunds, captures) — the non-harvest
    /// pool. Kept separate from harvested `tiberium` so a house may *start* with
    /// more money than its storage capacity without the harvest cap wasting it
    /// (`HouseClass::Credits`, `house.cpp`). Spendable money is `available()`.
    pub credits: i32,
    /// Harvested ore in storage, capped at the house's `Capacity` (sum of
    /// building `Storage=`). Harvest income beyond the cap is wasted
    /// (`HouseClass::Tiberium` / `Harvested`, `house.cpp:1975`). `0` until the
    /// house harvests into a storage building (M7.7 Chunk C).
    pub tiberium: i32,
    /// Sum of positive building `Power=` (output).
    pub power_output: i32,
    /// Sum of `-power` for draining buildings (consumption).
    pub power_drain: i32,
    /// Count of live buildings owned, indexed by building type id (for
    /// prerequisite checks). Grows as building types are encountered.
    pub building_counts: Vec<u16>,
    /// In-progress structure build.
    pub building_prod: Option<Production>,
    /// In-progress vehicle build.
    pub unit_prod: Option<Production>,
    /// In-progress infantry build (the barracks strip, M7.6).
    pub infantry_prod: Option<Production>,
    /// A completed building type id awaiting a [`crate::Command::PlaceBuilding`].
    pub ready_building: Option<u32>,
    /// Difficulty stat handicap (M7.9 P2a). Neutral (all `1.0`) for a human on
    /// Normal and every synthetic catalog; set from the catalog's difficulty
    /// table when an AI is assigned to this house (`World::set_ai`).
    pub handicap: Handicap,
    /// Per-attacker kill tallies (M7.10, Expert_AI enemy scoring). Index =
    /// attacker house; `units_killed_by[A]` counts **this** house's units that
    /// house `A` has destroyed (`HouseClass::UnitsKilled`, house.cpp:4951), and
    /// `buildings_killed_by` the same for structures (`BuildingsKilled`,
    /// house.cpp:4950). Grown lazily. **Not folded into the sim hash** — it is
    /// deterministic derived state (driven entirely by the already-hashed combat)
    /// read only by the AI, so leaving it out keeps every combat golden (which has
    /// no AI to read it) byte-identical while staying same-seed reproducible.
    pub units_killed_by: Vec<u32>,
    /// See [`House::units_killed_by`] — the building counterpart.
    pub buildings_killed_by: Vec<u32>,
    /// The house that most recently dealt damage to this one
    /// (`HouseClass::LAEnemy`, house.cpp:4966 — the "last attacker" bonus in
    /// enemy scoring). `None` until first attacked. Not hashed (same rationale as
    /// the kill tallies).
    pub last_attacker: Option<u8>,
    /// The house's **IQ rating** (`HouseClass::IQ`, `house.cpp:7454`), gating which
    /// automatic behaviours it may perform against the `[IQ]` thresholds
    /// ([`crate::IqRules`]). A computer house runs at `Rule.MaxIQ`
    /// (`scenario.cpp:2890`); a human runs at `0` — set by [`crate::World::set_ai`]
    /// for skirmish AI houses (M7.14 P0). Defaults to `0`. Folded into the hash
    /// **only when non-zero**, so every human/synthetic house (iq 0) is
    /// byte-identical to the pre-M7.14 layout and only AI-bearing houses change.
    pub iq: i32,
}

impl House {
    /// A new house with `credits` starting cash and no power/buildings.
    pub fn new(credits: i32) -> House {
        House {
            credits,
            tiberium: 0,
            power_output: 0,
            power_drain: 0,
            building_counts: Vec::new(),
            building_prod: None,
            unit_prod: None,
            infantry_prod: None,
            ready_building: None,
            handicap: Handicap::default(),
            units_killed_by: Vec::new(),
            buildings_killed_by: Vec::new(),
            last_attacker: None,
            iq: 0,
        }
    }

    /// Record that house `attacker` destroyed one of this house's units.
    pub fn record_unit_killed_by(&mut self, attacker: u8) {
        let i = attacker as usize;
        if self.units_killed_by.len() <= i {
            self.units_killed_by.resize(i + 1, 0);
        }
        self.units_killed_by[i] = self.units_killed_by[i].saturating_add(1);
    }

    /// Record that house `attacker` destroyed one of this house's buildings.
    pub fn record_building_killed_by(&mut self, attacker: u8) {
        let i = attacker as usize;
        if self.buildings_killed_by.len() <= i {
            self.buildings_killed_by.resize(i + 1, 0);
        }
        self.buildings_killed_by[i] = self.buildings_killed_by[i].saturating_add(1);
    }

    /// Units of this house that house `attacker` has killed (0 if never).
    pub fn units_killed_by(&self, attacker: u8) -> u32 {
        self.units_killed_by
            .get(attacker as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Buildings of this house that house `attacker` has killed (0 if never).
    pub fn buildings_killed_by(&self, attacker: u8) -> u32 {
        self.buildings_killed_by
            .get(attacker as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Total spendable money = given credits + stored harvested tiberium
    /// (`HouseClass::Available_Money`, `house.cpp:2022`).
    pub fn available(&self) -> i32 {
        self.credits + self.tiberium
    }

    /// Spend `amount`, drawing from stored `tiberium` first, then `credits`
    /// (`HouseClass::Spend_Money`, `house.cpp`). Amounts beyond `available()`
    /// simply drive `credits` negative (callers gate on `available()` first).
    pub fn deduct(&mut self, amount: i32) {
        if amount <= self.tiberium {
            self.tiberium -= amount;
        } else {
            self.credits -= amount - self.tiberium;
            self.tiberium = 0;
        }
    }

    /// Book harvest `income` into storage, wasting anything beyond `capacity`
    /// (`HouseClass::Harvested`, `house.cpp:1975`). A house with **no** storage
    /// capacity (`capacity == 0`, e.g. synthetic test catalogs with no `Storage=`
    /// building) is left uncapped — the income is added to `credits` directly, so
    /// those economies (and their goldens) are byte-identical to pre-cap.
    pub fn add_harvest(&mut self, income: i32, capacity: i32) {
        if capacity <= 0 {
            self.credits += income;
        } else {
            self.tiberium = (self.tiberium + income).min(capacity);
        }
    }

    /// Reconcile stored `tiberium` against a (possibly reduced) storage
    /// `capacity`, discarding any excess. Mirrors the original recomputing
    /// `House->Capacity` the moment a storage building is added or removed and
    /// clamping `House->Tiberium` to it (`HouseClass::Silo_Redraw` /
    /// `Adjust_Capacity`): the over-cap remainder is **lost**, never refunded to
    /// credits. Returns the amount wasted. No-op when nothing overflows.
    pub fn reconcile_capacity(&mut self, capacity: i32) -> i32 {
        if capacity >= 0 && self.tiberium > capacity {
            let wasted = self.tiberium - capacity;
            self.tiberium = capacity;
            wasted
        } else {
            0
        }
    }

    /// Whether the house owns at least one live building of type `id`.
    ///
    /// **Cache invariant (read before you trust this).** This reads the
    /// [`House::building_counts`] cache, which is *only* kept in sync by the
    /// building lifecycle paths that call [`House::adjust_building_count`]:
    /// [`crate::World::spawn_building`] (+1), `remove_building`/sell/destroy (−1),
    /// and `capture_building` (−1 old owner / +1 new). Mutating `buildings`
    /// arena membership by any *other* route (e.g. a test poking the arena
    /// directly) will desync this count from reality. Always add/remove buildings
    /// through the command / sim paths so the cache stays correct.
    pub fn owns_building(&self, id: u32) -> bool {
        self.building_counts
            .get(id as usize)
            .map(|&c| c > 0)
            .unwrap_or(false)
    }

    /// Adjust the owned-building count for type `id` by `delta` (+1 place, −1
    /// destroy), growing the vector as needed.
    pub fn adjust_building_count(&mut self, id: u32, delta: i32) {
        let i = id as usize;
        if self.building_counts.len() <= i {
            self.building_counts.resize(i + 1, 0);
        }
        let c = self.building_counts[i] as i32 + delta;
        self.building_counts[i] = c.max(0) as u16;
    }

    /// Power supply fraction as an integer ratio `(num, den)` in `[0,1]`
    /// (num/den), matching `HouseClass::Power_Fraction` (`house.cpp:4423`):
    /// `1/1` when output ≥ drain or drain is zero, else `output/drain`.
    pub fn power_fraction(&self) -> (i32, i32) {
        if self.power_output >= self.power_drain || self.power_drain == 0 {
            (1, 1)
        } else {
            (self.power_output.max(0), self.power_drain)
        }
    }

    /// Whether the house is low on power (output below drain).
    pub fn low_power(&self) -> bool {
        self.power_drain > 0 && self.power_output < self.power_drain
    }

    /// The discrete low-power build-time multiplier as an integer ratio
    /// `(num, den)`, to be snapshotted **once at production start** (not
    /// recomputed per tick). Port of the `Time_To_Build` power branch
    /// (`techno.cpp:6819-6831`): `power == 0 → ×4`, `< 1/2 → ×2.5`, `< 1 → ×1.5`,
    /// else `×1`, where `power` is [`House::power_fraction`] (`house.cpp:4423`),
    /// i.e. `min(1, output/drain)` (and `1` when `drain == 0`). Faithful to the
    /// original, which bakes this factor into the factory Rate in
    /// `FactoryClass::Start` and never revisits it mid-build (`factory.cpp:432`).
    pub fn build_time_scale(&self) -> (i32, i32) {
        let (pn, pd) = self.power_fraction();
        if pn >= pd {
            (1, 1) // full power (also the drain == 0 case, where power_fraction is (1,1))
        } else if pn == 0 {
            (4, 1)
        } else if pn * 2 < pd {
            (5, 2)
        } else {
            (3, 2)
        }
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.credits);
        // Harvested-tiberium storage is folded in ONLY when non-zero — a house
        // that never harvests into a storage building (every synthetic-catalog
        // economy: no `Storage=`, so `add_harvest` routes income to `credits`)
        // keeps `tiberium == 0` and appends no bytes, so those goldens are
        // byte-identical to the pre-cap hash.
        if self.tiberium != 0 {
            h.write_u8(0x71);
            h.write_i32(self.tiberium);
        }
        h.write_i32(self.power_output);
        h.write_i32(self.power_drain);
        h.write_u32(self.building_counts.len() as u32);
        for &c in &self.building_counts {
            h.write_u16(c);
        }
        match &self.building_prod {
            Some(p) => {
                h.write_u8(1);
                p.hash_into(h);
            }
            None => h.write_u8(0),
        }
        match &self.unit_prod {
            Some(p) => {
                h.write_u8(1);
                p.hash_into(h);
            }
            None => h.write_u8(0),
        }
        match self.ready_building {
            Some(id) => {
                h.write_u8(1);
                h.write_u32(id);
            }
            None => h.write_u8(0),
        }
        // Infantry lane (M7.6). Folded ONLY when present, appending no bytes when
        // absent — so every M5/M6/M7 economy golden (which never runs an infantry
        // build) hashes byte-identically. Same inertness argument as the unit
        // sub-cell field.
        if let Some(p) = &self.infantry_prod {
            h.write_u8(0x1F);
            p.hash_into(h);
        }
        // Difficulty handicap (M7.9 P2a). Folded in ONLY when non-neutral, so a
        // human-on-Normal / synthetic-catalog house (all biases 1.0) appends no
        // bytes and hashes identically to every pre-M7.9 golden.
        if !self.handicap.is_neutral() {
            h.write_u8(0x2A);
            self.handicap.hash_into(h);
        }
        // House IQ (M7.14 P0). Folded in ONLY when non-zero, so every human /
        // synthetic house (iq 0) appends no bytes and hashes identically to the
        // pre-M7.14 layout; only AI-bearing houses (iq = MaxIQ) change.
        if self.iq != 0 {
            h.write_u8(0x1B);
            h.write_i32(self.iq);
        }
    }
}
