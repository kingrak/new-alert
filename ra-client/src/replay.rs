//! M7.23 P1: always-on replay recording, client side.
//!
//! [`ReplayRecorder`] owns the output file and turns the tap points in
//! [`crate::appcore::AppCore::step_tick`] — the exact `(tick, TickBundle)` the
//! sim consumes and the post-tick state hash — into the `ra-net` replay stream
//! (format & encoding live in [`ra_net::replay`]; this layer only does I/O and
//! the wall-clock timestamp, both forbidden below `ra-client` by §4.2/§4.7).
//!
//! **Failure discipline (the load-bearing rule).** Recording must NEVER break
//! the game. Every I/O fallibility — the directory can't be created, the file
//! can't be opened, a write fails mid-game (disk full, unplugged drive) —
//! degrades to *not recording*: the recorder latches disabled, logs one line to
//! stderr, and every subsequent call is a no-op. The sim is untouched either
//! way (nothing here feeds back into the world).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use ra_net::replay::{
    encode_end, encode_hash, encode_header, encode_tick, EndReason, ReplayHeader,
};
use ra_net::{TickBundle, HASH_INTERVAL};

/// Records one interactive game's command + hash stream to a `.rarp` file.
pub struct ReplayRecorder {
    /// The open file, or `None` once disabled (never opened, failed, or
    /// finished). All write methods short-circuit when this is `None`.
    file: Option<File>,
    /// The path, for the one diagnostic line on failure.
    path: PathBuf,
    /// Latched once the terminating end record is written, so a second
    /// `finish` (e.g. game-over transition *and* window close) is a no-op.
    finished: bool,
}

impl ReplayRecorder {
    /// Create a recorder writing to `path`, first ensuring the parent directory
    /// exists and writing the header. Any failure returns a **disabled**
    /// recorder (not an error): the caller installs it unconditionally and the
    /// game plays on, recording or not.
    pub fn create(path: PathBuf, header: &ReplayHeader) -> ReplayRecorder {
        let mut rec = ReplayRecorder {
            file: None,
            path: path.clone(),
            finished: false,
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "replay: recording disabled — cannot create {}: {e}",
                    parent.display()
                );
                return rec;
            }
        }
        match File::create(&path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&encode_header(header)) {
                    eprintln!(
                        "replay: recording disabled — write failed on {}: {e}",
                        path.display()
                    );
                    return rec;
                }
                rec.file = Some(f);
            }
            Err(e) => {
                eprintln!(
                    "replay: recording disabled — cannot create {}: {e}",
                    path.display()
                );
            }
        }
        rec
    }

    /// Whether recording is live (open file, not yet finished). Observability
    /// for tests and the shell's status line.
    pub fn is_recording(&self) -> bool {
        self.file.is_some() && !self.finished
    }

    /// The output path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `bytes`, disabling the recorder (with one log line) on any error.
    fn write(&mut self, bytes: &[u8], what: &str) {
        if let Some(f) = self.file.as_mut() {
            if let Err(e) = f.write_all(bytes) {
                eprintln!(
                    "replay: recording stopped — {what} write failed on {}: {e}",
                    self.path.display()
                );
                self.file = None;
            }
        }
    }

    /// Record a tick's command bundle — **only if it carried commands** (empty
    /// ticks are omitted from the stream, per the format).
    pub fn on_tick(&mut self, tick: u32, bundle: &TickBundle) {
        if self.finished || bundle.command_count() == 0 {
            return;
        }
        let bytes = encode_tick(tick, bundle);
        self.write(&bytes, "tick");
    }

    /// Record the post-tick state hash on the [`HASH_INTERVAL`] cadence.
    pub fn on_hash(&mut self, tick: u32, hash: u64) {
        if self.finished || !tick.is_multiple_of(HASH_INTERVAL) {
            return;
        }
        let bytes = encode_hash(tick, hash);
        self.write(&bytes, "hash");
    }

    /// Write the terminating end record and flush. Idempotent — the first call
    /// wins; later calls are no-ops.
    pub fn finish(&mut self, reason: EndReason, final_tick: u32) {
        if self.finished {
            return;
        }
        let bytes = encode_end(reason, final_tick);
        self.write(&bytes, "end");
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
        self.finished = true;
    }
}

impl Drop for ReplayRecorder {
    fn drop(&mut self) {
        // A recorder dropped without an explicit `finish` (the game object went
        // away — a process exit path we did not route through the shell's quit)
        // still terminates its stream, so the file is never left header-only.
        if !self.finished {
            // final_tick is unknown here; 0 is a benign sentinel and the tick
            // records still carry the true progression.
            self.finish(EndReason::Quit, 0);
        }
    }
}
