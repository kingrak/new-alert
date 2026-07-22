# M9 Server Design — the Relay Sequencer (`ra-server`)

Status: **design approved for implementation** · Owner: session lead · Implements DESIGN.md §4.6 Stage 3.
Read DESIGN.md §3.4 (lockstep), §3.6 (snapshots), §4.2 (determinism contract), §4.6 (networking
evolution), §4.7 (platform layering) first. This document is law for the M9 cycles the same way
DESIGN.md is for the engine.

---

## 1. Goals and non-goals

**Goals**
- Internet play for our clients: N players (2 first, N-seat capable) connect **out** to a server
  (NAT-friendly), play the same lockstep sim they play on LAN.
- The server is an **authoritative sequencer, never a sim host**: it orders commands into canonical
  tick bundles, broadcasts them, arbitrates hash disputes, brokers snapshot resyncs, and records
  the canonical command log (= replay) per game. It never loads a map, never runs a tick.
- Lobby service: create/list/join sessions over the internet (replaces LAN broadcast discovery,
  which cannot cross networks).
- Cheap to run: one small static Rust binary, one UDP port, in-memory state, dozens of concurrent
  games on a shared box (deploy target: any Linux host; see §10 Ops).

**Non-goals (documented, deliberate)**
- No authoritative-sim anti-maphack (server would need to run the sim; out of scope per §4.6 —
  CnCNet-parity posture). We validate what a sequencer *can* validate (§7).
- No accounts/auth/ranking in M9 (session-scoped identities only; a join token per game).
  Persistent identity is a later milestone if ever.
- No TCP/WebSocket fallback in M9-A (UDP-only, same as the game transport; revisit only if real
  deployments show UDP-blocked players).
- No server-initiated matchmaking (players pick a session from the list; no ELO pairing).

## 2. Topology and trust model

```
client A ──UDP──▶ ┌──────────────┐ ◀──UDP── client B
                  │  ra-server   │
client C ──UDP──▶ │  (sequencer) │ ◀──UDP── client D
                  └──────────────┘
   each client has exactly ONE socket peer: the server.
```

- Clients never learn each other's addresses (no P2P, no address leaking; snapshot resync is
  **relayed** through the server, §6.5).
- Trust boundary per §4.6: the server treats every datagram as adversarial (wire.rs's
  never-panic decode is already the standard), enforces per-connection rate/size caps, and binds
  each connection to exactly one seat after join (§7). Clients trust the server's sequencing
  absolutely — that is the design: one authority, no peer ambiguity. (2-player LAN's
  "host-authoritative" rule from M8-C generalizes to "server-authoritative".)

## 3. Crate layout

```
ra-server/            # NEW workspace member. Binary + lib (lib for in-process CI tests).
  src/main.rs         # arg parsing, socket bind, run loop
  src/lib.rs          # Server struct: pure poll-driven state machine (testable in-process)
  src/session.rs      # lobby sessions + live games (state machines, §5/§6)
  src/relay.rs        # tick sequencing, bundle broadcast, hash arbitration (§6)
  src/replay.rs       # canonical command-log writer (§8)
ra-net/src/relay.rs   # NEW: RelayTransport (client side), implements CommandTransport
ra-net/src/wire.rs    # message set extended (§4); PROTOCOL_VERSION bump 1 → 2
```

Rules carried over: `ra-server` and the ra-net additions are **std-only** (no tokio, no async, no
serde). One nonblocking UDP socket + a single-threaded poll loop (§9) handles the M9 scale target
(dozens of games × 4 seats × ~15 datagrams/s each — thousands of small packets/s, trivial for one
core). `ra-server` depends on `ra-net` (wire + shared constants) and **must not** depend on
`ra-sim` (enforces "never a sim host" structurally; command payloads are treated as opaque
validated bytes, §7).

## 4. Wire protocol extension

`PROTOCOL_VERSION: u16 = 2`. All M8 rules stand: little-endian, `u8`-length-prefixed capped
strings, length-checked never-panic decode, tag validation, exact consumption, size caps.
LAN (v1) messages are unchanged — a v2 client still speaks v1 LAN. New/changed messages:

