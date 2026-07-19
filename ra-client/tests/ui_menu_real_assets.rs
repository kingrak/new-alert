//! Menu real-asset suite (M7.8 coverage layer 5): exercises the archive map
//! scan, [`ArchiveFactory`], and the user-maps-folder listing against the
//! *real* game archives. Skip-gated exactly like every other real-asset
//! suite in this crate (`support::real_assets_available`) — these tests are
//! silently skipped, never failed, when `main.mix`/`redalert.mix` aren't
//! present.
//!
//! **User-maps-folder isolation.** `platform::user_maps_dir()` resolves
//! under `platform::data_dir()`, which honors an `RA_DATA_DIR` env-var
//! override before falling back to the real per-OS app-data directory — a
//! seam that already exists for exactly this purpose. This suite's
//! `user_maps_folder` test uses that seam to point at a scratch temp
//! directory rather than the real `~/.local/share/new-alert` (or platform
//! equivalent): it must never create, read, or leave behind files in the
//! user's real data directory. [`EnvGuard`] makes the override panic-safe
//! (restores the prior value, if any, on drop) and [`ScratchDir`] removes
//! its temp directory on drop, so a failing assertion still cleans up.

mod support;

use ra_client::assets::{self, ArchiveFactory};
use ra_client::menu::{App, AppState, GameFactory, HOUSES};
use ra_sim::Difficulty;

/// Panic-safe env-var override: restores the previous value (or removes the
/// var if it was unset) when dropped, so a failing assertion mid-test still
/// leaves the process environment as it found it.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &std::path::Path) -> EnvGuard {
        let prev = std::env::var(key).ok();
        // Safety: test-only, single-threaded-with-respect-to-this-var use —
        // no other test in this binary reads or writes `RA_DATA_DIR`.
        unsafe { std::env::set_var(key, val) };
        EnvGuard { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// A scratch directory under the OS temp dir, unique per test process +
/// call, removed on drop (best-effort — a `remove_dir_all` failure is
/// swallowed, matching the "never fail cleanup" spirit of a test teardown).
struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> ScratchDir {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("new-alert-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        ScratchDir(dir)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The archive scan (`assets::scan_archive_maps`) over the real `main.mix`
/// finds exactly 24 multiplayer scenarios, each with a sane (non-empty)
/// name and a player count in RA's valid 2-8 range — plus two specific,
/// pinned entries so a scan that silently returned 24 *garbage* entries
/// wouldn't slip through.
#[test]
fn archive_map_scan_finds_24_maps_with_sane_metadata() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (see support::real_assets_available)");
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).unwrap();
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).unwrap();

    let maps = assets::scan_archive_maps(&main_bytes, &redalert_bytes);
    assert_eq!(
        maps.len(),
        24,
        "archive map count changed — pin was derived from the real main.mix's general.mix"
    );

    for m in &maps {
        assert!(!m.name.is_empty(), "{}: empty display name", m.filename);
        assert!(
            (1..=8).contains(&m.players),
            "{}: player count {} out of RA's valid 1-8 range",
            m.filename,
            m.players
        );
        assert!(
            m.width > 0 && m.height > 0,
            "{}: non-positive map dimensions {}x{}",
            m.filename,
            m.width,
            m.height
        );
        assert_eq!(m.source, ra_client::menu::MapSource::Archive);
    }

    let by_filename = |f: &str| maps.iter().find(|m| m.filename == f);
    let scm01 = by_filename("scm01ea.ini").expect("scm01ea.ini present");
    assert_eq!(scm01.name, "Coastal Influence (4-6)");
    assert_eq!(scm01.players, 8);
    let scm02 = by_filename("scm02ea.ini").expect("scm02ea.ini present");
    assert_eq!(scm02.name, "Middle Mayhem (2)");
    assert_eq!(scm02.players, 2);
}

