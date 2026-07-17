# Red Alert Reproduction — Architecture & Design

Status: DRAFT (design phase). Pain-point sections are being verified against the
released source in `references/vanilla-conquer/redalert/` (EA GPL v3 release, 2020,
via the buildable Vanilla Conquer fork).

## 1. Goals

- Reproduce Command & Conquer: Red Alert (1996) gameplay faithfully: same units,
  same economy, same feel — loading the **original freeware assets** (.MIX/.SHP/.PAL/
  .AUD/rules.ini) directly, not re-authored content.
- Fix the original engine's *architectural* problems without changing its *design*
  (the game rules stay; the desyncs, hardcoded limits, and spaghetti go).
- Deterministic simulation from day one: replays, lockstep multiplayer, and
  save-anywhere fall out of that decision instead of being retrofitted.
- **Staged networking roadmap** (see §4.6): ship single-player first; then LAN
  multiplayer (peer lockstep); then internet play synchronized through a
  server. The sim never changes across these stages — only the transport that
  feeds it commands.
- Language: **Rust**. Rendering/input/audio via **macroquad** (pure Rust, no system
  deps; RA is palette-based 8-bit art so we generate RGBA textures ourselves anyway).
- **First-class on Windows, macOS, and Linux** — one codebase, platform-specific
  code quarantined in one module (§4.7), CI builds all three targets.

Non-goals (for now): Remastered HD assets, Tiberian Sun isometrics, gameplay
"improvements" à la OpenRA, mod support beyond what rules.ini already gives us.

## 2. Reference material

| Resource | Where | Use |
|---|---|---|
| Original RA source (GPL) | `references/vanilla-conquer/redalert/` | Ground truth for game rules & constants |
| Vanilla Conquer | github.com/TheAssemblyArmada/Vanilla-Conquer | Buildable original for behavior comparison |
| OpenRA | github.com/OpenRA/OpenRA | Prior art for a from-scratch reimplementation |
| Freeware RA assets (EA, 2008) | `assets/` (main.mix, redalert.mix, …) | The actual game content |
| Format docs | XCC/ModEnc community docs | .MIX/.SHP/.PAL/.AUD/.TMP/.VQA layouts |

## 3. Original engine — pain points and our answers

> Each subsection: what the 1996 engine does (with citations), why it hurts, and
> the architectural answer in the rebuild. Evidence being filled in from source
> survey — TODO markers below.

### 3.1 Deep inheritance object model → composition over a flat entity store

**What the original does.** A 7-deep inheritance chain per game object:
`AbstractClass → ObjectClass → MissionClass → RadioClass → TechnoClass →
FootClass → DriveClass → UnitClass` (`redalert/object.h:54`, `techno.h:57`,
`foot.h:49`, `unit.h:51`), with `TechnoClass` inheriting from **five** bases at
once (RadioClass + Flasher/Stage/Cargo/Door mixins, `techno.h:57`). `ObjectClass`
declares **71 virtual methods**, `TechnoClass` another **76** — query, combat,
AI, rendering, and file I/O concerns all on one interface. A parallel
`*TypeClass` hierarchy (`type.h`) mirrors the whole tree for static stats.

**Why it hurts.** The hierarchy doesn't actually carry the polymorphism: there
are **410 call sites of `What_Am_I()`** runtime type checks, with `switch` +
C-cast downcasts sprinkled through generic logic (`foot.cpp:2205-2217`,
`cell.cpp:646,714,776`, `house.cpp:841,...`). Even `Is_Foot()` is a hardcoded
RTTI OR-chain, not a virtual (`object.h:176-184`). Behavior for one feature is
smeared across 4–5 levels; adding a unit kind means touching the whole tree.

**Our answer.** No inheritance. Entities live in generational arenas; shared
behavior is plain component structs composed into entity structs; kind-specific
logic is explicit `match` on a small closed enum *at the system boundary*, not
scattered casts. Rust makes the original's implicit contract ("this TARGET is
really a Building, trust me") a compile-time question. See §4.3.

### 3.2 Pervasive global mutable state → one explicit `World` value