| Message              | Dir            | Payload (beyond 5-byte header)                                                | Purpose |
|----------------------|----------------|--------------------------------------------------------------------------------|---------|
| `SRV_HELLO`          | C→S            | game_version u32, client_nonce u32                                             | first contact; server replies `SRV_WELCOME` or `REJECT` |
| `SRV_WELCOME`        | S→C            | server_nonce u32, conn_id u32                                                  | conn_id = random u32, echoed in every later C→S message (cheap off-path spoof guard, §7) |
| `SESS_CREATE`        | C→S            | name, map_name, seats u8, credits u32, seed u32, catalog_hash u64              | create lobby session; creator gets seat 0 (the "host" seat = settings authority) |
| `SESS_LIST_REQ`      | C→S            | —                                                                              | request session list |
| `SESS_LIST`          | S→C            | n × {session_id u32, name, map_name, seats_taken u8, seats u8, in_progress u8} | capped page (≤ 32 entries) |
| `SESS_JOIN`          | C→S            | session_id u32, player_name                                                    | join; server assigns next free seat |
| `SESS_STATE`         | S→C            | session_id, seats[] {seat u8, name, ready u8}, settings…                       | authoritative lobby state, re-broadcast on every change (idempotent, loss-tolerant) |
| `SESS_READY`         | C→S            | ready u8                                                                       | ready toggle |
| `SESS_LEAVE`         | C→S / S→C      | reason u8                                                                      | leave/kick/dissolve |
| `SESS_START`         | S→C            | start_tick u32, input_delay u8, seat_map[]                                     | all-ready → server fires; carries the **initial input delay** (§6.2) |
| `TICK_CMDS`          | C→S            | redundant window: n × {tick u32, cmds[]} (same shape as LAN `BUNDLES`)         | client's own commands, stamped by the client's `InputScheduler` exactly as on LAN |
| `TICK_BUNDLE`        | S→C            | redundant window: n × {tick u32, per-seat cmds[][]}                            | **canonical** sequenced bundle for tick T, all seats, seat-ascending |
| `TICK_HASH`          | C→S            | n × {tick u32, hash u64} (redundant window)                                    | per-tick state hashes |
| `HASH_VERDICT`       | S→C            | tick u32, verdict u8 (OK / YOU_DIVERGED / WAIT), majority_hash u64             | arbitration result; only sent on dispute or query |
| `NACK` / `KEEPALIVE` | both           | unchanged from v1                                                              | same semantics vs the server |
| `SNAP_*` (OFFER/CHUNK/ACK/DONE) | relayed | unchanged from M8-C, but addressed via server: payload gains target_seat u8 | server relays donor→victim (§6.5) |
| `REPLAY_REQ` / `REPLAY_CHUNK`   | C→S / S→C | game_id u32, offset u32                                                   | post-game replay download (M9-B) |

Notes:
- `TICK_CMDS`/`TICK_BUNDLE`/`TICK_HASH` reuse the LAN redundant-carry + NACK discipline verbatim
  (proven in M8-B torture tests); the only change is the counterpart is the server.
- `catalog_hash` in `SESS_CREATE` + joiner echo in `SESS_JOIN` reject content-mismatched clients at
  the lobby, same rule as M8-C snapshots.

## 5. Session lifecycle (server side)

```
        SESS_CREATE                    all READY                 game over / empty
  ∅ ────────────────▶ Lobby ─────────────────────▶ Running ───────────────────────▶ Closed
                        │  join/leave/ready              │ every seat gone, or        (replay
                        │  (SESS_STATE rebroadcast)      │ END_OF_GAME from all       finalized,
                        └── creator leaves → dissolve    │ live seats                 session GC)
                                                         └── resync episodes (§6.5) loop within Running
```