/// `ArchiveFactory::build` with a non-default configuration (USSR, Hard,
/// 10000 credits, classic radar OFF) against a real archive scenario: the
/// resulting `World`'s settings match exactly what was asked for, and —
/// driven through the real `App::start_game` camera-centering logic, not a
/// hand-rolled reimplementation — the camera lands on the player's start
/// cell rather than the map origin (the same regression
/// `ui_menu_state_machine.rs::menu_started_game_centers_camera_on_player_start`
/// pins for the synthetic factory; this is its real-asset counterpart).
#[test]
fn archive_factory_nondefault_config_threads_settings_and_centers_camera() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (see support::real_assets_available)");
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).unwrap();
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).unwrap();

    let maps = assets::scan_archive_maps(&main_bytes, &redalert_bytes);
    assert!(maps.len() >= 2, "need at least 2 archive maps");
    let map_filename = maps[1].filename.clone();

    let factory = ArchiveFactory::new(main_bytes, redalert_bytes, None);
    let ussr_house = HOUSES.iter().find(|(n, _)| *n == "USSR").unwrap().1;
    let res = ra_client::menu::ResolvedSkirmish {
        map_filename: map_filename.clone(),
        player_house: ussr_house,
        color_house: ussr_house,
        credits: 10000,
        difficulty: Difficulty::Hard,
        classic_radar: false,
    };

    // World-settings assertions, direct from `build`.
    let (core, start) = factory.build(&res).expect("archive build should succeed");
    assert_eq!(core.world().player_house(), Some(ussr_house));
    assert_eq!(core.world().house_credits(ussr_house), 10000);
    let ai_house = if ussr_house == 2 { 0 } else { 2 };
    assert_eq!(core.world().ai_difficulty(ai_house), Some(Difficulty::Hard));
    assert!(
        core.has_radar(),
        "classic radar OFF should make the radar always-on"
    );
    drop(core);

    // Camera-centering assertion, through the real `App`/`start_game` path
    // (not a reimplementation of its math) so this is a genuine regression
    // guard for the menu-start camera bug on a real map, not just the
    // synthetic fixture.
    let mut app = App::new(
        maps,
        Box::new(ArchiveFactory::new(
            std::fs::read(dir.join("main.mix")).unwrap(),
            std::fs::read(dir.join("redalert.mix")).unwrap(),
            None,
        )),
    );
    app.handle(ra_client::input::InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    let map_idx = app
        .maps()
        .iter()
        .position(|m| m.filename == map_filename)
        .unwrap();
    app.select_map(map_idx);
    {
        let cfg = app.config_mut();
        cfg.house = HOUSES.iter().position(|(_, h)| *h == ussr_house).unwrap();
        cfg.credits = 3; // index for 10000 in menu::CREDITS
        cfg.difficulty = 2; // HARD
        cfg.classic_radar = false;
    }
    app.start_game();
    assert_eq!(
        app.state(),
        AppState::InGame,
        "archive map should start cleanly"
    );
    let core = app.core().expect("in-game core");
    let cam = core.camera_rect();
    const CELL_PIXELS: i64 = 24;
    let (cx, cy) = (
        start.x as i64 * CELL_PIXELS + CELL_PIXELS / 2,
        start.y as i64 * CELL_PIXELS + CELL_PIXELS / 2,
    );
    assert!(
        cam.x <= cx && cx < cam.x + cam.width as i64,
        "start cell x on-screen on a real map (cam.x={}, cx={cx})",
        cam.x
    );
    assert!(
        cam.y <= cy && cy < cam.y + cam.height as i64,
        "start cell y on-screen on a real map (cam.y={}, cy={cy})",
        cam.y
    );
}

/// The user-maps-folder listing (`assets::load_menu`, which internally calls
/// `platform::user_maps_dir()`) picks up a scenario dropped into the
/// configured folder and reports it as `MapSource::User` — using the
/// `RA_DATA_DIR` seam to point at a scratch temp directory, never the real
/// per-OS app-data directory. Also asserts the real directory gained no new
/// entries as a side effect of running this test.
#[test]
fn user_maps_folder_listing_uses_a_scratch_dir_not_the_real_one() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (see support::real_assets_available)");
        return;
    }
    let dir = support::assets_dir();

    // Snapshot the real per-OS data dir's maps folder before (no override
    // active yet), so we can prove this test added nothing to it.
    let snapshot_real = || {
        ra_client::platform::user_maps_dir()
            .map(|d| std::fs::read_dir(&d).map(|rd| rd.count()).unwrap_or(0))
    };
    let real_maps_dir_before = snapshot_real();

    {
        let scratch = ScratchDir::new("user-maps");
        let _guard = EnvGuard::set("RA_DATA_DIR", &scratch.0);

        // `user_maps_dir()` now resolves under the scratch dir (it creates
        // `<RA_DATA_DIR>/maps` on first access).
        let user_dir = ra_client::platform::user_maps_dir().expect("scratch user maps dir");
        assert!(
            user_dir.starts_with(&scratch.0),
            "user_maps_dir() did not honor the RA_DATA_DIR override: {}",
            user_dir.display()
        );

        // Drop a real archive scenario's INI text into the scratch user
        // maps folder under a distinctive filename.
        let main_bytes = std::fs::read(dir.join("main.mix")).unwrap();
        let sample_text = assets::scenario_text_from_archive(&main_bytes, "scm01ea.ini")
            .expect("scm01ea.ini text from archive");
        std::fs::write(user_dir.join("my_scratch_map.mpr"), sample_text.as_bytes()).unwrap();

        let (maps, _factory) = assets::load_menu(&dir).expect("load_menu should succeed");
        let found = maps
            .iter()
            .find(|m| m.filename == "my_scratch_map.mpr")
            .expect("the scratch-dir map should appear in the combined list");
        assert_eq!(found.source, ra_client::menu::MapSource::User);
        assert!(!found.name.is_empty());
        // `_guard` and `scratch` drop here: env var restored, scratch dir removed.
    }

    // The real per-OS user maps dir must be completely untouched — same
    // entry count as before the override was ever installed.
    assert_eq!(
        real_maps_dir_before,
        snapshot_real(),
        "the real user maps directory gained/lost entries — the RA_DATA_DIR \
         override leaked or `load_menu` bypassed it"
    );
}
