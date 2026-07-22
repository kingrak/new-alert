//! M8-B depth audit, item 4: LAN discovery + lobby edge cases beyond the
//! happy-path flow already proven in `lan_lockstep.rs`
//! (`full_lobby_flow_announce_join_ready_start_then_lockstep`) and
//! `ra-client/tests/ui_lan_lobby.rs`. All sockets are 127.0.0.1 with
//! OS-assigned ports — the fixed `DISCOVERY_PORT` is never bound here.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ra_net::wire::{self, Datagram, GAME_VERSION};
use ra_net::{DiscoveryConfig, HostLobby, JoinLobby, SessionBrowser, SessionSettings};

const WALL_SECS: u64 = 60;

fn loopback_socket() -> UdpSocket {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    s.set_nonblocking(true).expect("nonblocking");
    s
}

fn ephemeral_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        announce_targets: Vec::new(),
        listen_port: 0,
    }
}

fn settings(map: &str) -> SessionSettings {
    SessionSettings {
        map: map.to_string(),
        seed: 0xD00D_F00D,
        credits: 5000,
        host_seat: 1,
        join_seat: 2,
        delay: 3,
    }
}

// ---------------------------------------------------------------------------
// (4a) Two hosts announcing simultaneously: the browser lists both; joining
// one works.
// ---------------------------------------------------------------------------

#[test]
fn two_simultaneous_hosts_both_appear_and_either_is_joinable() {
    let mut browser = SessionBrowser::bind(&ephemeral_discovery()).unwrap();
    let browser_addr: SocketAddr = format!("127.0.0.1:{}", browser.port()).parse().unwrap();
    let cfg = DiscoveryConfig {
        announce_targets: vec![browser_addr],
        listen_port: 0,
    };
    let mut host_a = HostLobby::create("ALPHA", settings("map_a.ini"), &cfg).unwrap();
    let mut host_b = HostLobby::create("BRAVO", settings("map_b.ini"), &cfg).unwrap();

    let start = Instant::now();
    loop {
        host_a.poll();
        host_b.poll();
        browser.poll();
        if browser.sessions().len() >= 2 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "both hosts never appeared in the browser (saw {})",
            browser.sessions().len()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let names: std::collections::BTreeSet<String> =
        browser.sessions().iter().map(|s| s.name.clone()).collect();
    assert!(
        names.contains("ALPHA") && names.contains("BRAVO"),
        "both sessions must be listed distinctly: {names:?}"
    );
    let maps: std::collections::BTreeSet<String> =
        browser.sessions().iter().map(|s| s.map.clone()).collect();
    assert!(maps.contains("map_a.ini") && maps.contains("map_b.ini"));

    // Join the second-listed session specifically (proves the browser's list
    // addressing is per-session, not "whichever host happens to answer").
    let target = browser
        .sessions()
        .iter()
        .find(|s| s.name == "BRAVO")
        .expect("BRAVO must be in the list")
        .clone();
    let mut join = JoinLobby::join(target.addr, "JOINER").unwrap();
    let start = Instant::now();
    while join.welcome().is_none() {
        host_a.poll();
        host_b.poll();
        join.poll();
        assert!(join.error().is_none(), "join failed: {:?}", join.error());
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(2));
    }
    let w = join.welcome().unwrap();
    assert_eq!(w.map, "map_b.ini", "must have joined BRAVO specifically");
    assert_eq!(w.host_name, "BRAVO");
    // ALPHA must be unaffected: no joiner seated on it.
    assert!(
        host_a.joiner_name().is_none(),
        "ALPHA must not have gained a joiner"
    );
    assert_eq!(host_b.joiner_name(), Some("JOINER"));
}

// ---------------------------------------------------------------------------
// (4b) Joiner joins as the host cancels before ever processing the JOIN:
// clean timeout, no hang.
// ---------------------------------------------------------------------------

