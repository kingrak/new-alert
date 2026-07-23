//! M9-A relay sequencer tests (SERVER-DESIGN.md §11): in-process server + N
//! real `RelayClient`/`RelayTransport` clients over 127.0.0.1 with OS-assigned
//! ports, plus direct-Server validation tests with synthetic time.
//!
//! Wall-guarded throughout (no unbounded loop). A trivial deterministic
//! pseudo-sim (fold the bundle's encoded commands into a running hash) stands in
//! for `ra-sim`: because every client executes the *same* canonical bundles,
//! their hash chains must be identical — the exact property the sequencer exists
//! to guarantee, isolated from the real sim's complexity.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram, HashVerdict, GAME_VERSION};
use ra_net::{
    CommandTransport, EndReason, PollResult, RelayClient, RelayIntent, RelayTransport,
    ReplayReader, ReplayRecord, SeatId, TickBundle,
};
use ra_server::{Server, ServerConfig};
use ra_sim::coords::CellCoord;
use ra_sim::{Command, Handle};

const WALL_SECS: u64 = 25;
const DELAY: u8 = 3;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = Instant::now().elapsed().as_nanos();
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("ra-server-test-{tag}-{pid}-{nanos}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A pseudo-sim command for `house`, varied by `n` so bundles differ tick to tick.
fn cmd(house: u8, n: u32) -> Command {
    Command::Move {
        unit: Handle { index: n, gen: 1 },
        dest: CellCoord::new(n as i32, house as i32),
        house,
    }
}

/// Fold a tick's canonical bundle into the running hash (deterministic; identical
/// bundles → identical chains).
fn fold(prev: u64, tick: u32, bundle: &TickBundle) -> u64 {
    let mut h = prev ^ (tick as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for c in bundle.flatten() {
        for b in wire::encode_command(&c) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}

// ---------------------------------------------------------------------------
// Real-socket harness (exercises the client transport code)
// ---------------------------------------------------------------------------

struct Harness {
    server: Server,
    sock: UdpSocket,
    addr: SocketAddr,
    /// Percent loss injected both directions on in-game traffic (0 = none).
    loss: u32,
    rng: u64,
    /// Once true, loss applies (kept off during the lobby handshake).
    game: bool,
}

impl Harness {
    fn new(config: ServerConfig, loss: u32) -> Harness {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sock.set_nonblocking(true).unwrap();
        let addr = sock.local_addr().unwrap();
        Harness {
            server: Server::new(config),
            sock,
            addr,
            loss,
            rng: 0x1234_5678_9ABC_DEF0,
            game: false,
        }
    }

    fn drop_it(&mut self) -> bool {
        if !self.game || self.loss == 0 {
            return false;
        }
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.rng >> 33) % 100 < self.loss as u64
    }

    fn pump(&mut self, now: Instant) {
        let mut buf = [0u8; 65536];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if self.drop_it() {
                        continue; // client→server loss
                    }
                    let bytes = buf[..n].to_vec();
                    self.server.recv(src, &bytes, now);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        self.server.advance_time(now);
        for (addr, bytes) in self.server.take_outgoing() {
            if self.drop_it() {
                continue; // server→client loss
            }
            let _ = self.sock.send_to(&bytes, addr);
        }
    }
}

/// A client's in-game bookkeeping.
struct Player {
    transport: RelayTransport,
    house: SeatId,
    tick: u32,
    submitted: u32,
    hash: u64,
    chain: Vec<(u32, u64)>,
    executed: BTreeMap<u32, TickBundle>,
    /// If set, corrupt every reported hash (arbitration test).
    corrupt: bool,
    desynced: bool,
}

impl Player {
    fn new(transport: RelayTransport, house: SeatId, corrupt: bool) -> Player {
        Player {
            transport,
            house,
            tick: 0,
            submitted: 0,
            hash: 0,
            chain: Vec::new(),
            executed: BTreeMap::new(),
            corrupt,
            desynced: false,
        }
    }
}

/// Bring `n` clients from connect → START and return their transports+houses.
/// Client 0 creates; the rest join. Distinct names so joiners self-identify.
fn lobby_to_start(harness: &mut Harness, n: u8, seats: u8) -> Vec<(RelayTransport, SeatId)> {
    let mut creator = RelayClient::connect(
        harness.addr,
        "p0",
        RelayIntent::Create {
            name: "game".to_string(),
            map: "scm01ea.ini".to_string(),
            seats,
            credits: 8000,
            seed: 0x1234,
            catalog_hash: 0xCAFE,
        },
    )
    .unwrap();

    // Drive the creator until it has a session id (so joiners can target it).
    let start = Instant::now();
    let session_id = loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: create"
        );
        creator.poll();
        harness.pump(Instant::now());
        // The server assigns id 1 to the first session; once the creator is in
        // the lobby we know it exists.
        if creator.in_lobby() || creator.started() {
            break 1u32;
        }
        if let Some(e) = creator.error() {
            panic!("creator failed: {e}");
        }
    };

    let mut joiners: Vec<RelayClient> = (1..n)
        .map(|i| {
            RelayClient::connect(
                harness.addr,
                &format!("p{i}"),
                RelayIntent::Join { session_id },
            )
            .unwrap()
        })
        .collect();

    // Everyone readies once in the lobby; drive to START.
    let mut clients: Vec<RelayClient> = Vec::new();
    clients.push(creator);
    clients.append(&mut joiners);

    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: start"
        );
        for c in &mut clients {
            c.poll();
            if c.in_lobby() {
                c.set_ready();
            }
            if let Some(e) = c.error() {
                panic!("lobby failed: {e}");
            }
        }
        harness.pump(Instant::now());
        if clients.iter().all(|c| c.started()) {
            break;
        }
    }

    clients
        .into_iter()
        .map(|c| {
            let house = c.my_house().unwrap();
            (c.into_transport().unwrap(), house)
        })
        .collect()
}