**What the original does.** ~275 `extern` globals (`externs.h`), defined across
680 lines of `globals.cpp`: the entire map/display/input stack is a global
`MouseClass Map` (`globals.cpp:412`), plus `Houses`, `Scen`, `Rule`, `Session`,
`PlayerPtr`, the global `Frame` counter, and — critically — the sync RNG living
inside `Scen.RandomNumber` (`scenario.h:63`).

**Why it hurts.** Nothing is testable in isolation; every function can touch
every other subsystem; save/load must know about all of it; and "which state is
sim-critical vs. cosmetic" is tribal knowledge (see the two RNGs in §3.4).

**Our answer.** One `World` struct in `ra-sim` owns *all* sim state — entities,
map, houses, RNGs, tick counter. It is passed explicitly, serializes as a unit,
hashes as a unit, and constructs in a test in one line. Client-side state
(camera, selection UI, audio) lives in the client and can never leak into the
hash. The type system enforces the sim/cosmetic split the original maintained
by hand.

### 3.3 Fixed heaps & hardcoded limits → generational arenas, no compile-time caps

**What the original does.** Every object pool is a `TFixedIHeapClass` with
compile-time caps: `UNIT_MAX 500`, `BUILDING_MAX 500`, `VESSEL_MAX 100`,
`TEAMTYPE_MAX 60` (`defines.h:2897-2912`), with per-player caps hardcoding a
5-player assumption (`INFANTRY_MAX/5`). The map is fixed 128×128
(`defines.h:459-461`); `CELL` packs X:7/Y:7 bits with zero headroom
(`defines.h:520`); an object reference (`TARGET`) is an `int` packing a 24-bit
heap index + 8-bit RTTI type (`defines.h:538-564`), hard-coupling entity
identity to both the pool layout and the type enum.

**Why it hurts.** Every limit is load-bearing in multiple encodings at once —
you can't grow the map without redesigning `CELL`, `COORDINATE`, *and* every
save game. A `TARGET` can dangle: index reuse after death means a stale
reference silently points at a new object (the original patches this with
liveness checks at use sites).

**Our answer.** Generational handles (`slotmap`) — a stale handle is
*detectably* stale, not silently rebound. Dynamic capacity. Map size is data
(width×height from the scenario), coordinates are explicit newtype structs, not
bit-packed unions; the lepton (1/256 cell) survives as the world unit because
it's a good determinism idea, but nothing packs cell+lepton into byte layouts.

### 3.4 Fragile lockstep determinism → determinism as an enforced contract

**What the original does.** Peer-to-peer lockstep: player actions become
`EventClass` records (~30 event types, `event.h:55-104`) queued into `OutList`,
exchanged, and executed from `DoList` in canonical house order
(`queue.cpp:3154`). Peers may run at most `MaxAhead` frames apart (default 5,
`session.cpp:167`, retuned at runtime via TIMING events). Desync detection: a
32-frame ring of game CRCs (`Compute_Game_CRC()` hashes coords, facings,
missions, credits, and the RNG seed, `queue.cpp:3587-3667`) compared against
each incoming FRAMEINFO.

**Why it hurts.** Detection is good; *recovery is a message box*. On CRC
mismatch the game shows "Out of sync" and tears down the connections
(`queue.cpp:3298-3307`) — the match is dead. Sync correctness depends on
hand-discipline: the sim RNG (`Scen.RandomNumber`) vs. cosmetic RNG
(`NonCriticalRandomNumber`) split is enforced by nothing but convention
(`inline.h:923` vs `:972`), object iteration order is shared state, and CRC
checks don't even run in single player, so nondeterminism bugs ship silently
until a multiplayer game hits them.

**Our answer** (§4.2, §4.4): the same architecture — lockstep + command queue +
state hash is the right design for an RTS — but with determinism *enforced*:
sim-vs-cosmetic separation by crate boundary (client code cannot reach the sim
RNG), state hashing always on (single-player replays assert the hash chain in
CI), and snapshots (§3.6) enabling desync *recovery* (resync from snapshot) and
mid-game join, which the original's memory-dump saves made impossible.

