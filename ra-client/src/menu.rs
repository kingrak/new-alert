//! M7.8 pre-game state machine (`App`) — the windowless outer shell that wraps
//! the in-game [`AppCore`] with a main menu, a skirmish-setup screen, and a
//! pause / game-over flow (DESIGN.md §4.8: everything reachable through
//! `handle`/`update`/`compose`, no window required).
//!
//! ```text
//!   MainMenu ──Skirmish──▶ SkirmishSetup ──Start──▶ InGame ──Esc──▶ Paused
//!      ▲                        │  ▲                   │  │            │
//!      └────Quit-to-menu────────┘  └──Back────────────┘  │        Resume│
//!      ▲                                                  ▼              │
//!      └───────────Continue──────────── GameOver ◀──Victory/Defeat──────┘
//! ```
//!
//! The macroquad shell stays a dumb adapter: it only forwards real input as
//! [`InputEvent`]s, calls [`App::update`] with the frame time, and uploads
//! [`App::compose`]. All state transitions happen inside this module, so a
//! headless test can drive the whole flow.
//!
//! **The menu states are pre-`World`.** They never touch [`AppCore::compose_game`]
//! or the sim, so enabling them cannot move any in-game golden — the game surface
//! is byte-identical whether or not the menu wraps it.

use ra_sim::{CellCoord, Difficulty};

use crate::appcore::{AppCore, Frame, SoundEvent};
use crate::compositor::RgbaImage;
use crate::font;
use crate::input::{InputEvent, Key, MouseButton};
use crate::unit_render::{draw_rect_outline, fill_rect};

/// Difficulty options offered on the setup screen (label + sim value).
pub const DIFFICULTIES: [(&str, Difficulty); 3] = [
    ("EASY", Difficulty::Easy),
    ("NORMAL", Difficulty::Normal),
    ("HARD", Difficulty::Hard),
];

/// Player house options (label + house index). Allies countries + USSR — the
/// minimum the brief asks for plus the rest of the eight for completeness.
pub const HOUSES: [(&str, u8); 8] = [
    ("GREECE", 1),
    ("USSR", 2),
    ("ENGLAND", 3),
    ("UKRAINE", 4),
    ("GERMANY", 5),
    ("FRANCE", 6),
    ("TURKEY", 7),
    ("SPAIN", 0),
];

/// Player colour options (label + `PlayerColorType` / `PALETTE.CPS` row that
/// paints it). These are the original's real eight schemes in enum order
/// (`DEFINES.H:1226-1235`: GOLD=0, LTBLUE=1, RED=2, GREEN=3, ORANGE=4, BLUE=5,
/// GREY=6, BROWN=7); the value is fed straight to `build_color_remaps` so the
/// rendered colour matches the label. (The old table mislabelled every row —
/// "BLUE"→3 actually selected GREEN's row, the reported bug — and offered
/// PURPLE/TEAL, which RA has no schemes for, while omitting LTBLUE and BROWN.)
pub const COLORS: [(&str, u8); 8] = [
    ("GOLD", 0),
    ("LTBLUE", 1),
    ("RED", 2),
    ("GREEN", 3),
    ("ORANGE", 4),
    ("BLUE", 5),
    ("GREY", 6),
    ("BROWN", 7),
];

/// Starting-credit steps.
pub const CREDITS: [i32; 4] = [2500, 5000, 7500, 10000];

/// Where a map came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapSource {
    /// Scanned from the game archives (general.mix `scm*.ini`).
    Archive,
    /// A user-supplied file in the platform maps folder.
    User,
}

/// One selectable skirmish map, with a small pre-rendered terrain preview.
#[derive(Clone, Debug)]
pub struct MapEntry {
    /// Display name (from `[Basic] Name=`, else the filename).
    pub name: String,
    /// Scenario filename / key (e.g. `"scm01ea.ini"`).
    pub filename: String,
    /// Player count (start-waypoint count), best-effort.
    pub players: u8,
    /// Playable width in cells.
    pub width: u32,
    /// Playable height in cells.
    pub height: u32,
    /// Where it came from.
    pub source: MapSource,
    /// A small RGBA terrain preview (may be empty = a placeholder is drawn).
    pub preview: RgbaImage,
}

/// The resolved skirmish choices handed to a [`GameFactory`] to build the World.
#[derive(Clone, Debug)]
pub struct ResolvedSkirmish {
    /// The chosen map's scenario filename.
    pub map_filename: String,
    /// Player house index.
    pub player_house: u8,
    /// Chosen player colour as a `PlayerColorType` / `PALETTE.CPS` row (0..8,
    /// from [`COLORS`]); `build_color_remaps[color_house]` paints the units.
    pub color_house: u8,
    /// Starting credits.
    pub credits: i32,
    /// AI difficulty.
    pub difficulty: Difficulty,
    /// Classic radar rules (true = DOME gating, false = always-on).
    pub classic_radar: bool,
}

/// Builds an in-game [`AppCore`] from resolved skirmish choices. The real
/// implementation reads the archives; tests inject a synthetic one so the whole
/// state machine runs with no assets.
pub trait GameFactory {
    /// Build a ready-to-play core plus the player's start cell (the camera is
    /// centered there on entry — a shrouded map with the camera at the origin
    /// renders as a black screen), or an error string for the setup screen.
    fn build(&self, res: &ResolvedSkirmish) -> Result<(AppCore, CellCoord), String>;
}

/// One selectable campaign mission (from `general.mix`'s `scg*ea.ini` set).
#[derive(Clone, Debug)]
pub struct CampaignEntry {
    /// Scenario key (e.g. `"scg01ea.ini"`).
    pub scenario: String,
    /// Mission display name (`[Basic] Name`).
    pub name: String,
}

/// A fully-built campaign mission: the core, its start camera cell, and the
/// briefing text to show before play.
pub struct BuiltMission {
    /// The ready-to-drive core.
    pub core: AppCore,
    /// Initial camera cell.
    pub start: CellCoord,
    /// Mission name.
    pub name: String,
    /// Briefing text.
    pub briefing: String,
}

/// Enumerates and builds single-player campaign missions. Kept separate from
/// [`GameFactory`] so the skirmish flow is unaffected; tests inject a synthetic
/// one (or `None`, disabling the Campaign button).
pub trait CampaignFactory {
    /// The ordered Allied mission list (those that resolve in the archives).
    fn missions(&self) -> Vec<CampaignEntry>;
    /// Build one mission by scenario key at the chosen difficulty, or an error
    /// string. Difficulty applies the campaign handicaps (computer houses get the
    /// chosen difficulty, the player the inverse — see `World::set_campaign_difficulty`).
    fn build(&self, scenario: &str, difficulty: Difficulty) -> Result<BuiltMission, String>;
}

/// Everything a LAN game build needs (M8-B): the host-authoritative settings
/// plus which seat is local. Both peers call their factory with the same
/// settings (map/seed/credits and both seats), differing only in
/// `local_house`/`remote_house` orientation — the built `World`s must be
/// byte-identical.
#[derive(Clone, Debug)]
pub struct LanGameSpec {
    /// Scenario filename both sides load.
    pub map_filename: String,
    /// World RNG seed (host-chosen).
    pub seed: u32,
    /// Starting credits for both houses.
    pub credits: i32,
    /// The local player's house (sidebar/camera/orders gate to it).
    pub local_house: u8,
    /// The remote player's house.
    pub remote_house: u8,
    /// The session host's house (start-position assignment is keyed to the
    /// host seat on both sides, so the worlds agree).
    pub host_house: u8,
}

/// Builds a LAN-game [`AppCore`] (world identical across peers; presentation
/// oriented to the local seat). The real implementation reads the archives;
/// tests inject a synthetic one.
pub trait LanGameFactory {
    /// Build the core plus the local player's start cell.
    fn build(&self, spec: &LanGameSpec) -> Result<(AppCore, CellCoord), String>;
}

/// The current top-level UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppState {
    /// Title screen.
    MainMenu,
    /// Skirmish map + options selection.
    SkirmishSetup,
    /// Campaign mission list.
    CampaignList,
    /// Mission briefing text screen (before play).
    Briefing,
    /// Multiplayer host/join chooser (M8-B).
    MultiplayerMenu,
    /// LAN host: map + credits selection before opening the lobby.
    LanHostSetup,
    /// LAN host: session open, waiting for a joiner / their READY.
    LanHostLobby,
    /// LAN join: discovered-session list.
    LanJoinBrowse,
    /// LAN join: joined a session, confirming READY / awaiting START.
    LanJoinLobby,
    /// A running game (delegates to [`AppCore`]).
    InGame,
    /// In-game pause overlay (sim frozen).
    Paused,
    /// Victory/Defeat resolved; awaiting Continue (sim frozen).
    GameOver,
    /// A LAN session ended abnormally (peer left / connection lost / out of
    /// sync); showing the message, awaiting Continue.
    NetEnded,
}

/// The setup screen's current selections (indices into the option tables).
#[derive(Clone, Copy, Debug)]
pub struct SkirmishConfig {
    /// Selected map index into [`App::maps`].
    pub map: usize,
    /// Index into [`DIFFICULTIES`].
    pub difficulty: usize,
    /// Index into [`HOUSES`].
    pub house: usize,
    /// Index into [`COLORS`].
    pub color: usize,
    /// Index into [`CREDITS`].
    pub credits: usize,
    /// Classic radar rules toggle (default on).
    pub classic_radar: bool,
}

impl Default for SkirmishConfig {
    fn default() -> SkirmishConfig {
        SkirmishConfig {
            map: 0,
            difficulty: 1, // Normal
            house: 0,      // Greece
            color: 0,      // Gold
            credits: 1,    // 5000
            classic_radar: true,
        }
    }
}