/// Run a scripted game to `ticks` execution ticks. `script(house, tick)` returns
/// the commands that house issues (submitted) at submit-tick `tick`.
fn run_game(
    harness: &mut Harness,
    players: &mut [Player],
    ticks: u32,
    script: &dyn Fn(u8, u32) -> Vec<Command>,
) {
    harness.game = true;
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: game"
        );
        for p in players.iter_mut() {
            if p.tick >= ticks || p.desynced {
                continue;
            }
            // Submit this tick's scripted commands exactly once, before its first poll.
            if p.submitted == p.tick {
                for c in script(p.house, p.tick) {
                    p.transport.submit(c);
                }
                p.submitted = p.tick + 1;
            }
            match p.transport.poll(p.tick) {
                PollResult::Ready(bundle) => {
                    p.hash = fold(p.hash, p.tick, &bundle);
                    let reported = if p.corrupt { p.hash ^ 0xDEAD } else { p.hash };
                    p.chain.push((p.tick, reported));
                    p.executed.insert(p.tick, bundle);
                    p.transport.report_hash(p.tick, reported);
                    p.tick += 1;
                }
                PollResult::Waiting => {}
                PollResult::Desync(_) => p.desynced = true,
                PollResult::ConnectionLost(l) => panic!("unexpected connection loss: {l:?}"),
            }
        }
        harness.pump(Instant::now());
        if players.iter().all(|p| p.tick >= ticks || p.desynced) {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: real-socket end-to-end
// ---------------------------------------------------------------------------

/// ACCEPTANCE (§10): create → join → ready → START → scripted game through the
/// relay → identical hash chains → clean end → replay log on disk decodes and its
/// bundles + hash chain match the clients'.
#[test]
fn e2e_two_client_game_identical_chains_and_replay_matches() {
    let dir = temp_dir("e2e");
    let config = ServerConfig {
        input_delay: DELAY,
        replay_dir: Some(dir.clone()),
        max_dgrams_per_sec: 1_000_000, // fast test loop: don't misread bursts as flood
        ..Default::default()
    };
    let mut h = Harness::new(config, 0);
    let started = lobby_to_start(&mut h, 2, 2);
    let mut players: Vec<Player> = started
        .into_iter()
        .map(|(t, house)| Player::new(t, house, false))
        .collect();

    let script = |house: u8, tick: u32| -> Vec<Command> {
        // Each house issues at a couple of distinct submit ticks.
        if (house == 1 && (tick == 2 || tick == 8)) || (house == 2 && (tick == 5 || tick == 11)) {
            vec![cmd(house, tick)]
        } else {
            Vec::new()
        }
    };

    run_game(&mut h, &mut players, 40, &script);

    // Identical hash chains across clients.
    assert_eq!(
        players[0].chain, players[1].chain,
        "hash chains must be identical"
    );
    assert!(players[0].chain.len() >= 40);

    // Clean end: both leave → session empties → Closed → replay finalized.
    for p in &mut players {
        p.transport.send_leave();
    }
    let start = Instant::now();
    while h.server.session_count() > 0 {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: teardown"
        );
        h.pump(Instant::now());
    }

    // The replay log decodes and matches.
    let path = dir.join("1.rar1");
    let bytes = std::fs::read(&path).expect("replay file must exist");
    let (header, reader) = ReplayReader::open(&bytes).expect("replay header decodes");
    assert_eq!(header.seed, 0x1234);
    assert_eq!(header.game_version, GAME_VERSION);
    let records = reader.collect_records().expect("records decode cleanly");

    let mut saw_end = false;
    let mut hash_records = 0;
    let client_chain: BTreeMap<u32, u64> = players[0].chain.iter().copied().collect();
    for rec in &records {
        match rec {
            ReplayRecord::Tick { tick, bundle } => {
                // The recorded canonical bundle equals what the client executed.
                let executed = players[0].executed.get(tick).expect("executed tick");
                assert_eq!(
                    &bundle.seats, &executed.seats,
                    "replay bundle must match execution at tick {tick}"
                );
            }
            ReplayRecord::Hash { tick, hash } => {
                hash_records += 1;
                assert_eq!(
                    client_chain.get(tick),
                    Some(hash),
                    "replay hash chain must match the client's at tick {tick}"
                );
            }
            ReplayRecord::End { reason, .. } => {
                saw_end = true;
                assert_eq!(*reason, EndReason::Quit);
            }
        }
    }
    assert!(saw_end, "replay must be finalized with an End record");
    assert!(
        hash_records >= 2,
        "replay must carry the winning hash chain"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// §11.2: under injected jitter/loss the redundant carry + NACK heal, and all
/// clients still receive identical canonical bundle streams (identical chains).
#[test]
fn sequencing_identical_under_loss() {
    let config = ServerConfig {
        input_delay: DELAY,
        replay_dir: None,
        max_dgrams_per_sec: 1_000_000,
        ..Default::default()
    };
    let mut h = Harness::new(config, 25); // 25% loss both directions in-game
    let started = lobby_to_start(&mut h, 2, 2);
    let mut players: Vec<Player> = started
        .into_iter()
        .map(|(t, house)| Player::new(t, house, false))
        .collect();

    let script = |house: u8, tick: u32| -> Vec<Command> {
        if tick % 4 == house as u32 {
            vec![cmd(house, tick)]
        } else {
            Vec::new()
        }
    };
    run_game(&mut h, &mut players, 30, &script);

    assert_eq!(
        players[0].chain, players[1].chain,
        "chains identical despite loss"
    );
    assert!(
        players.iter().all(|p| !p.desynced),
        "no spurious desync under loss"
    );
    // The loss backstop must actually have exercised (either stalls or NACKs).
    assert!(
        players
            .iter()
            .any(|p| p.transport.stall_count() > 0 || p.transport.nacks_sent() > 0),
        "loss should have forced at least one stall/NACK"
    );
}

/// §11.3: corrupt one of three clients' hashes → server arbitrates, the two
/// honest clients agree and keep playing, the corrupt one gets YOU_DIVERGED and
/// latches Desync (the M9-A terminal end; resync is M9-B).
#[test]
fn three_client_majority_arbitration() {
    let config = ServerConfig {
        input_delay: DELAY,
        replay_dir: None,
        max_dgrams_per_sec: 1_000_000,
        ..Default::default()
    };
    let mut h = Harness::new(config, 0);
    let started = lobby_to_start(&mut h, 3, 3);
    let mut players: Vec<Player> = started
        .into_iter()
        .enumerate()
        .map(|(i, (t, house))| Player::new(t, house, i == 2)) // corrupt the 3rd
        .collect();

    // Drive until the corrupt client latches Desync (the server arbitrated a
    // disputed tick, the majority won, and the loser was told YOU_DIVERGED). We
    // do NOT require the honest clients to finish: once the corrupt seat stops
    // sending, the game stalls for the others — keeping it running through a
    // relayed resync is M9-B (§6.5). This is the documented M9-A terminal end.
    h.game = true;
    let script = |house: u8, tick: u32| -> Vec<Command> {
        if tick % 5 == house as u32 % 5 {
            vec![cmd(house, tick)]
        } else {
            Vec::new()
        }
    };
    let start = Instant::now();
    while !players[2].desynced {
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: arbitration"
        );
        for p in players.iter_mut() {
            if p.desynced {
                continue;
            }
            if p.submitted == p.tick {
                for c in script(p.house, p.tick) {
                    p.transport.submit(c);
                }
                p.submitted = p.tick + 1;
            }
            match p.transport.poll(p.tick) {
                PollResult::Ready(bundle) => {
                    p.hash = fold(p.hash, p.tick, &bundle);
                    let reported = if p.corrupt { p.hash ^ 0xDEAD } else { p.hash };
                    p.chain.push((p.tick, reported));
                    p.transport.report_hash(p.tick, reported);
                    p.tick += 1;
                }
                PollResult::Waiting => {}
                PollResult::Desync(_) => p.desynced = true,
                PollResult::ConnectionLost(_) => p.desynced = true,
            }
        }
        h.pump(Instant::now());
    }

    // The corrupt client diverged and was told so; the two honest clients agree
    // on every tick they both reached (the majority stream is canonical).
    assert!(
        players[2].desynced,
        "corrupt client must latch Desync from YOU_DIVERGED"
    );
    assert!(
        players[2].transport.desync().is_some(),
        "corrupt transport carries the DesyncDetected"
    );
    let common = players[0].chain.len().min(players[1].chain.len());
    assert!(common >= 1, "honest clients must have made progress");
    assert_eq!(
        players[0].chain[..common],
        players[1].chain[..common],
        "honest clients agree on their common prefix"
    );
}

/// §11.2: a command that arrives after its tick closed is dropped and answered
/// with a LATE advisory — proven at the Server level with a hand-built late
/// TICK_CMDS after the tick was already broadcast.
#[test]
fn late_command_dropped_and_advised() {
    let mut d = Direct::new(ServerConfig {
        input_delay: DELAY,
        replay_dir: None,
        ..Default::default()
    });
    let (a, b) = d.start_two_seat_game();

    // Advance the game: both seats submit an empty run so bundles close past a
    // tick, THEN seat A sends a real command for that already-closed tick.
    let ca = d.conn_id(a);
    let cb = d.conn_id(b);
    // Both reach exec ticks DELAY..=DELAY+3 (empty), closing them.
    for exec in DELAY as u32..=(DELAY as u32 + 3) {
        d.recv(
            a,
            Datagram::TickCmds {
                conn_id: ca,
                entries: vec![(exec, vec![])],
            },
        );
        d.recv(
            b,
            Datagram::TickCmds {
                conn_id: cb,
                entries: vec![(exec, vec![])],
            },
        );
    }
    let lates_before = d.server.counters().lates;
    // Seat A now sends a real command for tick DELAY, long since broadcast empty.
    let blob = wire::encode_command(&cmd(1, 99));
    d.recv(
        a,
        Datagram::TickCmds {
            conn_id: ca,
            entries: vec![(DELAY as u32, vec![blob])],
        },
    );

    assert!(
        d.server.counters().lates > lates_before,
        "a late command must be counted"
    );
    // A LATE advisory (HASH_VERDICT{WAIT}) went to seat A.
    assert!(
        d.outgoing_to(a).iter().any(|dg| matches!(
            dg,
            Datagram::HashVerdictMsg { verdict: HashVerdict::Wait, tick, .. } if *tick == DELAY as u32
        )),
        "seat A must receive a LATE advisory"
    );
}

// ---------------------------------------------------------------------------
// Direct-Server validation harness (synthetic time, no sockets)
// ---------------------------------------------------------------------------

struct Direct {
    server: Server,
    t0: Instant,
    conn_ids: BTreeMap<SocketAddr, u32>,
}

impl Direct {
    fn new(config: ServerConfig) -> Direct {
        Direct {
            server: Server::new(config),
            t0: Instant::now(),
            conn_ids: BTreeMap::new(),
        }
    }

    fn addr(i: u16) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 40000 + i)
    }

    fn recv(&mut self, addr: SocketAddr, d: Datagram) {
        let bytes = wire::encode(&d);
        self.server.recv(addr, &bytes, self.t0);
    }

    fn recv_raw(&mut self, addr: SocketAddr, bytes: &[u8]) {
        self.server.recv(addr, bytes, self.t0);
    }

    fn outgoing_to(&mut self, addr: SocketAddr) -> Vec<Datagram> {
        self.server
            .take_outgoing()
            .into_iter()
            .filter(|(a, _)| *a == addr)
            .filter_map(|(_, b)| wire::decode(&b).ok())
            .collect()
    }

    fn conn_id(&self, addr: SocketAddr) -> u32 {
        *self.conn_ids.get(&addr).expect("hello first")
    }

    /// Handshake one conn to a SRV_WELCOME and record its conn_id.
    fn hello(&mut self, addr: SocketAddr) -> u32 {
        self.recv(
            addr,
            Datagram::SrvHello {
                game_version: GAME_VERSION,
                client_nonce: 1,
            },
        );
        let cid = self
            .outgoing_to(addr)
            .into_iter()
            .find_map(|d| match d {
                Datagram::SrvWelcome { conn_id, .. } => Some(conn_id),
                _ => None,
            })
            .expect("SRV_WELCOME");
        self.conn_ids.insert(addr, cid);
        cid
    }

    /// Create a 2-seat session, join, both ready → Running. Returns the two addrs.
    fn start_two_seat_game(&mut self) -> (SocketAddr, SocketAddr) {
        let a = Self::addr(1);
        let b = Self::addr(2);
        let ca = self.hello(a);
        let cb = self.hello(b);
        self.recv(
            a,
            Datagram::SessCreate {
                conn_id: ca,
                name: "host".into(),
                map: "m".into(),
                seats: 2,
                credits: 5000,
                seed: 7,
                catalog_hash: 1,
            },
        );
        // Session id is 1 (first created).
        self.recv(
            b,
            Datagram::SessJoin {
                conn_id: cb,
                session_id: 1,
                name: "joiner".into(),
            },
        );
        self.recv(
            a,
            Datagram::SessReady {
                conn_id: ca,
                ready: true,
            },
        );
        self.recv(
            b,
            Datagram::SessReady {
                conn_id: cb,
                ready: true,
            },
        );
        // Drain the START/state traffic.
        let _ = self.server.take_outgoing();
        (a, b)
    }
}

