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
        }
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

    /// Whether the house owns at least one live building of type `id`.
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
    }
}
