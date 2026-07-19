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
2. **Wall overlays** (`SBAG`/`BRIK`/… → `LAND_WALL`, `odata.cpp`) are not yet
   folded into the masks; only *template* land types are. Ore overlays are
   correctly passable. Wall-blocking is a small follow-up (the playtest complaint
   was cliffs/mountains, which are templates).
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
4. **Arcing grenade → straight flight.** E2's grenade is `Arcing` in rules.ini;
   the projectile flies the straight flat-trajectory path (`bullet.rs` advance) —
   the arc is cosmetic and the impact point is the same. (`bullet.cpp:751` fires
   straight; the arc is a draw-time parabola we do not render.)
5. **Vehicle/infantry cell coexistence forbidden (updated M7.7):** vehicles and
   infantry no longer co-occupy a cell at all — the mover re-routes/holds rather
   than crushing or stacking (see Q5.3). Turrets: infantry are correctly
   turretless (`has_turret=false`, M7.7 P0c) — they aim by rotating their body,
   matching `udata.cpp` (`is_turret_equipped=false` for every infantry type).