#[test]
fn host_vanishes_before_processing_join_joiner_times_out_cleanly() {
    let host = HostLobby::create("GONE", settings("m.ini"), &ephemeral_discovery()).unwrap();
    let host_addr: SocketAddr = format!("127.0.0.1:{}", host.port()).parse().unwrap();

    let mut join = JoinLobby::join(host_addr, "JOINER").unwrap();
    join.set_timeout(Duration::from_millis(200));

    // The host is torn down *without ever polling* — its JOIN datagram is
    // still sitting unread in the kernel socket buffer when the host's
    // socket is dropped. No WELCOME, no explicit REJECT/LEAVE: the joiner
    // has nothing to go on but its own timeout.
    drop(host);

    let start = Instant::now();
    while join.error().is_none() {
        join.poll();
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "joiner never resolved a vanished, never-polled host to a clean error (hang)"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!join.started());
    assert!(
        join.error().unwrap().contains("timed out"),
        "expected a timeout message, got: {:?}",
        join.error()
    );
}

// ---------------------------------------------------------------------------
// (4c) READY then immediate LEAVE, before START: the host must not be left
// thinking it can start with a vanished joiner.
// ---------------------------------------------------------------------------

#[test]
fn ready_then_immediate_leave_before_start_clears_can_start() {
    let mut host = HostLobby::create("HOSTY", settings("m.ini"), &ephemeral_discovery()).unwrap();
    let host_addr: SocketAddr = format!("127.0.0.1:{}", host.port()).parse().unwrap();
    let mut join = JoinLobby::join(host_addr, "JOINER").unwrap();

    let start = Instant::now();
    while join.welcome().is_none() {
        host.poll();
        join.poll();
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(2));
    }

    // READY, then LEAVE, back to back, before the host ever sees can_start.
    join.set_ready();
    join.poll(); // flush the READY onto the wire
    host.poll(); // host may or may not have processed READY yet — either is fine
    join.leave(); // consumes `join`, sends LEAVE immediately

    let start = Instant::now();
    while host.joiner_name().is_some() {
        host.poll();
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "host never noticed the joiner leaving"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        !host.can_start(),
        "can_start() must be false once the (possibly-ready) joiner is gone — a stale \
         READY must not survive the joiner's own LEAVE"
    );
    assert!(
        !host.joiner_ready(),
        "joiner_ready() must also have cleared"
    );
}

// ---------------------------------------------------------------------------
// (4d) Joiner attempting JOIN twice: idempotent seat assignment, no
// double-seat.
// ---------------------------------------------------------------------------

