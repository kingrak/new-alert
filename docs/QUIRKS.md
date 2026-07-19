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