### 3.5 Sim/render entanglement → fixed-timestep sim, interpolated presentation

**What the original does.** One thread, one loop: `Main_Loop()`
(`conquer.cpp:1708`) does input → render → `Logic.AI()` (the whole sim tick) →
network → `Frame++`, then `Sync_Delay()` busy-waits to hold **15 logic frames
per second** (`TICKS_PER_SECOND 15`, `defines.h:3121`) — re-running input and
render inside the wait spin (`conquer.cpp:1670-1686`). The sim tick period *is*
the game-speed slider (`conquer.cpp:1786`). Modal dialogs run nested loops that
re-enter `Main_Loop()` from inside themselves (9+ dialog files). Most striking:
the renderer's Y-sort of the ground layer is performed in the logic path *"for
sync purposes"* (`conquer.cpp:1816-1823`) — **draw order is shared multiplayer
state**.

**Why it hurts.** Visual smoothness is capped at 15 Hz; game speed and
simulation speed are the same knob; any UI can reentrantly tick the world; and
render-side data structures participate in the sync CRC, so a rendering
refactor can cause a desync.

**Our answer.** `ra-sim` ticks at a fixed 15 Hz (matching original gameplay
speed; the speed slider scales tick period, sim stays discrete), knows nothing
about rendering, and exposes only snapshots + events. The client renders at
display rate with interpolation. Draw order is computed client-side from
positions and is *not* state. Dialogs are client UI over a running (or paused)
sim — no reentrancy.

### 3.6 Raw-memory save games → serde snapshots + command-log replays

**What the original does.** Saving writes each object's raw memory:
`file.Put(Ptr(i), sizeof(T))` (`heap.cpp:526`); loading reads bytes back over
the object and re-runs an in-place `new (ptr) T(NoInitClass())` purely to
restore the vtable pointer (`heap.cpp:587-588`) — an entire constructor-variant
convention (`NoInitClass`) exists only to not clobber loaded bytes. Object
pointers are hand-swizzled to `TARGET`s before save and back after load via
virtual `Code_Pointers`/`Decode_Pointers` on every class (`ioobj.cpp:611-642`).
The save version constant is literally a **sum of `sizeof()` of ~30 classes**
(`saveload.cpp:69-79`).

**Why it hurts.** Add one field to any class → all prior saves invalid, by
design. The format has no schema; endianness, padding, and compiler are all
load-bearing. Pointer swizzling must be manually maintained per class per field.

**Our answer.** This problem evaporates given §3.2 + §3.3: `World` contains no
pointers (only generational handles, which are just numbers) and derives
`serde::Serialize`. A save is a versioned snapshot; a replay is the initial
snapshot + command log (§4.4); vtables don't exist. Field-level schema evolution
comes free with serde formats.

### 3.7 Pathfinding & movement jams → grid A* + cell reservation

**What the original does.** `Find_Path` (`findpath.cpp:416`) is **not A*** — it
traces a straight line to the target, and on hitting an obstacle walks the
obstacle's edge clockwise and counter-clockwise (`Follow_Edge`,
`findpath.cpp:780`), keeping the shorter walk. A wall-following "bug
algorithm", with hard caps: edge-follow abandons after 400 cells, and each unit
*stores* only a 12-step path (`CONQUER_PATH_MAX 12`, `foot.h:223`), re-pathing
continuously. Blockage is resolved cooperatively: a stuck unit asks the blocker
to scatter (`drive.cpp:970`), retries up to 10 times (`PATH_RETRY`), then gives
up with the "unable to comply" scold sound (`drive.cpp:977-1007`). Harvesters
have zero mutual coordination — they converge on the same ore patch and queue
at refineries via scatter-retry, exactly the traffic-jam behavior every RA
player remembers.

**Why it hurts.** Non-optimal routes around concave obstacles, units wandering
along walls, mass-move traffic collapse, harvester deadlocks.