#[test]
fn joining_twice_from_the_same_address_is_idempotent() {
    let mut host = HostLobby::create("HOSTY", settings("m.ini"), &ephemeral_discovery()).unwrap();
    let host_addr: SocketAddr = format!("127.0.0.1:{}", host.port()).parse().unwrap();

    let probe = loopback_socket();
    let join = Datagram::Join {
        game_version: GAME_VERSION,
        name: "DUPJOIN".to_string(),
    };
    let bytes = wire::encode(&join);

    let recv_welcome = |host: &mut HostLobby, probe: &UdpSocket| -> Datagram {
        let start = Instant::now();
        let mut buf = [0u8; 2048];
        loop {
            host.poll();
            match probe.recv_from(&mut buf) {
                Ok((n, _)) => return wire::decode(&buf[..n]).expect("host reply must decode"),
                Err(_) => {
                    assert!(
                        start.elapsed() < Duration::from_secs(WALL_SECS),
                        "no reply from host"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    };

    // First JOIN.
    probe.send_to(&bytes, host_addr).unwrap();
    let w1 = recv_welcome(&mut host, &probe);
    let (seat1, host_seat1) = match w1 {
        Datagram::Welcome {
            seat, host_seat, ..
        } => (seat, host_seat),
        other => panic!("expected WELCOME, got {other:?}"),
    };
    assert_eq!(host.joiner_name(), Some("DUPJOIN"));

    // Second JOIN, same source address, same name — a re-send (e.g. the
    // first WELCOME was lost from the joiner's point of view, so it retried).
    probe.send_to(&bytes, host_addr).unwrap();
    let w2 = recv_welcome(&mut host, &probe);
    let (seat2, host_seat2) = match w2 {
        Datagram::Welcome {
            seat, host_seat, ..
        } => (seat, host_seat),
        other => panic!("expected a second WELCOME (re-welcome), got {other:?}"),
    };

    assert_eq!(
        seat1, seat2,
        "re-JOIN must be re-welcomed with the SAME seat, not a new one"
    );
    assert_eq!(host_seat1, host_seat2);
    assert_eq!(
        host.joiner_name(),
        Some("DUPJOIN"),
        "still exactly one joiner tracked, not two"
    );

    // A THIRD, genuinely different address attempting to join while the
    // (idempotently re-joined) seat is occupied must be rejected as full —
    // proving the double-JOIN did not somehow free up a phantom second seat.
    let probe2 = loopback_socket();
    let join2 = Datagram::Join {
        game_version: GAME_VERSION,
        name: "INTRUDER".to_string(),
    };
    probe2.send_to(&wire::encode(&join2), host_addr).unwrap();
    let start = Instant::now();
    let mut buf = [0u8; 2048];
    let reply = loop {
        host.poll();
        match probe2.recv_from(&mut buf) {
            Ok((n, _)) => break wire::decode(&buf[..n]).unwrap(),
            Err(_) => {
                assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(
        reply,
        Datagram::Reject {
            reason: ra_net::wire::RejectReason::SessionFull
        },
        "a genuinely different joiner must be rejected — no phantom second seat"
    );
    assert_eq!(
        host.joiner_name(),
        Some("DUPJOIN"),
        "original joiner still seated, unaffected"
    );
}

// ---------------------------------------------------------------------------
// (4e) Truncated/garbage ANNOUNCE from a foreign broadcaster: ignored, the
// browser survives and keeps working.
// ---------------------------------------------------------------------------

#[test]
fn garbage_on_the_discovery_port_is_ignored_and_the_browser_keeps_working() {
    let mut browser = SessionBrowser::bind(&ephemeral_discovery()).unwrap();
    let browser_addr: SocketAddr = format!("127.0.0.1:{}", browser.port()).parse().unwrap();

    // A foreign broadcaster (not our protocol at all — some other LAN
    // service, or a malformed/truncated copy of our own datagram) hammers
    // the discovery port.
    let stranger = loopback_socket();
    let real_announce = wire::encode(&Datagram::Announce {
        game_version: GAME_VERSION,
        game_port: 4242,
        name: "REAL".to_string(),
        map: "real.ini".to_string(),
    });
    let garbage_payloads: Vec<Vec<u8>> = vec![
        vec![], // empty
        vec![0u8; 1],
        vec![0xFFu8; 3],
        b"NOT-EVEN-CLOSE-TO-A-DATAGRAM-FORMAT".to_vec(),
        real_announce[..real_announce.len() / 2].to_vec(), // truncated real one
        real_announce[..real_announce.len() - 1].to_vec(), // truncated by 1 byte
    ];
    for payload in &garbage_payloads {
        stranger.send_to(payload, browser_addr).unwrap();
    }

    // Must not panic and must not fabricate a session out of noise.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(150) {
        browser.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        browser.sessions().is_empty(),
        "garbage must never manifest as a discovered session: {:?}",
        browser.sessions()
    );

    // The browser must still work afterward: a real host's announcement is
    // picked up normally.
    let cfg = DiscoveryConfig {
        announce_targets: vec![browser_addr],
        listen_port: 0,
    };
    let mut host = HostLobby::create("REAL", settings("real.ini"), &cfg).unwrap();
    let start = Instant::now();
    loop {
        host.poll();
        browser.poll();
        if !browser.sessions().is_empty() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "browser never recovered to see a real host after absorbing garbage"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(browser.sessions().len(), 1);
    assert_eq!(browser.sessions()[0].name, "REAL");
    assert!(browser.sessions()[0].compatible);

    // A same-protocol-but-different-version foreign host also should not
    // crash the browser; it is explicitly listed as incompatible (existing
    // documented behavior) rather than silently dropped or panicking.
    let mismatched = wire::encode_with_protocol(
        &Datagram::Announce {
            game_version: GAME_VERSION,
            game_port: 1,
            name: "X".to_string(),
            map: "X".to_string(),
        },
        wire::PROTOCOL_VERSION + 1,
    );
    stranger.send_to(&mismatched, browser_addr).unwrap();
    let start = Instant::now();
    while browser.sessions().len() < 2 {
        browser.poll();
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
        std::thread::sleep(Duration::from_millis(5));
    }
    let incompatible = browser.sessions().iter().find(|s| !s.compatible);
    assert!(
        incompatible.is_some(),
        "protocol-mismatched foreign host must surface as incompatible, not vanish"
    );
}