/// A clickable menu item's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    GotoSkirmish,
    GotoCampaign,
    GotoMultiplayer,
    GotoLanHostSetup,
    GotoLanJoin,
    /// Open the host lobby with the current map/credits selections.
    LanCreate,
    /// Host: fire START (enabled only when the joiner is READY).
    LanStart,
    /// Host: cancel the open session.
    LanCancelHost,
    /// Joiner: join the session list entry at this index.
    LanJoinSession(usize),
    /// Joiner: confirm READY.
    LanReady,
    /// Joiner: leave the joined lobby.
    LanLeaveJoin,
    /// Leave the NetEnded screen.
    NetContinue,
    Quit,
    SelectMap(usize),
    ScrollMaps(i32),
    Cycle(Field, i32),
    ToggleRadar,
    StartGame,
    BackToMenu,
    Resume,
    QuitToMenu,
    Continue,
    /// Select campaign mission `idx` (go to its briefing).
    SelectMission(usize),
    /// Start the currently-briefed mission.
    StartMission,
    /// Retry the current mission after a defeat.
    RetryMission,
    /// Choose the campaign difficulty on the briefing screen (index into [`DIFFICULTIES`]).
    SetCampaignDifficulty(usize),
    Disabled,
}

/// Which option a `Cycle` action changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Difficulty,
    House,
    Color,
    Credits,
}

/// A laid-out clickable item (geometry shared by draw + hit-test).
struct Item {
    rect: (i32, i32, i32, i32),
    label: String,
    action: Action,
    enabled: bool,
    selected: bool,
}

/// The windowless outer application: menu + wrapped in-game core.
pub struct App {
    state: AppState,
    maps: Vec<MapEntry>,
    config: SkirmishConfig,
    map_scroll: usize,
    factory: Box<dyn GameFactory>,
    campaign_factory: Option<Box<dyn CampaignFactory>>,
    campaign_missions: Vec<CampaignEntry>,
    /// Index of the mission currently briefed / playing.
    campaign_current: usize,
    /// Selected campaign difficulty (index into [`DIFFICULTIES`]); persists across
    /// missions. Default Normal (1) — a neutral no-op handicap.
    campaign_difficulty: usize,
    /// Briefing text for the current mission.
    briefing_text: String,
    /// Whether the running game is a campaign mission (vs. skirmish).
    in_campaign: bool,
    /// A mission core built eagerly by the briefing screen, ready to play.
    pending_core: Option<(AppCore, CellCoord)>,
    // --- LAN multiplayer (M8-B). All `None`/empty unless `with_lan` was
    // called AND the player entered the multiplayer flow; the single-player
    // states never touch them. ---
    lan_factory: Option<Box<dyn LanGameFactory>>,
    lan_cfg: ra_net::DiscoveryConfig,
    /// Local player display name (announcements / lobby lists).
    lan_name: String,
    host_lobby: Option<ra_net::HostLobby>,
    join_browser: Option<ra_net::SessionBrowser>,
    join_lobby: Option<ra_net::JoinLobby>,
    /// Last lobby-flow error, shown on the multiplayer screens.
    lan_error: Option<String>,
    /// Whether the running game is a LAN game.
    in_lan_game: bool,
    /// Message shown on the [`AppState::NetEnded`] screen.
    net_end_message: String,
    core: Option<AppCore>,
    /// Directory for always-on replay recording (M7.23 P1), or `None` to not
    /// record. The real windowed shell points this at `<assets>/../replays`; the
    /// menu test suites leave it `None` so they never touch the disk.
    replay_dir: Option<std::path::PathBuf>,
    viewport_w: u32,
    viewport_h: u32,
    mouse_x: i32,
    mouse_y: i32,
    sounds: Vec<SoundEvent>,
    quit: bool,
    last_error: Option<String>,
    focus: usize,
}

/// Number of map rows visible in the setup list at once.
const MAP_ROWS: usize = 8;
/// Row height in the map list, pixels.
const ROW_H: i32 = 16;

impl App {
    /// Create the app at the main menu, with a scanned map list and a factory to
    /// build games from. Starts at a default viewport size.
    pub fn new(maps: Vec<MapEntry>, factory: Box<dyn GameFactory>) -> App {
        App {
            state: AppState::MainMenu,
            maps,
            config: SkirmishConfig::default(),
            map_scroll: 0,
            factory,
            campaign_factory: None,
            campaign_missions: Vec::new(),
            campaign_current: 0,
            campaign_difficulty: 1, // Normal
            briefing_text: String::new(),
            in_campaign: false,
            pending_core: None,
            lan_factory: None,
            lan_cfg: ra_net::DiscoveryConfig::default(),
            lan_name: "COMMANDER".to_string(),
            host_lobby: None,
            join_browser: None,
            join_lobby: None,
            lan_error: None,
            in_lan_game: false,
            net_end_message: String::new(),
            core: None,
            replay_dir: None,
            viewport_w: 1024,
            viewport_h: 768,
            mouse_x: -1,
            mouse_y: -1,
            sounds: Vec::new(),
            quit: false,
            last_error: None,
            focus: 0,
        }
    }

    /// Install the campaign factory, enabling the Campaign button and pre-scanning
    /// the mission list. Chainable after [`App::new`].
    pub fn with_campaign(mut self, factory: Box<dyn CampaignFactory>) -> App {
        self.campaign_missions = factory.missions();
        self.campaign_factory = Some(factory);
        self
    }

    /// Install the LAN factory + discovery wiring, enabling the Multiplayer
    /// button (M8-B). Without this the multiplayer states are unreachable and
    /// the main menu is pixel-identical to before (same golden-preserving
    /// pattern as the campaign button). Chainable after [`App::new`].
    pub fn with_lan(
        mut self,
        factory: Box<dyn LanGameFactory>,
        cfg: ra_net::DiscoveryConfig,
        player_name: &str,
    ) -> App {
        self.lan_factory = Some(factory);
        self.lan_cfg = cfg;
        self.lan_name = player_name.to_string();
        self
    }

    /// Mutable discovery config (test hook: aim announcements at an
    /// OS-assigned browser port instead of the fixed LAN port).
    pub fn lan_config_mut(&mut self) -> &mut ra_net::DiscoveryConfig {
        &mut self.lan_cfg
    }

    /// The open host lobby, if hosting (tests / status display).
    pub fn host_lobby(&self) -> Option<&ra_net::HostLobby> {
        self.host_lobby.as_ref()
    }

    /// The joined lobby, if joining (tests / status display).
    pub fn join_lobby(&self) -> Option<&ra_net::JoinLobby> {
        self.join_lobby.as_ref()
    }

    /// The discovery browser's actually-bound port (tests aim the host at it).
    pub fn browser_port(&self) -> Option<u16> {
        self.join_browser.as_ref().map(|b| b.port())
    }

    /// The current discovered-session list (joiner browse screen).
    pub fn lan_sessions(&self) -> Vec<ra_net::DiscoveredSession> {
        self.join_browser
            .as_ref()
            .map(|b| b.sessions().to_vec())
            .unwrap_or_default()
    }

    /// The last lobby-flow error, if any.
    pub fn lan_error(&self) -> Option<&str> {
        self.lan_error.as_deref()
    }

    /// The NetEnded screen's message (tests).
    pub fn net_end_message(&self) -> &str {
        &self.net_end_message
    }

    /// Whether the running game is a LAN game.
    pub fn in_lan_game(&self) -> bool {
        self.in_lan_game
    }

    /// The scanned campaign mission list (for tests / the shell).
    pub fn campaign_missions(&self) -> &[CampaignEntry] {
        &self.campaign_missions
    }

    /// The current briefing text (for tests).
    pub fn briefing_text(&self) -> &str {
        &self.briefing_text
    }

    /// The selected campaign difficulty (for tests / the shell).
    pub fn campaign_difficulty(&self) -> Difficulty {
        DIFFICULTIES[self.campaign_difficulty.min(DIFFICULTIES.len() - 1)].1
    }

    /// Choose the campaign difficulty (briefing screen). Rebuilds the pending
    /// mission core through the factory at the new difficulty — the same
    /// "factory config" path the skirmish setup uses — so the difficulty is a
    /// single source of truth (the built world already carries its handicaps).
    fn set_campaign_difficulty(&mut self, idx: usize) {
        self.campaign_difficulty = idx.min(DIFFICULTIES.len() - 1);
        if self.state != AppState::Briefing {
            return;
        }
        if let Some(f) = &self.campaign_factory {
            if let Some(entry) = self.campaign_missions.get(self.campaign_current) {
                if let Ok(built) = f.build(&entry.scenario, self.campaign_difficulty()) {
                    self.briefing_text = built.briefing;
                    self.pending_core = Some((built.core, built.start));
                }
            }
        }
    }

    /// The current UI state (for the shell + tests).
    pub fn state(&self) -> AppState {
        self.state
    }

    /// Whether the user asked to quit the whole app (shell exits).
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Borrow the in-game core, if a game is running/paused/over.
    pub fn core(&self) -> Option<&AppCore> {
        self.core.as_ref()
    }

    /// Mutable in-game core (verification hook, e.g. to script orders).
    pub fn core_mut(&mut self) -> Option<&mut AppCore> {
        self.core.as_mut()
    }

    /// The scanned map list (for tests / the shell).
    pub fn maps(&self) -> &[MapEntry] {
        &self.maps
    }

    /// The current setup selections.
    pub fn config(&self) -> SkirmishConfig {
        self.config
    }

