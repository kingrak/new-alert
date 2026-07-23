//! Canonical command-log writer (SERVER-DESIGN.md §8): per Running game the
//! server appends every broadcast `TICK_BUNDLE` and the winning `TICK_HASH`
//! chain to `games/<game_id>.rar1`, finalising on Closed.
//!
//! The on-disk format **is** the `ra-net` replay encoding (a versioned header
//! then length-prefixed records) — the same [`ra_net::ReplayReader`] the client
//! uses decodes it, so a server log is a replayable `CommandTransport` source
//! (playback is the M9-B `ReplayTransport` feature; M9-A just writes it
//! correctly and proves it decodes). Because the server holds each command as an
//! opaque blob, tick records go through [`ra_net::encode_tick_blobs`], which
//! writes the identical bytes [`ra_net::encode_tick`] would.
//!
//! **Failure discipline** mirrors the client recorder: any I/O fallibility
//! degrades to *not recording* (one stderr line, then a silent no-op). Recording
//! must never take a game down.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use ra_net::{
    encode_end, encode_hash, encode_header, encode_tick_blobs, EndReason, ReplayHeader, SeatId,
    HASH_INTERVAL,
};

/// Write-behind buffer capacity (§9 "replay write-behind buffer 1 MiB").
const WRITE_BEHIND: usize = 1024 * 1024;

/// Appends one game's canonical bundle + hash-chain stream to a `.rar1` file.
#[derive(Debug)]
pub struct ServerReplay {
    /// The buffered sink, or `None` once disabled (open failed or a write
    /// failed). Every method short-circuits when `None`.
    sink: Option<BufWriter<File>>,
    /// The output path (for the one diagnostic line on failure).
    path: PathBuf,
    /// Latched once the end record is written, so a double finalize is a no-op.
    finished: bool,
}

impl ServerReplay {
    /// Open `path` and write the header. Any failure returns a **disabled**
    /// writer (never an error): the game runs, log or no log.
    pub fn create(path: PathBuf, header: &ReplayHeader) -> ServerReplay {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "replay: game log disabled — cannot create {}: {e}",
                    parent.display()
                );
                return ServerReplay {
                    sink: None,
                    path,
                    finished: false,
                };
            }
        }
        match File::create(&path) {
            Ok(f) => {
                let mut w = BufWriter::with_capacity(WRITE_BEHIND, f);
                if let Err(e) = w.write_all(&encode_header(header)) {
                    eprintln!(
                        "replay: game log disabled — header write failed on {}: {e}",
                        path.display()
                    );
                    return ServerReplay {
                        sink: None,
                        path,
                        finished: false,
                    };
                }
                ServerReplay {
                    sink: Some(w),
                    path,
                    finished: false,
                }
            }
            Err(e) => {
                eprintln!(
                    "replay: game log disabled — cannot create {}: {e}",
                    path.display()
                );
                ServerReplay {
                    sink: None,
                    path,
                    finished: false,
                }
            }
        }
    }

    /// Whether the log is live.
    pub fn is_recording(&self) -> bool {
        self.sink.is_some() && !self.finished
    }

    fn write(&mut self, bytes: &[u8], what: &str) {
        if let Some(w) = self.sink.as_mut() {
            if let Err(e) = w.write_all(bytes) {
                eprintln!(
                    "replay: game log stopped — {what} write failed on {}: {e}",
                    self.path.display()
                );
                self.sink = None;
            }
        }
    }

    /// Record a broadcast bundle — **only if it carried commands** (empty ticks
    /// are omitted, matching the format and the client recorder).
    pub fn on_bundle(&mut self, tick: u32, seats: &[(SeatId, Vec<Vec<u8>>)]) {
        if self.finished {
            return;
        }
        let any = seats.iter().any(|(_, blobs)| !blobs.is_empty());
        if !any {
            return;
        }
        let bytes = encode_tick_blobs(tick, seats);
        self.write(&bytes, "bundle");
    }

    /// Record the winning arbitrated hash on the [`HASH_INTERVAL`] cadence (the
    /// canonical hash chain, §8 — matching the client recorder's cadence so a
    /// server log and a client log carry comparable chains).
    pub fn on_winning_hash(&mut self, tick: u32, hash: u64) {
        if self.finished || !tick.is_multiple_of(HASH_INTERVAL) {
            return;
        }
        let bytes = encode_hash(tick, hash);
        self.write(&bytes, "hash");
    }

    /// Write the terminating end record and flush (§8 "finalize on Closed").
    /// Idempotent.
    pub fn finalize(&mut self, reason: EndReason, final_tick: u32) {
        if self.finished {
            return;
        }
        let bytes = encode_end(reason, final_tick);
        self.write(&bytes, "end");
        if let Some(w) = self.sink.as_mut() {
            let _ = w.flush();
        }
        self.finished = true;
    }
}

impl Drop for ServerReplay {
    fn drop(&mut self) {
        if !self.finished {
            self.finalize(EndReason::Quit, 0);
        }
    }
}