/// §7.3: a command bound to the wrong house kicks the seat.
#[test]
fn wrong_house_command_kicks_seat() {
    let mut d = Direct::new(ServerConfig {
        input_delay: DELAY,
        replay_dir: None,
        ..Default::default()
    });
    let (a, _b) = d.start_two_seat_game();
    let ca = d.conn_id(a);
    let kicks_before = d.server.counters().kicks;
    // Seat A is house 1; send a command claiming house 2.
    let blob = wire::encode_command(&cmd(2, 1));
    d.recv(
        a,
        Datagram::TickCmds {
            conn_id: ca,
            entries: vec![(DELAY as u32, vec![blob])],
        },
    );
    assert!(
        d.server.counters().kicks > kicks_before,
        "wrong-house must kick"
    );
    assert!(
        d.outgoing_to(a)
            .iter()
            .any(|dg| matches!(dg, Datagram::SessLeave { .. })),
        "kicked seat receives SESS_LEAVE"
    );
}

/// §7.5: sustained flooding kicks the connection.
#[test]
fn flood_kicks_connection() {
    let mut d = Direct::new(ServerConfig::default());
    let a = Direct::addr(1);
    let ca = d.hello(a);
    let kicks_before = d.server.counters().kicks;
    // Blast well past 2×60 datagrams at the same instant (all inside the 1s
    // window) → flood kick.
    for _ in 0..200 {
        d.recv(a, Datagram::SessListReq { conn_id: ca });
    }
    assert!(
        d.server.counters().kicks > kicks_before,
        "sustained flood must kick"
    );
}