**Our answer.** Grid A* with a proper open list (16 384 cells is nothing on
modern hardware — the original's algorithm was a 1996 memory/CPU compromise,
not a design preference), terrain-class movement costs from rules.ini, plus a
next-cell **reservation** layer so movers negotiate crossings instead of
colliding. Keep the *feel*: same speeds, same turn rates, same scatter behavior
on true blockage. Fidelity caveat (open question §5): better pathing changes
effective unit responsiveness; if we ever want frame-authentic campaign
behavior we can gate the pathfinder choice per-scenario, but default to fixed
pathing.

### 3.8 Half data-driven rules → rules.ini as the single source of stats

**What the original does.** RA pioneered rules.ini for unit stats — but unit
*behavior* stays hardcoded as enum-equality checks inside generic logic. The
survey found the MAD tank special-cased at 8+ sites in `unit.cpp`/`team.cpp`,
the harvester across six different subsystems (`unit.cpp`, `building.cpp:2644`,
`cell.cpp:2519`, `drive.cpp:766`, …), plus Spy, Thief, Tanya, Minelayer,
Chrono Tank, the Weapons Factory, and the Ant units — all as `*this ==
UNIT_MAD`-style checks. Even flags that look data-driven (`IsScanner`, `IsDog`)
are assigned in code (`idata.cpp:1078`), not read from the INI.

**Why it hurts.** "Add a second stealable-tech unit" means find-all-references
on an enum constant across 20 files. It's also why total-conversion mods of the
original engine hit walls that rules.ini can't express.

The same split shows up beyond units: the warhead-vs-armor damage matrix *is*
data-driven (`Verses=` parsing, `warhead.cpp:169-172`), but damage falloff math
is hardcoded (`combat.cpp:109-113`), the warhead/armor/superweapon enum sets
are compile-time fixed, and every superweapon *effect* is a bespoke `switch`
arm in `house.cpp:2788-3010`.

**Our answer.** Stats stay rules.ini-driven (we load the original file
unchanged). Behaviors become named **capability components** (`Harvests`,
`Infiltrates`, `LaysMines`, `DetonatesOnDeploy`…) attached to unit types at
data-load time — original units get the original capabilities, so gameplay is
identical, but the mapping lives in one table instead of 410 scattered type
checks. We deliberately do NOT build a general scripting/modding layer (OpenRA
exists); capabilities are plain Rust code selected by data.

### 3.9 8-bit palette pipeline → decode-at-load, atlas + shader remap

**What the original does.** Everything is 8-bit paletted with a per-cell
dirty-flag redraw system (`CellRedraw` bit array over 128×128 cells,
`display.cpp:123`); the display iterates live `ObjectClass*` pointers and calls
their virtual `Render` (`display.cpp:2203-2223`). House colors are 256-byte
index remap LUTs applied at blit time (`remapcontrol.h:21-35`,
`techno.cpp:4555`); water/radar animation is palette-index cycling (indices
96-102 rotate, index 255 pulses, `conquer.cpp:1301-1323`). Resolution is baked:
320×200 in `Set_View_Dimensions` (`display.cpp:191`), with a global `RESFACTOR`
multiplier smeared as a literal across dozens of UI files to reach 640×400.

**Why it hurts.** Dirty-rect bookkeeping infects gameplay classes (every object
carries redraw flags); draw code reads live sim state directly; resolution and
UI layout are unliftable without touching hundreds of sites.

