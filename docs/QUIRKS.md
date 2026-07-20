# QUIRKS — bug-for-bug / behavioral-divergence log

Per DESIGN.md §5 ("Bug-for-bug rules compat: keep a `QUIRKS.md` log of each
case"). Each entry records a place where our behavior deliberately matches an
original-engine quirk, or deliberately diverges from it, with the reasoning and
the reference-source citation so the decision is auditable.

---

## Q1 — Refinery destroyed while a harvester is unloading

**Milestone:** M6 (harvester economy, first surfaced by ra-tester's M5 edge test
`refinery_removed_while_unloading_drops_the_pending_credit_and_goes_idle`).

**Our behavior.** The harvest FSM (`ra-sim/src/world.rs::process_harvester`)
runs a `house_has_refinery` guard *before* the state match every tick. If a
house's last refinery is destroyed while one of its harvesters is in
`Unloading`, the guard fires first and forces the harvester to `Idle` **before**
the `Unloading` arm's payout runs. Consequences:

- The cargo currently held is **retained** (`cargo`/`gold`/`gems` are not
  cleared) — it is not destroyed, just not yet cashed.
- **No credits are booked** for that load until a real unload *completes* at a
  live refinery. With no other refinery on the map, the harvester sits idle
  holding the load indefinitely.
- If the house owns another refinery, the FSM re-homes to it on the next
  `FindHome`/`HeadingHome` cycle and banks the load normally — no loss.

**What the original does.** In RA the harvester unloads through a radio/mission
protocol (`UnitClass::Mission_Harvest` → `MISSION_ENTER`, `unit.cpp:2898+`) and
books credits **incrementally as each bail is dumped** into the refinery
(`Credit_Load`/`Harvester_Dump_List`, `unit.cpp:5003`). If the refinery is
destroyed mid-dump, the radio contact breaks; whatever bails were already dumped
are already credited, and the harvester keeps the remaining cargo and re-seeks
another refinery (or idles if none).

**Divergence + decision.** Two differences, both benign:

1. *Payout granularity.* The original credits per-bail during the dump; we credit
   the whole load atomically on unload completion. So a mid-unload destruction
   forfeits the *timing* of the current load's payout but never destroys the
   cargo — the value is deferred to a future completed unload, not lost. Because
   our model has no partial "already-dumped" state, there is nothing to
   partially credit.
2. *Re-home vs idle.* Both engines retain the cargo and re-home to another
   refinery when one exists. They differ only in the single-refinery corner
   case, which is terminal in both (nowhere to cash the load).

We **document** this divergence rather than aligning it: staged per-bail
crediting would require modelling the refinery-side dump list and the radio
protocol (out of M6 scope), and the observable outcome — cargo retained, no
phantom credits, re-home when possible — is faithful enough. No credits are ever
created or destroyed by the edge case; the invariant that credits equal
completed-unload value holds.

**Revisit when:** M7+ adds staged/animated unloading or a second-refinery
re-home is exercised in real play against Vanilla Conquer.

---

## Q2 — Simultaneous elimination resolves to Defeat (intentional match)

**Milestone:** M7 (item 3b — audited against the reference, kept as-is).

**Our behavior.** `update_game_over` (`ra-sim/src/world.rs`) checks the tracked
player house first: `!house_alive(player) → Defeat`, and only then tests whether
every AI house is dead (`→ Victory`). So on a tick where the player's last asset
and the last AI's last asset are destroyed **together**, the player-defeat check
wins and the result is **Defeat**.

**What the original does.** Houses are processed in ascending index order each
frame (`LogicClass::AI`, `logic.cpp:427`), and defeat is booked per house inside
`HouseClass::MPlayer_Defeated` (`house.cpp:3801`), which sets that house's
`IsDefeated` **before** counting survivors. When two houses die the same frame,
both end that frame `IsDefeated`; `Tally_Score` then awards `Wins++` only to a
house that is *not* defeated (`house.cpp:4101`), so nobody wins, and the local
result is decided purely by `if (PlayerPtr->IsDefeated) PlayerLoses`
(`house.cpp:3963`). With the player defeated, that is a **loss**.

**Decision.** Our Defeat-first order produces the same observable outcome the
original does for a simultaneous elimination (the player loses if its own flag is
set), so we **keep it** and do not reorder the check. Documented here rather than
"fixed" because there is nothing to fix — the behaviors already agree. Note our
model has no ally grouping and no draw state; a genuine mutual-annihilation with
no player house tracked stays `Ongoing` (the check early-outs on
`player_house == None`), which only affects headless AI-vs-AI harnesses.

---

## Q3 — `compose()` is the debug surface; `compose_game()` is the game surface

**Milestone:** M7 (item 3c — documented and made explicit).

**Two distinct client render surfaces**, deliberately kept separate:

- **`AppCore::compose(viewport)`** — the *raw-terrain debug surface*. It takes a
  caller-supplied **map-space** rectangle (camera state is not read), paints the
  terrain base, and draws units on top. **No shroud, no ore overlay art, no
  buildings, no sidebar, no HUD.** This is what the map-sweep tests and the
  `dump` CLI use to exercise "every corner of the map" (§4.8 layer 2)
  independently of camera/game state, and what the M2/M3 golden frames pin.
  Changing it would churn those goldens for no gameplay reason, so it stays a
  minimal, stable, game-agnostic surface.

- **`AppCore::compose_game()`** — the *game surface* (the documented 1996 HUD).
  Camera-positioned, full viewport, with the layered pipeline: terrain → ore art
  → buildings → units/turrets/muzzle-flash/bullets → **client animation layer**
  (M7) → shroud → placement preview → drag box → sidebar (cameos + radar) →
  game-over banner → F1 controls overlay. `compose_camera()` dispatches here when
  the sidebar is enabled (game mode) and otherwise falls back to `compose()`.

**Decision.** We keep both rather than unifying: the debug surface's value is
precisely that it is *not* the game surface (stable goldens, camera-independent
sweeps). New game visuals land in `compose_game()` only. This split is the
render-side expression of the §4.2 sim-vs-cosmetic separation.

---

## Q4 — Splash damage is full friendly-fire; guard retaliation is smart-defense-on

**Milestone:** M7 (items 1 & 2 — documented deviations from a faithful port).

**Splash friendly-fire.** `explosion_damage` (`ra-sim/src/world.rs`, port of
`Explosion_Damage`, `combat.cpp:162`) damages **every** unit and building within
the 384-lepton blast radius except the firing unit itself (`object != source`,
`combat.cpp:203`) — allies included. This matches the original exactly (the
original spares only the source), and it is intentionally *not* softened to a
friendly-fire-immune model.

**Retaliation gating.** `assign_retaliation` wakes a damaged unit to fire back
at its attacker (`FootClass::Take_Damage → Assign_Target(source)`,
`foot.cpp:1189`). Two documented simplifications:
1. We retaliate only when the unit is **truly idle** (no target *and* no move
   path), so an explicit player Move/Attack order is never hijacked. The original
   also keeps an existing TarCom/NavCom, snapping out only of sticky modes.
2. `Is_Allowed_To_Retaliate` gates *human* houses behind `Rule.IsSmartDefense`
   (`techno.cpp:5641`); we enable retaliation for **all** houses (smart-defense
   on) so the player's guarding units fight back instead of standing and dying —
   the exact playtest complaint that motivated this item. The warhead-can-harm
   and AI threat-comparison gates are omitted; we require only that the retaliator
   is armed and the source is a live enemy unit.

---

## Q5 — Unit cell occupancy: one vehicle per cell, group dispersal, simplified blocker reaction

**Milestone:** M7.6 (coordinator scope additions — vehicle stacking + group move).

**Our behavior.** `move_units` (`ra-sim/src/world.rs`) maintains a per-tick
[`UnitGrid`] cache and enforces the original's cell-ownership rules:

- **One vehicle per cell.** A vehicle never moves onto a cell another vehicle
  occupies (`CellClass::Occupier` / `Can_Enter_Cell`, `unit.cpp:3400`). The
  guard validates the **actual landing cell** of each tick's straight-line step
  (not just the path's next cell), so a diagonal step cannot corner-clip an
  occupied neighbour. A `debug_assert` verifies movement never increases the
  vehicle-overlap count each tick (zero in a real game — a harness that
  deliberately spawns stacked units, e.g. the splash-armor tests, is tolerated).
- **Up to five infantry per cell**, one per sub-cell spot (see Q7).
- **Group dispersal.** A box-selected group ordered to one cell disperses to
  distinct nearby free cells (`Adjust_Dest` scatter, `unit.cpp`): `pick_dest`
  spirals out from the target, one vehicle per cell / up to five infantry per
  cell, so a tank group ends packed *adjacently*, not stacked.

**Simplifications vs. the original (documented deviations).**
1. **No ask-the-blocker-to-scatter radio protocol** (`drive.cpp` `MOVE_MOVING_BLOCK`
   → `Do_Uncloak`/radio). A vehicle blocked by another vehicle re-routes **around**
   it (`find_path_avoiding` — our A* ignores units, so a blocker in the straight
   path is routed around at drive time); if no detour helps this tick it simply
   **holds** and retries. A true 1-wide corridor with no detour just waits
   (rare; benign).

   **Head-on tie-break (M7.7 P0a).** Two vehicles of *exactly* identical speed
   meeting head-on in a passable-width corridor used to re-route in lock-step
   forever (both detour, both return, repeat) — the old `known_bug_symmetric…`
   test. `move_units` now breaks the symmetry deterministically by slot order:
   when the blocker is a *moving* vehicle with a **lower handle index**, the
   higher-index unit **yields** (holds one tick) instead of re-routing, so only
   the lower-index unit detours and the pair passes. This stands in for the
   original's implicit asymmetry (one unit made passive by the scatter request).
   A *parked* blocker never triggers a yield, so ordinary re-routing around
   stationary obstacles is unchanged. No RNG is introduced (determinism intact).
2. **Closest-free-spot centre fallback** uses the fixed `_sequence[0]` order rather
   than the RNG-picked `_alternate` row (`cell.cpp:1948`), avoiding a new sim-RNG
   draw in the movement path (determinism, and it keeps existing goldens' RNG
   sequence intact).
3. **No crushing; co-occupancy forbidden (M7.7 P0b).** Heavy vehicles do not
   crush infantry (`unit.cpp:Overrun`, deferred). Instead the cell-ownership
   guard is now symmetric: a **vehicle** may not enter a cell holding *any*
   infantry (`spot_bits & 0x1F != 0`), and an **infantryman** may not enter a
   cell holding a vehicle (`veh_other.is_some()`) — the movement gate in
   `move_units`, plus `dest_ok` at command/dispersal time. This is the
   no-crush reading of `Can_Enter_Cell` (`unit.cpp:3400`): an
   occupied-by-the-other-kind cell is impassable-equivalent, so the mover
   re-routes around it or holds. (Previously the gate only checked same-kind
   occupancy, so vehicles drove through infantry and vice versa; that gap —
   pinned by `subcell_suite`'s two "currently…unblocked" tests — is now closed
   and those tests assert the block.)

**Hash impact.** This is a real movement behavior change and legitimately moves
real-map movement goldens (re-pinned in `determinism.rs` / `ui_shroud_golden.rs`
with this citation). **Single-unit and non-colliding movement is byte-identical**
to pre-M7.6 (the advance is the original multi-waypoint step computed on a copy;
the gate/dispersal/re-route only fire on an actual collision), so synthetic
single-unit goldens are unchanged.

---

## Q6 — Land-type passability: impassability modelled, per-class speed deferred

**Milestone:** M7.6 (coordinator scope addition — real land types).

**Our behavior.** Passability is now **per-locomotor** (`Foot`/`Track`/`Wheel`),
replacing the M3 water-only stand-in. A cell's land type comes from its theater
tileset's per-icon **ColorMap control byte** run through the fixed 16-entry table
(`TemplateTypeClass::Land_Type`, `cdata.cpp:1011`); whether a locomotor may enter
is `Ground[land].Cost[speed] != 0` (`unit.cpp:3429`, `infantry.cpp:1568`), with
the cost percentages read from rules.ini `[Clear]/[Road]/[Water]/[Rock]/[Wall]/
[Ore]/[Beach]/[Rough]/[River]` `Foot=`/`Track=`/`Wheel=` (`rules.cpp:831`). So
rock/cliff/slope templates block **everything**, water/river block **ground** (all
three land locomotors), and infantry (Foot) vs vehicles (Track/Wheel) get their
genuinely different rules. Tanks are `Track`, jeep/APC/harvester `Wheel`, infantry
`Foot` (`udata.cpp:1301`, `idata.cpp:1081`).

**Deferrals (documented).**
1. **Speed *modifiers* per land class are not modelled** — only impassability
   (`cost == 0`). The `<100%` costs (Beach/Rough slowing vehicles, etc.) are
   collapsed to "passable"; every unit moves at full MPH on any drivable cell.
   The must-have (movement correctness — no driving over mountains/cliffs) is met.
2. **Wall overlays** (`SBAG`/`BRIK`/… → `LAND_WALL`, `odata.cpp`) ~~are not yet
   folded into the masks~~. **Closed in M7.7 Chunk B (see Q9):** walls are placed
   as 1×1 buildings whose footprint stamps the building-occupancy layer
   (`Passability::set_occupied`), so they block ground movement exactly like any
   structure — the mover routes around a wall or holds. This is the observable
   `LAND_WALL` behaviour (impassable to ground) without a separate overlay
   land-type row.
3. **Misparse safety:** a cell whose template has no ColorMap, or an unloaded
   template, or a clear sentinel, resolves to `Clear` (passable) — so a bad parse
   degrades to drivable rather than walling the map off.

**Hash impact.** Real-map pathing changes (units route around cliffs/water), so
real-map goldens that legitimately move are re-pinned with this citation. Synthetic
grids (built via `Passability::new` from a uniform mask) apply the same mask to all
three locomotors, so every synthetic movement golden is byte-identical.

---

## Q7 — Infantry: sub-cell spots first-class; prone/veterancy/death-variants deferred

**Milestone:** M7.6 (the milestone's core: soldiers + barracks).

**Our behavior.** Infantry live in the shared `Units` arena with a `kind`
discriminant (not a separate arena — DESIGN §4.3), so movement, combat, targeting,
retaliation, bullets, and selection treat them as first-class with no duplication
(matching the existing `is_harvester` capability pattern). Each infantryman
occupies one of **five sub-cell spots** — centre + 4 quadrants at the original's
`StoppingCoordAbs` lepton offsets (`const.cpp:282`) — tracked as a 5-bit occupancy
mask like `CellClass::Flag.Occupy` (`cell.h:207`); on arrival it settles into the
closest free spot (`Closest_Free_Spot`, `cell.cpp:1897`). Infantry pathfind
cell-to-cell over the same grid with the `Foot` locomotor and MPH speed from
rules.ini. E1 (M1Carbine), E2 (grenade), E3 (RedEye) fight through the existing
weapon/warhead/Verses path; `Armor=none` means a JEEP's SA machine gun does full
damage. The barracks (TENT) is a third production strip (`ProdKind::Infantry`),
independent of the war factory, matching the original's separate infantry queue.

**Deferrals (documented).**
1. **Prone/crawling** (`DO_PRONE`/`DO_CRAWL`) — infantry always move/fire upright;
   the prone speed/defence bonus is not modelled.
2. **No veterancy** (RA1 has none anyway).
3. **Death animations + `InfDeath` variants** (`DO_GUN_DEATH`/`EXPLOSION_DEATH`/…)
   — infantry are removed from the arena on death like vehicles; the client draws
   the shared explosion, not the per-warhead infantry death SHP band.
4. **Arcing grenade → straight flight.** ~~E2's grenade is `Arcing` in rules.ini;
   the projectile flies the straight flat-trajectory path.~~ **Closed in M7.7
   (P1 arcing pass):** E2's grenade (`Lobbed` projectile, `Arcing=yes`) now flies
   the real ballistic lob (`bullet.cpp:809/838` + `object.cpp:233`) like ARTY's
   155mm — a height/riser parabola sized to land at the impact point. The impact
   point and damage are unchanged, so combat outcomes are identical; only the
   in-flight trajectory (and its render arc) changed. See Q8.
5. **Vehicle/infantry cell coexistence forbidden (updated M7.7):** vehicles and
   infantry no longer co-occupy a cell at all — the mover re-routes/holds rather
   than crushing or stacking (see Q5.3). Turrets: infantry are correctly
   turretless (`has_turret=false`, M7.7 P0c) — they aim by rotating their body,
   matching `udata.cpp` (`is_turret_equipped=false` for every infantry type).

---

## Q8 — Ground-roster completion: dual weapons, arcing lobs, and deferred vehicle abilities

**Milestone:** M7.7 Chunk A (P1 — 3TNK/4TNK/ARTY/V2RL/APC/TRUK/MNLY).

**Dual weapons (`Secondary=`).** The mammoth tank (4TNK) carries a 120mm AP cannon
(`Primary`) *and* MammothTusk HE missiles (`Secondary`); 3TNK's `Secondary` equals
its `Primary` (105mm). The sim picks primary vs. secondary per shot from a port of
`TechnoClass::What_Weapon_Should_I_Use` (`techno.cpp:360`): score each weapon by its
warhead's `Verses[target_armor]` modifier (doubled when already in range), take the
secondary only when it *strictly* outscores the primary. So a mammoth uses its cannon
(AP, high vs. heavy) against tanks and its missiles (HE, high vs. none) against
infantry — entirely from the `Verses` table, no per-unit special-case. Verified: the
4TNK fires Damage-40 (120mm) at a heavy tank and Damage-75 (MammothTusk) at infantry.
`Unit::secondary` is a type constant (like `locomotor`/`sight`), so it is **not** hashed
— single-weapon units and all pre-M7.7 goldens are byte-identical.

**Arcing / ballistic projectiles.** ARTY's 155mm (`Ballistic` projectile, `Arcing=yes`)
now flies a real parabolic lob: horizontal speed `MaxSpeed + Distance/32` (min 25) and a
launch `riser` of `((Distance/2)/(speed+1))*Gravity` (min 10, `Gravity=3`), integrated
each tick (`height += riser; riser -= Gravity`) — a port of `bullet.cpp:809/838` +
`object.cpp:233`. The shell detonates at its pre-computed impact point (horizontal path
unchanged), the `height` tracing an arc that returns to ~0 on arrival; the client lifts
the sprite by `height` and draws a ground shadow. E2's grenade shares this path (closes
Q7.4 #4). Arcing state is hashed **only** for arcing bullets, so straight/hitscan goldens
are byte-identical. **Bonus fix:** `Bullet::advance` now forces a 1-lepton step when the
truncated per-axis division underflows to zero, eliminating the `speed==1` non-axis-aligned
stall (the old `known_bug_speed_one` ignore is un-ignored and passing; inert for every real
weapon, which all resolve `proj_speed >= 12`).

**V2RL is NOT arcing.** The coordinator grouped ARTY+V2RL under "arcing", but rules.ini is
the source of truth (DESIGN §3.8): V2RL's `SCUD` weapon uses the `FROG` projectile, which
has **no** `Arcing` flag (`High=yes, Rotates=yes` — a fast, high-flying straight rocket).
We follow the data: V2RL fires straight, ARTY arcs. Documented rather than forced to arc.

**Deferred vehicle abilities (behaviour QUIRKs).**
1. **APC — armed transport, no passenger loading.** The APC spawns with its M60mg and
   fights as an armed vehicle, but the **transport/passenger system is deferred** (no
   load/unload, no capacity). It is, for now, just a fast armoured gun platform. The
   original's `Mission_Unload`/cargo hold (`unit.cpp`) is out of Chunk A scope.
2. **MNLY — plain vehicle, mine-laying deferred.** The minelayer is a buildable vehicle
   with no weapon and no mine-laying (`LaysMines` capability, `Ammo=5` mines, deferred).
   It drives and dies like any vehicle but lays nothing. (The mine/overlay system does
   not exist yet.)
3. **TRUK — unarmed supply truck.** Buildable, drivable, unarmed; the supply/convoy money
   mechanic is not modelled (it is `TechLevel=-1` — not normally player-buildable in the
   original either, surfaced here to complete the roster).

**AI production.** The AI's `AI_Unit` table now weighs each buildable non-harvester vehicle
**20 if armed, else 1** (`house.cpp:6172`), so the new armed vehicles (3TNK/4TNK/ARTY/V2RL/
APC) join the weighted-random pool and the unarmed TRUK/MNLY are built rarely. AI-vs-AI
still reaches decisive outcomes.

---

## Q9 — Defense buildings and walls-as-1×1-buildings

**Milestone:** M7.7 Chunk B (P2 defenses + P3 walls).

**Defenses (PBOX/HBOX/GUN/FTUR/TSLA).** Combat buildings fire through the *same*
bullet path as units. A new `run_building_combat` system (tick order: after unit
`run_combat`, before movement) gives each armed, alive building a rearm timer, an
auto-acquired target, an optional turret, and a charge counter. Auto-acquisition
is a simplified `BuildingClass::Mission_Guard` → `Greatest_Threat`/
`Target_Something_Nearby` (`building.cpp:3568`, `techno.cpp:5912`): the nearest live
enemy **unit** within weapon range (ties broken by slot order). Deviations:
- **Units only** are auto-targeted (not enemy buildings / force-fire cells) — base
  defences overwhelmingly shoot attacking units; a defence never auto-sieges a base.
- **GUN** has a rotating turret (`ROT=12`, `has_turret`): it rotates `turret_facing`
  toward the target and only fires when aligned (`aligned_to_fire`). The other
  emplacements (PBOX/HBOX/FTUR) are fixed and fire in any direction.
- **TSLA charge-up** (`Charges=yes`, `Charging_AI`, `building.cpp:45`): when it has a
  target, is rearmed, and the house has power, it charges for a fixed
  `TESLA_CHARGE_TICKS = 15` (≈1 s; the original ramps a 9-stage animation), then
  looses an instant `Super`-warhead bolt (100 dmg). Losing power/target/range
  abandons the charge. The client renders a charge glow and a two-segment zap line
  (the bolt is an invisible hitscan projectile — no persistent bullet).
- The bullet's `source_unit` is a **sentinel** handle (a building is not a unit), so
  the blast-exclusion and retaliation-naming lookups simply never resolve it.
- **AGUN deferred** (no aircraft to shoot).

Defense combat *state* (turret facing, rearm, charge, target) is hashed **only for
armed buildings**, so every pre-Chunk-B golden (no armed building) is byte-identical.

**Walls (SBAG/CYCL/BRIK) — modeled as 1×1 buildings, not a separate overlay layer.**
Each wall is a 1×1 `Building` with `is_wall=true` and no weapon. This reuses the whole
building system for free:
- **Occupancy / passability (closes Q6 #2):** the footprint stamps
  `Passability::set_occupied`, so a wall cell is impassable to ground movers — they
  route around it or hold. Verified: an enemy ordered across a wall line never enters
  a wall cell.
- **Attackable:** walls take damage through the normal `Target::Building` →
  `modify_damage` path (`Armor=none`, `Strength=1`), so any weapon whose warhead
  harms the none-armor column destroys them.
- **Placement:** the existing proximity rule lets a wall extend from any owned
  building — including another wall — so wall lines chain.
- **Not a base structure:** `house_alive` and the AI's base logic ignore `is_wall`
  buildings, matching the original where walls are overlays, not buildings (a house
  whose only survivors are walls is defeated).

**Deviations / deferrals (walls).**
1. **Single-frame render, no adjacency-connect.** Walls draw as a single sprite/fill
   per cell; the original's overlay art picks a connected frame from the 4-neighbour
   wall mask. Deferred (coordinator-sanctioned "single-frame ok, note it").
2. **Single-cell placement, no linear drag.** Each wall is placed one cell at a time
   through the normal building placement flow (coordinator-sanctioned "single-cell ok").
3. **No crushing.** Heavy vehicles do not crush walls (consistent with Q5.3 no-crush).

**AI.** `AI_Building` gains a base-defense tier (`house.cpp:5696`): once a war factory
is up, the AI keeps `2 + refineries` combat defences, preferring the strongest
buildable one (reverse catalog order → tesla/gun before pillbox). Simplified to a
deterministic priority tier (no new sim-RNG draw), consistent with the rest of
`next_structure`. AI-vs-AI still reaches decisive outcomes.

---

## Q10 — Support buildings and infantry specialists

**Milestone:** M7.7 Chunk C (P4 support buildings + P5 infantry specialists — the
final M7.7 chunk).

**DOME — radar gated on a powered dome.** The radar minimap is no longer always
on: `AppCore::has_radar()` requires the player to own a **live, powered** radar
dome (DOME) — `RadarClass::Radar_Activate` gated on `IsRadarActive` +
`House->Power_Fraction()`. A catalog that models no DOME (synthetic test fixtures)
keeps the radar always-on, so those goldens are unaffected; the real skirmish/econ
loaders now **start without radar** until a DOME is built and powered (a
coordinator-authorised skirmish-default change; the `ui_shroud_golden` sweep is
re-pinned — with the radar gone, sweep frames that differed only by the radar's
per-position view-box collapse to a single hash, which is itself the proof).

**SILO — storage capacity (two-pool credit model).** Ported `HouseClass`'s split:
a house has **given credits** (`Credits` — scenario start, sell refunds, captures)
and **harvested tiberium** (`Tiberium` — capped at `Capacity`, the sum of building
`Storage=`). Spendable money is `available() = credits + tiberium`
(`Available_Money`, `house.cpp:2022`); spending draws from tiberium first then
credits (`Spend_Money`); harvest income books into tiberium, and anything beyond
capacity is **wasted** (`Harvested`, `house.cpp:1975`). This split is why a house
may *start* with more money than its storage without the cap eating its harvest.
PROC contributes `Storage=2000`, each SILO `Storage=1500`. **Backward-compat:** a
house with no storage-declaring building (`capacity == 0`, every synthetic-catalog
economy) routes harvest straight to `credits` and keeps `tiberium == 0`, which is
folded into the hash only when non-zero — so all pre-Chunk-C economy goldens are
byte-identical; only real-asset economies (PROC storage) change.

**FIX — service depot repair.** On a global repair cadence (`Rule.RepairRate =
.016` min ≈ 15 ticks), each FIX heals one friendly, damaged **vehicle** parked on
or adjacent to its footprint by `Rule.URepairStep = 10` HP, charging
`Rule.URepairPercent = 20%` of the unit's build cost proportional to the HP
restored (drawn from `available()`). Simplified dock: nearest adjacent damaged
friendly vehicle, no radio/`MISSION_ENTER` protocol. (FIX is identified by catalog
name, like DOME/FIX role checks — a table-free single-type test.)

**APWR / ATEK / STEK** are plain structures: APWR is a `Power=200` plant; the tech
centres are prerequisite gates (ATEK for Allied tech, STEK gates E4). No special
behaviour beyond power/prereqs.

**Infantry specialists.**
- **E4 flamethrower** and **DOG**: ordinary roster additions — units with a weapon
  (E4's `Flamer`/Fire, DOG's `DogJaw`/Organic). DOG's `Organic` warhead only harms
  unarmored targets (`Verses = 100%,0%,0%,0%,0%`), so it is anti-infantry. **No leap
  animation** — the `LeapDog` projectile flies straight (QUIRK; cosmetic only).
- **MEDI medic (heal).** The medic's `Heal` weapon has **negative** `Damage=-50`.
  The M4 `modify_damage` port kept `combat.cpp:83`'s heal special case (negative
  damage applied full strength only at point-blank, `distance < 8` leptons, against
  `Armor=none`), and `explosion_damage` now applies negative results as **healing**
  (raise health, capped at max; never kills or triggers retaliation). A medic
  auto-acquires the nearest wounded friendly infantryman in range and fires the
  heal through the normal combat path. (Heal capability is **derived** from
  `weapon.damage < 0` — no new flag.)
- **E6 engineer (capture).** Ported `infantry.cpp:659`: an engineer ordered to
  attack an enemy building marches to the footprint and, on arrival, **captures** it
  (ownership + power + build-count flip) when its health ratio is `≤
  EngineerCaptureLevel = ConditionRed = 1/4` (`rules.cpp:281`), otherwise **damages**
  it by `EngineerDamage = 1/3` of its max strength (`rules.cpp:280`); the engineer is
  **consumed** either way (`delete this`, `infantry.cpp:680`). Engineer capability is
  **derived** as "unarmed non-harvester infantry" — E6 is the only such unit — so no
  new flag (§3.8). SPY/THF/Tanya stay deferred (QUIRK).

**AI.** The AI's infantry lane now builds **offensive** infantry only (a weapon with
positive damage) — admitting E4/DOG, excluding the medic/engineer (which need micro
the skirmish AI lacks) — `house.cpp:6400`. The structure priority gains a **radar
dome** once the economy is running (`AI_Building`, `house.cpp:5696`). SILO/FIX/tech
centres are not in the AI's build order (situational; deferred). AI-vs-AI still
reaches decisive outcomes.

---

## Q11 — M7.8 carried fixes (audit follow-ups)

**Milestone:** M7.8 (the four design gaps ra-tester's M7.7 audit pinned in
`ra-sim/tests/support_suite.rs`, now resolved against the reference source).

**(a) SILO sell/destroy — immediate tiberium reconcile, excess lost.** Selling or
destroying a storage building (SILO/PROC) used to leave over-cap tiberium stale
until the *next* harvest tick silently clamped it. We now reconcile in
`remove_building` (and on `capture_building` for the former owner) the instant
capacity drops: `House::reconcile_capacity` clamps `tiberium` to the recomputed
`house_storage_capacity` and **wastes** the excess with no credit refund. This
matches `HouseClass::Adjust_Capacity` (`house.cpp:2104-2125`): the clamp is eager,
and building removal/destruction passes `inanger = true` (`building.cpp:2514`
`Limbo`), which discards the excess (`IsMaxedOut = true`) rather than refunding it
(the peacetime `inanger = false` path *would* `Refund_Money`). **Deferral:** the
original credits the *capturer* with the old owner's excess as "booty"
(`building.cpp:3288`); we discard it on capture too rather than transferring it —
a minor, documented divergence on the rare storage-capture path. Pin:
`selling_a_full_silo_reconciles_stored_tiberium_immediately_no_refund`.

**(b) Engineer + friendly building — renovate-and-consume (brief was wrong).** The
brief hypothesised RA engineers refuse friendly buildings; the reference shows the
opposite. `InfantryClass::Per_Cell_Process`/`MISSION_CAPTURE` (`infantry.cpp:636-680`)
calls `Renovate()` on an allied building — `TechnoClass::Renovate` sets
`Strength = MaxStrength` (`techno.cpp:3988`) — and the engineer is deleted at the
shared terminal (`infantry.cpp:782`). So an engineer marched onto a **friendly**
building now **heals it to full and is consumed** (the classic RA instant-repair),
per rule 3 (reference is ground truth). Pin:
`engineer_renovates_a_damaged_friendly_building_and_is_consumed`.

**(c) Engineer + wall — refused, not consumed.** Walls are `OverlayType`s in the
original, not `BuildingClass`es, so they can never be capture/enter targets
(`Can_Capture` needs `RTTI_BUILDING` + `IsCaptureable`, `building.cpp:3537`;
`object.cpp:421` returns false; wall stubs default `IsCaptureable = false`,
`bdata.cpp:2746`). We model walls as 1×1 buildings (Q9), so `run_engineers` gates
`is_wall` out explicitly: an engineer ordered onto a wall (friend or foe) refuses,
is **not** consumed, and the wall is untouched. Pin:
`engineer_cannot_capture_a_wall_and_is_not_consumed`.

**(d) Medic — symmetric friendly-infantry-only guard.** The medic's
"keep the current target" fast path now applies the *same* validity test as fresh
acquisition (`is_infantry` + friendly + alive + wounded). Previously a friendly
*vehicle* survived re-validation and healed (via an explicit order) while the
identical enemy order was clobbered — an asymmetry. Now both invalid explicit
orders (friendly vehicle, enemy unit) are cleared identically the same tick, so a
medic can only ever heal friendly infantry — matching the original's
`THREAT_INFANTRY`-only, ally-only heal logic (`techno.cpp:1606,2154`;
`combat.cpp:84` nullifies heals on non-`ARMOR_NONE` targets — vehicle healing is
the separate MECHANIC unit). Pins:
`medic_never_heals_a_vehicle_even_when_explicitly_ordered`,
`medic_explicit_order_to_heal_an_enemy_is_silently_clobbered_back_to_a_no_op`.

---

## Q12 — Pre-game state machine wraps the game surface (M7.8)

**Milestone:** M7.8 (main menu + skirmish setup + pause/game-over flow).

**Our design.** A new windowless `App` (`ra-client/src/menu.rs`) is the outer
state machine — `MainMenu → SkirmishSetup → InGame → Paused/GameOver` — driven
entirely through `handle`/`update`/`compose` (DESIGN §4.8). It *wraps* an optional
in-game `AppCore`: `InGame` delegates to it; `Paused`/`GameOver` freeze it by not
calling `update` (the sim tick count does not advance); the menus are **pre-World**
and never touch `compose_game` or the sim.

**Why it doesn't move goldens.** Because the menu states render their own frames
and never invoke the game surface, enabling the state machine cannot perturb any
in-game golden — `compose_game` output is byte-identical whether or not the menu
wraps it. The two new `AppCore` seams are inert by default: `radar_always_on`
starts `false` (so `has_radar` is unchanged), and `set_house_remap` is only called
by the configured skirmish loader (never on the existing golden paths). The
existing `ui_*` golden suites remain the proof.

**Classic-radar toggle.** The setup screen's "CLASSIC RADAR" option threads to
`AppCore::set_classic_radar`: ON keeps authentic DOME power-gating (Q10); OFF sets
`radar_always_on`, making `has_radar` bypass the DOME check (always on). Cosmetic
only — never touches the sim hash.

**Map scanning.** `general.mix` indexes by name-hash with no directory, so
`scan_archive_maps` probes the RA multiplayer naming space (`scmNN<t><v>.ini`) and
keeps every name that resolves (24 maps on the freeware set). User maps are scanned
from the per-OS data dir (`platform::user_maps_dir`, e.g.
`~/.local/share/new-alert/maps`), created on first run, and load via the same
INI-text path (`load_from_text` / `load_skirmish_configured`) as archive maps.

---

## Q13 — Build-time fidelity: the missing `BuildSpeed` bias and STEP_COUNT quantise (M7.9 P0)

**Milestone:** M7.9 P0 (player report: "builds feel too slow").

**The bug.** `Catalog::time_to_build` computed `Cost × TICKS_PER_MINUTE / 1000`
and used the result directly as the tick count. It dropped **two** pieces of
`TechnoTypeClass::Time_To_Build` + `FactoryClass::Start`:

1. **`Rule.BuildSpeedBias`** (`[General] BuildSpeed`, rules.cpp:464). Our real
   `redalert.mix` rules.ini ships `BuildSpeed=.8` (note: **`.8`, not `.7`** — the
   brief's `.7` is retail; rules.ini is ground truth). We applied no bias at all,
   so every item took `1/0.8 = 1.25×` too long.
2. The **STEP_COUNT rate conversion** (`factory.cpp:432`): the factory divides the
   raw time `T` by `STEP_COUNT = 54`, `Bound`s the per-step rate to `1..=255`, and
   then takes `STEP_COUNT` steps — so the real build time is `rate × 54`, which
   truncates `T` down to a multiple of 54 (and floors any trivially cheap item to
   54 ticks). We never quantised.

**The fix** (`ra-sim/src/catalog.rs`). `build_time_base(cost)` reproduces
`round(round(Cost × 0.8) × 0.9)` with integer 16.16 math (round-to-nearest at each
`int × fixed` step, matching `common/fixed.h`); `time_to_build(cost, scale_n,
scale_d)` then applies the low-power snapshot and the STEP_COUNT rate conversion.
`BuildSpeed` is loaded from rules.ini into `EconRules::build_speed_bias_raw`
(default `1.0` for synthetic catalogs, matching the reference compile-time
default).

**Before → after (full power, Normal, single factory):**

| item | before | after (measured) | reference |
|------|--------|------------------|-----------|
| POWR $300 | 270 t (18.0 s) | **216 t (14.4 s)** | 216 |
| WEAP $2000 | 1800 t (120.0 s) | **1404 t (93.6 s)** | 1404 |
| 2TNK $800 | 720 t (48.0 s) | **540 t (36.0 s)** | 540 |

Pinned with the derivation in `ra-sim/tests/build_time_fidelity.rs`. Units use the
same formula family as buildings (stock `UnitBuildPenalty = 100 → ×1`). No golden
churn: synthetic economy goldens are same-script determinism checks, and no pinned
frame captured a mid-build state that this timing shift would move.

---

## Q14 — Player sell / repair interface + building self-repair (M7.9 P1)

**Milestone:** M7.9 P1 (the sell UI deferred since M6; repair as the bonus).

**Sell mode.** `Command::Sell` has existed in the sim since M6; M7.9 adds the UI
through the AppCore seam: a **SELL button** in the sidebar header toggles
`sell_mode`; a tactical left-click while armed sells the **own** building under it
(`own_building_at_map` gates strictly on own + alive + non-wall), refunding
`RefundPercent`. Right-click or Esc cancels the mode (the `App` layer forwards Esc
to the core only while a mode is armed, else it opens pause). **Monkey/scripted-
drive safe:** a click on an enemy building, a unit, or empty ground emits nothing.
A red footprint hover-tint shows what a click would sell.

**Repair (bonus, implemented).** New `Command::Repair` toggles
`Building::is_repairing` (`BuildingClass::Repair(-1)`, building.cpp:2725); a
**REPAIR button** + repair mode drive it, with a green hover-tint. A new
`run_building_repair` system heals on the global repair cadence
(`REPAIR_INTERVAL = 15` ticks ≈ `Rule.RepairRate`): `+Rule.RepairStep` HP per
step, charging `Rule.RepairPercent × (Cost / (MaxStrength / RepairStep))`
credits (`TechnoTypeClass::Repair_Cost`, techno.cpp:6907, floored ≥1). It stops at
full health or when the house can't pay the step — the original's two exits
(building.cpp:5860-5878). Walls refuse repair (they're overlays in the original,
per Q9/Q11c).

> **M7.9.1 audit correction (ra-tester).** The original M7.9 landing pinned
> `RepairStep = 5` / `RepairPercent = 1/4` from the reference's **compile-time**
> defaults (`rules.cpp:221-222`). The real `redalert.mix` rules.ini *overrides*
> both (`[General] RepairStep=7`, `RepairPercent=20%` = `1/5`) — confirmed by
> extracting the actual asset (`radump extract redalert.mix rules.ini --in
> local.mix`). This is the same category of bug as the P0 `BuildSpeedBias` miss:
> a compile-time default used where rules.ini ground truth differs. Fixed in
> `ra-sim/src/world.rs` (`BREPAIR_STEP`/`BREPAIR_PERCENT_NUM`/`_DEN`); pinned in
> `ra-sim/tests/repair_suite.rs` (full-cycle cost, insolvency stop/resume,
> sell-mid-repair, destroyed-mid-repair). The unit
> (service-depot) repair constants (`UREPAIR_STEP=10`, `UREPAIR_PERCENT=20%`)
> already matched rules.ini and needed no change.

**Hash / golden discipline.** `is_repairing` is folded into the building hash
**only while `true`**, so a building never ordered to repair (every pre-M7.9
golden) hashes identically. The SELL/REPAIR buttons render in the sidebar header,
which legitimately moves the four **sidebar-enabled** `compose_game` frame goldens
(`ui_shroud_golden` ×2, `ui_menu_golden_frames` paused/gameover ×2) — re-pinned
with citations; sidebar rendering only, no sim/geometry change.

---

## Q15 — Difficulty stat handicaps, and why the RA sections invert for AI opponents (M7.9 P2a)

**Milestone:** M7.9 P2a ("the authentic Easy/Normal/Hard"; "Hard must reliably
beat Easy in AI-vs-AI").

**What we ported.** `HouseClass::Assign_Handicap` (house.cpp:278): each AI house
carries a `Handicap` — the `[Easy]/[Normal]/[Difficult]` bias multipliers
(`Difficulty_Get`, rules.cpp:307) — applied house-scoped at the reference's own
computation sites:
- **Firepower** (damage dealt) at `fire()` (`techno.cpp:3303`).
- **Armor** (damage taken) per-target in `explosion_damage` (`techno.cpp:4099`).
- **ROF** (rearm delay) in unit + building combat (`techno.cpp:3066`).
- **Groundspeed** (move speed + turn rate) in `move_units` (`drive.cpp:648/1354`).
- **Cost** (credits charged) and **BuildTime** in `apply_start_production`
  (`Assign_Handicap` BuildSpeedBias / Purchase_Price).

Biases are raw 16.16 fixed, loaded from rules.ini by the client into
`EconRules::difficulty`; a house with no AI (the human) and every synthetic catalog
keep the **neutral all-`1.0`** handicap, which is a byte-exact no-op (`fx_mul(v,
1.0) == v`) and is folded into the house hash **only when non-neutral** — so no
pre-M7.9 golden moves.

**The inversion (the quirk).** RA's difficulty sections are *player-centric*:
`Rule.Diff[DIFF_EASY]` = `[Easy]` = the **buffed** handicap (FirePower 1.2, ROF .8,
Cost/BuildTime .8) — what the *player* gets on the easy setting. There is no
separate "AI strength" knob; the AI opponent is just neutral. But the brief (and
intuition) wants an AI **labelled** by how hard it is to *beat*: a "Hard" opponent
should be **strong**. So `Catalog::difficulty_handicap` maps our labels to the
sections that make the label true: **`Hard → [Easy]`** (the buffs), `Normal →
[Normal]`, **`Easy → [Difficult]`** (the nerfs). The bias *values* are 100%
authentic rules.ini; only the label→section pairing is inverted, and it is
inverted precisely because a "hard game" in RA means the player is nerfed (i.e. the
opponent is relatively strong). Verified start-independent in
`ui_ai_vs_ai::real_hard_ai_reliably_beats_easy_ai` (same map, sides swapped, Hard
wins both).

**Deferred (documented).** The rest of P2 — Expert_AI weighted enemy selection
(house.cpp:4941), base-size rubber-banding (house.cpp:4929), composed attack teams
with staging cells + harvester harassment + retreat thresholds, and economic
reflexes (AI repair/sell/fire-sale, `AI_Raise_Money`/`Fire_Sale`) — is **not** in
M7.9; only the difficulty handicaps (a) and the existing wave cadence are wired.
Those four are each a substantial system and are left for a dedicated AI milestone.
The repair *machinery* an AI would reuse (Q14) is in place.

---

## Q16 — Expert AI: enemy selection, rubber-banding, composed teams, economic reflexes (M7.10)

**Milestone:** M7.10 (the deferred M7.9 P2 b–e, as a dedicated AI milestone).

Ported the four remaining `HouseClass` AI systems, all deterministic, all through
the normal `Command` pipeline, all sim-RNG at cited original call sites.

**(b) Expert_AI enemy selection** (`house.cpp:4941`, on the ~10 s `AITimer`
cadence — `EXPERT_PERIOD = 150`, not every tick). Weighted score per candidate
enemy: `((MAP_CELL_W*2) − dist)·2` (distance-dominant) `+ BuildingsKilled[me]·5 +
UnitsKilled[me]` (kills I've scored against them) `+ (theirUnits−myUnits) +
(theirBuildings−myBuildings) + (theirInfantry−myInfantry)/4` (relative size) `+
100` if they're my last attacker. The kill tallies and last-attacker live on
`House` (`units_killed_by`/`buildings_killed_by`/`last_attacker`, attributed in
`explosion_damage` when a cross-house hit lands/kills) and are **not hashed** —
deterministic derived state read only by the AI, so every combat golden (no AI)
stays byte-identical.

**(c) Base-size rubber-banding** (`house.cpp:5010`). Expert_AI also raises
`max_units`/`max_buildings` to the average enemy's army/base size + 10 (never
shrinking). `max_units` gates combat-vehicle production (not harvesters);
`max_buildings` gates the discretionary base-expansion tail of `next_structure`.
The **building cap is load-bearing**: without it the spare-power-plant fallback
built forever and walled the base in (units couldn't path out to attack).

**(d) Composed attack teams** (stand-in for `TeamTypeClass`/`TeamClass` scripts,
teamtype.h). On the `AlertTime` cadence a team forms with a weighted
vehicle+infantry mix, gathers at a **staging cell** on the base edge toward the
enemy, then attack-moves the objective; it **dissolves** (survivors retreat to
base) when decimated below half its starting size. An occasional (1-in-4)
**harvester-harassment** mission targets an enemy harvester instead. Team RNG
draws in fixed order: harass roll, vehicle count, infantry count.
- **Deviation — reachability-filtered recruitment.** Team members are only
  recruited from idle armed units that can actually `find_path` to the staging
  cell, so a unit boxed inside our own base ring is never picked (it would just
  stall the team). The original's `TeamClass` recruits by type/zone, not a
  reachability probe; ours is the pragmatic equivalent that survives a dense base.
- **Deviation — team-level retreat, not per-unit fear.** The original's per-unit
  `Fear`/`IsScaredToDeath` thresholds (`foot.cpp`) are **deferred**; we dissolve
  the whole team when its survivor count drops below half. Observable outcome
  (decimated attackers fall back) matches; the granularity differs.

**(e) Economic reflexes.**
- **Repair** (`Repair_AI`, building.cpp:5834): when `Available_Money ≥
  Rule.RepairThreshhold (1000)` the AI toggles repair (P1's `Command::Repair`) on
  its most-damaged building; `run_building_repair` heals it and stops it when full
  or unaffordable.
- **Sell-when-broke** (`AI_Raise_Money`, house.cpp:5552): when money `< 100` and
  the house **can't make money** (no refinery+harvester), it sells its
  least-essential building (defenses/tech before the core economy) via
  `Command::Sell`.
- **Fire-sale + all-out** (`Check_Fire_Sale`/`Fire_Sale`/`Do_All_To_Hunt`,
  house.cpp:5252/7622/7651): a house that has **deployed** and then lost all
  production (no yard/factory/barracks) with no MCV to recover sells every
  building and throws every unit at the enemy. **The `deployed` guard is
  essential** — without it a not-yet-deployed house (or a scenario/test house
  holding a lone non-factory building) would fire-sale itself into elimination at
  game start (surfaced by `building_combat_economy_edges`'s last-building test).

**Determinism / goldens.** New AI decision state (`enemy`, `max_units`,
`max_buildings`, `team`) is folded into the AI hash **only when set/present**, and
`AiPlayer` state is hashed only for worlds that have an AI — so no non-AI golden
(combat/movement/economy/menu) moved. AI-vs-AI resolves decisively on both real
scenarios at every difficulty, Hard still reliably beats Easy (both sides), and
same-seed determinism holds (`ai_suite`). Showcase:
`ai_suite::showcase_composed_team_lifecycle_and_repair` logs a full team lifecycle
(compose → stage → attack → dissolve) and a repair reflex.

---

## Q17 — Campaign scripting: trigger/teamtype engine, simplified evac, and deferred placements (M7.5 Chunk A)

**Milestone:** M7.5 Chunk A (campaign foundations — Allied mission 1 `scg01ea`
playable start-to-victory through its real scenario data).

**What we ported (faithfully).** The RA `TriggerTypeClass`/`TeamTypeClass` tables
and their evaluation (`trigtype.cpp`/`tevent.cpp`/`taction.cpp`/`teamtype.cpp`/
`trigger.cpp`/`reinf.cpp`). Events/actions keep the original raw `(code, team,
trigger, data)` form (`ra-sim/src/campaign.rs`); `run_campaign` (world.rs)
evaluates them each tick in INI order with the real `MultiStyleType` event/action
mapping (ONLY/AND/OR/LINKED) and persistence (VOLATILE/SEMI/PERSISTANT). The
implemented **event** subset (what scg01–03ea use): `TIME`, `DESTROYED`,
`GLOBAL_SET/CLEAR`, `EVAC_CIVILIAN`, `PLAYER_ENTERED`/`CROSS_*` (cell triggers),
`LOW_POWER`, `BUILDING_EXISTS`, `ALL_/UNITS_/BUILDINGS_DESTROYED`. The **action**
subset: `WIN`, `LOSE`, `TEXT_TRIGGER`, `PLAY_SPEECH`, `REINFORCEMENTS`,
`CREATE_TEAM`, `DZ`, `SET/CLEAR_GLOBAL`, `FORCE_TRIGGER`, `DESTROY_TRIGGER`,
`DESTROY_OBJECT`, `REVEAL_ALL/SOME`, `ALL_HUNT`, `START/STOP/SET_TIMER`. Team
mission lists implement Attack/Move-to-waypoint/Patrol/Guard minimally. All state
(globals, per-trigger spring/timer/carrier, mission timer, evac latches, alliance
matrix) is hashed **only when a campaign is present** — every skirmish/combat/AI
golden is byte-identical (verified: full suite green, 593→ tests).

**Deviations / deferrals (documented).**
1. **`TEVENT_EVAC_CIVILIAN` — reach-the-LZ stand-in, not aircraft-leaves-map.** In
   RA the flag latches when a transport *aircraft* carrying the VIP flies off the
   radar edge on `MISSION_RETREAT` (`aircraft.cpp:4280` `Edge_Of_World_AI`). We
   have no aircraft/transport sim, so a friendly civ-evac VIP standing on (or
   adjacent to) a `TACTION_DZ` flare cell is counted as evacuated and removed
   (`process_evac`, world.rs). The win *condition* (Greece's `IsCivEvacuated` →
   `win` trigger → Victory) is the real engine; only the physical evac vehicle is
   simplified. Evac-removal is pardoned against the VIP's own `DESTROYED` trigger
   (Einstein carries `elos` = "he died → LOSE"; leaving the map must not trip it).
2. **`TACTION_CREATE_TEAM` recruits existing units (no per-class type match).** The
   reference recruits eligible on-map units of the team's house; we take up to the
   team's total count of idle house units and apply its mission — no naval/air or
   per-class matching. `REINFORCEMENTS` **does** spawn (from resolved protos).
3. **Naval + aircraft teamtype classes are dropped.** `CA` (cruiser), `TRAN`
   (Chinook), etc. have no sim, so those team members are skipped (logged in the
   loader's `skipped` list). Ground reinforcements (Tanya's `E7`, Einstein) spawn
   normally, so the rescue chain runs; the naval bombardment + evac chopper are
   cosmetic and deferred.
4. **`[TERRAIN]` = occupancy only, render deferred.** Trees/rocks stamp the
   passability grid (`World::block_cell`, port of `TerrainClass::Occupy_List`) so
   ground movers route around them, but the theater terrain SHP is **not drawn**
   yet (coordinator-sanctioned "occupy + note it"). Occupancy is the must-have
   (movement correctness); the sprite layer is a follow-up.
5. **Cost-less civilian structures.** `[STRUCTURES]` props (`BARL`/`BRL3`/`V19`,
   and the `MISS`/`FCOM` mission-3 structures) carry a rules.ini section but no
   `Cost=`; `building_stats` now defaults a missing cost to `0` (a scenario-placed
   civilian structure is never built, so cost 0 is correct) so they resolve and
   place with a footprint. All 25 of scg01ea's `[STRUCTURES]` lines now place.
   (Fixed in the M7.5-A audit; before, the 7 cost-less props were skipped.)
6. **Alliances are symmetric + collapse extra houses.** `[Basic]/house Allies=`
   builds a symmetric alliance bitmask (`build_alliances`); `World::are_allies`
   gates enemy auto-acquisition (hunt + defense buildings) so allied civilians
   aren't targeted. Non-country houses (GoodGuy=8..Special=11) have no CPS colour
   row and render in unremapped (native) art.
7. **Win/lose ignore the action's `Data.House`.** `WIN`/`LOSE` resolve to the
   player's Victory/Defeat directly (single-player campaign), not the reference's
   `Data.House == PlayerPtr` player-vs-computer distinction — correct for a
   one-player mission, and it sidesteps the `-255` sentinel these lines carry.
8. **Reinforcement lands on a mask-impassable waypoint.** A team's origin waypoint
   can sit on a tile our simplified land mask (Q6: only impassability modelled,
   `<100%` costs collapsed) marks Foot-impassable, so a spawned VIP may need a
   one-cell nudge to a passable cell before it can path (done in the verification
   harness). This is a land-cost-fidelity limitation (Q6), not a campaign bug.

**Verification.** `ra-client/tests/campaign_scg01ea.rs` loads the real
`scg01ea.ini`, reports the full inventory, drives a scripted playthrough to
VICTORY through the real triggers (`set1` TIME-0 reinforces Tanya at tick 0 →
destroying the two `eins`-carrier guards springs `eins` → `REINFORCEMENTS einst`
[Einstein] + `FORCE_TRIGGER ein2` → `DZ` flare + `SET_GLOBAL 1` → escort Einstein
to the LZ → `EVAC_CIVILIAN` → `win`/Victory), asserts same-script-twice hash
equality, and dumps start/briefing/victory PNGs. The menu campaign flow (Campaign
button → 14-mission Allied list → briefing text → play → Victory advances / Defeat
retries) is covered asset-free by `ui_campaign_flow.rs`.

---

## Q18 — Per-unit mission layer + APC transport (M7.5-B)

**Milestone:** M7.5-B (playtest-driven: "enemy units don't fight actively even on
Hard; APC soldiers don't get off and fight; units must react to being attacked").

### P0 — per-unit missions (the INI `[UNITS]`/`[INFANTRY]` order, now executed)

Until now the scenario mission string (final field: `Guard`, `Area Guard`, `Hunt`,
`Sleep`, `Sticky`, `Harvest`) was parsed but dropped — placed units only *retaliated*
when hit and never proactively engaged. Guards therefore "stood and watched". We now
carry a [`Unit::mission`](../ra-sim/src/unit.rs) ([`Mission`] enum) and act on it.

**Guard** (`FootClass::Mission_Guard`, `foot.cpp:594` → `Target_Something_Nearby(THREAT_RANGE)`,
`techno.cpp:5912`; scan range from `Threat_Range`, `techno.cpp:5194`). A Guard unit
idle at its post auto-acquires the nearest enemy within **weapon range** and engages
through the existing combat path. **Leash:** the acquired target is dropped the moment
it leaves weapon range (`In_Range` → `Assign_Target(TARGET_NONE)`) — plain Guard *never
chases*. Implemented as `maybe_acquire_guard_target` (acquire) + a leash branch in
`run_combat`'s out-of-range path.

**Area Guard** (`FootClass::Mission_Guard_Area`, `foot.cpp:1001`): acquires within
**twice** weapon range of the guard post (`THREAT_AREA`, centred on the post /
`ArchiveTarget`), chases the target, but races back to the post once it strays more than
weapon range from it (`Distance(ArchiveTarget) > Threat_Range(1)/2`, `foot.cpp:1057`).
The post is the spawn cell; it is stored on [`Unit::guard_post`].

**Hunt** (`FootClass::Mission_Hunt`, `foot.cpp:670`): seek the nearest enemy anywhere.
Reuses the existing `hunt` auto-hunt path (`maybe_acquire_hunt_target`), which now also
fires for `Mission::Hunt` — so `ALL_HUNT`, attack-teams, and an INI `Hunt` order all
share one implementation.

**Sleep / Sticky** (`MissionClass::Mission_Sleep`, `mission.cpp:93` — the handler never
touches TarCom): fully inert. Never auto-acquire **and never retaliate** (`assign_retaliation`
early-returns for these two). This is the conservative reading — an ambusher on Sticky
stays hidden until scripted. *Deviation:* the original's `Take_Damage` snaps a
`IsNoThreat` mission out via `Enter_Idle_Mode` and *then* lets it retaliate
(`foot.cpp:1172`); we keep it passive instead (the `NoThreat`/`Zombie` MissionControl
flags live in a data INI we don't parse, and "held-still means held-still" is the more
useful campaign behaviour). Documented rather than half-modelled.

**Default mission.** `Unit::new` defaults to `Mission::Guard`, matching the original's
`Enter_Idle_Mode` (`unit.cpp:1343`, `order = MISSION_GUARD`). Harvesters keep the harvest
FSM regardless of INI order.

**Scope of proactive acquisition — ~~campaign only~~ UNIVERSAL since M7.11 (see Q20).**
~~The *proactive* guard scan (`maybe_acquire_guard_target`) and the base-under-attack alert
fire **only in campaign worlds** (`World::campaign.is_some()`)... [campaign-scoping
rationale]~~ **SUPERSEDED by M7.11 (Q20).** The campaign-only gate was removed: proactive
guard acquisition and the base-alert now run in **all** worlds — skirmish and campaign
alike — matching the original (`Enter_Idle_Mode` → `MISSION_GUARD` is universal,
`unit.cpp:1343`), which is what the follow-up playtest ("the AI players still don't do
active fight" — units on *both* sides stand passive in skirmish) demanded. The original
scoping rationale (skirmish AI-vs-AI stalls with active defenders) was real, and it is
resolved not by suppressing acquisition but by **retuning the skirmish AI to stay decisive**
(escalating waves, weakest-point/production targeting, sustained-failure all-out, and a
building-runaway fix — all in Q20). One correctness carve-out added with the universal
change: a **healer** (negative-damage weapon, e.g. the medic) is excluded from guard
acquisition and the base-alert (`w.damage >= 0`), so it never guard-acquires an enemy and
heals it. The old skirmish-scoping pins in `mission_guard_depth_suite.rs` §1
(`skirmish_world_idle_armed_unit_never_auto_acquires...`, `..._guard_target_flag_never...`)
encoded the OLD behaviour and were **flipped** to pin the NEW universal behaviour (renamed
`..._auto_acquires...` / `..._becomes_true_on_proactive_acquire`), with M7.11 justification
comments.

**Player orders vs. guard.** A player `Move`/`Attack` sets the target directly and clears
[`Unit::guard_target`], so it always *chases* (never leashed); when the order finishes the
unit reverts to acquiring under its standing mission (the `Enter_Idle_Mode` return to
Guard). Retaliation likewise sets `guard_target=false` (chases its attacker), preserving
the exact M7 retaliation behaviour. Only *auto-acquired* guard targets are leashed.

**Base-under-attack alert.** When a live enemy shot lands, `alert_nearby_guards` wakes any
idle friendly Guard/Area-Guard unit within [`GUARD_ALERT_CELLS`] (4) to target the
attacker — even one outside that unit's own acquire/sight range — so a guarded base fights
back as a whole. This stands in for the original's house/team alert propagation
(`FootClass::Take_Damage → Team->Took_Damage`, `foot.cpp:1157`); alerted responders chase
(not leashed). *Deviation:* proximity broadcast rather than the full team/house alert graph.

**Hash / golden discipline.** `mission` is folded into the unit hash **only when
non-default** (≠ Guard); `guard_post`/`guard_target`/`cargo`/`board_target`/`unload_at`
only when set/non-empty/true. So a default-guard vehicle-only world with no transport
activity appends **no** new bytes and its byte layout is unchanged. ~~Combined with the
campaign-scoping above, every pre-M7.5-B skirmish/synthetic golden is byte-identical.~~
**M7.11 update:** with the campaign gate removed, skirmish worlds now *behave* differently
(units auto-acquire), but the hash *layout* is unchanged — a skirmish golden only moves if
its scene actually has an enemy in a guard's envelope (most goldens don't, so in practice
no frame golden moved; the depth-suite §1 pins that *did* pin the old behaviour were
flipped — see Q20). `type_id`/`sight`/`locomotor`/`capacity` remain unhashed type constants
(their effect flows through hashed state).

### P1 — transports / passengers (closes Q8's APC deferral)

`Passengers=` (rules.ini) → [`UnitProto::passengers`] → [`Unit::capacity`]; loaded riders
live on [`Unit::cargo`] (`Vec<Passenger>` snapshots) while off the map. New commands:
- **`Command::Load { passenger, transport, house }`** — an infantryman boards an adjacent
  own transport with spare capacity immediately, else walks to it (`board_target`) and
  boards on arrival (`run_transports`, tick system 5.5). Mirrors `MISSION_ENTER`, simplified
  (no radio handshake).
- **`Command::Unload { transport, house }`** — disgorges every passenger to a free adjacent
  spot (`free_unload_cell`, respecting the Q5.3 no-co-occupancy rule), resuming each
  passenger's mission; a passenger with no free spot stays aboard.

**Passengers die with the transport.** No explicit kill path is needed: a destroyed
transport is removed from the arena, and its `cargo` vector is dropped with it — the
passengers are gone. This matches the original (cargo is deleted when the carrier dies,
`DriveClass`/`FootClass` limbo cleanup). *Deviation:* the original ejects survivors on some
deaths; we always lose them (simpler, documented).

**Teamtype `LOAD`/`UNLOAD`** (`TMISSION_LOAD`=14 / `TMISSION_UNLOAD`=8, `teamtype.h:54,60`):
a scripted team carrying a transport + foot members with a `LOAD` mission boards the foot
members at spawn; an `UNLOAD` mission flags the transport's [`Unit::unload_at`] = objective,
so it disgorges on arrival and the riders (given `Hunt` for an attack team) attack. So a
campaign `[TeamTypes]` assault (APC + squad → move → unload → attack) runs end-to-end.

**UI (AppCore).** Select infantry + right-click an own APC → `Load`; select a loaded APC +
Deploy key → `Unload`. Cursor/indicator minimal (no bespoke load cursor yet).

**Hash.** `cargo`/`board_target`/`unload_at` folded only when non-empty/Some; a world with
no transport activity is byte-identical.

### Cuts (documented)

- **P2 (campaign enemy activation) — deferred.** Difficulty selection on the campaign
  briefing screen, `Autocreate`/`TACTION_AUTOCREATE` team formation, and `[Base]` +
  `BEGIN_PRODUCTION` enemy rebuild are **not** in M7.5-B. The M7.9/7.10 handicap machinery
  (Q15/Q16) and team machinery exist to build on; the campaign flow currently starts enemies
  scripted-only. `BEGIN_PRODUCTION`/`AUTOCREATE` `TActionType` codes remain inert (as before).
- **P3 (mission-timer HUD, terrain SHP render) — deferred** (unchanged from Q17.4).

---

## Q19 — Campaign enemy activation: difficulty, autocreate teams, scripted production (M7.5-C)

**Milestone:** M7.5-C (the P2 cut from M7.5-B, now landed). Closes the
`TACTION_AUTOCREATE`/`TACTION_BEGIN_PRODUCTION` "inert" note in Q18's cuts.

### P0 — campaign difficulty (briefing Easy/Normal/Hard)

The briefing screen gained an Easy/Normal/Hard selector (default **Normal**),
threaded through the menu state machine into `CampaignFactory::build(scenario,
difficulty)` — the same "factory config" path skirmish uses — and applied by
[`World::set_campaign_difficulty`].

**What the source does (and we mirror).** `HouseClass::Assign_Handicap`
(house.cpp:278) is called at two campaign sites: the constructor handicaps
**every** house with the *computer* difficulty `Scen.CDifficulty` (house.cpp:742),
then `Read_Scenario_INI` overrides only the `Player=` house with the *player*
difficulty `Scen.Difficulty` (scenario.cpp:2332). The classic 3-position slider
maps a selection to a **(player, computer) pair** (init.cpp:681-705): the player is
*buffed* at the Easy end (`Scen.Difficulty = DIFF_EASY`) and *nerfed* at the Hard
end (`DIFF_HARD`), the mirror of the computers. In single-player the biases come
straight from `Rule.Diff[handicap]` (the `else` branch, no ActLike multiplier).

**Our mapping.** Because our `Catalog::difficulty_handicap` table already inverts
label→rules.ini section for AI opponents (Q15: a "Hard" AI gets the buffed `[Easy]`
biases), the computer houses take `difficulty_handicap(chosen)` directly
(`Scen.CDifficulty`), and the **player takes the inverse label**'s handicap
(`Scen.Difficulty`): **Easy game → player buffed** (`[Easy]`), **Hard game → player
nerfed** (`[Difficult]`), Normal → neutral. This is exactly the source's
player-side bias (yes, the original buffs the player on Easy — we implement it).

**Golden discipline.** On **Normal every house is neutral** (the `[Normal]` section
is all-`1.0`), a byte-exact no-op — so the campaign default perturbs nothing, and
the existing `handicap` hash-gating (folds only when non-neutral) means only a
non-Normal campaign appends handicap bytes. The briefing *frame* golden was
re-pinned once (the visible difficulty-button row, like Q14's sidebar buttons) — a
menu-frame change, not a sim golden.

### P1 — autocreate teams (`TACTION_AUTOCREATE`)

`TACTION_AUTOCREATE` (code 13) sets the target house's `IsAlerted` flag
(taction.cpp:645). An alerted house forms autocreate teams on the `AlertTime`
cadence (house.cpp:1042): each wave creates `Random_Pick(2, (TechLevel-1)/3+1)`
teams (house.cpp:1047), each a uniform random pick among the house's
**autocreate-flagged** team types (`Suggested_New_Team(true)`, teamtype.cpp:414;
the flag is bit `0x4`, `IsAutocreate`, teamtype.h:219), created by recruiting
existing idle house units (`Create_One_Of` → `TeamClass` recruit, team.cpp:1179).
After each wave `AlertTime = Rule.AutocreateTime(5) × Random_Pick(TPM/2, TPM×2)`
(house.cpp:1056). We reuse the existing CREATE_TEAM recruitment path and run the
team's mission list; the common autocreate script is `TMISSION_DO:MISSION_HUNT`
(code 11 arg 14, teamtype.h:57) — the recruited units hunt the player.

**Two-condition gate (both required).** A team forms only when the **house** is
alerted *and* the **team type** carries the autocreate flag — matching the
`(alerted && !IsAutocreate) → excluded` filter (teamtype.cpp:430). Verified both
ways in `campaign_activation_suite`.

### P2 — scripted production + `[Base]` rebuild (`TACTION_BEGIN_PRODUCTION`)

`TACTION_BEGIN_PRODUCTION` (code 3) sets `IsStarted` (taction.cpp:621 →
`Begin_Production()` → house.h:781). A started house (a) **produces** from its live
factories using the AI weighted table (armed vehicle weight 20 / unarmed 1,
house.cpp:6172; offensive infantry from the barracks), draining its scenario
`Credits=` pool through the existing production machinery — **no free money**
(factory.cpp:203); and (b) if it owns the `[Base]` list, **rebuilds** the first
destroyed node in **list order** (`Next_Buildable`, base.cpp:377) when it has a
construction yard + credits, placing it back on the scripted `node->Cell`
(building.cpp:2196 — **bypassing the proximity rule**, a documented deviation from
normal player/AI placement).

**`[Base]` parsing.** `Player=<house>`, `Count=N`, then `000=NAME,cell` … in
priority order (`BaseClass::Read_INI`, base.cpp:432). The client resolves node
names to building-proto ids at load; list order is the rebuild priority.

**Trigger-action house resolution.** RA stores the action's target house in the
**low byte** of the `Data.Value` union (taction.cpp:226 writes `Data.Value`, the
handlers read `Data.House`). Scenario editors write sentinel-padded negatives:
scg03ea's `acrt` carries `-247` (`& 0xFF = 9` = BadGuy) and scg04ea's `set1` a bare
`9`; `win` carries `-255` (`& 0xFF = 1` = the player). [`action_house`] resolves
`data & 0xFF` (with `0xFF` = `HOUSE_NONE`), falling back to the **trigger's own
house**. (WIN/LOSE keep ignoring the house per Q17.7. ALL_HUNT's existing
`data.max(0)` resolution was left untouched to avoid perturbing its pins — a known
minor latent mismatch on sentinel-encoded houses, only reachable by an ALL_HUNT
action no early Allied mission fires against a sentinel house.)

### Real missions that exercise this

Scan of `scg01-06ea` (all extracted from the real `general.mix`): the **earliest**
Allied mission using either feature is **scg03ea** ("Dead End") — its `acrt`
trigger (a `PLAYER_ENTERED` cell trigger at cells 8107-8109, house 9) fires **both**
AUTOCREATE (→ BadGuy autocreate teams `bad1`/`bad2`, flags 12) and BEGIN_PRODUCTION.
The first mission with a real prebuilt `[Base]` is **scg04ea** (15 BadGuy buildings:
POWR/BARR/PROC/WEAP/FTUR/SILO/AFLD…). scg01ea/02ea/05ea/06ea have `[Base] Count=0`;
scg01ea/02ea use neither trigger (so they are unaffected — mission 1 stays green).

### Determinism / hash gating

The whole system lives in a small `EnemyActivation` side-struct on `World`
(alerted/production latches, per-house `AlertTime`, the resolved `[Base]` list +
tech level), installed for every campaign but folded into the hash **only once a
house is actually alerted or has begun production** (`is_active()`), and the system
is a **no-op (RNG-free) until then**. So a scripted-only mission (Allied mission 1)
and every skirmish/synthetic world is byte-identical, and same-script-twice
determinism holds through the new sim-RNG draws (wave count, team pick, AlertTime
reset, production pick — all at cited original call sites). Verified in
`campaign_activation_suite` (asset-free) + `campaign_activation_scg03ea`
(real-asset, real trigger).

### What the player sees on Hard now

On **Hard**, the enemy houses are buffed (`[Easy]` biases — 1.2× firepower/armor/
speed, 0.8× ROF/cost/build-time) and the player is nerfed (`[Difficult]` — 0.8×
firepower, etc.); enemy attacks land harder and faster. In missions that fire
AUTOCREATE/BEGIN_PRODUCTION (from scg03ea on), the enemy also **actively forms
attack teams and rebuilds/produces** instead of sitting scripted-only — a
noticeably more aggressive campaign opponent. On **Normal** (the default) behaviour
is unchanged from M7.5-B.

### Cuts / deferrals

- **P3 (mission-timer HUD, terrain SHP render)** — still deferred (Q17.4).
- **Team `MaxAllowed`/`Number` live-count tracking** — autocreate is bounded by the
  long `AlertTime` cadence + idle-unit availability rather than a per-type live
  count; a documented simplification of teamtype.cpp:428's `Number < MaxAllowed`.
- **`IQ >= Rule.IQProduction` auto-alert** (house.cpp:987) — we only activate via
  the explicit trigger actions, not the AI's self-alert, so campaign enemies stay
  scripted until the mission says otherwise (matching the scenario author's intent
  and keeping non-triggering missions inert).

---

## Q20 — Active-fight parity in skirmish + decisive AI retune (M7.11)

**Milestone:** M7.11 (playtest-driven: "the AI players still don't do active fight" —
in skirmish, units on *both* sides stood passive until hit; only campaign got the
M7.5-B guard layer, because of the Q18 campaign-only scoping gate).

### P0 — remove the campaign-only gate (guard acquisition is universal)

The Q18 gate (`if world.campaign.is_none() { return; }` in `maybe_acquire_guard_target`,
and `if world.campaign.is_some()` around `alert_nearby_guards`) is **gone**. Proactive
Guard/Area-Guard acquisition and the base-under-attack alert now run in **every** world —
skirmish player units, skirmish AI units, campaign — matching the original, where
`Enter_Idle_Mode` puts every produced/placed unit into `MISSION_GUARD` universally
(`unit.cpp:1343`) and a guarding unit auto-acquires via `Target_Something_Nearby`
(`foot.cpp:594`, `techno.cpp:5912`) regardless of single-player-vs-skirmish. Produced/placed
units already default to `Mission::Guard` (the gate was the only thing scoped). See Q18,
whose scoping rationale is now **superseded**.

**Healer carve-out.** A **healer** — a unit whose weapon does negative damage (the medic's
`Heal`, capability derived per Q10/Q11d) — is excluded from both `maybe_acquire_guard_target`
and `alert_nearby_guards` (`weapon.damage >= 0`). Without this, universal guard would make a
medic auto-target the nearest *enemy* and fire its heal at it, **healing the enemy**. Medics
still act only through `maybe_acquire_heal_target` (friendly wounded infantry).

**What the player sees now.** In skirmish, a Guard unit (produced or placed) fires first on
any enemy that walks into its weapon range — a player tank driving past an AI base is
engaged by the defenders *before* it lands a shot (verified: a skirmish Guard acquires on
combat pass 1 and hits within a few ticks, never having been hit itself). Explicit
Move/Attack orders still override guard (clear `guard_target`, chase not leash — the M7
invariant, Q18); returning-to-post guards remain re-orderable at any time.

### P1 — skirmish AI retune (stay decisive with active defenders)

Enabling universal guard reproduced exactly the Q18-predicted stall: reactive/active
defenders grind down the small dribbled attack waves, so games stopped resolving
(`scg05ea` Normal went to a >45-min stall; Hard/Easy/`scm01ea` ballooned). Fixed by making
attacks competent, in fidelity order (`ra-sim/src/ai.rs`):

- **(a) Escalating waves.** A new `AiPlayer::failed_attacks` counter bumps each time a team
  dissolves by decimation; the next wave's target vehicle/infantry counts scale with it
  (`want_v += escalation*2`, `want_i += escalation`), capped at `MAX_ESCALATION`. Dribbled
  waves that always retreat at 50% losses would stalemate forever; escalation ratchets each
  loss into a larger commitment. No single reference mechanism maps 1:1 (the shipped
  `Check_Attack`/`Attack` urgency counters, house.cpp:5226, are the spirit); documented as
  tuning.
- **(b) Attack the weakest point.** `sector_threat(house, cell)` sums enemy armed-building
  strength within `SECTOR_THREAT_RADIUS` (6) cells of a candidate — a simplified port of
  `HouseClass::Adjust_Threat`'s region-threat accumulation (house.cpp:2475). Target
  selection routes the team at the enemy production building in the **lowest-threat sector**.
- **(c) Focus and finish.** `enemy_target` now prefers **production** buildings (war
  factory / construction yard / barracks) — the original's `QUARRY_FACTORIES` quarry
  (`defines.h:2477`) — over the nearest arbitrary building, so a breakthrough cripples the
  enemy's ability to reinforce. Falls back to nearest building, then nearest unit.
- **(d) Sustained-failure all-out.** Once `failed_attacks >= ALL_OUT_ESCALATION` (4), the AI
  abandons the cautious stage-and-retreat cadence and commits every armed non-harvester unit
  to a relentless assault on enemy production, re-pointing only idle/auto-guarding units each
  tick (a port of `Do_All_To_Hunt`, house.cpp:7651, triggered by offensive failure rather
  than only production loss). This is what guarantees a decision. The existing fire-sale +
  all-hunt lost-cause endgame (Q16) still applies to the loser.

**Building-runaway fix (the actual decisiveness blocker).** The rubber-band building cap
`max(self, avg_enemy+10)` (house.cpp:5010) is a positive-feedback loop: two symmetric bases
raise each other's cap without bound, so the discretionary spare-power tail spammed
*hundreds* of plants and **walled the base in** — units could no longer path out to attack,
an eternal stalemate (surfaced once active defenders made attacks fail before damaging the
enemy base). The spare power plant in `next_structure` step 4 is now gated on an actual power
**deficit** (`low_power`) — step 1 already covers real deficits — bounding base growth to
what the economy/defense/tech steps justify. This was the dominant fix; without it no amount
of attack tuning resolved the synthetic symmetric fixture.

**No test-budget change needed.** With the retune, real-map AI-vs-AI resolves decisively at
**every** difficulty on **both** scenarios within the existing 45-sim-minute budget (the
P1d "raise budget + progress assertion" fallback was not needed). Game-length before→after
the retune (real assets, universal guard on both sides):

| scenario / config        | before (P0-only) | after (P1 retune) |
|--------------------------|------------------|-------------------|
| scg05ea Hard-vs-Hard     | 10 594 t         | 16 663 t (18.5 m) |
| scg05ea Normal-vs-Normal | STALL (>45 m)    | 14 566 t (16.2 m) |
| scg05ea Easy-vs-Easy     | STALL (>45 m)    | 20 071 t (22.3 m) |
| scm01ea Hard-vs-Hard     | 31 577 t         | 14 904 t (16.6 m) |

(Pre-M7.11 campaign-scoped baselines, for reference: scg05ea Hard 7 832 t, Normal 10 102 t,
Easy 26 786 t; scm01ea Hard 18 544 t.) Hard still reliably beats Easy in both orientations
(start-independent). Same-seed determinism holds (new AI state — `failed_attacks` — folds
into the hash only when non-zero, and the whole AI hash only fires for worlds that have an AI,
so no non-AI golden moved).

### Golden / re-pin inventory

- **Flipped (legitimate — old behaviour was the campaign-scoping gate):**
  `mission_guard_depth_suite.rs` §1 —
  `skirmish_world_idle_armed_unit_never_auto_acquires...` →
  `..._auto_acquires_a_nearby_enemy` (now asserts skirmish acquisition);
  `skirmish_world_guard_target_flag_never_becomes_true` →
  `..._becomes_true_on_proactive_acquire`. Both carry M7.11 justification comments.
- **Fixture isolation (behaviour under test unchanged, only shielded from the new universal
  guard):** `world.rs::attack_needs_ownership_and_a_weapon` (enemy moved out of guard range);
  `splash_suite::splash_wakes_a_non_addressed_bystander...` (bystander allied to the primary +
  short weapon, so only splash-retaliation can set its target); `subcell_suite::infantry_death
  _frees_its_spot_for_reuse` and `ui_scripted_drive::synthetic_selection_does_not_survive_slot
  _reuse_after_kill` + its `support::synthetic_world_for_selection_regression` fixture (the
  scripted armed killer set to `Sleep` so it stops after its one kill instead of auto-hunting
  the next friendly). `support_suite`'s medic pins pass unchanged once the healer carve-out
  landed.
- **Did NOT move (verified):** every frame golden (`ui_shroud_golden`, `ui_golden_frames`,
  `ui_menu_golden_frames`), the synthetic single-unit movement oracle (`determinism.rs`), and
  **all campaign goldens** (`campaign_scg01ea`, `campaign_activation_*`, `campaign_difficulty
  _depth_suite`) — campaign already ran guard acquisition, so its behaviour is unchanged.
