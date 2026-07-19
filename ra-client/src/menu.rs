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

/// Player colour options (label + the house index whose remap gives that colour,
/// per the radar palette). Lets colour be chosen independently of house.
pub const COLORS: [(&str, u8); 8] = [
    ("GOLD", 1),
    ("RED", 2),
    ("BLUE", 3),
    ("GREEN", 4),
    ("ORANGE", 5),
    ("PURPLE", 6),
    ("TEAL", 7),
    ("GREY", 0),
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
    /// House index whose colour-remap paints the player's units.
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
    /// Build one mission by scenario key, or an error string.
    fn build(&self, scenario: &str) -> Result<BuiltMission, String>;
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
    /// A running game (delegates to [`AppCore`]).
    InGame,
    /// In-game pause overlay (sim frozen).
    Paused,
    /// Victory/Defeat resolved; awaiting Continue (sim frozen).
    GameOver,
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
    /// Briefing text for the current mission.
    briefing_text: String,
    /// Whether the running game is a campaign mission (vs. skirmish).
    in_campaign: bool,
    /// A mission core built eagerly by the briefing screen, ready to play.
    pending_core: Option<(AppCore, CellCoord)>,
    core: Option<AppCore>,
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
            briefing_text: String::new(),
            in_campaign: false,
            pending_core: None,
            core: None,
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

    /// The scanned campaign mission list (for tests / the shell).
    pub fn campaign_missions(&self) -> &[CampaignEntry] {
        &self.campaign_missions
    }

    /// The current briefing text (for tests).
    pub fn briefing_text(&self) -> &str {
        &self.briefing_text
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
    /// time — no state leaks between games).
    pub fn quit_to_menu(&mut self) {
        self.core = None;
        self.state = AppState::MainMenu;
        self.focus = 0;
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
            | AppState::Paused
            | AppState::GameOver => self.handle_menu(ev),
        }
    }

    fn handle_ingame(&mut self, ev: InputEvent) {
        // Esc first cancels an armed sell/repair mode (the original's cursor
        // mode); only when no such mode is active does it open the pause overlay.
        if matches!(ev, InputEvent::KeyDown(Key::Menu)) {
            if let Some(c) = self.core.as_mut() {
                if c.sell_mode() || c.repair_mode() {
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
                    AppState::SkirmishSetup | AppState::CampaignList => {
                        self.state = AppState::MainMenu
                    }
                    AppState::Briefing => self.state = AppState::CampaignList,
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
            Action::Quit => self.quit = true,
            Action::SelectMap(i) => self.select_map(i),
            Action::ScrollMaps(d) => self.scroll_maps(d),
            Action::Cycle(field, d) => self.cycle(field, d),
            Action::ToggleRadar => self.config.classic_radar = !self.config.classic_radar,
            Action::StartGame => self.start_game(),
            Action::SelectMission(i) => self.goto_briefing(i),
            Action::StartMission => self.start_mission(self.campaign_current),
            Action::RetryMission => self.start_mission(self.campaign_current),
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
                if let Ok(built) = f.build(&entry.scenario) {
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
                match f.build(&entry.scenario) {
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
        if self.state == AppState::InGame {
            if let Some(c) = self.core.as_mut() {
                c.update(dt_ms);
                let mut cues = c.drain_sounds();
                self.sounds.append(&mut cues);
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
            AppState::InGame => self.compose_ingame(),
            AppState::Paused => self.compose_paused(),
            AppState::GameOver => self.compose_gameover(),
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

    // -------------------------------------------------------------------------
    // Layout: the single source of geometry for both draw and hit-test.
    // -------------------------------------------------------------------------

    fn items(&self) -> Vec<Item> {
        match self.state {
            AppState::MainMenu => self.items_main_menu(),
            AppState::SkirmishSetup => self.items_setup(),
            AppState::CampaignList => self.items_campaign_list(),
            AppState::Briefing => self.items_briefing(),
            AppState::Paused => self.items_paused(),
            AppState::GameOver => self.items_gameover(),
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
        items.push(push("QUIT", Action::Quit, true, y));
        items
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
        let y = self.viewport_h as i32 - 80;
        vec![
            Item {
                rect: (cx - 260, y, cx - 20, y + 34),
                label: "START MISSION".to_string(),
                action: Action::StartMission,
                enabled: true,
                selected: false,
            },
            Item {
                rect: (cx + 20, y, cx + 200, y + 34),
                label: "BACK".to_string(),
                action: Action::GotoCampaign, // return to the mission list
                enabled: true,
                selected: false,
            },
        ]
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