    /// The last game-build error, if the most recent Start failed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Directly set the selected map index (verification hook / keyboard).
    pub fn select_map(&mut self, idx: usize) {
        if idx < self.maps.len() {
            self.config.map = idx;
            self.ensure_map_visible();
        }
    }

    /// Mutable setup config (verification hook for scripting choices without
    /// synthesising clicks).
    pub fn config_mut(&mut self) -> &mut SkirmishConfig {
        &mut self.config
    }

    /// Resolve the current selections into concrete skirmish parameters.
    fn resolve(&self) -> Option<ResolvedSkirmish> {
        let map = self.maps.get(self.config.map)?;
        Some(ResolvedSkirmish {
            map_filename: map.filename.clone(),
            player_house: HOUSES[self.config.house.min(HOUSES.len() - 1)].1,
            color_house: COLORS[self.config.color.min(COLORS.len() - 1)].1,
            credits: CREDITS[self.config.credits.min(CREDITS.len() - 1)],
            difficulty: DIFFICULTIES[self.config.difficulty.min(DIFFICULTIES.len() - 1)].1,
            classic_radar: self.config.classic_radar,
        })
    }

    /// Enable always-on replay recording (M7.23 P1): every interactive game
    /// started thereafter appends its stream to
    /// `<dir>/<scenario>-<timestamp>.rarp`. The windowed shell calls this with
    /// the replays directory beside the assets dir; leaving it unset (the test
    /// default) records nothing.
    pub fn enable_recording(&mut self, dir: std::path::PathBuf) {
        self.replay_dir = Some(dir);
    }

    /// Install a recorder on a freshly-built game core if recording is enabled.
    /// A recording-setup failure never blocks the game — the recorder degrades
    /// to disabled internally (see [`crate::replay::ReplayRecorder`]).
    fn maybe_install_recorder(
        &self,
        core: &mut AppCore,
        scenario: &str,
        difficulty_u8: u8,
        credits: i32,
        seats: Vec<ra_net::ReplaySeat>,
    ) {
        let Some(dir) = self.replay_dir.as_ref() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let w = core.world();
        // A filesystem-safe stem: drop any ".ini", keep the scenario key.
        let stem = scenario.strip_suffix(".ini").unwrap_or(scenario);
        let header = ra_net::ReplayHeader {
            replay_version: ra_net::REPLAY_VERSION,
            game_version: ra_net::wire::GAME_VERSION,
            protocol_version: ra_net::wire::PROTOCOL_VERSION,
            scenario: scenario.to_string(),
            seed: w.rng_seed(),
            difficulty: difficulty_u8,
            credits,
            catalog_hash: w.catalog().content_hash(),
            start_millis: now,
            seats,
        };
        let path = dir.join(format!("{stem}-{now}.rarp"));
        core.install_recorder(crate::replay::ReplayRecorder::create(path, &header));
    }

    /// Map a [`Difficulty`] to the replay header's `u8` code.
    fn difficulty_code(d: Difficulty) -> u8 {
        match d {
            Difficulty::Easy => 0,
            Difficulty::Normal => 1,
            Difficulty::Hard => 2,
        }
    }

    /// Build and enter a game from the current selections. Public so a scripted
    /// drive can start a game after setting the config directly.
    pub fn start_game(&mut self) {
        let Some(res) = self.resolve() else {
            self.last_error = Some("no map selected".to_string());
            return;
        };
        match self.factory.build(&res) {
            Ok((mut core, start)) => {
                core.handle(InputEvent::Resize {
                    width: self.viewport_w,
                    height: self.viewport_h,
                });
                let tw = core.tactical_width();
                core.set_camera(
                    (start.x * crate::appcore::CELL_PIXELS) as f32 - tw as f32 / 2.0,
                    (start.y * crate::appcore::CELL_PIXELS) as f32 - self.viewport_h as f32 / 2.0,
                );
                self.maybe_install_recorder(
                    &mut core,
                    &res.map_filename,
                    Self::difficulty_code(res.difficulty),
                    res.credits,
                    vec![ra_net::ReplaySeat {
                        seat: res.player_house,
                        house: res.player_house,
                        color: res.color_house,
                    }],
                );
                self.core = Some(core);
                self.last_error = None;
                self.state = AppState::InGame;
            }
            Err(e) => {
                self.last_error = Some(e);
            }
        }
    }

    /// Return to the main menu, discarding any running game (fresh World next
    /// time — no state leaks between games). A LAN game tells the peer first
    /// (clean "player left" instead of their keepalive timeout).
    pub fn quit_to_menu(&mut self) {
        if self.in_lan_game {
            if let Some(c) = self.core.as_mut() {
                c.notify_net_quit();
            }
        }
        self.in_lan_game = false;
        self.lan_teardown();
        self.core = None;
        self.state = AppState::MainMenu;
        self.focus = 0;
    }

    // -------------------------------------------------------------------------
    // LAN multiplayer flow (M8-B). Host is authority on settings; both sides
    // must confirm before START; every wait is bounded (the lobby objects
    // carry the timeouts).
    // -------------------------------------------------------------------------

    /// The fixed LAN seats: host = Greece (1), joiner = USSR (2). Seat ids
    /// are house ids (canonical bundle order, `Execute_DoList`'s house-array
    /// order); a seat picker is deliberately out of scope for M8-B.
    pub const LAN_HOST_HOUSE: u8 = 1;
    /// The joiner's house.
    pub const LAN_JOIN_HOUSE: u8 = 2;

    /// Drop every live lobby object (cancelling/leaving politely).
    fn lan_teardown(&mut self) {
        if let Some(h) = self.host_lobby.take() {
            h.cancel();
        }
        if let Some(j) = self.join_lobby.take() {
            j.leave();
        }
        self.join_browser = None;
    }

