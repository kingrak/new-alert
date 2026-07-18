//! The one module allowed to hold platform-specific code (DESIGN.md §4.7). CI
//! greps for `cfg(target_os/windows/unix)` everywhere *except* here.
//!
//! For M2 the only platform concern is locating the asset directory, and the
//! search order needed so far (explicit flag → env var → `./assets`) is
//! OS-neutral, so there is no conditional code yet. The per-OS data/config
//! directories from §4.7 (`%APPDATA%`, `~/Library/Application Support`,
//! `~/.local/share`) will be added here — and only here — when saves/config
//! land.

use std::path::PathBuf;

/// Environment variable naming the asset directory.
pub const ASSETS_ENV: &str = "RA_ASSETS_DIR";

/// Resolve the asset directory: an explicit path wins, then `RA_ASSETS_DIR`,
/// then `./assets`. Returns the first candidate that exists.
pub fn resolve_assets_dir(explicit: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(env) = std::env::var(ASSETS_ENV) {
        candidates.push(PathBuf::from(env));
    }
    candidates.push(PathBuf::from("assets"));

    candidates.into_iter().find(|p| p.is_dir())
}