- Lobby state is **server-authoritative**; clients render `SESS_STATE` verbatim (no client-side
  lobby truth — eliminates the LAN lobby's self-healing complexity: there is one brain now).
- Seat 0 (creator) owns settings until start; server enforces (settings from non-seat-0 → ignored).
- Idle timeouts: lobby session with no traffic 120 s → dissolved; Running game where **all** seats
  time out 60 s → Closed (replay still finalized).
- One connection = one seat in ≤ 1 session (server enforces; a second SESS_JOIN from the same
  conn_id moves the seat only if still in Lobby, per the M8-B double-JOIN idempotency precedent).

## 6. The sequencer (core of M9)

### 6.1 Tick sequencing
- Clients stamp their own commands with their `InputScheduler` exactly as on LAN (sender-clock-pure,
  the §4.2-compatible rule pinned since M8-A). `TICK_CMDS` arrive at the server tagged with their
  execution tick.
- The server, per tick T, in **seat-ascending order** (the canonical order pinned in M8-A),
  assembles `TICK_BUNDLE[T]` = every seat's commands stamped for T (empty list for silent seats)
  and broadcasts it when T's **bundle deadline** passes (§6.2). The bundle is what clients execute —
  clients never execute their own commands from local state in M9; everything round-trips through
  the sequencer (unchanged from LAN where everything round-trips through the barrier — same seam,
  `PollResult::Ready(TickBundle)`).
- A `TICK_CMDS` for tick T arriving **after** T's bundle was broadcast is **late**: dropped, counted,
  and answered with `HASH_VERDICT{WAIT}`-style advisory `LATE` notice so the client can raise its
  delay (§6.2). Late commands are NEVER merged into a later tick by the server (would violate
  sender-stamp purity); the client's own scheduler owns re-stamping. This is the M8-A
  "arrival timing can stall, never reschedule" rule, now enforced at the server.

### 6.2 Adaptive input delay (the QUEUE.CPP:1440-1461 port, finally)
- LAN used fixed delay 3. Internet latency varies; M9 ports the original's runtime MaxAhead
  retune: the server measures per-seat round-trip continuously (KEEPALIVE echoes + cmd→bundle
  timing), and when the p95 one-way latency for any seat exceeds `delay_ticks × TICK_MS` budget,
  broadcasts `SESS_TIMING{new_delay, effective_tick}` (a `TICK_BUNDLE`-embedded control record so
  it is ordered with the stream). All clients raise their scheduler delay **at the same effective
  tick** — deterministic, ordered, mirrors `EventClass::TIMING` (QUEUE.CPP:1440-1461: "Compute our
  new MaxAhead"). Delay only ratchets up mid-game (shrinking is a lobby-time decision — matches
  the original's posture and avoids oscillation).
- Bundle deadline for tick T = its stream position at the current cadence; the server never waits
  for a slow seat beyond the deadline (that seat's entry is empty; the seat gets `LATE` advisories
  and, past `STALL_TICKS`, the whole game gets a server-paused `WAITING{seat}` control record —
  the LAN "waiting for player" overlay, now server-arbitrated so all peers pause identically).

### 6.3 Hash arbitration
- Clients report `TICK_HASH` on the LAN cadence. The server compares per tick across seats
  (it holds no truth of its own — it cannot compute world hashes; it arbitrates *agreement*):
  - all equal → nothing sent (silence = OK).
  - disagreement → **majority hash wins** (≥ ⌈live_seats/2⌉+ … strictly: the largest equal-hash
    group; ties broken by lowest-seat-in-group, making seat 0 the 2-player tiebreak — which
    degenerates exactly to M8-C's host-authoritative rule). Divergent seats get
    `HASH_VERDICT{YOU_DIVERGED, majority_hash}` → client enters resync (§6.5).
- The server records the winning hash per tick in the replay log (§8) — replays carry the
  canonical hash chain for free.

### 6.4 Disconnect / rejoin
- Seat timeout mid-game → `WAITING{seat}` pause (as above) for up to `REJOIN_WINDOW = 60 s`.
  Within the window, a `SRV_HELLO` + `SESS_JOIN{session_id}` carrying the same join token
  (issued at first join, §7) reclaims the seat → server marks the seat resyncing and brokers a
  snapshot from a healthy donor seat (§6.5) → game resumes. Past the window: seat is dropped
  permanently; its units follow the sim's existing player-left rule (M8-B `PlayerLeft` command
  injected into the bundle by the server — the ONE command type the server may originate; it is
  validated by clients as only-from-server).
- This makes internet drops survivable, which LAN never had — and it reuses M8-C wholesale.

### 6.5 Snapshot resync, relayed
- Roles: **victim** (diverged/rejoining seat), **donor** (lowest-numbered healthy majority seat —
  deterministic choice, no negotiation), server = relay + progress supervisor.
- Flow: server → donor `SNAP_REQUEST{victim_seat, tick}` → donor `SNAP_OFFER` → chunks stream
  donor→server→victim with per-chunk ACK end-to-end (server forwards ACKs; it buffers at most
  `SNAP_WINDOW = 32` chunks — bounded memory) → victim loads, hash-verifies vs the majority hash,
  reports `TICK_HASH` at the snapshot tick → server re-admits the seat into sequencing at the next
  bundle. Attempt cap 2 (M8-C rule), then the seat is dropped (not the game).
- During a resync episode the game **keeps playing** (the victim buffers `TICK_BUNDLE`s from the
  snapshot tick forward — they are small — and fast-forwards after load; only if the victim's lag
  exceeds `FAST_FORWARD_CAP = 900 ticks` does the server pause the game for it). This is the key
  UX upgrade over M8-C LAN, where resync paused both players.

## 7. Server-side validation (what a sequencer CAN check)

Per §4.6 the hooks exist because every `Command` carries its issuing house. The server validates,
per datagram → per command, **without running the sim**:

1. wire validity (never-panic decode, caps, exact consumption) — existing standard;
2. `conn_id` matches the connection that owns the claimed seat (`SRV_WELCOME` binding);
3. **seat-house binding**: every command's `house` equals the seat's assigned house — a client can
   never issue for another house (the maphack-adjacent low-hanging fruit closed);
4. tick sanity: stamped tick within `[current_tick, current_tick + MAX_AHEAD_WINDOW]`;
5. rate caps: ≤ `MAX_CMDS_PER_TICK_PER_SEAT = 64` (a human peaks well under this; the LAN cap of
   4096 was a decode bound, not a policy bound), ≤ `MAX_DGRAMS_PER_SEC = 60` per connection,
   sustained-violation → seat kicked with `SESS_LEAVE{reason: Flood}`;
6. session-state gating: `TICK_CMDS` only in Running, `SESS_*` only in the right phase, `SNAP_*`
   only during an authorized resync episode, and only between the assigned donor/victim.

Command *semantic* validity ("does house X own unit Y") is NOT checkable sim-free and is
explicitly out of scope (§1 non-goals); the sim itself already ignores illegal commands
deterministically (M6+ rule: `apply` validates ownership and no-ops invalid commands identically
on every client — the determinism contract makes cheating by invalid command a no-op, not a
desync).

## 8. Replays (fall out for free, made real)

- Per Running game the server appends to `games/<game_id>.rar1` (RA replay v1, versioned header:
  protocol, game version, map, seed, seats, catalog_hash) each broadcast `TICK_BUNDLE` and the
  winning `TICK_HASH` chain. Closed → file finalized with end-of-game marker + duration.
- Format is the wire encoding itself (one length-prefixed datagram after another) — the replay
  reader in ra-client reuses wire decode; a replay IS a `CommandTransport` (a `ReplayTransport`
  playing bundles back on schedule — M9-B client feature, closing the §4.6 "replays work" loop
  end-to-end with hash verification against the recorded chain).
- Retention: config knob, default keep last 200 games or 7 days (in-memory index, files on disk).

## 9. Server runtime model

Single-threaded poll loop (std-only, mirrors the client's discipline; ~§4.7 platform quarantine —
socket setup in one module):

```
loop {
    recv_all_pending();            // nonblocking drain, per-datagram validate→dispatch
    advance_time(now);             // bundle deadlines, keepalives, timeouts, resync supervision
    flush_outgoing();              // batched sends (bundles for due ticks, SESS_STATE, verdicts)
    sleep_until(next_deadline);    // one timer: min(next bundle deadline, next timeout)
}
```

- All state in a `Server` struct with **no wall-clock reads inside logic** — `now` is passed in
  (`advance_time(Instant)`), so CI tests drive the server with synthetic time exactly like the
  lockstep tests drive jitter (the M8 testability doctrine applied to the server).
- Memory bounds: every queue/buffer capped (sessions ≤ 256, seats ≤ 8/game, snapshot window 32
  chunks, replay write-behind buffer 1 MiB); overflow → oldest-drop + counter, never unbounded
  growth. Counters exported in `STATUS` (below).
- Ops surface: `--port` (default 21058, distinct from LAN discovery 21057), `--replay-dir`,
  `--max-sessions`; SIGTERM → broadcast `SESS_LEAVE{ServerShutdown}`, finalize replays, exit;
  a `STATUS` datagram (localhost-only by default) returns counters (sessions, games, seats,
  drops, lates, resyncs) for monitoring — plain text, curl-able via socat/nc.

## 10. Ops & deployment (M9 scale)

- One static binary (`cargo build --release -p ra-server`), one UDP port forwarded. Runs anywhere
  Linux; for our own testing: the shared .66 box per `~/dev/ai/innov/docs/infra-setup.md`
  conventions (tmux session, never bind the GPU, coordinate as usual) or any small VPS.
- No TLS in M9 (UDP game traffic; nothing secret flows — names and commands). Revisit only with
  accounts (out of scope).

## 11. Test plan (CI, no real internet needed — the M8 doctrine continued)

1. **In-process server**: `Server` driven by synthetic `Instant`s + in-process sockets
   (127.0.0.1:0). Every test runs server + N clients in one process, single thread.
2. **Sequencing properties**: N clients submit scripted commands with jitter/loss (the M8-B lossy
   proxy, reused) → all clients' bundle streams identical, seat-ascending, hash chains identical;
   late-cmd drop + advisory verified; delay ratchet fires under injected latency and all clients
   shift at the same tick (revert-drill: unsynchronized shift ⇒ divergence caught by the existing
   pins).
3. **Arbitration**: corrupt 1 of 3 clients → majority verdict correct, victim resyncs via relayed
   snapshot while the other two keep playing (assert no pause below FAST_FORWARD_CAP); 2-player
   tie → seat-0 wins (degenerates to M8-C behavior).
4. **Rejoin**: kill a client mid-game, reconnect within window with token → seat reclaimed,
   snapshot, hash chain identical after; past window → PlayerLeft injected, game decisive.
5. **Validation/abuse**: wrong-house commands rejected + kick on sustained flood; spoofed conn_id
   ignored; malformed fuzz (extend wire_fuzz_deep to v2 messages) never panics; session-state
   gating enforced (SNAP_CHUNK outside an episode → dropped+counted).
6. **Replay**: record a full game → `ReplayTransport` playback reproduces the exact hash chain.
7. **Wall-clock realism smoke** (one test, generous bounds): real sockets, real time, 2 clients,
   short game — the only non-synthetic-time test, wall-guarded.

## 12. Milestones

- **M9-A — sequencer core**: crate + v2 wire + SRV/SESS lifecycle + TICK sequencing + hash
  arbitration + fixed delay (no adaptive), 2-client internet-style play in CI, validation §7,
  STATUS counters. Client: `RelayTransport` + "INTERNET" menu entry (server address field) reusing
  the LAN lobby UI shell.
- **M9-B — resilience + adaptive**: relayed snapshot resync (§6.5), rejoin (§6.4), adaptive delay
  (§6.2), replays end-to-end (§8 + `ReplayTransport` playback in client), N>2 seats.
- **M9-C — hardening**: abuse/flood suite, soak test (hours-long AI-vs-AI via relay, memory-bound
  assertions), ops polish (STATUS, retention), deployment doc.

Each milestone follows the standing loop: Opus coder cycle → session-lead independent verification
→ commit → Sonnet depth audit (revert-drills, fuzz, citation checks) → corrections committed.

---
*Cross-references: QUEUE.CPP:1440-1461 (MaxAhead retune — ported in §6.2), QUEUE.CPP:960-976
(deadlock-breaking resend — inherited via NACK), QUEUE.CPP:3286-3290 (canonical order — inherited
via TickBundle), DESIGN.md §3.4/§3.6/§4.6. The LAN stack (M8) remains fully supported; the relay
path is additive.*