    /// Host: open the lobby with the currently selected map + credits.
    /// Public as a verification hook (the UI reaches it via CREATE GAME).
    pub fn lan_host_create(&mut self) {
        let Some(map) = self.maps.get(self.config.map) else {
            self.lan_error = Some("no map selected".to_string());
            return;
        };
        // Seed: wall clock is fine here — this is the menu layer picking a
        // session parameter; the sim only ever sees the resulting constant,
        // which the host transmits to the joiner (host authority).
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_secs() as u32) ^ d.subsec_nanos())
            .unwrap_or(0x1234_5678);
        let settings = ra_net::SessionSettings {
            map: map.filename.clone(),
            seed,
            credits: CREDITS[self.config.credits.min(CREDITS.len() - 1)],
            host_seat: App::LAN_HOST_HOUSE,
            join_seat: App::LAN_JOIN_HOUSE,
            delay: ra_net::DEFAULT_INPUT_DELAY,
        };
        match ra_net::HostLobby::create(&self.lan_name, settings, &self.lan_cfg) {
            Ok(lobby) => {
                self.host_lobby = Some(lobby);
                self.lan_error = None;
                self.state = AppState::LanHostLobby;
                self.focus = 0;
            }
            Err(e) => self.lan_error = Some(format!("could not open session: {e}")),
        }
    }

    /// Joiner: enter the browse screen (binds the discovery listener).
    fn lan_goto_browse(&mut self) {
        match ra_net::SessionBrowser::bind(&self.lan_cfg) {
            Ok(b) => {
                self.join_browser = Some(b);
                self.lan_error = None;
                self.state = AppState::LanJoinBrowse;
                self.focus = 0;
            }
            Err(e) => {
                self.lan_error = Some(format!(
                    "could not listen for sessions: {e} (another joiner on this machine?)"
                ));
            }
        }
    }

    /// Joiner: join the session-list entry at `idx` (verification hook).
    pub fn lan_join(&mut self, idx: usize) {
        let sessions = self.lan_sessions();
        let Some(s) = sessions.get(idx) else {
            return;
        };
        if !s.compatible {
            self.lan_error = Some("session version is incompatible".to_string());
            return;
        }
        match ra_net::JoinLobby::join(s.addr, &self.lan_name) {
            Ok(j) => {
                self.join_lobby = Some(j);
                self.lan_error = None;
                self.state = AppState::LanJoinLobby;
                self.focus = 0;
            }
            Err(e) => self.lan_error = Some(format!("could not join: {e}")),
        }
    }

    /// Joiner: confirm READY (verification hook).
    pub fn lan_ready(&mut self) {
        if let Some(j) = self.join_lobby.as_mut() {
            j.set_ready();
        }
    }

    /// Host: fire START and enter the game (verification hook; the UI's
    /// START button is enabled only when the joiner is READY).
    pub fn lan_start_game(&mut self) {
        let Some(lobby) = self.host_lobby.take() else {
            return;
        };
        if !lobby.can_start() {
            self.host_lobby = Some(lobby);
            return;
        }
        let settings = lobby.settings().clone();
        match lobby.start() {
            Ok(tp) => {
                let spec = LanGameSpec {
                    map_filename: settings.map.clone(),
                    seed: settings.seed,
                    credits: settings.credits,
                    local_house: settings.host_seat,
                    remote_house: settings.join_seat,
                    host_house: settings.host_seat,
                };
                self.lan_enter_game(&spec, tp);
            }
            Err(e) => {
                self.lan_error = Some(format!("could not start: {e}"));
                self.state = AppState::MultiplayerMenu;
            }
        }
    }

    /// Joiner side: START arrived — build and enter.
    fn lan_join_enter_game(&mut self) {
        let Some(lobby) = self.join_lobby.take() else {
            return;
        };
        let Some(w) = lobby.welcome().cloned() else {
            return;
        };
        match lobby.into_transport() {
            Ok(tp) => {
                let spec = LanGameSpec {
                    map_filename: w.map.clone(),
                    seed: w.seed,
                    credits: w.credits,
                    local_house: w.seat,
                    remote_house: w.host_seat,
                    host_house: w.host_seat,
                };
                self.lan_enter_game(&spec, tp);
            }
            Err(e) => {
                self.lan_error = Some(format!("could not start: {e}"));
                self.state = AppState::MultiplayerMenu;
            }
        }
    }

    /// Build the LAN world via the factory, install the transport, enter.
    fn lan_enter_game(&mut self, spec: &LanGameSpec, transport: ra_net::LanTransport) {
        let Some(f) = &self.lan_factory else {
            self.lan_error = Some("no LAN factory".to_string());
            self.state = AppState::MultiplayerMenu;
            return;
        };
        match f.build(spec) {
            Ok((mut core, start)) => {
                core.install_lan(transport, spec.remote_house);
                core.handle(InputEvent::Resize {
                    width: self.viewport_w,
                    height: self.viewport_h,
                });
                let tw = core.tactical_width();
                core.set_camera(
                    (start.x * crate::appcore::CELL_PIXELS) as f32 - tw as f32 / 2.0,
                    (start.y * crate::appcore::CELL_PIXELS) as f32 - self.viewport_h as f32 / 2.0,
                );
                // Record on both peers (M7.23 P1). Difficulty is irrelevant
                // between two humans (code 1 = neutral); both seats are named so
                // a viewer paints the right colours.
                self.maybe_install_recorder(
                    &mut core,
                    &spec.map_filename,
                    1,
                    spec.credits,
                    vec![
                        ra_net::ReplaySeat {
                            seat: spec.local_house,
                            house: spec.local_house,
                            color: spec.local_house,
                        },
                        ra_net::ReplaySeat {
                            seat: spec.remote_house,
                            house: spec.remote_house,
                            color: spec.remote_house,
                        },
                    ],
                );
                self.core = Some(core);
                self.in_lan_game = true;
                self.lan_teardown();
                self.last_error = None;
                self.lan_error = None;
                self.state = AppState::InGame;
            }
            Err(e) => {
                self.lan_error = Some(e);
                self.state = AppState::MultiplayerMenu;
            }
        }
    }

    // -------------------------------------------------------------------------
    // Event handling
    // -------------------------------------------------------------------------

    /// Handle one input event.
    pub fn handle(&mut self, ev: InputEvent) {
        // Resize always updates the menu viewport and is forwarded to the core.
        if let InputEvent::Resize { width, height } = ev {
            self.viewport_w = width.clamp(1, 8192);
            self.viewport_h = height.clamp(1, 8192);
            if let Some(c) = self.core.as_mut() {
                c.handle(ev);
            }
            return;
        }
        if let InputEvent::MouseMoved { x, y } = ev {
            self.mouse_x = x;
            self.mouse_y = y;
        }

        match self.state {
            AppState::InGame => self.handle_ingame(ev),
            AppState::MainMenu
            | AppState::SkirmishSetup
            | AppState::CampaignList
            | AppState::Briefing
            | AppState::MultiplayerMenu
            | AppState::LanHostSetup
            | AppState::LanHostLobby
            | AppState::LanJoinBrowse
            | AppState::LanJoinLobby
            | AppState::Paused
            | AppState::GameOver
            | AppState::NetEnded => self.handle_menu(ev),
        }
    }

    fn handle_ingame(&mut self, ev: InputEvent) {
        // Esc first cancels an armed sell/repair mode (the original's cursor
        // mode); only when no such mode is active does it open the pause overlay.
        if matches!(ev, InputEvent::KeyDown(Key::Menu)) {
            if let Some(c) = self.core.as_mut() {
                if c.action_mode_armed() {
                    c.handle(ev);
                    return;
                }
            }
            self.state = AppState::Paused;
            self.focus = 0;
            return;
        }
        // Confirm is a menu-only key; drop it in game.
        if matches!(
            ev,
            InputEvent::KeyDown(Key::Confirm) | InputEvent::KeyUp(Key::Confirm)
        ) {
            return;
        }
        if let Some(c) = self.core.as_mut() {
            c.handle(ev);
        }
    }

    fn handle_menu(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::MouseDown {
                button: MouseButton::Left,
                x,
                y,
            } => {
                if let Some(action) = self.hit_test(x, y) {
                    self.activate(action);
                }
            }
            InputEvent::KeyDown(Key::Menu) => {
                // Back out one level.
                match self.state {
                    AppState::SkirmishSetup
                    | AppState::CampaignList
                    | AppState::MultiplayerMenu => self.state = AppState::MainMenu,
                    AppState::Briefing => self.state = AppState::CampaignList,
                    AppState::LanHostSetup => self.state = AppState::MultiplayerMenu,
                    AppState::LanHostLobby => self.activate(Action::LanCancelHost),
                    AppState::LanJoinBrowse => {
                        self.join_browser = None;
                        self.state = AppState::MultiplayerMenu;
                    }
                    AppState::LanJoinLobby => self.activate(Action::LanLeaveJoin),
                    AppState::NetEnded => self.activate(Action::NetContinue),
                    AppState::Paused => self.state = AppState::InGame,
                    _ => {}
                }
            }
            InputEvent::KeyDown(Key::Up) => self.move_focus(-1),
            InputEvent::KeyDown(Key::Down) => self.move_focus(1),
            InputEvent::KeyDown(Key::Confirm) => {
                let items = self.items();
                if let Some(it) = items.get(self.focus) {
                    if it.enabled {
                        let a = it.action;
                        self.activate(a);
                    }
                }
            }
            _ => {}
        }
    }

    fn move_focus(&mut self, delta: i32) {
        let items = self.items();
        let enabled: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.enabled)
            .map(|(i, _)| i)
            .collect();
        if enabled.is_empty() {
            return;
        }
        // Find the current position among enabled items, then step.
        let cur = enabled.iter().position(|&i| i == self.focus).unwrap_or(0) as i32;
        let n = enabled.len() as i32;
        let next = ((cur + delta) % n + n) % n;
        self.focus = enabled[next as usize];
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<Action> {
        for it in self.items() {
            let (x0, y0, x1, y1) = it.rect;
            if it.enabled && x >= x0 && x < x1 && y >= y0 && y < y1 {
                return Some(it.action);
            }
        }
        None
    }

    fn activate(&mut self, action: Action) {
        match action {
            Action::GotoSkirmish => {
                self.state = AppState::SkirmishSetup;
                self.focus = 0;
            }
            Action::GotoCampaign => {
                self.state = AppState::CampaignList;
                self.focus = 0;
            }
            Action::GotoMultiplayer => {
                self.state = AppState::MultiplayerMenu;
                self.lan_error = None;
                self.focus = 0;
            }
            Action::GotoLanHostSetup => {
                self.state = AppState::LanHostSetup;
                self.focus = 0;
            }
            Action::GotoLanJoin => self.lan_goto_browse(),
            Action::LanCreate => self.lan_host_create(),
            Action::LanStart => self.lan_start_game(),
            Action::LanCancelHost => {
                if let Some(h) = self.host_lobby.take() {
                    h.cancel();
                }
                self.state = AppState::MultiplayerMenu;
                self.focus = 0;
            }
            Action::LanJoinSession(i) => self.lan_join(i),
            Action::LanReady => self.lan_ready(),
            Action::LanLeaveJoin => {
                if let Some(j) = self.join_lobby.take() {
                    j.leave();
                }
                // Back to browsing (the listener stays bound across a join
                // attempt, so this cannot fail).
                if self.join_browser.is_some() {
                    self.state = AppState::LanJoinBrowse;
                    self.focus = 0;
                } else {
                    self.lan_goto_browse();
                }
            }
            Action::NetContinue => self.quit_to_menu(),
            Action::Quit => self.quit = true,
            Action::SelectMap(i) => self.select_map(i),
            Action::ScrollMaps(d) => self.scroll_maps(d),
            Action::Cycle(field, d) => self.cycle(field, d),
            Action::ToggleRadar => self.config.classic_radar = !self.config.classic_radar,
            Action::StartGame => self.start_game(),
            Action::SelectMission(i) => self.goto_briefing(i),
            Action::StartMission => self.start_mission(self.campaign_current),
            Action::RetryMission => self.start_mission(self.campaign_current),
            Action::SetCampaignDifficulty(i) => self.set_campaign_difficulty(i),
            Action::BackToMenu => {
                self.state = AppState::MainMenu;
                self.focus = 0;
            }
            Action::Resume => self.state = AppState::InGame,
            Action::QuitToMenu => self.quit_to_menu(),
            Action::Continue => self.on_continue(),
            Action::Disabled => {}
        }
    }

    /// Show the briefing for mission `idx` (loads its text from the factory).
    fn goto_briefing(&mut self, idx: usize) {
        self.campaign_current = idx;
        self.focus = 0;
        // Fetch the briefing text by building nothing — the factory carries it in
        // the built mission, so we read it at start; here we show the list-known
        // briefing lazily by attempting a build for text only would be wasteful,
        // so we defer text to `start_mission`. Show a placeholder until then.
        self.briefing_text = self
            .campaign_missions
            .get(idx)
            .map(|m| format!("MISSION: {}", m.name))
            .unwrap_or_default();
        // Build eagerly so the real briefing text is available on the screen.
        if let Some(f) = &self.campaign_factory {
            if let Some(entry) = self.campaign_missions.get(idx) {
                if let Ok(built) = f.build(&entry.scenario, self.campaign_difficulty()) {
                    self.briefing_text = built.briefing;
                    // Stash the built core for immediate play (avoids a second load).
                    self.pending_core = Some((built.core, built.start));
                }
            }
        }
        self.state = AppState::Briefing;
    }

    /// Start (or restart) campaign mission `idx`.
    fn start_mission(&mut self, idx: usize) {
        self.campaign_current = idx;
        // Prefer the core stashed by the briefing build; else build fresh (retry).
        let built = self.pending_core.take();
        let (mut core, start) = match built {
            Some(cs) => cs,
            None => {
                let Some(f) = &self.campaign_factory else {
                    self.last_error = Some("no campaign factory".into());
                    return;
                };
                let Some(entry) = self.campaign_missions.get(idx) else {
                    return;
                };
                match f.build(&entry.scenario, self.campaign_difficulty()) {
                    Ok(b) => {
                        self.briefing_text = b.briefing;
                        (b.core, b.start)
                    }
                    Err(e) => {
                        self.last_error = Some(e);
                        return;
                    }
                }
            }
        };
        core.handle(InputEvent::Resize {
            width: self.viewport_w,
            height: self.viewport_h,
        });
        let tw = core.tactical_width();
        core.set_camera(
            (start.x * crate::appcore::CELL_PIXELS) as f32 - tw as f32 / 2.0,
            (start.y * crate::appcore::CELL_PIXELS) as f32 - self.viewport_h as f32 / 2.0,
        );
        self.core = Some(core);
        self.in_campaign = true;
        self.last_error = None;
        self.state = AppState::InGame;
    }

    /// The game-over "Continue" button: in a campaign, a Victory advances to the
    /// next mission's briefing (or the menu if the campaign is complete); a Defeat
    /// (or a skirmish) returns to the menu.
    fn on_continue(&mut self) {
        let victory = self
            .core
            .as_ref()
            .map(|c| c.game_over() == ra_sim::GameOver::Victory)
            .unwrap_or(false);
        if self.in_campaign && victory {
            let next = self.campaign_current + 1;
            self.core = None;
            self.in_campaign = false;
            if next < self.campaign_missions.len() {
                self.goto_briefing(next);
            } else {
                self.quit_to_menu();
            }
        } else {
            self.in_campaign = false;
            self.quit_to_menu();
        }
    }

    fn cycle(&mut self, field: Field, d: i32) {
        let step = |cur: usize, len: usize, d: i32| -> usize {
            if len == 0 {
                return 0;
            }
            let n = len as i32;
            (((cur as i32 + d) % n + n) % n) as usize
        };
        match field {
            Field::Difficulty => {
                self.config.difficulty = step(self.config.difficulty, DIFFICULTIES.len(), d)
            }
            Field::House => self.config.house = step(self.config.house, HOUSES.len(), d),
            Field::Color => self.config.color = step(self.config.color, COLORS.len(), d),
            Field::Credits => self.config.credits = step(self.config.credits, CREDITS.len(), d),
        }
    }

    fn scroll_maps(&mut self, d: i32) {
        let max_scroll = self.maps.len().saturating_sub(MAP_ROWS);
        let ns = (self.map_scroll as i32 + d).clamp(0, max_scroll as i32);
        self.map_scroll = ns as usize;
    }

    fn ensure_map_visible(&mut self) {
        if self.config.map < self.map_scroll {
            self.map_scroll = self.config.map;
        } else if self.config.map >= self.map_scroll + MAP_ROWS {
            self.map_scroll = self.config.map + 1 - MAP_ROWS;
        }
    }

    // -------------------------------------------------------------------------
    // Update
    // -------------------------------------------------------------------------

    /// Advance virtual time. Only the `InGame` state ticks the sim; `Paused` and
    /// `GameOver` freeze it (tick count does not advance), and the menus have no
    /// sim.
    pub fn update(&mut self, dt_ms: u32) {
        // LAN lobby states are poll-driven: pump their sockets every frame.
        match self.state {
            AppState::LanHostLobby => {
                if let Some(h) = self.host_lobby.as_mut() {
                    h.poll();
                    if h.take_joiner_lost() {
                        self.lan_error = Some("player left the lobby".to_string());
                    }
                }
            }
            AppState::LanJoinBrowse => {
                if let Some(b) = self.join_browser.as_mut() {
                    b.poll();
                }
            }
            // A paused LAN game must stay network-alive: the peer's barrier
            // shows "waiting for player", not a spurious connection loss.
            AppState::Paused | AppState::GameOver => {
                if self.in_lan_game {
                    if let Some(c) = self.core.as_mut() {
                        c.net_service();
                    }
                }
            }
            AppState::LanJoinLobby => {
                if let Some(j) = self.join_lobby.as_mut() {
                    j.poll();
                    if j.started() {
                        self.lan_join_enter_game();
                    } else if let Some(e) = j.error() {
                        self.lan_error = Some(e.to_string());
                        self.join_lobby = None;
                        if self.join_browser.is_some() {
                            self.state = AppState::LanJoinBrowse;
                        } else {
                            self.state = AppState::MultiplayerMenu;
                        }
                        self.focus = 0;
                    }
                }
            }
            _ => {}
        }

        if self.state == AppState::InGame {
            if let Some(c) = self.core.as_mut() {
                c.update(dt_ms);
                let mut cues = c.drain_sounds();
                self.sounds.append(&mut cues);
                // LAN: an abnormal session end (peer left / connection lost /
                // desync) preempts the normal game-over flow.
                if let Some(end) = c.net_end() {
                    self.net_end_message = match end {
                        crate::appcore::NetEnd::PeerLeft => "PLAYER LEFT THE GAME".to_string(),
                        crate::appcore::NetEnd::ConnectionLost => "CONNECTION LOST".to_string(),
                        crate::appcore::NetEnd::Desync => "OUT OF SYNC".to_string(),
                    };
                    self.state = AppState::NetEnded;
                    self.focus = 0;
                    return;
                }
                // Transition to the game-over screen when the sim resolves.
                if c.game_over() != ra_sim::GameOver::Ongoing {
                    self.state = AppState::GameOver;
                }
            }
        }
    }

    /// Drain queued commands from the in-game core (net layer / tests).
    pub fn drain_commands(&mut self) -> Vec<crate::appcore::Command> {
        self.core
            .as_mut()
            .map(|c| c.drain_commands())
            .unwrap_or_default()
    }

    /// Drain queued sound cues (the shell plays them; headless ignores them).
    pub fn drain_sounds(&mut self) -> Vec<SoundEvent> {
        std::mem::take(&mut self.sounds)
    }

    // -------------------------------------------------------------------------
    // Composition
    // -------------------------------------------------------------------------

    /// Compose the current frame.
    pub fn compose(&self) -> Frame {
        match self.state {
            AppState::MainMenu => self.compose_main_menu(),
            AppState::SkirmishSetup => self.compose_setup(),
            AppState::CampaignList => self.compose_campaign_list(),
            AppState::Briefing => self.compose_briefing(),
            AppState::MultiplayerMenu => self.compose_multiplayer_menu(),
            AppState::LanHostSetup => self.compose_lan_host_setup(),
            AppState::LanHostLobby => self.compose_lan_host_lobby(),
            AppState::LanJoinBrowse => self.compose_lan_join_browse(),
            AppState::LanJoinLobby => self.compose_lan_join_lobby(),
            AppState::InGame => self.compose_ingame(),
            AppState::Paused => self.compose_paused(),
            AppState::GameOver => self.compose_gameover(),
            AppState::NetEnded => self.compose_net_ended(),
        }
    }

    fn blank(&self) -> Frame {
        RgbaImage {
            width: self.viewport_w,
            height: self.viewport_h,
            pixels: vec![0u8; (self.viewport_w * self.viewport_h * 4) as usize],
        }
    }

    fn compose_ingame(&self) -> Frame {
        match self.core.as_ref() {
            Some(c) => c.compose_camera(),
            None => self.blank(),
        }
    }

    /// Draw the shared item list (buttons / rows) onto `frame`, highlighting the
    /// keyboard focus and the current selection.
    fn draw_items(&self, frame: &mut Frame) {
        let items = self.items();
        for (i, it) in items.iter().enumerate() {
            let (x0, y0, x1, y1) = it.rect;
            let focused = i == self.focus && it.enabled;
            let bg = if !it.enabled {
                [26, 26, 30]
            } else if it.selected {
                [40, 70, 40]
            } else if focused {
                [50, 55, 75]
            } else {
                [34, 36, 44]
            };
            fill_rect(frame, x0, y0, x1 - 1, y1 - 1, bg);
            let border = if it.selected {
                [90, 200, 90]
            } else if focused {
                [140, 160, 220]
            } else {
                [70, 74, 90]
            };
            draw_rect_outline(frame, x0, y0, x1 - 1, y1 - 1, border);
            let col = if !it.enabled {
                [90, 90, 96]
            } else {
                [220, 224, 232]
            };
            let ty = y0 + (y1 - y0 - font::GLYPH_H) / 2;
            font::draw_text(frame, x0 + 6, ty, &it.label, col);
        }
    }

    fn compose_main_menu(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        let title = "NEW-ALERT";
        let scale = 6;
        let tw = font::text_width(title) * scale;
        let tx = (self.viewport_w as i32 - tw) / 2;
        font::draw_text_scaled(&mut frame, tx, 90, title, [210, 60, 50], scale);
        let sub = "RED ALERT REPRODUCTION";
        let sw = font::text_width(sub) * 2;
        font::draw_text_scaled(
            &mut frame,
            (self.viewport_w as i32 - sw) / 2,
            90 + font::GLYPH_H * scale + 12,
            sub,
            [150, 160, 180],
            2,
        );
        self.draw_items(&mut frame);
        frame
    }

    fn compose_setup(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "SKIRMISH SETUP", [220, 200, 120], 3);

        // Map list header.
        font::draw_text(&mut frame, 40, 70, "MAP", [150, 160, 180]);
        // Selected map's minimap preview + metadata (right side).
        if let Some(map) = self.maps.get(self.config.map) {
            let px0 = self.viewport_w as i32 - 40 - 200;
            let py0 = 70;
            draw_rect_outline(&mut frame, px0, py0, px0 + 200, py0 + 200, [80, 90, 110]);
            fill_rect(
                &mut frame,
                px0 + 1,
                py0 + 1,
                px0 + 199,
                py0 + 199,
                [6, 8, 14],
            );
            blit_fit(&mut frame, &map.preview, px0 + 2, py0 + 2, 196, 196);
            let meta_y = py0 + 208;
            font::draw_text(&mut frame, px0, meta_y, &map.name, [220, 224, 232]);
            font::draw_text(
                &mut frame,
                px0,
                meta_y + 12,
                &format!("PLAYERS {}  {}X{}", map.players, map.width, map.height),
                [150, 160, 180],
            );
            let src = match map.source {
                MapSource::Archive => "ARCHIVE MAP",
                MapSource::User => "USER MAP",
            };
            font::draw_text(&mut frame, px0, meta_y + 24, src, [130, 150, 130]);
        }
        self.draw_items(&mut frame);
        if let Some(e) = &self.last_error {
            font::draw_text(
                &mut frame,
                40,
                self.viewport_h as i32 - 20,
                &format!("ERROR: {e}"),
                [220, 100, 100],
            );
        }
        frame
    }

    fn compose_campaign_list(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "ALLIED CAMPAIGN", [220, 200, 120], 3);
        if self.campaign_missions.is_empty() {
            font::draw_text(&mut frame, 40, 90, "NO MISSIONS FOUND", [200, 120, 120]);
        }
        self.draw_items(&mut frame);
        frame
    }

    fn compose_briefing(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [8, 12, 20]);
        let title = self
            .campaign_missions
            .get(self.campaign_current)
            .map(|m| m.name.as_str())
            .unwrap_or("MISSION");
        font::draw_text_scaled(&mut frame, 40, 30, "BRIEFING", [220, 200, 120], 3);
        font::draw_text_scaled(
            &mut frame,
            40,
            70,
            &title.to_uppercase(),
            [180, 200, 230],
            2,
        );
        // Word-wrap the briefing text into the content area.
        let margin = 40;
        let max_w = (self.viewport_w as i32 - margin * 2).max(80);
        let cols = (max_w / font::text_width("M").max(1)).max(10) as usize;
        let mut y = 120;
        for line in wrap_text(&self.briefing_text, cols) {
            font::draw_text(&mut frame, margin, y, &line, [210, 214, 222]);
            y += font::GLYPH_H + 6;
        }
        self.draw_items(&mut frame);
        frame
    }

    fn compose_paused(&self) -> Frame {
        // The frozen game frame, dimmed, with a pause panel over it.
        let mut frame = self.compose_ingame();
        dim(&mut frame, 45);
        let title = "PAUSED";
        let scale = 4;
        let tw = font::text_width(title) * scale;
        font::draw_text_scaled(
            &mut frame,
            (self.viewport_w as i32 - tw) / 2,
            self.viewport_h as i32 / 2 - 120,
            title,
            [230, 230, 240],
            scale,
        );
        self.draw_items(&mut frame);
        frame
    }

    fn compose_gameover(&self) -> Frame {
        // The game frame already carries the VICTORY/DEFEAT banner; add Continue.
        let mut frame = self.compose_ingame();
        self.draw_items(&mut frame);
        frame
    }

    fn compose_multiplayer_menu(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "MULTIPLAYER - LAN", [220, 200, 120], 3);
        self.draw_items(&mut frame);
        self.draw_lan_error(&mut frame);
        frame
    }

    fn compose_lan_host_setup(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "HOST LAN GAME", [220, 200, 120], 3);
        font::draw_text(&mut frame, 40, 70, "MAP", [150, 160, 180]);
        if let Some(map) = self.maps.get(self.config.map) {
            let px0 = self.viewport_w as i32 - 40 - 200;
            let py0 = 70;
            draw_rect_outline(&mut frame, px0, py0, px0 + 200, py0 + 200, [80, 90, 110]);
            fill_rect(
                &mut frame,
                px0 + 1,
                py0 + 1,
                px0 + 199,
                py0 + 199,
                [6, 8, 14],
            );
            blit_fit(&mut frame, &map.preview, px0 + 2, py0 + 2, 196, 196);
            font::draw_text(&mut frame, px0, py0 + 208, &map.name, [220, 224, 232]);
        }
        self.draw_items(&mut frame);
        self.draw_lan_error(&mut frame);
        frame
    }

    fn compose_lan_host_lobby(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(
            &mut frame,
            40,
            24,
            "LAN LOBBY - HOSTING",
            [220, 200, 120],
            3,
        );
        let mut y = 84;
        let line = |frame: &mut Frame, text: &str, col: [u8; 3], y: &mut i32| {
            font::draw_text(frame, 40, *y, text, col);
            *y += font::GLYPH_H + 8;
        };
        if let Some(h) = &self.host_lobby {
            line(
                &mut frame,
                &format!("SESSION: {}", self.lan_name.to_uppercase()),
                [210, 214, 222],
                &mut y,
            );
            line(
                &mut frame,
                &format!("MAP: {}", h.settings().map.to_uppercase()),
                [210, 214, 222],
                &mut y,
            );
            line(
                &mut frame,
                &format!("UDP PORT: {}", h.port()),
                [150, 160, 180],
                &mut y,
            );
            y += 10;
            match h.joiner_name() {
                Some(name) => {
                    let status = if h.joiner_ready() {
                        "READY"
                    } else {
                        "NOT READY"
                    };
                    line(
                        &mut frame,
                        &format!("PLAYER JOINED: {} ({status})", name.to_uppercase()),
                        [120, 220, 120],
                        &mut y,
                    );
                }
                None => line(
                    &mut frame,
                    "WAITING FOR A PLAYER TO JOIN...",
                    [180, 190, 210],
                    &mut y,
                ),
            }
        }
        self.draw_items(&mut frame);
        self.draw_lan_error(&mut frame);
        frame
    }

    fn compose_lan_join_browse(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "JOIN LAN GAME", [220, 200, 120], 3);
        if self.lan_sessions().is_empty() {
            font::draw_text(
                &mut frame,
                40,
                90,
                "SEARCHING FOR GAMES ON THE LAN...",
                [180, 190, 210],
            );
        }
        self.draw_items(&mut frame);
        self.draw_lan_error(&mut frame);
        frame
    }

    fn compose_lan_join_lobby(&self) -> Frame {
        let mut frame = self.blank();
        fill_background(&mut frame, [10, 14, 26]);
        font::draw_text_scaled(&mut frame, 40, 24, "LAN LOBBY", [220, 200, 120], 3);
        let mut y = 84;
        let line = |frame: &mut Frame, text: &str, col: [u8; 3], y: &mut i32| {
            font::draw_text(frame, 40, *y, text, col);
            *y += font::GLYPH_H + 8;
        };
        match self.join_lobby.as_ref().and_then(|j| j.welcome()) {
            Some(w) => {
                line(
                    &mut frame,
                    &format!("HOST: {}", w.host_name.to_uppercase()),
                    [210, 214, 222],
                    &mut y,
                );
                line(
                    &mut frame,
                    &format!("MAP: {}", w.map.to_uppercase()),
                    [210, 214, 222],
                    &mut y,
                );
                line(
                    &mut frame,
                    &format!("CREDITS: {}", w.credits),
                    [210, 214, 222],
                    &mut y,
                );
                y += 10;
                let ready = self
                    .join_lobby
                    .as_ref()
                    .map(|j| j.is_ready())
                    .unwrap_or(false);
                line(
                    &mut frame,
                    if ready {
                        "READY - WAITING FOR HOST TO START..."
                    } else {
                        "CLICK READY WHEN SET"
                    },
                    [180, 190, 210],
                    &mut y,
                );
            }
            None => line(&mut frame, "CONTACTING HOST...", [180, 190, 210], &mut y),
        }
        self.draw_items(&mut frame);
        self.draw_lan_error(&mut frame);
        frame
    }

    fn compose_net_ended(&self) -> Frame {
        // The frozen game frame, dimmed, with the reason + Continue over it.
        let mut frame = self.compose_ingame();
        dim(&mut frame, 45);
        let title = if self.net_end_message.is_empty() {
            "CONNECTION LOST"
        } else {
            &self.net_end_message
        };
        let scale = 4;
        let tw = font::text_width(title) * scale;
        font::draw_text_scaled(
            &mut frame,
            (self.viewport_w as i32 - tw) / 2,
            self.viewport_h as i32 / 2 - 120,
            title,
            [235, 120, 100],
            scale,
        );
        self.draw_items(&mut frame);
        frame
    }

    fn draw_lan_error(&self, frame: &mut Frame) {
        if let Some(e) = &self.lan_error {
            font::draw_text(
                frame,
                40,
                self.viewport_h as i32 - 20,
                &format!("ERROR: {}", e.to_uppercase()),
                [220, 100, 100],
            );
        }
    }

    // -------------------------------------------------------------------------
    // Layout: the single source of geometry for both draw and hit-test.
    // -------------------------------------------------------------------------

    fn items(&self) -> Vec<Item> {
        match self.state {
            AppState::MainMenu => self.items_main_menu(),
            AppState::SkirmishSetup => self.items_setup(),
            AppState::CampaignList => self.items_campaign_list(),
            AppState::Briefing => self.items_briefing(),
            AppState::MultiplayerMenu => self.items_multiplayer_menu(),
            AppState::LanHostSetup => self.items_lan_host_setup(),
            AppState::LanHostLobby => self.items_lan_host_lobby(),
            AppState::LanJoinBrowse => self.items_lan_join_browse(),
            AppState::LanJoinLobby => self.items_lan_join_lobby(),
            AppState::Paused => self.items_paused(),
            AppState::GameOver => self.items_gameover(),
            AppState::NetEnded => self.items_net_ended(),
            AppState::InGame => Vec::new(),
        }
    }

    fn items_main_menu(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let bw = 260;
        let bh = 36;
        let x0 = cx - bw / 2;
        let mut y = self.viewport_h as i32 / 2 - 20;
        let gap = bh + 14;
        let mut items = Vec::new();
        let push = |label: &str, action: Action, enabled: bool, y: i32| Item {
            rect: (x0, y, x0 + bw, y + bh),
            label: label.to_string(),
            action,
            enabled,
            selected: false,
        };
        items.push(push("SKIRMISH", Action::GotoSkirmish, true, y));
        y += gap;
        let has_campaign = self.campaign_factory.is_some() && !self.campaign_missions.is_empty();
        if has_campaign {
            items.push(push("CAMPAIGN", Action::GotoCampaign, true, y));
        } else {
            // No campaign factory (asset-free menu goldens) keeps the original
            // disabled label + geometry, so the main-menu golden is unchanged.
            items.push(push("CAMPAIGN - COMING SOON", Action::Disabled, false, y));
        }
        y += gap;
        // The Multiplayer row exists only when a LAN factory is installed
        // (M8-B) — asset-free menu goldens construct the App without one, so
        // their button list and geometry are byte-identical to before.
        if self.lan_factory.is_some() {
            items.push(push("MULTIPLAYER", Action::GotoMultiplayer, true, y));
            y += gap;
        }
        items.push(push("QUIT", Action::Quit, true, y));
        items
    }

    fn items_multiplayer_menu(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let bw = 260;
        let bh = 36;
        let x0 = cx - bw / 2;
        let mut y = self.viewport_h as i32 / 2 - 60;
        let gap = bh + 14;
        let mut items = Vec::new();
        let mut push = |label: &str, action: Action| {
            items.push(Item {
                rect: (x0, y, x0 + bw, y + bh),
                label: label.to_string(),
                action,
                enabled: true,
                selected: false,
            });
            y += gap;
        };
        push("HOST GAME", Action::GotoLanHostSetup);
        push("JOIN GAME", Action::GotoLanJoin);
        push("BACK", Action::BackToMenu);
        items
    }

    fn items_lan_host_setup(&self) -> Vec<Item> {
        let mut items = Vec::new();
        // Map rows (same scrollable list geometry as the skirmish setup).
        let list_x0 = 40;
        let list_x1 = 40 + 300;
        let list_y0 = 84;
        let end = (self.map_scroll + MAP_ROWS).min(self.maps.len());
        for (row, idx) in (self.map_scroll..end).enumerate() {
            let y = list_y0 + row as i32 * ROW_H;
            let map = &self.maps[idx];
            items.push(Item {
                rect: (list_x0, y, list_x1, y + ROW_H - 1),
                label: format!("{} ({}P)", trunc(&map.name, 30), map.players),
                action: Action::SelectMap(idx),
                enabled: true,
                selected: idx == self.config.map,
            });
        }
        if self.maps.len() > MAP_ROWS {
            let by = list_y0 + MAP_ROWS as i32 * ROW_H + 4;
            items.push(Item {
                rect: (list_x0, by, list_x0 + 60, by + ROW_H),
                label: "UP".to_string(),
                action: Action::ScrollMaps(-1),
                enabled: self.map_scroll > 0,
                selected: false,
            });
            items.push(Item {
                rect: (list_x0 + 70, by, list_x0 + 130, by + ROW_H),
                label: "DOWN".to_string(),
                action: Action::ScrollMaps(1),
                enabled: self.map_scroll + MAP_ROWS < self.maps.len(),
                selected: false,
            });
        }
        let ox0 = 40;
        let ow = 300;
        let oh = 28;
        let mut oy = list_y0 + MAP_ROWS as i32 * ROW_H + 32;
        let gap = oh + 8;
        items.push(Item {
            rect: (ox0, oy, ox0 + ow, oy + oh),
            label: format!(
                "CREDITS: {}",
                CREDITS[self.config.credits.min(CREDITS.len() - 1)]
            ),
            action: Action::Cycle(Field::Credits, 1),
            enabled: true,
            selected: false,
        });
        oy += gap;
        items.push(Item {
            rect: (ox0, oy, ox0 + 140, oy + oh + 4),
            label: "CREATE GAME".to_string(),
            action: Action::LanCreate,
            enabled: !self.maps.is_empty(),
            selected: false,
        });
        items.push(Item {
            rect: (ox0 + 160, oy, ox0 + 300, oy + oh + 4),
            label: "BACK".to_string(),
            action: Action::GotoMultiplayer,
            enabled: true,
            selected: false,
        });
        items
    }

    fn items_lan_host_lobby(&self) -> Vec<Item> {
        let ox0 = 40;
        let oh = 32;
        let oy = self.viewport_h as i32 - 80;
        vec![
            Item {
                rect: (ox0, oy, ox0 + 180, oy + oh),
                label: "START GAME".to_string(),
                action: Action::LanStart,
                enabled: self
                    .host_lobby
                    .as_ref()
                    .map(|h| h.can_start())
                    .unwrap_or(false),
                selected: false,
            },
            Item {
                rect: (ox0 + 200, oy, ox0 + 340, oy + oh),
                label: "CANCEL".to_string(),
                action: Action::LanCancelHost,
                enabled: true,
                selected: false,
            },
        ]
    }

    fn items_lan_join_browse(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let x0 = 40;
        let w = (self.viewport_w as i32 - 80).min(560);
        let mut y = 90;
        for (i, s) in self.lan_sessions().iter().enumerate() {
            let label = if s.compatible {
                format!("{} - {}", trunc(&s.name, 20), trunc(&s.map, 24))
            } else {
                format!("{} - INCOMPATIBLE VERSION", trunc(&s.name, 20))
            };
            items.push(Item {
                rect: (x0, y, x0 + w, y + 24),
                label,
                action: Action::LanJoinSession(i),
                enabled: s.compatible,
                selected: false,
            });
            y += 28;
        }
        y += 12;
        items.push(Item {
            rect: (x0, y, x0 + 140, y + 32),
            label: "BACK".to_string(),
            action: Action::GotoMultiplayer,
            enabled: true,
            selected: false,
        });
        items
    }

    fn items_lan_join_lobby(&self) -> Vec<Item> {
        let ox0 = 40;
        let oh = 32;
        let oy = self.viewport_h as i32 - 80;
        let ready = self
            .join_lobby
            .as_ref()
            .map(|j| j.is_ready())
            .unwrap_or(false);
        let welcomed = self
            .join_lobby
            .as_ref()
            .map(|j| j.welcome().is_some())
            .unwrap_or(false);
        vec![
            Item {
                rect: (ox0, oy, ox0 + 180, oy + oh),
                label: if ready { "WAITING..." } else { "READY" }.to_string(),
                action: Action::LanReady,
                enabled: welcomed && !ready,
                selected: ready,
            },
            Item {
                rect: (ox0 + 200, oy, ox0 + 340, oy + oh),
                label: "LEAVE".to_string(),
                action: Action::LanLeaveJoin,
                enabled: true,
                selected: false,
            },
        ]
    }

    fn items_net_ended(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let bw = 240;
        let bh = 36;
        let x0 = cx - bw / 2;
        let y = self.viewport_h as i32 - 120;
        vec![Item {
            rect: (x0, y, x0 + bw, y + bh),
            label: "CONTINUE".to_string(),
            action: Action::NetContinue,
            enabled: true,
            selected: false,
        }]
    }

    fn items_campaign_list(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let x0 = 40;
        let w = (self.viewport_w as i32 - 80).min(560);
        let mut y = 90;
        for (i, m) in self.campaign_missions.iter().enumerate() {
            items.push(Item {
                rect: (x0, y, x0 + w, y + 24),
                label: format!("MISSION {} - {}", i + 1, trunc(&m.name, 40)),
                action: Action::SelectMission(i),
                enabled: true,
                selected: i == self.campaign_current,
            });
            y += 28;
        }
        y += 12;
        items.push(Item {
            rect: (x0, y, x0 + 140, y + 32),
            label: "BACK".to_string(),
            action: Action::BackToMenu,
            enabled: true,
            selected: false,
        });
        items
    }

    fn items_briefing(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let mut items = Vec::new();
        // START / BACK come **first in list order** so the keyboard focus default
        // (focus 0 = START MISSION) is unchanged by the added difficulty row; the
        // difficulty buttons are laid out visually *above* them (rect y), decoupling
        // draw position from focus order.
        let y = self.viewport_h as i32 - 80;
        items.push(Item {
            rect: (cx - 260, y, cx - 20, y + 34),
            label: "START MISSION".to_string(),
            action: Action::StartMission,
            enabled: true,
            selected: false,
        });
        items.push(Item {
            rect: (cx + 20, y, cx + 200, y + 34),
            label: "BACK".to_string(),
            action: Action::GotoCampaign, // return to the mission list
            enabled: true,
            selected: false,
        });
        // Difficulty selector: one button per level (Easy/Normal/Hard), the current
        // one highlighted. Drawn above the Start/Back row.
        let dy = self.viewport_h as i32 - 130;
        let bw = 150;
        let gap = 8;
        let total = DIFFICULTIES.len() as i32 * (bw + gap) - gap;
        let dx0 = cx - total / 2;
        for (i, (label, _)) in DIFFICULTIES.iter().enumerate() {
            let x0 = dx0 + i as i32 * (bw + gap);
            items.push(Item {
                rect: (x0, dy, x0 + bw, dy + 30),
                label: (*label).to_string(),
                action: Action::SetCampaignDifficulty(i),
                enabled: true,
                selected: i == self.campaign_difficulty,
            });
        }
        items
    }

    fn items_setup(&self) -> Vec<Item> {
        let mut items = Vec::new();
        // Map rows (scrollable window).
        let list_x0 = 40;
        let list_x1 = 40 + 300;
        let list_y0 = 84;
        let end = (self.map_scroll + MAP_ROWS).min(self.maps.len());
        for (row, idx) in (self.map_scroll..end).enumerate() {
            let y = list_y0 + row as i32 * ROW_H;
            let map = &self.maps[idx];
            items.push(Item {
                rect: (list_x0, y, list_x1, y + ROW_H - 1),
                label: format!("{} ({}P)", trunc(&map.name, 30), map.players),
                action: Action::SelectMap(idx),
                enabled: true,
                selected: idx == self.config.map,
            });
        }
        // Scroll buttons (only if the list overflows).
        if self.maps.len() > MAP_ROWS {
            let by = list_y0 + MAP_ROWS as i32 * ROW_H + 4;
            items.push(Item {
                rect: (list_x0, by, list_x0 + 60, by + ROW_H),
                label: "UP".to_string(),
                action: Action::ScrollMaps(-1),
                enabled: self.map_scroll > 0,
                selected: false,
            });
            items.push(Item {
                rect: (list_x0 + 70, by, list_x0 + 130, by + ROW_H),
                label: "DOWN".to_string(),
                action: Action::ScrollMaps(1),
                enabled: self.map_scroll + MAP_ROWS < self.maps.len(),
                selected: false,
            });
        }

        // Option cyclers, laid out in a column below the map list.
        let ox0 = 40;
        let ow = 300;
        let oh = 28;
        let mut oy = list_y0 + MAP_ROWS as i32 * ROW_H + 32;
        let gap = oh + 8;
        let diff = DIFFICULTIES[self.config.difficulty].0;
        let house = HOUSES[self.config.house].0;
        let color = COLORS[self.config.color].0;
        let credits = CREDITS[self.config.credits];
        // Each cycler: a value box (click = cycle forward) — simple + testable.
        let cycler = |items: &mut Vec<Item>, label: &str, value: String, field: Field, oy: i32| {
            items.push(Item {
                rect: (ox0, oy, ox0 + ow, oy + oh),
                label: format!("{label}: {value}"),
                action: Action::Cycle(field, 1),
                enabled: true,
                selected: false,
            });
        };
        cycler(
            &mut items,
            "DIFFICULTY",
            diff.to_string(),
            Field::Difficulty,
            oy,
        );
        oy += gap;
        cycler(&mut items, "HOUSE", house.to_string(), Field::House, oy);
        oy += gap;
        cycler(&mut items, "COLOR", color.to_string(), Field::Color, oy);
        oy += gap;
        cycler(
            &mut items,
            "CREDITS",
            credits.to_string(),
            Field::Credits,
            oy,
        );
        oy += gap;
        items.push(Item {
            rect: (ox0, oy, ox0 + ow, oy + oh),
            label: format!(
                "CLASSIC RADAR: {}",
                if self.config.classic_radar {
                    "ON"
                } else {
                    "OFF"
                }
            ),
            action: Action::ToggleRadar,
            enabled: true,
            selected: false,
        });
        oy += gap;

        // Start / Back.
        items.push(Item {
            rect: (ox0, oy, ox0 + 140, oy + oh + 4),
            label: "START".to_string(),
            action: Action::StartGame,
            enabled: !self.maps.is_empty(),
            selected: false,
        });
        items.push(Item {
            rect: (ox0 + 160, oy, ox0 + 300, oy + oh + 4),
            label: "BACK".to_string(),
            action: Action::BackToMenu,
            enabled: true,
            selected: false,
        });
        items
    }

    fn items_paused(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let bw = 240;
        let bh = 36;
        let x0 = cx - bw / 2;
        let mut y = self.viewport_h as i32 / 2 - 30;
        vec![
            {
                let it = Item {
                    rect: (x0, y, x0 + bw, y + bh),
                    label: "RESUME".to_string(),
                    action: Action::Resume,
                    enabled: true,
                    selected: false,
                };
                y += bh + 14;
                it
            },
            Item {
                rect: (x0, y, x0 + bw, y + bh),
                label: "QUIT TO MENU".to_string(),
                action: Action::QuitToMenu,
                enabled: true,
                selected: false,
            },
        ]
    }

    fn items_gameover(&self) -> Vec<Item> {
        let cx = self.viewport_w as i32 / 2;
        let bw = 240;
        let bh = 36;
        let x0 = cx - bw / 2;
        // Skirmish (non-campaign) keeps the *exact* original single-CONTINUE
        // layout at `vh - 120`, so the game-over golden is byte-identical.
        if !self.in_campaign {
            let y = self.viewport_h as i32 - 120;
            return vec![Item {
                rect: (x0, y, x0 + bw, y + bh),
                label: "CONTINUE".to_string(),
                action: Action::Continue,
                enabled: true,
                selected: false,
            }];
        }
        // Campaign: a defeat offers RETRY above the CONTINUE (advance / retry).
        let defeat = self
            .core
            .as_ref()
            .map(|c| c.game_over() == ra_sim::GameOver::Defeat)
            .unwrap_or(false);
        let mut y = self.viewport_h as i32 - 130;
        let mut items = Vec::new();
        if defeat {
            items.push(Item {
                rect: (x0, y, x0 + bw, y + bh),
                label: "RETRY MISSION".to_string(),
                action: Action::RetryMission,
                enabled: true,
                selected: false,
            });
            y += bh + 14;
        }
        items.push(Item {
            rect: (x0, y, x0 + bw, y + bh),
            label: if defeat { "CONTINUE" } else { "NEXT MISSION" }.to_string(),
            action: Action::Continue,
            enabled: true,
            selected: false,
        });
        items
    }
}

