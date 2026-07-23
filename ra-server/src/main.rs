//! `ra-server` binary: arg parsing, socket bind, and the single-threaded poll
//! loop (SERVER-DESIGN.md §9). All game logic lives in the library
//! [`ra_server::Server`]; this file is the platform quarantine — the only place
//! that touches a real socket or the wall clock.
//!
//! ```text
//! loop {
//!     recv_all_pending();   // nonblocking drain, per-datagram validate→dispatch
//!     advance_time(now);    // bundle deadlines, keepalives, timeouts
//!     flush_outgoing();     // batched sends
//!     sleep_until_next();   // one short timer
//! }
//! ```

use std::io;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ra_server::{Server, ServerConfig, DEFAULT_PORT};

fn main() -> io::Result<()> {
    let mut port = DEFAULT_PORT;
    let mut replay_dir: Option<PathBuf> = Some(PathBuf::from("games"));
    let mut max_sessions = 256usize;

    // Minimal `--flag value` parsing (std-only; no clap).
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                if let Some(v) = args.next() {
                    port = v.parse().unwrap_or(DEFAULT_PORT);
                }
            }
            "--replay-dir" => {
                replay_dir = args.next().map(PathBuf::from);
            }
            "--no-replay" => replay_dir = None,
            "--max-sessions" => {
                if let Some(v) = args.next() {
                    max_sessions = v.parse().unwrap_or(256);
                }
            }
            "--help" | "-h" => {
                eprintln!("ra-server [--port N] [--replay-dir DIR|--no-replay] [--max-sessions N]");
                return Ok(());
            }
            other => eprintln!("ra-server: ignoring unknown arg {other:?}"),
        }
    }

    let sock = UdpSocket::bind(("0.0.0.0", port))?;
    sock.set_nonblocking(true)?;
    let bound = sock.local_addr()?;
    eprintln!("ra-server listening on {bound} (UDP), replay_dir={replay_dir:?}");

    let start_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let config = ServerConfig {
        max_sessions,
        input_delay: ra_server::RELAY_INPUT_DELAY,
        replay_dir,
        start_millis,
        rng_seed: start_millis.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ (bound.port() as u64),
        max_dgrams_per_sec: ra_server::MAX_DGRAMS_PER_SEC,
    };
    let mut server = Server::new(config);

    let mut buf = [0u8; 65536];
    loop {
        // Drain all pending datagrams (nonblocking).
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, src)) => server.recv(src, &buf[..n], Instant::now()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        server.advance_time(Instant::now());

        for (addr, bytes) in server.take_outgoing() {
            let _ = sock.send_to(&bytes, addr);
        }

        // One short timer: the min bundle-deadline / keepalive granularity. A
        // busy server is kept hot by inbound traffic; an idle one wakes here.
        std::thread::sleep(Duration::from_millis(5));
    }
}
