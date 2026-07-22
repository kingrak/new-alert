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
1. ~~**No ask-the-blocker-to-scatter radio protocol.**~~ **Closed in M7.12 (P0,
   the ore-truck deadlock).** A vehicle whose *only* route runs through a
   **friendly, stationary** unit now radios that unit to scatter aside, faithfully
   porting `DriveClass::Start_Of_Move`'s MOVE_TEMP reaction — see the
   **"Scatter completion"** block below. A vehicle blocked by another *moving*
   vehicle still re-routes **around** it (`find_path_avoiding`); an enemy or busy
   blocker is left alone.

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

   **Scatter completion (M7.12, P0 ore-truck deadlock).** The user reported a
   harvester (and any mover) permanently stuck when a stationary living friendly
   unit blocks its sole path — deviation #1's "wait forever" left the blocker
   un-nudged, unlike the original. `move_units` now ports the real policy: when a
   **vehicle** mover's next step is blocked and no detour exists
   (`find_path_avoiding` fails), and the blocking cell holds a **friendly**,
   **stationary** unit (`path.is_empty()` = the original's `is_moving == false`:
   no NavCom, not rotating, not driving — `UnitClass::Can_Enter_Cell`'s MOVE_TEMP
   condition, `unit.cpp:3336`), the mover fires `scatter_friendly_blockers` →
   `scatter_blocker`, a port of `CellClass::Incoming(0, true, false)` →
   `DriveClass::Scatter(threat=0, forced=true, nokidding=false)`
   (`drive.cpp:970/1034` → `drive.cpp:181`). The blocker picks a random adjacent
   `MOVE_OK` cell — bias = `Dir_Facing(PrimaryFacing) + (Random_Pick(0,2)-1)`
   (one SYNC-RNG draw; the call sites pass `threat==0`, so the bias is the
   blocker's own facing, not a threat direction), then the **last** `MOVE_OK`
   facing in the 8-way rotation wins (the original's non-breaking
   `Assign_Destination` loop) — and steps aside; the mover holds this tick and
   retries (our indefinite hold-and-retry stands in for `TryTryAgain`/`PATH_RETRY`).
   - **Precedence (explicit).** On a block: **yield** tie-break (moving lower-index
     blocker) → **re-route** (`find_path_avoiding`, detour exists) → **scatter +
     hold** (no detour *and* a friendly stationary blocker) → **plain hold**
     (enemy / terrain / no clear cell). Yield and scatter are mutually exclusive by
     construction — a stationary blocker is never in `moved_this_tick`, and scatter
     only fires when re-route already failed. Scatter fires **instead of** the old
     plain hold, never before re-route (mirroring the original, which scatters only
     when even `Basic_Path` cannot avoid the friendly).
   - **Not scattered (verified + cited).** *Enemy* blockers are MOVE_DESTROYABLE/
     MOVE_NO, never MOVE_TEMP (`unit.cpp:3355+`), so they are left alone; *busy*
     (moving) friendlies are MOVE_MOVING_BLOCK, not MOVE_TEMP (`unit.cpp:3340`),
     handled by re-route/yield; a harvester **dumping** at a dock never scatters
     (`IsDumping`, `drive.cpp:191`). **Infantry movers never issue the request**
     — the reaction lives in `DriveClass::Start_Of_Move`, and infantry are
     `FootClass`/`InfantryClass`, not `DriveClass` (their per-cell movement never
     calls `Incoming`), so sub-cell packing/dispersal is unchanged. An infantry
     *blocker* is still scattered by a vehicle mover (`Incoming` scatters every
     occupier).
   - **Determinism / re-pins.** The scatter's sync draw only happens when a
     blocker is actually asked to move, so **every** existing golden — including
     the occupancy/corridor/mission suites and the synthetic single-unit oracle —
     is byte-identical (verified: full `--no-fail-fast` matrix green with **zero**
     re-pins). The old `one_wide_corridor_head_on_is_a_documented_wait_not_a_swap`
     pin stays valid: it is a **head-on of two *moving* enemy vehicles** (neither a
     stationary friendly), which scatter deliberately does not touch — the "idle
     friendly blocker in a 1-wide corridor" case (which *does* resolve) is the new
     `scatter_suite`, not that test. New coverage lives in
     `ra-sim/tests/scatter_suite.rs` (parked-friendly-in-corridor resolves,
     enemy-blocker-not-scattered, single-unit determinism, and the user's
     harvester-docks-past-a-parked-friendly-and-banks-credits end-to-end).
   - **Chain propagation (M7.12 audit P0a).** The single-link scatter above
     could *create* a new deadlock the old "wait forever" never had: a mover
     pushing blocker *b1* east into blocker *b2* left *b1* boxed (mover to its
     west, *b2* to its east) with nobody ever asking *b1* to re-scatter against
     *b2* — a permanent multi-blocker gridlock. Fixed by porting the original's
     **`CellClass::Incoming` cascade** (`cell.cpp:2013`): `Incoming` scatters
     *each* occupier, and a unit asked to scatter that cannot move because a
     friendly boxes it is, on the next entry attempt, itself `Incoming`'d by
     whoever now needs its cell — so a file of parked allies unclogs from the far
     end. `scatter_blocker` now, when it finds **no free adjacent cell**,
     propagates the request to any **friendly, stationary, non-dumping**
     neighbour boxing it in (recursively, `visited`-guarded in slot/face order),
     eagerly within the same request rather than one link per tick. So *b1*
     boxed by *b2* now scatters *b2* first; the chain marches and the deadlock is
     gone. **Determinism / RNG:** the recursion is `visited`-guarded (each unit
     scattered at most once per tick), so the per-tick sync draw count stays
     bounded by the number of distinct friendly units in the scene — the
     `scatter_livelock_proptest` RNG bound (`TICKS·blockers·8+64`) and the
     `fully_boxed`/`three_packed` per-tick draw pins stay green (a blocker boxed
     by an *enemy* has no friendly neighbour to cascade to, so it still draws
     exactly once). No new golden churn (cascade only fires on a real
     multi-friendly block, absent from every golden fixture). **Pin geometry
     corrected (ra-tester, M7.12 audit).** The regression test
     `multi_blocker_chain_..._mover_gets_through` originally used a plain 1-wide
     dead-end corridor and asserted the mover reaches `x ≥ 22`, which is
     geometrically impossible there (two blockers pushed east pile at x22/x23, so
     the mover reaches at most x21 — a test-authoring error, not a production
     bug). Rather than weaken the `reached` bound, the geometry was rebuilt: a
     1-wide corridor that dead-ends at its east edge with a pair of one-cell
     **alcoves** carved north and south of the dead-end cell (the only
     off-corridor escapes on the map). The mover still pushes *b1* flush into
     *b2* (b1 fully boxed → the cascade is the *only* way forward), but now each
     blocker steps **off** the corridor row into an alcove, clearing the path so
     the mover genuinely reaches the dead-end. The test asserts both blockers
     scatter, no overlap ever, and the mover arrives *because* they stepped
     aside; reverting this cascade returns it to the permanent gridlock (verified
     in the M7.12 revert-sensitivity pass), so the fix is proven load-bearing.
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

> **M7.12 art pass (ra-coder).** The text SELL/REP buttons are replaced with the
> original **icon** buttons when real assets are present: `SELL.SHP` (the gold `$`)
> and `REPAIR.SHP` (the wrench), extracted from `hires.mix` (34×28, 3 frames:
> up / pressed / disabled), matching `SidebarClass`'s
> `Upgrade.Set_Shape("SELL.SHP")` / `Repair.Set_Shape("REPAIR.SHP")`
> (`sidebar.cpp:319`/`:310`). They render at native size side-by-side in the header
> (repair left of sell, as `Repair.X < Upgrade.X`), drawing frame 1 (pressed) while
> the mode is armed and frame 0 otherwise (the original's `IsToggleType` +
> `ReflectButtonState`). **Text fallback stays** when the shapes are missing
> (`sell_button_art`/`repair_button_art` = `None`), so the no-asset goldens are
> byte-identical; only the real-asset (`hires`) `compose_game` frames move,
> re-pinned rendering-only. The button *rects* switch to native-art geometry only
> when art is installed, so click hit-testing always matches what is drawn. Lores
> (`lores.mix`, 17×14) art also exists and is a future low-res option; the hi-res
> client uses the 34×28 hires shapes.

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

---

## Q21 — House IQ + the `[IQ]`/`[AI]` tables: ratio-driven base building, auto-harvester, IQ-gated scatter (M7.14)

**Milestone:** M7.14 (the dedicated AI-fidelity milestone: "AI building/mining/
fighting must be smart — clone the old game's tricks"). Adds the two rules.ini
control tables the AI was missing and rebuilds base construction on the original's
ratio system.

### P0 — house IQ + the `[IQ]` table + the scatter gate

**`[IQ]` table (`RulesClass::IQ`, rules.cpp:IQ()).** Parsed into
[`IqRules`](../ra-sim/src/catalog.rs) on `EconRules` — `MaxIQLevels` + the
per-behaviour thresholds (`SuperWeapons`/`Production`/`GuardArea`/`RepairSell`/
`AutoCrush`/`Scatter`/`ContentScan`/`Aircraft`/`Harvester`/`SellBack`). Defaults
are the reference compile-time values (rules.cpp:137-147); the loader
(`assets.rs::iq_rules`) reads the real rules.ini overrides (stock RA:
`Harvester=2`, `RepairSell=1`, `DefenseRatio=.4`, …).

**House IQ.** New [`House::iq`](../ra-sim/src/house.rs). A **computer** house runs
at `Rule.MaxIQ` (`scenario.cpp:2890`: skirmish/MP `Session.Type != GAME_NORMAL →
IQ = Rule.MaxIQ`) — assigned in [`World::set_ai`] for every skirmish AI house; the
**human** and every synthetic-catalog house keep IQ `0`. **Campaign is left at IQ
0** on purpose: in `GAME_NORMAL` the scenario INI's `IQ=` key defaults to `0`
(`house.cpp:7454`), so campaign enemies stay scripted (and every campaign golden is
byte-identical). `iq` is folded into the house hash **only when non-zero**, so
human/synthetic houses append no bytes — only AI-bearing houses change.

**The scatter gate (`CellClass::Incoming`, cell.cpp:2025).** The gate is
`nokidding || Rule.IsScatter || House->IQ >= Rule.IQScatter` — computer units
auto-scatter from threats/blockers, a human does not, unless the call *forces* it
with `nokidding`. Ported as [`scatter_gate`](../ra-sim/src/world.rs).

- **The human harvester-dock case is resolved faithfully with NO deviation.** The
  M7.12 movement-deadlock reaction (mover committed to its sole landing cell, a
  friendly stationary blocker there, no detour) is the original's **`nokidding ==
  true`** site — `drive.cpp:1090/1214`, `Incoming(0,true,true)` — which forces the
  blocker out **regardless of IQ**. So a *human* harvester (IQ 0) still nudges a
  parked ally aside and reaches its dock (the original Q5 complaint), exactly as the
  real game does. The M7.12 quirk's earlier citation of `drive.cpp:970/1034`
  (`nokidding == false`) was imprecise: those softer sites *give up* first
  (`Distance(NavCom) < CloseEnoughDistance && !In_Radio_Contact → return`), whereas
  our port *forces* — the `nokidding == true` behaviour. Corrected in the Q5 call-
  site comment. **Consequence: every M7.13 scatter test stays green untouched, no
  synthetic house needed a high IQ or a `PlayerScatter` override, and no golden
  moved.** The gate is genuinely wired (each occupier consults `scatter_gate` with
  `nokidding = true`), so it is ready for the IQ-gated combat/threat scatter (the
  artillery-dodge, `nokidding == false`), which is where the gate visibly bites —
  see the P3 cut below.

### P1 — ratio-driven `AI_Building` (the "building" trick) + auto-harvester (the "mining" trick)

**`[AI]` table (`RulesClass::AI`, rules.cpp:AI()).** Parsed into
[`AiRules`](../ra-sim/src/catalog.rs): `AttackInterval`/`AttackDelay`/
`CreditReserve`/`PowerSurplus`/`BaseSizeAdd`, and the per-category
**`*Ratio`/`*Limit`** pairs (Refinery/Barracks/War/Defense/AA/Tesla/Helipad) plus
`PowerEmergency`. Ratios stored as raw 16.16 fixed.

**Ratio-driven base composition** ([`ai.rs::next_structure`], port of
`HouseClass::AI_Building`, house.cpp:5696). Replaces the old fixed
power→refinery→factory priority ladder. Each category is a build *choice* iff
`current < Round_Up(Rule.<Cat>Ratio × CurBuildings) && current < <Cat>Limit` (and
its money/prereq gate); each choice carries an **urgency** (`UrgencyType`,
defines.h:663 — refinery `HIGH` when none, power `LOW`/`MEDIUM`, most others
`MEDIUM`); the AI builds the **most urgent**, ties resolving to the
earlier-declared category (the original's strict `Urgency > best` scan,
house.cpp:5990). Declaration order matches the source: power → refinery → barracks
→ war factory → radar → defense.
- **Reference note.** Desired counts multiply `CurBuildings` directly — the
  `BaseSizeAdd` cap in `AI_Building` is present but **commented out**
  (house.cpp:5716), so the shipped game uses the raw ratio (rule 3: reference is
  ground truth; the brief's "+ BaseSizeAdd" description is the commented-out path).
- **Self-limiting (removes the M7.11 runaway).** The ratio×limit fixed point bounds
  base size naturally (e.g. defense `.4`, limit 40, converges to a modest base), so
  the M7.11 spare-power-plant "wall-in" runaway can no longer happen — the old
  discretionary spare-power tail and its reliance on the rubber-band building cap
  are gone from `next_structure` (the `max_units` combat-vehicle cap is retained).
- **Taxonomy adaptation (deviation).** We fold AA/Tesla into the single "defense"
  category (no aircraft modelled; the defense pick takes the strongest buildable
  armed building, reverse catalog order) and skip Helipad/Airstrip (no aircraft
  sim). Ratios/limits are 100% rules.ini; only the category *mapping* is adapted.

**Auto-harvester replacement** ([`ai.rs::produce_units`], port of
`HouseClass::AI_Unit`, house.cpp:6075). Now **IQ-gated**: `iq >= Rule.IQHarvester
&& BQuantity[REFINERY] > UQuantity[HARVESTER]` queues a replacement harvester in the
war factory. A computer house (IQ = MaxIQ ≥ IQHarvester) keeps one harvester per
refinery — kill an AI harvester and it buys another (the economic reflex ours
lacked). A human (IQ 0) gets no free replacement.
- **Deviation (cited).** The original also skips replacement on
  `Difficulty == DIFF_HARD` (house.cpp:6076). We do **not** replicate it — our
  difficulty labels are inverted for AI opponents (Q15) and the acceptance bar
  requires economic recovery at *every* difficulty, so a killed AI harvester is
  always replaced. (A refinery's *free* harvester on placement, house.cpp:2640, is
  ungated in both engines.)

**CreditReserve.** `[AI] CreditReserve` (stock 100) overrides `RepairThreshhold`
(the AI repair-affordability floor, rules.cpp:AI()); the synthetic default stays
`1000` so synthetic AI repair behaviour is unchanged, and the real-asset AI repairs
down to 100 credits.

### Determinism / goldens

`IqRules`/`AiRules` live on the immutable `Catalog` (not hashed). `House::iq` is
hash-gated to non-zero (AI houses only). The ratio-driven `next_structure` is
deterministic (no new sim-RNG draw; the weighted unit pick is unchanged), so
same-seed AI-vs-AI stays reproducible. **Re-pins (documented):**
`ai_suite::ai_builds_full_roster_in_next_structure_priority_order` — the fixture's
`PBOX` prereq was `vec![]` (buildable from tick 0), which under the faithful ratio
system let the MEDIUM-urgency defense category outrank the LOW-urgency early power
plant (the real AI never does this — RA's pillbox has a production-building
prereq). Corrected the fixture prereq to `[WEAP]` (matching its own comment) and
re-pinned the order to `POWR < PROC < BARR < WEAP < DOME < PBOX` (barracks is
declared before the war factory in `AI_Building`, so BARR now precedes WEAP — more
faithful than the old fixed WEAP<BARR). New acceptance coverage:
`ra-sim/tests/ai_ratio_suite.rs` (killed-harvester recovery, IQ-0 no-replacement,
base-composition ratio/limit bounds).

### Cuts (honest, cut from the bottom per the milestone's priority order)

- **P2 (Expert_AI urgency ranking across competing actions) — CUT.** The brief's
  P2 asks to rank build/attack/raise-money/raise-power/fire-sale/team-dispatch by
  urgency each pass and act on the highest (house.cpp:4874), replacing the fixed
  `step()` sequence. **Not done.** The existing AI already ports the substantive
  Expert_AI pieces — weighted **enemy selection** + rubber-band caps (Q16 b/c),
  **composed teams** with weakest-sector routing + escalation + all-out (Q16 d,
  Q20), and **economic reflexes** repair/sell/fire-sale (Q16 e) — just driven as a
  fixed per-tick sequence rather than a single urgency arbiter. Rewriting `step()`
  as one urgency loop is a substantial refactor with real regression risk to the
  M7.10/M7.11-tuned decisiveness, and the observable "fighting" behaviour is already
  competent, so it is deferred to keep this milestone green and committable.
- **P3 (combat reflexes) — CUT.** ~~(a) Produced units getting **Area Guard** when
  `IQ >= IQGuardArea`; (b) the **artillery-dodge**…; (c) `RepairSell` IQ-gating…~~
  **All three (and the new-beats-old harness) are now LANDED in the M7.14 audit
  follow-up — see Q22.**

---

## Q22 — M7.14 audit follow-up: honest new-vs-old A/B, faithful repair throttle, IQ-gated combat reflexes

**Milestone:** M7.14 audit follow-up (closes the three debts the M7.14 audit
surfaced — the CUT items in Q21). Priority order P0 → P1 → P2, cut from the bottom.

### P0 — Expert-vs-Legacy A/B (the "does new actually beat old?" gap)

M7.14 replaced the old fixed-priority AI **in place**, so the claim "the ratio/IQ
Expert AI beats the pre-M7.14 fixed-ladder AI" was never proven. Built the honest
head-to-head. New [`AiProfile::{Legacy, Expert}`](../ra-sim/src/ai.rs) on
`AiPlayer` (default **Expert** = the shipping policy):
- **`Legacy`** restores the verbatim pre-M7.14 policy — the fixed
  power→refinery→war→barracks→radar→defense→expand ladder
  (`next_structure_legacy`, a frozen snapshot of `next_structure` at git
  `9155fce^`) **plus** the pre-M7.14 unconditional harvester replacement. It is
  **test/measurement infrastructure only** — never installed in real play, and
  folded into the AI hash **only when != Expert**, so every real game and every
  pre-follow-up AI golden is byte-identical.
- Everything else (combat, movement, teams, economic reflexes) is **shared**, so a
  race isolates exactly the M7.14 delta: ratio-driven base composition + IQ-gated
  economy.

**Brief-vs-archaeology correction (documented, not rigged).** The brief described
pre-M7.14 as having *no* auto-harvester replacement. Git archaeology shows it *did*
(`refineries > harvesters`, ungated); Q21's "the economic reflex ours lacked" was
imprecise. Stripping replacement from `Legacy` would only rig the A/B in Expert's
favour, so the faithful snapshot **keeps** it (rule 3: reference/archaeology is
ground truth). The A/B therefore measures the base-composition policy honestly.

**THE HONEST FINDING (reported, not tuned away).** At **equal handicap** (both
Normal), on both real scenarios, both orientations (start-swap), the full record
(`ui_ai_vs_ai::real_expert_vs_legacy_ai_ab_record`) is:

| scenario | Expert=A          | Expert=B          |
|----------|-------------------|-------------------|
| scg05ea  | B wins (Legacy), 15 808 t | B wins (Expert), 18 810 t |
| scm01ea  | B wins (Legacy), 31 165 t | B wins (Expert), 28 902 t |

**All four games are won by house B, whichever policy sits there.** The winner is
decided by the **starting seat**, not the AI policy. Each policy wins exactly when
it holds house B (2/4 apiece) — a perfectly symmetric 1-1 on each scenario. **Expert
does NOT reliably beat Legacy: the two are at near-parity, and the map's start
asymmetry dominates the sub-threshold policy difference.** This is expected in
hindsight — M7.14 was a *fidelity + self-limiting* change (matching the original's
ratio system; bounding base growth), sharing all combat/team/economy code with the
already-M7.10/M7.11-tuned Legacy, so it did not make the AI raw-stronger. Per the
brief ("if Expert does not reliably beat Legacy, report it honestly, do not tune the
test to pass"), the test asserts only the **true** invariants — every A/B game
resolves *decisively*, and Expert is **not strictly dominated** (wins ≥1/4) — and
prints the full record. It deliberately does **not** assert an Expert sweep.

### P1 — faithful repair economics (RepairTimer throttle + real CreditReserve)

The audit found the AI repair reflex hardcoded a `1000` floor because it repaired
**every** decision pass with no cooldown; the real game throttles. Ported
faithfully (ai.rs `economic_reflexes`):
- **`RepairTimer` cooldown** (`Repair_AI`, building.cpp:5842). A new per-controller
  `repair_timer` counts down each tick; the AI may only *begin* a repair when it is
  0, then re-arms it to `Random_Pick(RepairDelay·(TICKS_PER_MINUTE/4),
  RepairDelay·TICKS_PER_MINUTE·2)` — `RepairDelay = .02` (rules.cpp:316 default),
  `TICKS_PER_MINUTE = 900` → `Random_Pick(4, 36)` ticks — drawn from the **sync
  RNG** at the cited site (this stands in for the engine's `DidRepair`/`RepairTimer`
  gate, house.cpp:1433).
- **Real `CreditReserve` floor.** The dead-until-now `AiRules::credit_reserve` (stock
  rules.ini `[AI] CreditReserve=100`, overriding `RepairThreshhold`, rules.cpp:724)
  is now the repair-affordability floor, replacing the hardcoded `1000`. The
  synthetic default stays `1000`, so synthetic AI repair economics are unchanged.
- **RepairSell IQ-gate** (P2c). Both the repair reflex and the sell-when-broke reflex
  are now wrapped in `IQ >= Rule.IQRepairSell` (building.cpp:5829), matching the
  reference — a house below the threshold does neither.

**Decisiveness confirmed; the throttle is fidelity, not the cause.** `ui_ai_vs_ai`
stays decisive at **every** difficulty on **both** scenarios (scg05ea Hard 4 997 t /
Normal 22 602 t / Easy 31 632 t; scm01ea Hard 19 757 t) and Hard-beats-Easy holds
start-independently. **Correction (M7.15 audit):** an earlier draft of this note
claimed the throttle "makes CreditReserve=100 safe / papers a deadlock the 1000
band-aid hid." The audit disabled the throttle (repair every pass at
CreditReserve=100) and **no starvation deadlock returned** — repair count moved
127→130 and the game stayed decisive (tick 22 571 vs 22 602). So the throttle is a
faithful port of the original's repair pacing, but it is **not** load-bearing for
decisiveness on the current M7.10/M7.11-tuned economy; the "prevents a deadlock"
justification is withdrawn as unsubstantiated.

### P2 — combat reflexes (activate the dead IQ scatter arm)

- **(a) Area-Guard on produce** (`Enter_Idle_Mode`: `IQ >= Rule.IQGuardArea &&
  Is_Weapon_Equipped → MISSION_GUARD_AREA`, infantry.cpp:1849-1856). In
  `spawn_produced_unit`, a **computer** house (IQ ≥ IQGuardArea = 4) starts each
  produced, armed unit in `Mission::AreaGuard` (guards a zone around its
  factory-exit post) instead of plain Guard; a human (IQ 0) keeps Guard. Inert for
  every synthetic house (IQ 0). Pinned both directions
  (`combat_reflex_suite::{computer_produced_unit_gets_area_guard,
  human_produced_unit_stays_plain_guard}`).
- **(b) Artillery/grenade-dodge** — the IQ-gated combat threat-scatter, the classic
  trick, now **observable**. At fire time, if the weapon's projectile is slow
  (`MaxSpeed < Rule.Incoming`), the engine lets the **target cell's occupiers** run
  away: `Fire_At → Map[As_Cell(TarCom)].Incoming(Coord, true)` (infantry.cpp:3841;
  the same at the `DriveClass`/`UnitClass` fire sites) → `CellClass::Incoming`
  (cell.cpp:2013) scatters *each* occupier, gated per-occupier by `nokidding ||
  Rule.IsScatter || House->IQ >= Rule.IQScatter`. Ported in `fire()` →
  `incoming_scatter()`, reusing `scatter_blocker` (generalised to take a **threat**
  coord — the dodger flees *away* from the firer, `Dir_Facing(Direction8(threat,
  Coord))`, drive.cpp:201) with `scatter_gate(nokidding=false)`. So a **computer**
  unit (IQ ≥ IQScatter = 3) dodges and a **human** unit (IQ 0) stands its ground —
  the human/computer differentiation the audit flagged as unobservable, pinned both
  directions (`combat_reflex_suite::{computer_unit_dodges_an_incoming_slow_projectile,
  human_unit_does_not_dodge_the_identical_shell}`), plus determinism and a
  fast-projectile no-dodge pin. Draws one sync-RNG jitter per dodger.

  **Fidelity correction (brief said "arty/V2"; rules.ini says otherwise).** The
  reflex is `Rule.Incoming` (`[General] Incoming=10`, → scaled 25, parsed into the
  new `EconRules::incoming_speed`, `MPH_IMMOBILE=0` default). Among ground weapons
  the **E2 grenade** (Speed 5 → 12) and the cruiser **8Inch** shell (6 → 15) trip
  it, but ARTY's **155mm** (12 → 30) and the V2's **SCUD** (25 → 64) are *faster*
  than the threshold and do **not** dodge — so, like the "V2 is not arcing"
  correction in Q8, only genuinely slow projectiles trigger it, per the data.

### Determinism / goldens (re-pin inventory: **ZERO**)

- `EconRules::incoming_speed` defaults to `0` (MPH_IMMOBILE), so the dodge is
  **inert for every synthetic catalog** — no synthetic combat golden, no
  single-unit oracle, and no synthetic-AI hash draws the new RNG. Verified: full
  `ra-sim` suite green with **zero** re-pins (including `determinism`, `scatter_suite`,
  the AI suites).
- `scatter_blocker`'s new `threat` param is `None` on both existing (friendly-blocker)
  call sites, taking the identical facing-based path — byte-identical.
- Area-Guard-on-produce is gated on `IQ >= IQGuardArea`, which every synthetic/human
  house (IQ 0) and every **campaign** house (IQ 0, Q21) fails — so no campaign or
  synthetic golden moves.
- Real-asset **campaign** goldens (`incoming_speed = 25`) were re-run and are
  **byte-identical** — the scripted playthroughs don't fire a scatter-triggering
  slow weapon at an eligible target, so no campaign trigger/placement/outcome golden
  moved (none needed re-pinning or a winnability re-check).
- `AiProfile` folds into the hash only when `!= Expert`; `repair_timer` only when
  non-zero — so no real game / pre-follow-up AI golden changed. The repair-throttle
  RNG draw only fires when an AI actually repairs (and at MaxIQ ≥ IQRepairSell); the
  synthetic AI suites don't damage buildings in their windows, so they stayed green.

---

## Q23 — Sell/repair mode cursors + reminder banner, sell/repair effects (sound + visual)

**Milestone:** live-playtest polish (P0 cursors + state reminder, P1 sell effect,
P2 repair effect). All client-side and **sim-inert** — same input script with
effects on vs. off yields identical sim hash chains (`ui_cosmetic_determinism::
sell_repair_cosmetic_layer_on_vs_off_yields_identical_sim_hashes`).

### P0 — mode cursors + reminder banner

**Cursor frames (cited).** The original's sidebar cursor states are in the
`MouseControl` table (`mouse.cpp:346`, `{StartFrame, Count, Rate, Small, HotX,
HotY}`), keyed by `MouseType` (`defines.h:2578`):
- `MOUSE_SELL_BACK` → MOUSE.SHP frame **68** (12-frame animated `$`), `mouse.cpp:358`.
- `MOUSE_REPAIR` → frame **35** (24-frame animated wrench), `mouse.cpp:360`.
- `MOUSE_NO_SELL_BACK` → frame **119** (`mouse.cpp:362`).
- `MOUSE_NO_REPAIR` → frame **120** (`mouse.cpp:361`).
- `MOUSE_NORMAL` → frame **0**.

`AppCore::cursor_kind()` (a new [`CursorKind`] accessor) maps the armed mode +
the object under the pointer to `Sell`/`NoSell`/`Repair`/`NoRepair`/`Normal`,
using the *same* own-alive-non-wall gate (`own_building_at_map`) that decides
whether a click acts — so the cursor never implies an illegal action, and reverts
to `Normal` over the sidebar. `compose_game` draws the cursor glyph at the pointer
(topmost) and a "SELL MODE"/"REPAIR MODE" reminder banner near the top of the
tactical area; the windowed shell hides the OS cursor while a mode is armed
(`show_mouse(false)`) so our glyph is the sole pointer. All pinned in
`ui_sell_repair_effects::cursor_kind_tracks_mode_and_hover_target`.

> **Deviation (documented): MOUSE.SHP is not decoded.** `MOUSE.SHP` (hires.mix /
> lores.mix) is a **legacy variable-size shape container** — a `u16 count` then a
> `u32` offset table, each frame carrying its own dimensions, with no global
> width/height — which our RA-format `SHP` decoder (`ra-formats/src/shp.rs`, header
> `count@0, width@6, height@8`) cannot read (it resolves width `0x03f1`, height `0`
> → "zero dimension"). Writing a decoder for that container was out of scope for a
> polish pass, so we render a **faithful stand-in**: the repair cursor and the
> repairing-building overlay both use the real `SELECT.SHP` wrench (frame 2,
> `SELECT_WRENCH`, decodes cleanly — it is a normal RA SHP), and the sell cursor is
> a gold bitmap-font "$". The "no" variants overlay a red prohibition slash. The
> frame indices above are cited so a future MOUSE.SHP-container decoder can swap in
> the exact art with no interface change (the `CursorKind` seam already carries the
> logical state).

### P1 — sell effect (sound + visual)

Faithful to `BuildingClass::Mission_Deconstruction` (`building.cpp:3722`). A
building removed by `Command::Sell` this tick is detected in `step_tick` (the tick's
`Command::Sell` targets that vanished) and given **sell-back** feedback instead of a
combat explosion:
- **Visual — reverse buildup.** `EffectKind::Deconstruct(type_id)` plays the
  building's `<NAME>MAKE.SHP` buildup band in *reverse* (the original's
  `Begin_Mode(BSTATE_CONSTRUCTION)` reversal, `building.cpp:602-606`), anchored at
  the top-left. Falls back to the shared explosion when a type has no MAKE art, so a
  sale is never fully invisible.
- **Sound.** `SoundEvent::Sell` → `CASHTURN.AUD` (the cash-turn SFX, `VOC_CASHTURN`,
  `building.cpp:3840`) and, for a **player**-owned sale, `SoundEvent::StructureSold`
  → `STRUSLD1.AUD` (EVA "Structure sold", `VOX_STRUCTURE_SOLD`, `building.cpp:3972`).
  A sale deliberately does **not** queue the combat-death `Explosion` cue.

Pinned: `ui_sell_repair_effects::selling_queues_cash_and_eva_and_spawns_deconstruct`.

### P2 — repair effect (sound + visual)

- **Sound.** Toggling repair **on** (a false→true `is_repairing` transition on a
  player building) queues `SoundEvent::Repair` → `RAMENU1.AUD`. This is the
  original's building self-repair toggle sound: `BuildingClass::Repair` plays
  `VOC_CLICK` (= RAMENU1, `building.cpp:2770`). **RA plays no EVA for building
  self-repair** — `VOX_REPAIRING` (`building.cpp:4313`) is the service-depot
  (`FIX`) unit-repair path only, so we do not queue one (faithful).
- **Visual — pulsing wrench.** `compose_game` draws the real `SELECT.SHP` wrench
  (frame 2, `SELECT_WRENCH`) centred over every building with `is_repairing == true`,
  blinking on the cosmetic clock — the original's `IsRepairing && IsWrenchVisible`
  overlay (`building.cpp:520`, `CC_Draw_Shape(SelectShapes, SELECT_WRENCH,…)`; the
  original toggles `IsWrenchVisible` per repair step). Synthetic spanner primitive
  when `SELECT.SHP` is absent.

Pinned: `ui_sell_repair_effects::toggling_repair_queues_sfx_and_renders_wrench`.

### Determinism / goldens (re-pin inventory: **ZERO**)

All of the above lives in `ra-client` and only reads sim state (in `step_tick`'s
post-tick diff and `compose_game`); it never mutates the sim or draws the sim RNG.
- **No sim change at all** — `ra-sim`, `ra-formats`, `ra-data`, `ra-net` untouched,
  so every sim/determinism/AI/campaign golden is byte-identical (full suites green).
- **No `compose_game` golden moved.** The cursor, banner, wrench overlay, and
  sell/repair effects only render when a mode is armed / a building is repairing /
  an effect is live — none of which any existing golden fixture sets. Every
  `ui_shroud_golden`, `ui_menu_golden_frames`, `ui_mode_button_art_suite`, and
  `ui_golden_frames` frame is unchanged (verified: full `ra-client --no-fail-fast`
  green, **zero** re-pins). New rendering is exercised only by the new
  `ui_sell_repair_effects` PNGs/asserts and the extended `ui_cosmetic_determinism`.
- New `SoundEvent` variants (`Sell`/`StructureSold`/`Repair`) and `EffectKind::
  Deconstruct` are additive; the audio queue and effect layer stay §4.2-cosmetic.

---

## Q24 — Aircraft: the flight locomotor, helicopters, the helipad, and AA defense (P0 aircraft arc)

**Milestone:** P0 aircraft arc (the skirmish-buildable air core — new
`Locomotor::Air` + `UnitKind::Aircraft`, `run_aircraft`, HPAD/AGUN/SAM).

**What we implemented (with reference cites).**

- **Flight locomotor (`fly.cpp`/`aircraft.cpp`).** Aircraft are a third
  [`UnitKind::Aircraft`] on the shared `Units` arena with an `altitude` (leptons,
  `0..=FLIGHT_LEVEL`=256, `object.h:299`), `ammo`, an [`AirState`] FSM, a `home`
  helipad, and a `rearm_timer`. They fly **straight to their goal at altitude,
  ignoring all ground land-type passability** (`FlyClass::Physics`, `fly.cpp:74`
  `Coord_Move` along facing; `Passability::is_passable_loco` returns `true` for
  `Locomotor::Air`). Movement rotates the flight facing toward the goal at the
  craft's `ROT` and steps `MaxSpeed` leptons/tick, snapping within one step
  (`Process_Fly_To`, `aircraft.cpp:2206`); an off-map step is refused
  (`IMPACT_EDGE`, `fly.cpp:92`). Aircraft occupy **no** ground cell — they are
  skipped by the ground movement grid, the one-vehicle-per-cell invariant, and
  `vehicle_in_cell`/`vehicle_targeting`, so several may overfly one cell (the
  original's air-layer has no cell occupancy). All state is integer/fixed — no
  float — and hashed **only for aircraft**, so every prior golden is byte-identical
  (verified: full `ra-sim --no-fail-fast` green, **zero** re-pins).

- **Helicopter attack + rearm cycle (`aircraft.cpp`).** `run_aircraft` is a
  slimmed, deterministic port of the `AircraftClass` mission handlers. A heli with
  a target and ammo takes off, flies into weapon range, hovers, aims and fires on
  ROF cadence (`Mission_Attack` `FIRE_AT_TARGET`, `aircraft.cpp:2642`), decrementing
  `ammo` per shot; at `ammo == 0` it flies to its home helipad (`MISSION_ENTER`
  after `Ammo == 0`, `aircraft.cpp:3869`), descends to land (altitude → 0), and
  the pad **reloads one round per `Rule.ReloadRate` cadence** (default `.05` min =
  45 ticks/round at full power, `building.cpp:4438`) until full, then takes off and
  re-attacks. Verified end-to-end in `ra-sim/tests/aircraft_suite.rs`
  (`heli_strafes_returns_to_helipad_rearms_and_reattacks`): 3-round magazine —
  **emptied @ tick 17, landed @ 84, refilled @ 219**, then re-attacks.

- **Helipad (HPAD) + buildability.** HPAD is a plain `2×2` building identified by
  **catalog name** (the table-free DOME/FIX role pattern, Q10 / §3.8) — no new
  `BuildingProto` flag. Aircraft dock there to rearm and land there when idle
  (`Enter_Idle_Mode`). Helicopters (HELI Longbow, HIND Hind) build on the **vehicle
  (Unit) production lane but gated on a live helipad** (not a war factory): a new
  `need_helipad` branch in `apply_start_production`, and produced aircraft
  materialise **on the helipad, airborne, full** (`AircraftClass` ctor,
  `aircraft.cpp:254`; exit routed to the pad in `finish_or_retry`).

- **AA defense (AGUN / SAM).** Anti-air emplacements are identified by catalog name
  (`building_is_aa`, AGUN/SAM) and fire **only at airborne aircraft**; every other
  defense fires **only at ground targets**. This is the reference's projectile
  `IsAntiAircraft`/`IsAntiGround` gate against target `Height > 0` (`Can_Fire`,
  `techno.cpp:2895`), threaded through `acquire_nearest_enemy`/
  `validate_building_target` as an `aa` selector. Ground weapons cannot acquire,
  retaliate against, or alert-respond to an airborne attacker (they have no way to
  hit it); a **landed/docked** aircraft (`altitude == 0`) is a ground target like
  any vehicle. Verified (`aa_gun_downs_an_airborne_heli_but_a_pillbox_cannot`): the
  AGUN downs the heli @ tick 80 while the adjacent PBOX never targets it.

- **Damage / crash.** Aircraft take damage through the normal `explosion_damage`
  path (they are units), with **half damage while airborne** (`if (Height) damage
  /= 2`, `aircraft.cpp:1685`), and are removed on death like any unit (the client
  draws the shared explosion — the crash spin/fireball is cosmetic, deferred).

**Deviations / simplifications (documented).**
1. **AA capability derived by building name, not a weapon/warhead flag.** The
   reference AA gate is a projectile `AA=`/`AG=` bool. We derive "is AA emplacement"
   from the catalog name (AGUN/SAM) instead of adding `anti_air`/`anti_ground` to
   `WeaponProfile` — because that struct is hashed on every armed unit/building/
   bullet and constructed by ~30 test literals, so a field there would churn every
   combat golden's struct/hash for a value that never changes at runtime. The
   name-derived check is the same §3.8 table-free role pattern already used for
   DOME/FIX. Consequence: a *unit-mounted* AA weapon (e.g. the mammoth's
   MammothTusk vs. air) is **not** modelled — only building AA (AGUN/SAM). Air-to-
   air is likewise deferred (helis attack ground only).
2. **Rearm cadence is the compile-time `ReloadRate=.05` default**, and the
   power-fraction slowdown (`Inverse(pfrac)`, `building.cpp:4438`) is deferred —
   rearm is a flat 45 ticks/round regardless of pad power.
3. **Altitude take-off/landing** is a linear `±Pixel_To_Lepton(1)` (≈10 leptons)
   per tick between 0 and `FLIGHT_LEVEL` (`Landing_Takeoff_AI`, `aircraft.cpp:4195`),
   without the staged approach-circuit speeds; the `Good_Fire_Location` reposition
   scan and `Rule.IsCurleyShuffle` between-shots repositioning are deferred (the
   heli hovers at its first in-range point and strafes).
4. **Player Move of an aircraft** flies straight to the ordered cell (no ground A*,
   terrain-ignoring); this is also how the fly-over-impassable-terrain acceptance
   is exercised.

**Cuts (reported, cut from the bottom of the priority stack).**
- **P1 — Chinook (TRAN) evac + fixed-wing (MIG/YAK/AFLD).** Not implemented.
  Campaign mission 1's Einstein evac stays the existing reach-the-LZ
  `EVAC_CIVILIAN` win (unchanged, still winnable — `campaign_scg01ea` green), and
  the `scg04ea` `[Base]` **still drops both AFLD nodes** (that golden pin is
  untouched — I deliberately did **not** add the AFLD footprint, so the count stays
  13/15). Scenario-placed aircraft remain dropped by `is_naval_or_air` (no campaign
  golden moves).
- **P2/P3 — BADGER/U2, passenger unload, and all AI air.** ~~The AI does not build
  helipads/aircraft/AA…~~ **AI air CLOSED in M7.17-B (see Q24.1):** the AI now builds
  a helipad + helis per `HelipadRatio`, AA per `AARatio` (threat-gated), and helis
  join attack waves — AI-vs-AI stays decisive at all difficulties on both scenarios.
  BADGER/U2 (fixed-wing) and passenger unload remain cut.
- **Sidebar cameos.** ~~kept out of the sidebar strip for now~~ **CLOSED in M7.17-B
  (see Q24.1):** HPAD/AGUN/SAM and HELI/HIND are now in the two strips with their
  real `<NAME>ICON.SHP` cameos; zero goldens moved (the new rows sit below the
  default-visible window).

**Handoff to ra-tester.** Exhaustive adversarial coverage owed: rearm-under-low-
power, SAM landed-aircraft special case, multi-heli air-collision overlap,
determinism proptest with aircraft, AA-vs-multiple-aircraft retarget, and — once
wired — the sidebar-cameo re-pin and any AI-air goldens. The P0 smoke proof is
`ra-sim/tests/aircraft_suite.rs` (5 tests: attack/rearm cycle, AA-downs-air +
pillbox-cannot, fly-over-impassable-terrain, determinism, idle hover/land).

---

## Q24.1 — Aircraft playability: sidebar cameos, altitude/shadow/rotor render, AI air (M7.17-B)

**Milestone:** M7.17-B (the playability follow-up to Q24 — closes the three cuts
"sidebar cameos", "aircraft render", and "AI air" so aircraft are actually
player-usable and appear in AI-vs-AI).

**P0 — sidebar cameos surfaced.** HPAD/AGUN/SAM are added to the structures strip
and HELI/HIND to the units strip in `build_content`'s `buildables`
(`ra-client/src/assets.rs`). Cameos load automatically from `<NAME>ICON.SHP` in
`hires.mix` — verified present via `radump`: HPADICON/AGUNICON/SAMICON/HELIICON/
HINDICON. Prereqs are enforced exactly as for every other buildable
(`describe_buildable` / `apply_start_production`): HELI/HIND carry
`Prerequisite=hpad` and HPAD/AGUN/SAM carry `Prerequisite=dome`, so the cameos are
visible-but-not-buildable until the pad/dome exists. **Re-pin inventory: ZERO.**
Contrary to the Q24 cut's expectation, no golden moved — the new cameos are
inserted *after* the defenses (structures) and *after* the vehicles (units), which
places them **below the sidebar's default ~4-row visible window**; every pinned
`compose_game` frame renders the same top rows it always did. Verified by the full
golden suite (`ui_shroud_golden`, `ui_golden_frames`, `ui_menu_golden_frames`,
`ui_radar_cameo_f1_suite`, `ui_sidebar_scripted_drive`) staying byte-green.

**P1 — aircraft render (altitude / shadow / rotor / AA aim / crash).** In
`ra-client/src/appcore.rs::draw_units`, an aircraft is lifted by
`leptons_to_pixel(altitude)` (`FLIGHT_LEVEL`=256 leptons = one cell = `CELL_PIXELS`
px), with a darkened body-silhouette **shadow** stamped at the ground cell (offset
`+1,+2`, `draw_sprite_shadow` in `unit_render.rs`) — a port of
`AircraftClass::Draw_It` (`aircraft.cpp:408`: body at `y - Lepton_To_Pixel(Height)`,
shadow at ground `y` with `SHAPE_FADING|SHAPE_PREDATOR`). Body/turret/muzzle-flash/
selection-box/health-bar all draw at the lifted `sy_d`. **Rotor blades**
(`RROTOR.SHP`, 12 frames, loaded once into `AppCore::rotor_sprite`) are drawn
spinning over the lifted body on the *cosmetic* clock (`aircraft.cpp:521`
`Draw_Rotors`: airborne `Fetch_Stage()%4` fast frames 0..4, landed `(%8)+4` idle
frames 4..12 — we key off `altitude > 0`). Body facing → frame via the existing
32-facing `frame_for` (HELI/HIND are 32-frame SHPs, no turret). **AA aim:**
`target_screen_pos` now lifts an airborne-aircraft target by its altitude, so the
AA emplacement's tracer/muzzle-flash (`draw_defense_effects`) points *up* at the
flying heli. **Crash:** the existing death→explosion diff now captures each dying
unit's airborne altitude in the pre-tick snapshot and spawns the fireball *lifted*
(`Effect::lift_px`), so a downed heli explodes at flight height, not on the ground.
All of this is **client-side and sim-inert** — it only *reads* `altitude`/
`air_state`; determinism is unchanged (the sim already hashes aircraft state, Q24).

**P2 — AI builds and flies aircraft (re-enabling the Q24 cut, decisiveness-safe).**
`ra-sim/src/ai.rs`:
- The vehicle-lane aircraft exclusion (`locomotor != LOCO_AIR_INDEX`) is **removed**;
  helis re-enter the weighted-random army pool (armed → weight 20), gated on a
  helipad by their `Prerequisite=hpad` (`unit_buildable`). They auto-recruit into
  attack teams (`find_path` returns `Some` for `Locomotor::Air`) and fly to the
  target via the Q24 `run_aircraft` FSM.
- `next_structure` gains two categories, declared **before** the catch-all ground
  defense (the reference keeps base defense in the separate `AI_Base_Defense` pass,
  `house.cpp:5613`, so the helipad in `AI_Building`'s main list, `house.cpp:5976`,
  is not starved by the perpetually-unsatisfied defense ratio): (1) a **helipad**
  per `HelipadRatio` (first one Medium urgency so the AI reliably reaches air, extras
  Low), gated on income + a war factory; (2) **anti-air** (AGUN/SAM) per `AARatio`,
  **gated on `enemy_air_threat`** — an enemy live aircraft must actually exist. This
  threat-gate is the M7.17-A stall guard: a pure-ground game never builds AA, and AA
  is a wholly separate category that never counts toward or substitutes for ground
  defense. The ground-defense `desired` no longer folds in `aa_ratio` (AA is its own
  category now).
- **Decisiveness re-verified (the load-bearing bar):** `ui_ai_vs_ai` stays green on
  **both** scenarios at **all three** difficulties — scg05ea Hard 4997 t / Normal
  16693 t / Easy 25274 t, scm01ea Hard 21977 t (all ≪ the 45-min cap), Hard reliably
  beats Easy, symmetric building count bounded. An instrumented run confirmed air is
  genuinely *in the mix* (scm01ea: first heli ~t10000, peak 6 aircraft; AA appeared
  under the threat-gate). The M7.17-A stall did **not** return. Outcome ticks moved
  (air changes game length — outcomes are behaviour-validated, not hash-pinned).
- **Legacy A/B baseline untouched:** `next_structure_legacy` never builds a helipad,
  so `unit_buildable`'s `hpad` prereq keeps Legacy air-free — the exclusion removal
  is inert for the frozen baseline.

**Determinism / re-pins.** Synthetic catalogs contain no aircraft/helipad/AA, so
every AI/sim/determinism code path added here is dead for them — the full `ra-sim`
suite (including the single-unit oracle and determinism goldens) is byte-green with
**zero** re-pins; the entire `ra-client` suite (real + synthetic) is green with zero
re-pins. Full matrix verified: fmt, clippy (default + `--no-default-features` +
`--features window`), and `--no-fail-fast` tests across all five crates.

**PNG evidence** (`ra-client/tests/aircraft_render_png.rs`, real assets):
`aircraft_scene.png` (Longbow airborne — lifted body + ground shadow + rotor —
firing at a tank while an AA gun fires up at it), `aircraft_crash.png` (the heli
downed by the AA gun, fireball at flight altitude), `aircraft_cameos.png` (both
strips scrolled to show HELIPAD/AA GUN/SAM SITE and LONGBOW/HIND cameos).

**Still cut (unchanged from Q24):** Chinook/fixed-wing (MIG/YAK/AFLD), passenger
unload, air-to-air, curley-shuffle reposition, power-scaled rearm.

---

## Q25 — Naval arc P0: the water locomotor, shipyards, combat ships, and submarine stealth

**Milestone:** Naval arc P0 (the skirmish-buildable naval core — new
`Locomotor::Water` + naval yards + DD/CA/SS, over water). The same shape as the
aircraft arc (Q24): a new locomotor + new buildings + new units, but — unlike
aircraft — vessels reuse the **ground** movement/combat systems rather than a
bespoke FSM, because a ship is cell-based (one-per-cell, A* pathing, normal
combat) and differs from a vehicle only in *which* cells it may enter.

**What we implemented (with reference cites).**

- **Water movement (`Locomotor::Water`, `SPEED_FLOAT`).** A fourth locomotor whose
  passability is the **inverse** of the ground masks: a ship may enter a cell only
  where its land type is open water (`Passability::water` mask, `true` only on
  `LandType::Water`), never land/beach/rock/river. Vessels are ordinary
  `UnitKind::Vehicle` units carrying `locomotor == Water`, so they flow through the
  **existing** `move_units` (grid A*, one-vessel-per-cell occupancy via the shared
  `UnitGrid`, scatter/re-route) and `run_combat` (target/rotate/fire) with **no new
  movement or combat code** — the locomotor alone selects the water mask in
  `Passability::is_passable_loco`. A ground unit can never path to a water cell and
  a ship can never path to a land cell, so the two never contend for a cell despite
  sharing the occupancy grid. Deterministic, float-free. Verified
  (`naval_suite::ship_paths_over_water_only_never_onto_land`): a ship routes around
  a land wall to a water goal, never occupying a non-water cell, and refuses a Move
  ordered onto land (`find_path` returns `None`).

- **Naval yard (SYRD) + sub pen (SPEN) — shore placement + water spawn.** Identified
  by catalog **name** (`building_is_shipyard`, the §3.8 table-free role pattern, like
  helipad/AA). They are placed on **land** (footprint `is_static_passable`) but the
  footprint's 8-neighbour ring must include **≥1 open-water cell** — the reference's
  `WaterBound=yes` shore requirement (`bdata.cpp` SYRD/SPEN; simplified from the full
  `Passes_Proximity_Check` naval bib), enforced in `footprint_placeable` so both the
  client placement preview (`can_place_building`) and the sim reject an inland yard.
  A produced vessel **spawns into an adjacent water cell**: `finish_or_retry` routes
  a Water-locomotor unit to `find_shipyard_exit`, which searches the yard's exit ring
  with the Water locomotor (so the exit cell is guaranteed floatable and unoccupied);
  a blocked ring retries next tick like a blocked factory exit. Verified
  (`naval_suite::shipyard_requires_adjacent_water`).

- **Combat ships (DD/CA/SS).** Stats/prereqs from rules.ini: **DD** (destroyer,
  `Primary=Stinger`+`Secondary=DepthCharge`, `Sensors=Yes`, `Prerequisite=syrd`),
  **CA** (cruiser, `Primary=8Inch`, `Sensors=Yes`, `Prerequisite=syrd,atek`), **SS**
  (submarine, `Primary=TorpTube`, `Cloakable=yes`, `Prerequisite=spen`). They fight
  through the ordinary unit combat path (dual-weapon selection, ROF, warhead Verses)
  — a DD sinks a ship, a cruiser bombards at long range — with no per-ship code.
  Buildable on the vehicle (Unit) production lane gated on a **naval yard**
  (`need_shipyard` in `apply_start_production`, mirroring the helipad gate for air).

- **Submarine stealth (the signature mechanic).** A submarine (`is_submarine`,
  from `Cloakable=yes`) cruises **submerged** (`Unit::submerged`, cloaked). The
  `run_submarines` FSM (tick order 3.9, before combat) keeps it submerged while
  idle, **surfaces** it while it has a target, and holds it surfaced for a recloak
  grace window after (`SUB_RECLOAK_TICKS`, the reference's `PulseCountDown` /
  `VesselClass::Is_Allowed_To_Recloak`, `vessel.cpp:2044`). A submerged enemy sub is
  **hidden** (`is_hidden_submarine`): every target-acquisition path
  (`acquire_nearest_enemy` for buildings, `maybe_acquire_guard_target`,
  `maybe_acquire_hunt_target`) and the explicit `Command::Attack` skip it — a
  cloaked object is `MOVE_CLOAK`/untargetable to non-detectors (`vessel.cpp:296`) —
  **unless** a **detector** (`is_detector`, from `Sensors=Yes` — DD/CA) allied to the
  observer is within `SUB_DETECT_RANGE` (~5 cells) of the sub, which reveals it to
  the observer and its allies. So a destroyer hunts subs that a pillbox or a plain
  ship cannot even see. Verified both directions
  (`naval_suite::submarine_stealth_hidden_from_non_detector_visible_to_destroyer`).

**Client surfacing (skirmish).** `build_content` (`ra-client/src/assets.rs`) adds
SYRD/SPEN to the structures strip and SS/DD/CA to the units strip (appended **below
the default-visible sidebar window**, so — like the aircraft cameos, Q24.1 — no
pinned `compose_game` frame moves). Vessel body art (`ss/dd/ca.shp`) is in
conquer.mix, so ships **render through the existing vehicle draw path** (facing →
frame) with no new rendering code; they float on the water passability the client
now computes (`build_passability_masks_water`). Prereqs are enforced exactly like
every other buildable (SS→spen, DD→syrd, CA→syrd,atek).

**Determinism / golden discipline.** All new `Unit` fields are gated:
`submerged`/`recloak` are hashed **only for submarines**; `is_submarine`/`is_detector`
are type constants (like `locomotor`), unhashed. `run_submarines` and the stealth
gate short-circuit on non-submarines, so **every non-naval world is byte-identical**
(the full `ra-sim --no-fail-fast` matrix stayed green with **zero** re-pins). The AI
never builds a naval yard, and vessels carry real rules.ini prereqs (spen/syrd) the
AI never owns, so `unit_buildable` excludes them from the AI army pool — the
weighted-random RNG draws are unchanged, keeping AI-vs-AI hash chains intact.

**Cuts (reported, from the bottom of the priority stack).**
- **P1 — LST naval transport + rendering polish.** The landing craft (load a vehicle
  at shore → cross water → unload far shore) is **not** implemented; LST is in
  rules.ini (`Passengers=5`) and the transport cargo/load-unload machinery already
  exists (Q18), but wiring the shore load/unload is deferred. SYRD/SPEN **building**
  art is theater-side (not in conquer.mix) so the yards currently render frameless
  (placeholder); vessel cameos degrade to text (no `<NAME>ICON.SHP` found in
  hires.mix). Submarine submerged **visual** (semi-transparent/periscope/hidden) is
  not drawn — the sim hides the sub from targeting, but the client still draws its
  body; a proper stealth render is deferred.
- **P2 — AI naval + campaign LST reinforcements.** The AI does not build naval yards
  or ships (no coastal-base detection, no naval attack missions), and the campaign
  loader still **drops** naval placements (`is_naval_or_air`), so scg03ea's `aqua`
  LST team is not yet surfaced. Coastal-map AI-vs-AI is therefore land-only; **most
  current skirmish/test maps are land-locked**, so naval is exercised via the
  synthetic `naval_suite` (real-coastal-scenario acceptance is handed off — a coastal
  map must be selected/verified by ra-tester).
- **P3 — PT gunboat, depth-charge-vs-sub anim, unload-at-shore polish.** Not started.

**Handoff to ra-tester.** Exhaustive adversarial coverage owed: shipyard production
end-to-end on a real coastal scenario (place SYRD at a shore, build+spawn a DD in
water), naval combat outcomes (DD sinks a ship, CA bombards), multi-sub retarget and
recloak-grace timing, a determinism proptest with vessels+subs, and — once wired —
LST load/cross/unload and any AI-naval goldens. The P0 smoke proof is
`ra-sim/tests/naval_suite.rs` (4 tests: water-only pathing, sub stealth both
directions, determinism, shore placement). A **coastal scenario must be identified**
for the real-asset acceptance (ship-on-water, sub-stealth, shipyard-spawns-in-water
PNG evidence) — the current land-locked maps cannot exercise it.

---

## Q26 — Naval arc P0/P1: AI fields naval on coastal maps + authored campaign naval spawn (M7.18)

**Milestone:** Completes the M7.18-A naval cuts (AI naval, authored spawn) — the
follow-up to Q25's skirmish-buildable naval core. Closes the two audit blockers:
the AI never built naval (naval was human-only), and the scenario/campaign catalog
dropped every naval type so authored ships never spawned.

**P0 — AI builds & uses naval, coastline-gated, decisiveness-safe.** (`ra-sim/src/ai.rs`)

- **Coastline gating via the placement rule (no wasted production).** The AI adds a
  naval-yard build choice (SYRD) only when `has_income && has_war` **and**
  `placement_cell(SYRD).is_some()`. That last gate IS the coastline heuristic:
  `placement_cell` spirals `can_place_building`→`footprint_placeable`, which for a
  shipyard requires a land footprint whose ring touches open water (Q25). A
  **landlocked** base can never satisfy it, so the choice is never added, the AI
  never produces a yard it cannot place (no stuck `ready_building`), and **every
  downstream naval path is dead** — byte-identical to the pre-naval AI on land maps.
  RA has **no `NavalRatio`** (`AI_Building` never builds a shipyard; `AI_Vessel`,
  house.cpp, builds vessels in GAME_NORMAL only to fill naval *team types*, which
  our skirmish lacks), so this is a documented *addition* over the strict port: one
  yard, like the DOME.
- **Yard declared HIGH + early (before dome/helipad).** A cheap 650-cost yard
  deferred to a late Medium slot only won urgency once the war had drained the house
  to 0 credits, where it stalled unpaid until its construction yard died (observed on
  scm11ea). High + early builds it during the calm build-out while starting credits
  remain; it is one building, gated on the economy already standing, so it does not
  meaningfully delay the army.
- **Ship production is a surplus-funded supplement, NOT a tax on the land war (the
  decisiveness guard).** Vessels (DD/CA/SS) build on the shared unit lane, but only
  when the land army is **≥ ½ its rubber-band `MaxUnit`** *or* the house holds a
  large **cash surplus** (`NAVAL_SURPLUS`), capped at `NAVAL_DESIRED = 4` (a small
  behaviour-tuned bound; the reference cap is `MaxVessel = VesselMax/6 ≈ 16`,
  house.cpp:793). Vessels are **excluded from the weight-20 land vehicle pool** so
  they never flood it once a yard exists (the naval analogue of the M7.17-A AA-flood
  stall). This ordering keeps the land army at full strength — the land war remains
  the game's decider and still resolves in budget.
- **Naval attack behaviour (`command_navy`).** Idle armed vessels attack the nearest
  enemy **vessel** reachable over water (ships hunt ships; a submerged enemy sub is
  targetable only by a detector, mirroring `is_hidden_submarine`); when no enemy
  vessel is reachable they **bombard the nearest enemy coastal building** — a
  structure with a water cell within weapon range the vessel can reach — so a
  dominant navy can finish a coastal base the land army cannot reach across water.
- **Ship bombardment of coastal structures (`ra-sim/src/world.rs`).** `run_combat`'s
  building-approach is **Water-locomotor-gated**: a vessel closes to the nearest
  **water** cell within weapon range of the building (`nearest_water_approach`) and
  shells from there, instead of `nearest_adjacent_passable` (a *ground* cell it can
  never occupy). Ground attackers are byte-identical (the branch only fires for
  `Locomotor::Water`).

**Decisiveness — coastal vs. landlocked (both verified).**
- **scm11ea (58% water, coastal):** AI-vs-AI Hard-vs-Hard, both houses build naval
  yards; with a surplus economy both field combat vessels (peak 4/3) that hunt +
  bombard, and the game reaches a **decisive** outcome at **tick 25200 (~28 min)**,
  inside the 45-min budget. (At the stock 6000-credit tight economy the fast Hard
  game resolves via land ~t16476 before any surplus accrues — yards built, navy-less,
  still decisive.) *Note:* scm11ea's two AI bases sit on separate landmasses (naval
  trap); the map is fragile — at Normal it is an inherent stalemate (armies never
  engage) with **or** without naval. Verified in `ra-client/tests/naval_ai_vs_ai.rs`.
- **scg05ea / scm01ea (landlocked-in-harness):** the pinned land AI-vs-AI suite
  (`ui_ai_vs_ai.rs`) runs over **water-zeroed** passability (`Passability::new`), so
  the naval code is provably dead there — resolution ticks are **byte-identical** to
  pre-naval (scg05ea Hard 4997 / Normal 16693 / Easy 25274, scm01ea 21977, all exact
  matches). This is the landlocked-unchanged proof. (Under the *real* per-locomotor
  water passability those two maps do carry some water, so they are not a clean
  "no-naval" real-asset control — the water-zeroed harness is the invariant's home.)

**P1 — authored scenario/campaign naval spawn (the audit's single blocker).**
(`ra-client/src/assets.rs`) `register_campaign_unit` dropped every naval/air class
(`is_naval_or_air` → `None`). Replaced with **`campaign_locomotor`**, which resolves
the real locomotor: vessels (DD/CA/SS/MSUB/PT/LST) → Water, helicopters (HELI/HIND)
→ Air, else Foot/Wheel; only the classes we still cannot simulate (fixed-wing
MIG/YAK/U2/BADR, the TRAN air-transport, CARR/PTBOAT placeholders) return `None` and
stay dropped. The spawn path (`spawn_placed_unit`→sim) already derives
submarine/detector flags and Water spawning from `proto.locomotor` (Q25), so nothing
else was needed. **RA campaign naval is authored entirely in `[TeamTypes]`** (no
mission places a vessel in `[UNITS]`), so "authored vessels" means the scripted
reinforcement/attack teams. Verified (`naval_campaign_probe.rs`): scu08ea's 16 /
scu11ea's 22 naval team members now resolve to Water protos (0 before), none remain
in the loader `skipped` list, and driving the missions spawns real vessels during
play — **scu11ea: LST + CA, scu13ea: DD + LST, scu09ea: PT, scg03ea: the `aqua` LST**.
The scg04ea AFLD `[Base]` pin is untouched (that is a *building* path; `campaign_locomotor`
only affects units) — the campaign land suites (scg01/03/04ea) stay green.

**Determinism / golden discipline.** Naval `Unit` state is hashed only for
vessels/submarines (Q25); the AI naval paths are shipyard-gated (dead on landlocked
maps → no RNG drawn, byte-identical); the `run_combat` bombardment branch is
Water-gated (ground combat byte-identical); campaign naval protos are **appended**
to the catalog (existing unit ids unshifted) and only enter the hash when a vessel
actually spawns. Result: the **full `ra-sim` suite, the full `ra-client` suite
(all UI/golden/determinism/campaign frames), and all other crates are green with
ZERO re-pins**; fmt clean; clippy clean on default, `--no-default-features`, and
`--features window`.

**PNG evidence** (`/tmp/.../scratchpad/`): `naval_ai_battle_scm11ea.png` (an AI
destroyer firing on open water), `naval_authored_scu11ea_ships.png` (scu11ea's
scripted LST + cruiser afloat) — plus Q25's `naval_realmap_suite` frames.

**Cuts (reported, from the bottom of the priority stack).**
- **P2 — LST naval transport + skirmish-buildable.** The landing craft now **spawns
  and floats** (P1: it is a Water unit and appears in campaign teams), but the
  load-at-shore → cross → unload-at-far-shore choreography is **not** wired: the
  transport system (Q18) is infantry-only and would need extending to vehicles with
  shore-aware, locomotor-respecting unload cells. LST is also **not** yet added to
  the skirmish build catalog. Deferred.
- **P3 — PT gunboat as a skirmish buildable, naval combat render polish** (wake,
  depth-charge/torpedo/periscope anim, submerged-sub stealth visual). Not started.
  (PT already resolves + spawns in campaign teams via P1; it is just not a skirmish
  buildable and has no bespoke art.)

**Handoff to ra-tester.** Exhaustive coverage owed: AI naval determinism proptest
(same-seed-twice with vessels + bombardment); AI ship-vs-ship + sub-hunt outcomes;
the coastal-bombardment approach on varied shore geometries; a broader coastal-map
survey for AI-vs-AI decisiveness (scm11ea is naval-trap-fragile); and — once wired —
LST load/cross/unload and PT. Smoke proofs added: `ra-client/tests/naval_ai_vs_ai.rs`
(coastal decisive + naval built; PNG) and `ra-client/tests/naval_campaign_probe.rs`
(authored naval resolves + spawns; PNG).

---

## Q27 — Infiltration specialists: spy / thief / Tanya-C4, and the disguise divergence

**Milestone:** Marquee content arc P0 (spy, thief, Tanya) — reuses the M7.7 Chunk C
engineer-capture machinery (Q10).

**Machinery.** All four building-infiltrators share the engineer's march path: an
`Attack` order at an enemy building is accepted for an unarmed engineer *and* now
for an **infiltrator** (spy/thief) or a **bomber** (Tanya); the unit marches to the
footprint (`nearest_adjacent_passable` + `find_path`, `Foot`) and acts on arrival.
`is_engineer` was narrowed to exclude infiltrators (spy/thief are *also* unarmed
non-harvester infantry, so the old test would have mis-classified them). Capability
is derived from the unit's rules.ini short-name — the §3.8 role table, exactly like
`vessel_flags` — SPY/THF/E7/DOG → `(spy, thief, bomber, is_canine)`. New system
`run_infiltrators` (tick order 4.26, after `run_engineers`).

**Thief (THF).** Faithful port of `infantry.cpp:750-777`: on an enemy **storage**
building (refinery/silo, `Class->Capacity` — we key on `is_refinery || storage>0`)
it transfers **half** the victim house's `Available_Money` into its own house
(`cash = bldg->House->Available_Money()/2; Spend_Money(cash); Refund_Money(cash)`)
and is **consumed** (`delete this`, `infantry.cpp:783`).

**Spy (SPY).** Faithful part: on entering an enemy building it reveals the
building's surroundings to its house (`SpiedBy`, a 10-cell disc) and, for a **radar
dome** (DOME/`STRUCT_RADAR`), the **whole map** (`RadarSpied`, `infantry.cpp:704`).
**Consumed** on infiltration (`infantry.cpp:783`).
- **Divergence (documented).** The marquee brief asks the spy to also "steal a % of
  a refinery's credits". Vanilla-conquer routes the *money* steal through the
  **thief**, not the spy — the spy's branch (`infantry.cpp:687-746`) grants only
  recon + (sub-pen/airstrip) superweapons, never credits. To satisfy the brief we
  additionally leak a **quarter** (`SPY_STEAL_NUM/DEN = 1/4`) of an enemy refinery's
  credits to the spy's house, and flag this as a deliberate deviation from the
  reference (which assigns the larger half-steal to the thief). The reveal is the
  faithful part; the credit leak is brief-directed.

**Spy disguise + dog detection.** RA1/vanilla-conquer models **no visual disguise**
to cite (grep for `IsDisguised`/`Disguise` in `redalert/`/`common/` returns nothing —
disguise is an RA2 feature). We implement a simplified stealth in the spirit of the
brief and mirroring the submarine (Q25): a spy spawns `disguised`, and while
disguised it is **hidden from enemy target acquisition** (`is_hidden_spy`, the exact
shape of `is_hidden_submarine`) — an enemy guard never auto-acquires it, and it is
not an explicit target. A live **enemy dog** (`is_canine`, `IsCanine=yes`) within
`SPY_DETECT_RANGE = 3` cells **strips the disguise** (`run_spy_detection`, tick
3.95), after which any unit can engage and the dog's `DogJaw` kills it through
normal combat. The reference dog deals full-`Strength` instakill to its exact target
(`infantry.cpp:332-344`); we model the detection/reveal and let the normal weapon
path do the killing. `disguised` is hashed **only when true**, so no non-spy world
moves.

**Tanya / C4 (E7).** `C4=yes → IsBomber` (`idata.cpp:1468`). An armed bomber ordered
onto an enemy building marches in and **plants C4** (`MISSION_SABOTAGE`,
`infantry.cpp:916-925`): it sets the building's `c4_fuse = round(C4Delay ·
TICKS_PER_MINUTE)` (`Rule.C4Delay = .03` min, `rules.cpp:262`) unless the building is
**iron-curtained** (`!IronCurtainCountDown`, `infantry.cpp:919`), then clears its
order and survives (**not** consumed; the original `Scatter`s her away). The fuse
counts down in `tick_building_timers`; at zero the building is destroyed outright
(`building.cpp:995-1013`, `Take_Damage(Strength)`), crediting the saboteur — again
skipped if the building became iron-curtained meanwhile (`techno.cpp:4102`). Tanya's
Colt45 anti-infantry fire is ordinary combat (`run_combat` fires it at *unit*
targets; a *building* target routes to C4 instead). `c4_fuse` hashed only when armed.

---

## Q28 — Superweapons: the SuperClass charge/fire cycle, nuke / iron curtain / chronosphere

**Milestone:** Marquee content arc P1 (superweapon framework + three effects + AI).

**Framework (`SuperClass`, `super.cpp`).** Each house holds a `Vec<SuperWeapon>`
(`world.superweapons`, hashed only when non-empty). A superweapon is **present**
while its house owns the granting building — a per-tick function of building
ownership (`ActiveBScan`), **not** a one-time grant: MSLO→Nuclear, IRON→IronCurtain,
PDOX→Chronosphere (`house.cpp:1598/1667/1750`). `sync_and_charge_superweapons`
rebuilds the list each tick, adds a newly-granted weapon (charging from
`RechargeTime`), drops one whose building is gone, then charges each (`Control`
countdown → `IsReady` at 0, `super.cpp:265`), **suspending** while the house lacks
full power (`Suspend(Power_Fraction()<1)`, `house.cpp:1484`). Firing
(`Command::FireSuperWeapon` → `apply_fire_super`) applies the effect and restarts the
recharge (`Discharged`, `super.cpp:233`). Recharge times are the `[Recharge]`
rules.ini minutes (Nuke 13 / IronCurtain 11 / Chrono 7) × `TICKS_PER_MINUTE`
(`SuperKind::recharge_minutes`). **Byte-identity:** every world without a superweapon
building has an empty list and appends no hash bytes, so all prior goldens are
unchanged (verified: full ra-sim + client golden suites green, zero re-pins).

**Nuclear strike (MSLO).** Fire at a cell → a `NukeStrike` falls for
`NUKE_FALL_TICKS = 20` (the `BULLET_NUKE_DOWN` drop, `house.cpp:2818`), then
`nuke_detonate` applies the `WARHEAD_NUKE` blast (`NUKE_DAMAGE = 200`,
`house.cpp:2820`) to **every** unit/building within `NUKE_RADIUS_CELLS = 3` — area
devastation. **Deviation:** the ordinary `Explosion_Damage` 3×3 per-cell falloff is
flattened to a uniform full-strength hit across the radius so the superweapon is
decisive (documented). Iron-curtained targets are spared (`techno.cpp:4102`).

**Iron curtain (IRON).** Fire at a unit/building → `iron_curtain =
IronCurtainDuration · TICKS_PER_MINUTE` (`Rule.IronCurtainDuration = 0.5` min,
`rules.cpp:259` → `TICKS_PER_MINUTE/2`). Enforcement is a single chokepoint: every
damage path (`explosion_damage` units + buildings, `nuke_detonate`, the C4 blast)
**skips** a target whose `iron_curtain > 0` — the exact `if (IronCurtainCountDown ==
0)` gate at `techno.cpp:4102` (the whole `Take_Damage` is skipped, not zeroed).
Ticked down in `tick_building_timers`. Hashed only when active.

**Chronosphere (PDOX).** Fire at a vehicle + destination cell → teleport it there
(`u.coord = dest.center()`, clear path/target). An **infantry** target is killed by
the warp (`Take_Damage(Strength)`, `house.cpp:3021`). **Deferral:** the
`Rule.ChronoDuration` warp-**back** (`MoebiusCountDown`, the unit returning after 3
min) is not modelled — this is a one-way teleport, which satisfies the combat-use
case (move a vehicle across the map) and the acceptance (assert new position).

**AI (`Super_Weapon_Handler` → `Special_Weapon_AI`, `house.cpp:1458/2722`).** Gated
on `IQ >= IQSuperWeapons` (`house.cpp:1782`; a computer house runs at `MaxIQ ≥ 4`).
The reference auto-fires **only the nuclear strike**, at the highest-`Value()` enemy
building with a 90% chance (`Percent_Chance(90)`); we port that (max-`cost` live
enemy non-wall building, one sync-RNG draw for the 90%). As a decisive extension the
AI also drapes the **iron curtain** over its strongest armed unit when ready
(iron/chrono are player-only in the reference; documented). The AI does **not** build
superweapon structures in its base order yet — they are only present when placed
(scripted/campaign), so AI-vs-AI without them is byte-identical and decisive; an AI
that *owns* a superweapon fires it (smoke-proven). **RNG safety:** `fire_superweapons`
early-returns (no draw) for any house with no ready superweapon, so no existing
AI-vs-AI golden draws differently.

**Player surface.** The sim `Command::FireSuperWeapon` + `World` accessors
(`superweapon_ready`, `superweapon_charge_permille`, `nuke_strikes`) are the seam a
sidebar readiness clock + click-target mode drive; the rich targeting UI + the
sidebar special-weapon cameo/clock are a below-the-fold client follow-on (P2,
deferred — same treatment as the aircraft/naval sidebar cameos). **Closed in Q29.**

---

## Q29 — Specialists + superweapons made player-usable: skirmish-buildable, fire UI, effect visuals

**Milestone:** Marquee arc playability close-out (P0 sidebar surfacing, P1 player
fire UI, P2 effect visuals; P3 AI-SW-build deferred). Closes the M7.19 gap where
the specialist + superweapon *sim* (Q27/Q28) existed but nothing in the client let
a player build or fire them.

**P0 — skirmish-buildable (client `assets.rs`, `build_content`).** SPY/THF/E7 are
appended to the **units** buildables strip and KENN/MSLO/IRON/PDOX to the
**structures** strip, *below the fold* — appended after every existing item, the
same discipline as the aircraft/naval cameos (Q24.1/Q25). Verified **rendering-only,
zero re-pins**: no pinned `compose_game` frame contains these buildings/units, and
the default (scroll-0) view shows only the top rows, so `ui_shroud_golden`,
`ui_menu_golden_frames`, `ui_golden_frames`, and the sidebar suites are all
byte-identical (confirmed green). Prerequisites are enforced from rules.ini via the
existing catalog path — `prereq_ids` gained `dome`→DOME, `stek`→STEK, `kenn`→KENN
(atek/barr already mapped): SPY→dome, THF/E7/PDOX→atek, IRON/MSLO→stek, KENN→barr,
DOG→kenn (extracted from the real `redalert.mix` rules.ini; the sim's
`describe_buildable`/`apply_start_production` do the gating). Adding KENN also makes
the M7.7 DOG (`Prerequisite=kenn`) genuinely buildable for the first time. Cameos
load from `<NAME>ICON.SHP` in hires.mix — all six present (SPY/THF/E7/MSLO/IRON/
PDOX-ICON), degrading to the text row if absent.

**P1 — player fire UI (client `appcore.rs`, all through `handle`/`update`/`compose`).**
- **Ready/charge indicators.** `draw_sw_buttons` renders one full-width button per
  owned superweapon, stacked at the bottom of the sidebar strip, each with a
  **recharge clock** (`draw_charge_clock` — a pie whose lit sweep = the sim's
  `superweapon_charge_permille`, clockwise from 12 o'clock) and a bright "…RDY"
  state at full charge (the original's `Flash_Clock`/pie over the special-weapon
  button, `sidebar.cpp`). Drawn **only when the player owns a superweapon**, so no
  existing frame is touched (same conditional-render discipline as the iron-curtain
  hash-only-when-active rule).
- **Target-select mode.** Clicking a *ready* indicator arms `sw_fire_mode:
  Option<SuperKind>` (mutually exclusive with sell/repair/placement). A distinct
  targeting-reticle cursor (`CursorKind::SuperTarget`) + a per-kind reminder banner
  ("SELECT NUKE/IRON CURTAIN/CHRONO TARGET"). A tactical left-click fires through
  `Command::FireSuperWeapon`: **Nuclear** → the clicked cell (one click);
  **IronCurtain** → the unit (preferred) or building under the cursor;
  **Chronosphere** → a **two-click** gather (`sw_chrono_source`: unit first, then
  destination cell — the reticle tints cyan for the second step). Esc/right-click
  cancels (`cancel_action_modes`, surfaced via the new `action_mode_armed()` the
  `App` Esc-gate and shell cursor-hide now use). Readiness is re-validated on every
  click, so a click never fires an unready/destroyed-building weapon.

**P2 — effect visuals (client cosmetic, sim-inert).** Spawned by
`diff_superweapon_effects`, a post-tick sim-state diff in `step_tick` (the same
pattern as the death/sold/buildup diffs — reads sim state, writes only the cosmetic
effect/sound queues, never `world`):
- **Nuke** — `spawn_nuke_blast` on a strike's detonation tick: a **cluster** of
  `FBALL1` fireballs across the 3-cell radius plus two lifted skyward for the rising
  column (a scaled-up "mushroom", bigger than a single blast). No dedicated
  ATOMICEXP/NUKE shape ships in the freeware set — the coordinator-sanctioned
  scaled-FBALL fallback. Sounds: EVA "Nuclear weapon launched" (`VOX_ABOMB_LAUNCH`
  = `ALAUNCH1.AUD`) on launch, a heavy impact boom (`KABOOM25.AUD`) on detonation.
- **Iron curtain** — a pulsing blue/metallic tint over the curtained unit/building
  for its duration (`draw_iron_tint`, derived from the sim's `iron_curtain`
  countdown; the pulse rides the cosmetic clock). SFX `VOC_IRON1` = `IRONCUR9.AUD`.
- **Chronosphere** — a synthetic expanding-ring warp flash (`EffectKind::Warp`, no
  art) at the teleport source + destination, detected as a surviving unit whose
  position jumped >3 cells in one tick. SFX `VOC_CHRONO` = `CHRONO2.AUD`.
- **Determinism:** `superweapon_effects_are_sim_inert` (new) runs the same
  FireSuperWeapon script with the cosmetic layer on vs off → identical sim-hash
  chain, extending `ui_cosmetic_determinism`'s proof to SW effects.

**P3 — AI SW-structure building: DEFERRED (documented).** As Q28 notes, the AI
*fires* an owned superweapon but never *builds* MSLO/IRON/PDOX in its base order.
Making them player-buildable does **not** change that — the sidebar buildables list
is client-only; the sim AI's `next_structure` is untouched — so landlocked/skirmish
**AI-vs-AI stays byte-identical** (all 40 ra-sim test binaries green, unchanged).
Enabling AI SW-building would make AIs nuke each other (outcomes shift) and risk the
golden discipline / decisiveness, so per the priority-cut rule it is deferred and
AI SW-structure-building stays off. The player fires all three; the AI's scripted/
owned-SW firing is unchanged.

**Evidence.** `superweapon_fire_ui.rs` (always-runs, synthetic): player builds→
charges→arms→fires all three through the click seam, asserting the emitted command,
the sim effect (nuke strikes+detonation, `iron_curtain>0`, teleport), the cues, and
sim-inertness. `superweapon_fire_ui_png.rs` (real assets) dumps the ready-clock,
nuke fire-mode cursor+banner, mushroom cluster, iron-curtain glow, and chrono warp.

## Q30 — AI anti-gridlock: placement scoring, path-retry bounds, diagonal-corner fidelity, unstick + runaway guards (M7.20)

**Milestone:** M7.20. Trigger: M7.19-B's richer content re-rolled the lockstep
AI-vs-AI trajectories onto stalemate/pathological runs (`scg05ea` Normal stall;
`scm01ea` observed grinding 8+ hours). The diagnosed diseases were real and
user-visible in playtests: first-fit dense base placement walled AI bases solid,
pinned harvesters ran unbounded failing path searches every tick, and a
"zombie" attack team could monopolise the AI's one team slot forever. All
citations below are verified against the EA GPL source checkout
(`~/dev/game/reference/CnC_Remastered_Collection/REDALERT/`).

**P0 diagnosis (measured, bounded diagnostic in `ui_ai_vs_ai.rs::diag_*`).**
- `scm01ea` was **not** an unbounded unit-count runaway: units plateaued ~93.
  It was a **sim-rate collapse** — ~14,000 ticks/s → ~20 ticks/s (700×,
  ~50 ms/tick) once both bases gridlocked (6 of 7 harvesters pinned) — caused
  by every blocked/targeting unit re-running a *failing* full-grid A* every
  tick, forever.
- `scg05ea`/Normal was a stalemate: the stronger AI's team slot was held by a
  stalled 2-survivor team (28,000+ ticks in Attacking), so a 60-unit army sat
  idle with the escalation counter frozen.

**Diagonal corner rule — split (P1.5).** The original's pathfinder evaluates
only the **destination cell** of each step: `Passable_Cell` calls
`Can_Enter_Cell(cell, face)` (FINDPATH.CPP:1281) and every `Can_Enter_Cell`
overload **ignores** its `FacingType` parameter (e.g. UNIT.CPP:3208
`MoveType UnitClass::Can_Enter_Cell(CELL cell, FacingType ) const`) — so units
squeeze diagonally between corner-touching static blockers (why original walls
must be orthogonally continuous to seal). We were strictly harsher (both
orthogonal side cells had to be passable), which left AI bases with far fewer
exits. Now split:
- **Static blockers (terrain/buildings): destination-only** — squeeze allowed,
  matching the original.
- **Unit occupancy (`find_path_avoiding` only): corner check kept** — a
  re-route may not corner-clip a vehicle-occupied cell. **Divergence:** the
  original had no corner rule for units either (same ignored-facing evidence);
  we deliberately stay stricter to protect the one-vehicle-per-cell invariant
  and the head-on tie-break from lepton-interpolated overlap.
Pinned in `path::tests::no_corner_cutting` (both halves). Re-pin fallout was
exactly one real-map timing pair (see the M7.20 report's audit table):
`campaign_scg01ea`'s Einstein-death tick 63 → 58 (bisect-verified: restoring
the old corner rule alone restores 63).

**Blocked-move retry bounds (the sim-rate fix).** Ported the original's two
bounds and applied them at *both* failing-A* sites:
- `PATH_DELAY_TICKS = 14`: `PathDelay = Rule.PathDelay × TICKS_PER_MINUTE`
  (FOOT.CPP:461) with stock `[AI] PathDelay=.016` min (RULES.CPP:267) — one
  path recomputation per blocked unit per window, never per tick.
- `PATH_RETRY = 10`: the `TryTryAgain` budget (FOOT.H:241); when exhausted the
  move order is abandoned (`Assign_Destination(TARGET_NONE)`,
  DRIVE.CPP:988-995) and the unit goes idle/re-taskable.
- The **combat approach** applies the same state; an exhausted budget drops the
  can't-reach target, per DRIVE.CPP:1003-1008 (`IsScanLimited` +
  `Assign_Target(TARGET_NONE)` — "give this unit a range limit so that it might
  not pick a 'can't reach' target again"). Divergence: we don't model the
  persistent scan-limit flag; the AI may re-issue the target later, bounded by
  the same throttle.
- Harvester ore rescans back off `TICKS_PER_SECOND*7` after failure — the
  `GOINGTOIDLE` return value (UNIT.CPP:2961-2965) — instead of rescanning (up
  to 16 A* calls) every other tick.
The scatter request (`Incoming`) consequently fires per throttled *attempt*,
not per tick — matching the original, where the `Incoming` call sits inside the
`PathDelay`-gated `Basic_Path` block (DRIVE.CPP:940-1049). The
`scatter_boundary_suite` draw-cadence pins were re-derived accordingly (silent
RNG after abandonment, instead of the old draw-every-tick-forever).

**Placement scoring (P1).** `placement_cell` replaced first-fit max-density
spiral with deterministic integer scoring (fixed scan order, strict `>`, no
RNG). Reference: `Find_Build_Location` (HOUSE.CPP:4575-4674) rates base zones —
defenses to the most under-defended zone, everything else to a **random** zone
(HOUSE.CPP:4648), then the legal cell nearest a random spot in it
(`Find_Cell_In_Zone`, HOUSE.CPP:7874). It also *intends* combat buildings never
to be adjacent ("spacing defensive buildings out will yield a better defense",
HOUSE.CPP:4589-4597 — the computed `adj` flag is dead code in RA). Divergences
(all deliberate):
- deterministic scoring instead of `Random_Pick` zones (§4.2 — no RNG in
  placement);
- an explicit **corridor reservation**: each refinery's dock cell + a walked
  line toward its nearest ore is hard-rejected (the original never packs solid,
  so it needs no such guarantee; our bounded ring must not brick the economy);
- adjacency penalty (doubled for defenses, honouring the dead `adj` intent) +
  mild compactness + defenses biased toward the designated enemy (collapsed
  form of the zone-deficit rating).

**Harvester unstick (P2).** A harvester whose route the movement layer
abandoned (`PATH_RETRY` exhausted) bumps `HarvestState::retarget`; the ore scan
rotates that many candidates away from nearest-first, so a pinned harvester
targets a *different* ore cell; reset on a successful mine/dump. No original
counterpart line (the original's harvester relies on its mission delays +
`ArchiveTarget`); documented as the minimal deterministic equivalent.

**Zombie-team timeout.** `ATTACK_TIMEOUT = 3000` ticks: a team still in
Attacking that long dissolves as a **failed** attack (bumps the escalation
counter, retreats survivors, frees the slot). The original's `TeamClass::AI`
dissolves stalled teams through its own mission/timer machinery; ours collapses
that to one timeout. This was the actual scg05ea/scm01ea decider.

**Runaway guard.** Rubber-band caps now include **infantry**
(`Control.MaxInfantry` raise, HOUSE.CPP:4962-4963; `AI_Infantry` gate
`CurInfantry >= Control.MaxInfantry`, HOUSE.CPP:6281 — previously our infantry
lane was entirely uncapped), and **every** cap raise clamps at
`RUBBER_BAND_CEILING = 500/6 = 83` — the original's constructor values
`MaxUnit(Rule.UnitMax/6)` etc. (HOUSE.CPP:729-731, RULES.CPP:235-246).
**Divergence:** the original could raise past 83 until its global 500-object
heaps filled; we have no heaps, so the constructor value doubles as the hard
ceiling.

**P3 — Hard-mode cadence.** Verified the skirmish path delivers the menu
difficulty to the AI (`menu.rs:413` → `SkirmishSettings.difficulty` →
`assets.rs:2391 AiPlayer::new`). Hard now also goes **all-out earlier**:
`Difficulty::all_out_escalation()` = Easy 5 / Normal 4 / Hard 2 (plus the
pre-existing per-difficulty `attack_interval`). **The original has no live
per-difficulty cadence knob to cite** — `[Easy]/[Difficult]` carry only stat
biases + `RepairDelay`/`BuildDelay` (`Difficulty_Get`, RULES.CPP:311-327), and
`BuildDelay`/`IsBuildSlowdown` are loaded but never consumed (no reader of
`HouseClass::BuildDelay` beyond its assignment, HOUSE.CPP:294/304) — so the
cadence differentiation is entirely our extension.

**Pins:** `m720_antigridlock_suite.rs` (corridor-clear placement, harvester
unstick, infantry cap + ceiling, zombie-team timeout — each proven
revert-sensitive by disabling its mechanism), `path::tests::no_corner_cutting`
(split corner rule, both halves), `ai_retune_depth_suite::
hard_goes_all_out_after_exactly_two_dissolves` (P3 knob), and the re-derived
`scatter_boundary_suite` cadence pins.