/// Greedy word-wrap `text` into lines of at most `cols` characters.
fn wrap_text(text: &str, cols: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.len() + 1 + word.len() <= cols {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Truncate a string to `max` chars for a fixed-width row.
fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Fill the whole frame with a flat background colour.
fn fill_background(frame: &mut Frame, rgb: [u8; 3]) {
    fill_rect(
        frame,
        0,
        0,
        frame.width as i32 - 1,
        frame.height as i32 - 1,
        rgb,
    );
}

/// Darken every pixel toward black by `amount`/255 (a dim overlay for pause).
fn dim(frame: &mut Frame, keep: u16) {
    for px in frame.pixels.chunks_exact_mut(4) {
        px[0] = (px[0] as u16 * keep / 255) as u8;
        px[1] = (px[1] as u16 * keep / 255) as u8;
        px[2] = (px[2] as u16 * keep / 255) as u8;
    }
}

/// Blit `src` into `dst` fit inside a `w×h` box at `(x, y)` with integer
/// nearest-neighbour scaling, preserving aspect ratio and centring. A `src` with
/// zero dimensions draws a placeholder cross.
fn blit_fit(dst: &mut Frame, src: &RgbaImage, x: i32, y: i32, w: i32, h: i32) {
    if src.width == 0 || src.height == 0 {
        // Placeholder: a dim diagonal to signal "no preview".
        for i in 0..w.min(h) {
            let px = x + i;
            let py = y + i * h / w.max(1);
            if px >= 0 && py >= 0 && (px as u32) < dst.width && (py as u32) < dst.height {
                let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
                dst.pixels[di] = 40;
                dst.pixels[di + 1] = 46;
                dst.pixels[di + 2] = 60;
                dst.pixels[di + 3] = 255;
            }
        }
        return;
    }
    // Scale to fit while preserving aspect.
    let sx = w * 1000 / src.width as i32;
    let sy = h * 1000 / src.height as i32;
    let scale = sx.min(sy).max(1); // permille
    let out_w = (src.width as i32 * scale / 1000).max(1);
    let out_h = (src.height as i32 * scale / 1000).max(1);
    let ox = x + (w - out_w) / 2;
    let oy = y + (h - out_h) / 2;
    for dyp in 0..out_h {
        let syp = (dyp * 1000 / scale).min(src.height as i32 - 1);
        for dxp in 0..out_w {
            let sxp = (dxp * 1000 / scale).min(src.width as i32 - 1);
            let si = ((syp as u32 * src.width + sxp as u32) * 4) as usize;
            let px = ox + dxp;
            let py = oy + dyp;
            if px < 0 || py < 0 || px as u32 >= dst.width || py as u32 >= dst.height {
                continue;
            }
            let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
            dst.pixels[di] = src.pixels[si];
            dst.pixels[di + 1] = src.pixels[si + 1];
            dst.pixels[di + 2] = src.pixels[si + 2];
            dst.pixels[di + 3] = 255;
        }
    }
}