**Our answer.** Decode SHP frames to RGBA atlases at load; keep art *logically*
paletted by rendering index textures + a palette texture in a small fragment
shader — house remap, palette cycling, fading, and shroud all become palette
texture updates, byte-identical to the original's color math. Full-frame redraw
every frame (a 1996 optimization we simply don't need); arbitrary window
resolution with the tactical view scaled in integer multiples for crisp pixels.
The client reads immutable per-tick snapshots/interpolation data, never live
sim objects.

### 3.10 AI: scripted expert system → same design, honest structure

**What the original does.** Per-house AI (`HouseClass::AI`, `house.cpp:975`)
runs every frame inside the sim: an "expert system" of per-category production
heuristics (`AI_Building/Unit/Vessel/Infantry/Aircraft`,
`house.cpp:5696-6533`), enemy scoring by distance/kills/base-size
(`house.cpp:4941-4967`), and scripted attack waves via TeamTypes — linear
mission scripts with a loop instruction, no branching (`teamtype.h:43-66`).
Difficulty is stat handicap (FirePower/Armor/BuildTime multipliers,
`rules.cpp:310-319`), not information cheating.

**Our answer.** Reproduce this design — it *is* the RA experience — as ordinary
deterministic sim systems reading `World` like everything else. The
architectural improvement is honesty, not intelligence: the AI issues the same
`Command`s a player would through the §4.4 pipeline, so replays show AI
decisions, tests can script them, and a future better AI is a drop-in.

## 4. Proposed architecture

### 4.1 Crate layout (cargo workspace)

```
ra-formats/   # Pure parsers: MIX (incl. Blowfish/RSA header), SHP, PAL, TMP,
              # AUD, CPS, INI. No game knowledge, no I/O policy, fuzzable.
ra-data/      # rules.ini + scenario INI → typed static data (UnitType stats,
              # weapons, warheads, tech tree). The "TypeClass" layer, as data.
ra-sim/       # The deterministic core. Owns World state, systems, command
              # application. NO floats, NO rendering, NO wall-clock, NO I/O.
ra-client/    # macroquad app: decoding assets to textures, camera, input →
              # commands, interpolation, audio, UI shell.
ra-net/       # Command transport behind a trait: local loopback (single
              # player), LAN peer lockstep, then server relay. The sim is
              # network-shaped from day one; only this crate grows per stage.
```

Dependency rule: `ra-sim` depends only on `ra-data` types and `core`/`alloc`-ish
crates. Everything above it observes the sim; nothing inside it observes the
platform. This single rule is what makes replays, headless tests, and lockstep
possible.

### 4.2 Determinism contract (the load-bearing decision)

The original's biggest operational failure mode is the desync ("Out of sync —
game halted"), because determinism was aspirational rather than enforced. Ours:

- **No floating point in `ra-sim`** — `#![deny(clippy::float_arithmetic)]` in the
  crate root. Keep the original's fixed-point vocabulary: the **lepton**
  (1/256 cell) as the world unit, wrapped in newtypes (`Lepton`, `CellCoord`,
  `WorldCoord`) so unit confusion is a compile error, not a desync.
- **Facings** stay binary angles (`Dir(u8)`/`Dir16`) — wraparound arithmetic is
  free and exact, like the original.
- **One seeded PRNG per concern** (`sim`, `ai`, `effects`), owned by `World`.
  Visual-only randomness lives client-side and never touches sim state.
- **Stable iteration order everywhere**: arenas iterate in slot order; no
  `HashMap` iteration in sim code (use `BTreeMap`/`IndexMap` or sorted keys).
- **Per-tick state hash** (cheap xxhash of the mutable World fields) computed
  always, not just in multiplayer: single-player replays assert the hash chain,
  so any nondeterminism is caught in CI the day it's introduced, not the day
  two players desync.

### 4.3 Entity model: arena + components, not ECS framework, not inheritance

RA1 peaks at a few hundred live objects — this is not a bevy_ecs problem. Plan:

- `slotmap`-style **generational arena** per broad kind (`Units`, `Buildings`,
  `Bullets`, …) or one unified arena — decided in 3.1 once the survey shows how
  much behavior the kinds actually share.
- Shared behavior via **plain component structs** (`Health`, `Mover`, `Turret`,
  `Cargo`, `Harvester`) composed into the entity struct; systems are free
  functions `fn system(world: &mut World)` run in a fixed, explicit order each
  tick (fixed order is itself a determinism requirement).
- Entity references are **generational handles**, never indices or pointers:
  the original's TARGET encoding and pointer-swizzling save games both dissolve.

### 4.4 Command pipeline: single-player is multiplayer with one peer

```
input → Command (typed enum) → queue for tick T+delay → apply in canonical order
```

- All mutation of `World` enters through `apply(world, tick, &[Command])`.
- A **replay is just the command log** + initial seed; a save game is a serde
  snapshot of `World` (plus the log tail for resync/debugging).
- Lockstep networking later = exchanging the same command stream; nothing about
  the sim changes.

### 4.5 Rendering: decode once, present interpolated

- At load: SHP frames → RGBA texture atlases per palette context. House colors
  via the original remap ranges — applied either at decode (one atlas per
  house) or in a tiny fragment shader (preferred; palette texture + index
  texture).
- Sim ticks at the original logic rate; render at vsync with positions
  interpolated between the last two ticks — units glide at 144 Hz while the
  sim stays bit-exact at ~15 Hz.
- Palette animation (water, radar) and fading = palette-texture updates, free
  with the shader approach.

### 4.6 Networking evolution: three transports, one sim

The sim only ever consumes `(tick, ordered commands)` and emits state hashes.
Everything network-shaped hides behind one trait in `ra-net`:

```rust
trait CommandTransport {
    fn submit(&mut self, cmd: Command);              // local player input
    fn poll(&mut self, tick: Tick) -> TickBundle;    // all players' commands for a tick
    fn report_hash(&mut self, tick: Tick, hash: u64);
}
```

**Stage 1 — single player (now).** `LocalTransport`: zero-delay loopback.
Player and AI commands go straight into the tick bundle. Even here the full
pipeline runs — commands are logged (replays work), hashes are chained
(determinism is CI-tested) — so stages 2 and 3 inherit a battle-tested core.
This is the original's own trick in reverse: RA ran single player through the
same `Queue_AI` path as multiplayer (`queue.cpp:138`); we keep that unification
but test it from day one.

**Stage 2 — LAN peer lockstep.** `LanTransport`: UDP peer-to-peer on the local
network with discovery via broadcast (the modern equivalent of the original's
IPX flow). Same MaxAhead-style input delay scheme the original used (§3.4),
same hash exchange — but on mismatch we can *resync from a snapshot* (§3.6)
instead of ending the match. LAN's low, stable latency makes a small fixed
input delay (2–3 ticks at 15 Hz) imperceptible.

**Stage 3 — server-synchronized internet play.** `RelayTransport`: clients
connect out to a server (works through NAT — the reason peer-to-peer is wrong
for internet play); the server is the *authoritative sequencer*, not a sim
host. It orders each tick's commands, broadcasts the bundles, and arbitrates
hash disputes (majority hash wins; the divergent client resyncs from a peer
snapshot). Because it never runs the sim, the server is cheap, game-version
agnostic within a protocol version, and doubles as the lobby/matchmaking
service. Replays fall out for free: the server already holds the canonical
command log for every game. (An authoritative-sim server would prevent
maphacks, but costs server CPU per game and a client rewrite — out of scope;
the relay design is what CnCNet uses for the original games today.)

Stage boundaries are also trust boundaries: stage 1 trusts everything, stage 2
trusts the LAN, stage 3 must validate commands server-side (rate, ownership:
"does house X own unit Y") — validation hooks belong in the `Command` schema
from the start, which is why commands carry the issuing house explicitly rather
than implying it from the connection.

### 4.7 Platform strategy: Windows / macOS / Linux

The original was DOS + Win95 with platform code smeared everywhere (the
Remastered/Vanilla ports spend much of their diff on this). We prevent that
class of problem structurally:

**Layering rule.** Platform-specific code is *quarantined by crate*:

- `ra-formats`, `ra-data`, `ra-sim` — **platform-pure by definition**: operate
  on byte slices and values, no OS APIs, no path handling, no threads. These
  crates must compile for any target (wasm included) without a single `cfg`.
- `ra-net` — talks `std::net` only; anything OS-conditional (socket options,
  broadcast quirks) hides behind the `CommandTransport` trait impls.
- `ra-client` — the ONLY crate allowed platform awareness, and inside it all
  `#[cfg(target_os)]` code lives in one module: `ra-client/src/platform/`
  behind a small capability trait (asset-dir discovery, save/config dirs, file
  dialogs if ever needed). Window/GL/input/audio differences are macroquad's
  (miniquad's) job — GL on Windows/Linux, Metal on macOS — and never leak
  into our code. A `#[cfg(target_os = ...)]` anywhere outside
  `ra-client/src/platform/` fails code review, and CI greps for it.

**Filesystem conventions** (the usual porting landmine, decided once):

- Assets are user-supplied, searched in order: `--assets` flag → `RA_ASSETS_DIR`
  env → per-OS data dir (`dirs`-crate conventions: `%APPDATA%/new-alert`,
  `~/Library/Application Support/new-alert`, `~/.local/share/new-alert`) →
  `./assets/` beside the executable.
- Saves/replays/config go to the per-OS data/config dirs, never beside the
  executable (macOS bundles and Windows Program Files are read-only).
- All internal asset names are case-insensitive ASCII (MIX lookups are by
  hash anyway); we never depend on filesystem case behavior.
- Paths in code are `PathBuf` end-to-end; no string concatenation with `/`.

**Cross-platform determinism** (interacts with §4.2): mixed-OS LAN/server games
require the sim to be **bit-identical across OS and CPU targets**. The no-float
rule already removes the main risk (FP behavior differences). Two additions:

- **No `usize` in sim state or sim arithmetic** — its width varies by target;
  arena indices are `u32` in state, converted at the container boundary.
- No `std::collections::HashMap` even with sorted access patterns in sim
  (RandomState differs per process anyway — already banned by §4.2), and no
  reliance on sort *un*stability: `sort_unstable` only on keys with total
  order and no equal-key ties that matter.
- CI cross-check: the determinism replay suite runs on all three OS targets
  and asserts the same hash chain for the same seed+commands.

**CI matrix** from the first commit: build + test on
`windows-latest / macos-latest / ubuntu-latest` (GitHub Actions). Release
packaging per OS (zip + .app bundle + tarball) is an M7 concern, but the build
matrix exists now so portability rot is caught per-commit, not at release.

### 4.8 Milestones

1. **M1 — Formats**: `ra-formats` parses MIX (incl. encrypted headers), PAL,
   SHP, TMP; CLI dump tool; golden-file tests against known assets.
2. **M2 — Terrain**: load a scenario INI, render the map with camera scroll.
3. **M3 — Units**: spawn units, select, move with A*; sim/render split +
   command pipeline + state hash live from this milestone.
4. **M4 — Combat**: weapons, warhead/armor matrix, damage, death, bullets.
5. **M5 — Economy**: ore, harvester, refinery, power, build queue, placement.
6. **M6 — Fog & AI**: shroud, basic skirmish AI (teamtype-driven like original).
7. **M7 — Polish**: AUD audio, EVA, sidebar UI, palette effects.
8. **M8 — LAN multiplayer**: `LanTransport` peer lockstep + snapshot resync
   (stage 2). Single-player releases happen well before this.
9. **M9 — Server play**: relay/sequencer server, lobby, server-held replays
   (stage 3). Separate deliverable: the `ra-relay` server binary.

## 5. Resolved decisions & open questions

Resolved by the source survey:

- **Tick rate: 15 Hz** (`TICKS_PER_SECOND 15`, `defines.h:3121`). The game-speed
  slider scales tick *period*; the sim remains a discrete 15 Hz process.
- **Coordinates: 256 leptons/cell, 128×128-max maps, 8-bit facings** — we keep
  the units (they're part of how the game *feels*) but store them in ordinary
  struct fields, not packed encodings.
- **Entity arenas: per-kind** (Units/Infantry/Vessels/Aircraft/Buildings/
  Bullets/Anims mirror the original's heap split). The survey shows the kinds
  genuinely differ in behavior (Drive vs Fly vs Building logic); a unified
  arena would just reintroduce `What_Am_I()` as `match entity.kind`.

Open (decide when the milestone arrives):

- **Pathfinding fidelity**: default to proper A* + reservation, but do campaign
  missions balance-depend on the original's weak pathing? Revisit at M6 with
  side-by-side play against Vanilla Conquer.
- **Bug-for-bug rules compat**: where original behavior contradicts rules.ini
  documentation, we follow observed original behavior; keep a `QUIRKS.md` log
  of each such case as we hit them.
- **expand.mix / Aftermath**: not on the freeware CD (ships with patch 1.08).
  Fetch later if expansion units are wanted.
