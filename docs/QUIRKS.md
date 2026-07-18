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