/// §7.2: a spoofed conn_id (right address, wrong id) is ignored.
#[test]
fn spoofed_conn_id_ignored() {
    let mut d = Direct::new(ServerConfig::default());
    let a = Direct::addr(1);
    let ca = d.hello(a);
    // A create with the WRONG conn_id must not create a session.
    d.recv(
        a,
        Datagram::SessCreate {
            conn_id: ca ^ 0xFFFF_FFFF,
            name: "x".into(),
            map: "m".into(),
            seats: 2,
            credits: 0,
            seed: 0,
            catalog_hash: 0,
        },
    );
    assert_eq!(
        d.server.session_count(),
        0,
        "spoofed conn_id must not create a session"
    );
    // The correct conn_id does create one.
    d.recv(
        a,
        Datagram::SessCreate {
            conn_id: ca,
            name: "x".into(),
            map: "m".into(),
            seats: 2,
            credits: 0,
            seed: 0,
            catalog_hash: 0,
        },
    );
    assert_eq!(d.server.session_count(), 1);
}

/// §7.6: session-state gating — TICK_CMDS before the game is Running is ignored,
/// and a decode-garbage datagram never panics (fuzz-safety).
#[test]
fn state_gating_and_garbage_are_safe() {
    let mut d = Direct::new(ServerConfig::default());
    let a = Direct::addr(1);
    let ca = d.hello(a);
    d.recv(
        a,
        Datagram::SessCreate {
            conn_id: ca,
            name: "x".into(),
            map: "m".into(),
            seats: 2,
            credits: 0,
            seed: 0,
            catalog_hash: 0,
        },
    );
    // In Lobby (not Running): TICK_CMDS is a no-op, not a crash.
    let blob = wire::encode_command(&cmd(1, 1));
    d.recv(
        a,
        Datagram::TickCmds {
            conn_id: ca,
            entries: vec![(DELAY as u32, vec![blob])],
        },
    );
    // Garbage bytes: ignored, counted, never panic.
    let de_before = d.server.counters().decode_errors;
    d.recv_raw(a, &[0xFF, 0x00, 0x13, 0x37, 0x42]);
    assert!(d.server.counters().decode_errors > de_before);
    // Server still healthy.
    assert_eq!(d.server.session_count(), 1);
}
