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
    /// The output sink (an open [`File`] in production), or `None` once disabled
    /// (never opened, failed, or finished). Boxed behind `dyn Write` so a test
    /// can substitute a fault-injecting sink at the exact `io::Write` seam and
    /// prove the mid-stream write-failure discipline (one log line, then silent)
    /// without touching a real disk. All write methods short-circuit when `None`.
    sink: Option<Box<dyn Write>>,
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
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "replay: recording disabled — cannot create {}: {e}",
                    parent.display()
                );
                return ReplayRecorder {
                    sink: None,
                    path,
                    finished: false,
                };
            }
        }
        match File::create(&path) {
            Ok(f) => Self::from_writer(path, Box::new(f), header),
            Err(e) => {
                eprintln!(
                    "replay: recording disabled — cannot create {}: {e}",
                    path.display()
                );
                ReplayRecorder {
                    sink: None,
                    path,
                    finished: false,
                }
            }
        }
    }

    /// Build a recorder over an already-opened `io::Write` sink, writing the
    /// header immediately. Shared by [`ReplayRecorder::create`] (the sink is a
    /// [`File`]) and by tests, which pass a fault-injecting sink at this exact
    /// seam to exercise the mid-stream write-failure path. A header write that
    /// fails disables the recorder (one log line, then silent) — identical
    /// discipline to a create failure.
    pub fn from_writer(
        path: PathBuf,
        mut sink: Box<dyn Write>,
        header: &ReplayHeader,
    ) -> ReplayRecorder {
        if let Err(e) = sink.write_all(&encode_header(header)) {
            eprintln!(
                "replay: recording disabled — write failed on {}: {e}",
                path.display()
            );
            return ReplayRecorder {
                sink: None,
                path,
                finished: false,
            };
        }
        ReplayRecorder {
            sink: Some(sink),
            path,
            finished: false,
        }
    }

    /// Whether recording is live (open file, not yet finished). Observability
    /// for tests and the shell's status line.
    pub fn is_recording(&self) -> bool {
        self.sink.is_some() && !self.finished
    }

    /// The output path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `bytes`, disabling the recorder (with one log line) on any error.
    fn write(&mut self, bytes: &[u8], what: &str) {
        if let Some(f) = self.sink.as_mut() {
            if let Err(e) = f.write_all(bytes) {
                eprintln!(
                    "replay: recording stopped — {what} write failed on {}: {e}",
                    self.path.display()
                );
                self.sink = None;
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
        if let Some(f) = self.sink.as_mut() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ra_net::replay::{ReplaySeat, REPLAY_VERSION};
    use ra_net::SeatId;
    use ra_sim::{Command, ProdKind};
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    /// A write sink that lets the *first* write (the header) through, capturing
    /// its bytes, then fails every subsequent write with an injected error — the
    /// disk-full / drive-unplugged mid-game scenario, isolated to the `io::Write`
    /// seam.
    struct FailAfterHeader {
        wrote_header: bool,
        captured: Rc<RefCell<Vec<u8>>>,
    }

    impl Write for FailAfterHeader {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.wrote_header {
                Err(io::Error::other("injected mid-stream write failure"))
            } else {
                self.wrote_header = true;
                self.captured.borrow_mut().extend_from_slice(buf);
                Ok(buf.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn header() -> ReplayHeader {
        ReplayHeader {
            replay_version: REPLAY_VERSION,
            game_version: 0x0000_0100,
            protocol_version: 2,
            scenario: "scm01ea.ini".to_string(),
            seed: 1,
            difficulty: 1,
            credits: 8000,
            catalog_hash: 0,
            start_millis: 0,
            seats: vec![ReplaySeat {
                seat: 1,
                house: 1,
                color: 3,
            }],
        }
    }

    fn cmd_bundle(tick: u32) -> TickBundle {
        TickBundle {
            tick,
            seats: vec![(
                1 as SeatId,
                vec![Command::CancelProduction {
                    house: 1,
                    kind: ProdKind::Building,
                }],
            )],
        }
    }

    /// Pre-flight (M9-A): a write that fails **after** a successful create must
    /// degrade to "one log line, then silent" — the recorder was live (header
    /// written), the first mid-stream record write disables it, and every later
    /// call is a no-op that never panics and never writes another byte. Proven at
    /// the `io::Write` seam with a fault-injecting sink (no real disk).
    #[test]
    fn mid_stream_write_failure_disables_recorder_after_header() {
        let captured = Rc::new(RefCell::new(Vec::new()));
        let sink = Box::new(FailAfterHeader {
            wrote_header: false,
            captured: Rc::clone(&captured),
        });
        let h = header();
        let mut rec = ReplayRecorder::from_writer("mem://replay.rarp".into(), sink, &h);

        // Header write succeeded → recording is live, and exactly the header
        // bytes made it to the sink.
        assert!(
            rec.is_recording(),
            "header write should have enabled recording"
        );
        assert_eq!(
            *captured.borrow(),
            encode_header(&h),
            "only the header should have been written so far"
        );

        // First mid-stream record write fails → recorder disables itself.
        rec.on_tick(1, &cmd_bundle(1));
        assert!(
            !rec.is_recording(),
            "a failed mid-stream write must disable the recorder"
        );

        // Every subsequent call is a silent no-op (no panic, no further bytes).
        rec.on_tick(2, &cmd_bundle(2));
        rec.on_hash(15, 0xDEAD_BEEF);
        rec.finish(EndReason::Quit, 2);
        assert_eq!(
            *captured.borrow(),
            encode_header(&h),
            "nothing beyond the header may reach the sink after the failure"
        );
    }
}
