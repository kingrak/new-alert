//! The one module allowed to hold platform-specific code (DESIGN.md §4.7). CI
//! greps for `cfg(target_os/windows/unix)` everywhere *except* here.
//!
//! For M2 the only platform concern was locating the asset directory. M7.8 adds
//! the per-OS *data* directory from §4.7 (`%APPDATA%`, `~/Library/Application
//! Support`, `~/.local/share`) for the user maps folder — the only place the CI
//! `cfg(target_os = …)` grep is allowed to match.

use std::path::PathBuf;

/// Environment variable naming the asset directory.
pub const ASSETS_ENV: &str = "RA_ASSETS_DIR";

/// Application data-directory name (per-OS parent joins this).
const APP_DIR: &str = "new-alert";

/// The per-OS user data directory for this app (`…/new-alert`), per DESIGN §4.7:
/// Windows `%APPDATA%/new-alert`, macOS `~/Library/Application Support/new-alert`,
/// Linux/other `$XDG_DATA_HOME` or `~/.local/share/new-alert`. `RA_DATA_DIR`
/// overrides everything (used by tests to point at a scratch dir). Returns `None`
/// only if no home/appdata can be determined at all.
pub fn data_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RA_DATA_DIR") {
        return Some(PathBuf::from(p));
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join(APP_DIR))
            .or_else(|| dirs_home().map(|h| h.join("AppData").join("Roaming").join(APP_DIR)))
    }
    #[cfg(target_os = "macos")]
    {
        dirs_home().map(|h| h.join("Library").join("Application Support").join(APP_DIR))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
            if !x.is_empty() {
                return Some(PathBuf::from(x).join(APP_DIR));
            }
        }
        dirs_home().map(|h| h.join(".local").join("share").join(APP_DIR))
    }
}

/// The user maps folder (`<data_dir>/maps`), created on first access. Returns the
/// path even if creation fails (callers treat a missing dir as "no user maps").
pub fn user_maps_dir() -> Option<PathBuf> {
    let dir = data_dir()?.join("maps");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// The user's home directory, from `HOME` (Unix) or `USERPROFILE` (Windows).
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()))
        .map(PathBuf::from)
}

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
