//! `AppCore` — the windowless heart of the client (DESIGN.md §4.8). All UI and
//! game-view behavior lives here so every corner of it is reachable from tests
//! without a window: feed it [`InputEvent`]s, advance it with virtual time via
//! [`AppCore::update`], and read pixels back with [`AppCore::compose`]. The
//! macroquad shell is only a thin adapter over this seam.
//!
//! M2 gave it the terrain camera. M3 adds the simulation view: it owns a
//! [`ra_sim::World`], steps it at a fixed 15 Hz on **virtual** time, renders
//! units at interpolated positions with house-colour remap, and turns
//! left-drag selection + right-click into [`Command`]s that flow through the
//! deterministic pipeline. A core built with [`AppCore::new`] (terrain only)
//! has no units, so it behaves exactly as it did at M2.

use std::collections::BTreeMap;

use ra_data::house::{identity_remap, RemapTable};
use ra_formats::tmpl::ICON_WIDTH;
use ra_net::{CommandTransport, LocalTransport, PollResult};
use ra_sim::coords::{CellCoord, Facing, WorldCoord, LEPTONS_PER_CELL};
use ra_sim::{BuildItem, GameOver, Handle, Passability, ProdKind, SuperKind, Target, World};

use crate::compositor::{viewport_rgba, IndexedImage, Palette, RgbaImage};
use crate::font;
use crate::input::{InputEvent, Key, MouseButton, Rect};
use crate::unit_render::{
    draw_health_bar, draw_rect_outline, draw_sprite_centered, draw_sprite_shadow,
    draw_sprite_topleft, fill_rect, infantry_frame, InfAction, InfantryAnim, UnitSprite,
};

/// Sim commands the UI emits. Re-exported from the sim so the whole app speaks
/// one command vocabulary (DESIGN.md §4.4).
pub use ra_sim::Command;

/// Width of the build sidebar strip, in viewport pixels (§4.9 M5: "functional,
/// not pretty"). Only present when the sidebar is enabled (game mode); the M2/M3
/// terrain and combat test paths never enable it, so `compose_camera` there is
/// byte-identical to before.
pub const SIDEBAR_W: u32 = 130;
/// Row height for one sidebar buildable entry, in pixels.
const SIDEBAR_ROW_H: i32 = 22;
/// Sidebar background colour.
const SIDEBAR_BG: [u8; 3] = [24, 24, 28];
/// Gold-ore render colour (tactical overlay).
const ORE_GOLD_RGB: [u8; 3] = [196, 160, 40];
/// Gem-ore render colour.
const ORE_GEM_RGB: [u8; 3] = [70, 150, 210];

/// SELL / REPAIR mode-button size (M7.9 P1). Two small buttons stacked in the
/// right of the sidebar header, drawn over otherwise-blank header background so
/// the radar/build-row geometry (and every geometry-based test) is unchanged.
const MODE_BTN_W: i32 = 34;
const MODE_BTN_H: i32 = 9;

/// Radar minimap panel side length, in sidebar pixels (a square).
const RADAR_SIZE: i32 = 120;
/// Hi-res sidebar cameo dimensions (`<NAME>ICON.SHP`, 64×48 in `hires.mix`).
const CAMEO_W: i32 = 64;
const CAMEO_H: i32 = 48;
/// Taller sidebar row when cameo art is shown (cameo height + label strip).
const SIDEBAR_ROW_H_CAMEO: i32 = CAMEO_H + 12;

/// Two-strip sidebar (M7.7 P6). The build list is split into two columns like
/// the original's `Column[COLUMNS=2]` (`sidebar.h`): structures on the left,
/// units on the right (`SidebarClass::Which_Column`). Each column is one cameo
/// wide and scrolls independently.
const SIDEBAR_COLUMNS: usize = 2;
/// One build-column width, in sidebar pixels (a cameo). Two of these (128) fit
/// inside `SIDEBAR_W` (130) with a 1px margin each side, so `tactical_width`
/// (hence every `compose`/`compose_game` camera golden) is unchanged.
const COLUMN_W: i32 = CAMEO_W;
/// Height of the per-column scroll-button row at the bottom of the strips.
const SCROLL_BTN_H: i32 = 14;
/// Width of a single up/down scroll arrow button.
const SCROLL_BTN_W: i32 = 16;

/// Tesla-coil charge duration for the render glow ramp — mirrors the sim's
/// `TESLA_CHARGE_TICKS` (cosmetic only; the sim owns the real timing).
const TESLA_CHARGE_MAX: i32 = 15;

/// Superweapon ready/charge indicator button height (marquee arc). Full-width
/// buttons stacked at the bottom of the sidebar strip; only drawn when the player
/// owns a superweapon structure (absent from every pinned frame → no golden churn).
const SW_BTN_H: i32 = 22;
/// Chronosphere warp-flash effect lifetime, in synthetic frames.
const WARP_FRAMES: u64 = 9;

/// Approximate classic-RA per-house marker colours for the radar, indexed by
/// house id (Greece gold, USSR red, …); grey for anything out of range.
const HOUSE_DOT: [[u8; 3]; 8] = [
    [216, 180, 40], // 0 Spain
    [216, 180, 40], // 1 Greece — gold
    [200, 40, 40],  // 2 USSR — red
    [60, 120, 220], // 3 England — blue
    [90, 200, 90],  // 4 Ukraine — green
    [220, 120, 40], // 5 Germany — orange
    [180, 80, 200], // 6 France — purple
    [60, 200, 200], // 7 Turkey — teal
];

/// Radar/marker colour for a house.
fn house_dot(house: u8) -> [u8; 3] {
    HOUSE_DOT
        .get(house as usize)
        .copied()
        .unwrap_or([160, 160, 160])
}

/// A single buildable entry the sidebar exposes (also the queryable surface
/// tests drive the build UI through).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarItem {
    /// The sim command payload this row builds.
    pub item: BuildItem,
    /// Short display name (e.g. `"POWR"`, `"2TNK"`).
    pub name: String,
    /// Build cost in credits.
    pub cost: i32,
    /// Whether the player can start it right now (prereqs + factory + funds).
    pub buildable: bool,
    /// Build progress in permille if this item is currently in production.
    pub progress: Option<i32>,
    /// Whether this (structure) is finished and awaiting placement.
    pub ready: bool,
}

/// The composed output of a frame — an RGBA image ready to upload as a texture.
pub type Frame = RgbaImage;

/// The simulation tick rate (`TICKS_PER_SECOND 15`, DESIGN.md §5).
pub const TICKS_PER_SECOND: u64 = 15;

/// Pixels per cell edge in the terrain raster (SHP icon size).
pub(crate) const CELL_PIXELS: i32 = ICON_WIDTH as i32;

/// Maximum sim ticks stepped in a single [`AppCore::update`] call. Real frames
/// advance virtual time by ~16 ms (≈¼ tick), so this only bites under a
/// pathologically large `dt`, where we deliberately drop the excess rather than
/// spin catching up thousands of ticks — a documented cap (DESIGN.md §4.8
/// structural finding). Determinism is unaffected: the same `dt` sequence
/// always produces the same tick count.
const MAX_CATCHUP_TICKS: u32 = 8;

/// Largest viewport dimension AppCore will accept from a `Resize`, per axis. An
/// unbounded resize would let a compose allocate `w*h*4` bytes without limit;
/// this caps a single frame near a quarter-gigabyte. Documented structural
/// bound requested by ra-tester.
pub const MAX_VIEWPORT_DIM: u32 = 8192;

/// Default camera scroll speed, in map pixels per second.
const DEFAULT_SCROLL_SPEED: f32 = 640.0;
/// Distance from a viewport edge (pixels) within which the pointer edge-scrolls.
const EDGE_MARGIN: i32 = 16;
/// Below this drag size (pixels) a left-release is treated as a click, not a box.
const CLICK_SLOP: i32 = 3;
/// Click-select pick radius, in map pixels (full-cell — vehicles/buildings-sized).
const PICK_RADIUS: i32 = CELL_PIXELS;

/// Click pick radius scaled to a unit's on-screen footprint (M7.7 P0d). Infantry
/// draw at roughly a sub-cell size (their selection marker half is
/// `CELL_PIXELS/4` and health bar `CELL_PIXELS/2`), so a full-cell hitbox would
/// let a click land a whole cell away and still grab an infantryman — and would
/// out-prioritise a co-located vehicle by proximity fluke. Halve it so the pick
/// area tracks the visible soldier.
fn pick_radius(is_infantry: bool) -> i32 {
    if is_infantry {
        CELL_PIXELS / 2
    } else {
        PICK_RADIUS
    }
}

/// Selection marker / drag-box colour (classic RA green).
const SELECT_RGB: [u8; 3] = [0, 255, 0];

/// An in-progress left-drag box, in viewport pixels.
#[derive(Clone, Copy, Debug)]
struct DragBox {
    start: (i32, i32),
    cur: (i32, i32),
}

/// Milliseconds each cosmetic-animation frame is shown (≈ the original's anim
/// rate). Purely presentation; never touches the sim clock.
const FX_FRAME_MS: u64 = 55;

/// Blink period (ms) of the repairing-building wrench overlay and its cursor.
/// Purely presentation (the original toggles `IsWrenchVisible` per repair step).
const WRENCH_BLINK_MS: u64 = 180;

/// A client-side cosmetic animation instance (DESIGN.md §4.2: the cosmetic layer
/// is derived from sim state and driven by a *virtual* clock, never feeding back
/// into the sim). Spawned by diffing sim state across a tick (a vanished unit /
/// building → explosion; a new building → construction buildup).
#[derive(Clone, Copy, Debug)]
struct Effect {
    kind: EffectKind,
    /// World position (leptons). Explosions anchor centred here; buildups anchor
    /// their top-left here (matching how the building sprite is drawn).
    anchor: WorldCoord,
    /// Extra upward pixel offset applied at draw time — non-zero only for an
    /// airborne aircraft's crash explosion, so the fireball appears where the
    /// heli was flying rather than on the ground beneath it.
    lift_px: i32,
    /// The cosmetic-clock timestamp (ms) the effect began.
    start_ms: u64,
}

/// Pre-tick snapshot of one building, used by `step_tick` to diff sim state and
/// spawn the right cosmetic feedback (explosion vs. sell-deconstruct, repair
/// cue) after the tick. Purely presentation — never written back to the sim.
#[derive(Clone, Copy, Debug)]
struct PrevBuilding {
    handle: Handle,
    /// Footprint-centre coord (explosion anchor).
    center: WorldCoord,
    /// Top-left coord (buildup/deconstruct anchor, like the building sprite).
    top_left: WorldCoord,
    type_id: u32,
    house: u8,
    is_repairing: bool,
}

/// Which animation an [`Effect`] plays.
#[derive(Clone, Copy, Debug)]
enum EffectKind {
    /// A death/impact explosion (shared explosion SHP).
    Explosion,
    /// A structure's construction buildup, keyed by building type id.
    Buildup(u32),
    /// A structure's **de**construction — the same `<NAME>MAKE.SHP` buildup art
    /// played in *reverse*, keyed by building type id. This is the original's
    /// sell-back visual (`Mission_Deconstruction` → `Begin_Mode(BSTATE_CONSTRUCTION)`
    /// which reverses the shape sequence, `building.cpp:602-606`). Falls back to
    /// the shared explosion when the building has no MAKE art.
    Deconstruct(u32),
    /// A chronosphere warp flash (synthetic expanding rings; no SHP needed) at a
    /// teleport source or destination. Purely cosmetic (marquee superweapon arc).
    Warp,
}

/// A logical sound cue the UI wants played (DESIGN.md §4.2: cosmetic, derived
/// from sim state, never fed back into the sim). The shell maps each to an AUD
/// file and plays it; a headless build ignores the queue entirely. Emitting a
/// cue never draws sim RNG, so audio on/off leaves the sim hash chain identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundEvent {
    /// A weapon was fired (a projectile spawned).
    Fire,
    /// A unit or building was destroyed.
    Explosion,
    /// The player placed/finished a structure.
    ConstructionComplete,
    /// The player's power went into deficit.
    LowPower,
    /// The player selected one or more of their own units.
    Select,
    /// The player won the skirmish.
    Victory,
    /// The player lost the skirmish.
    Defeat,
    /// A building was sold — the cash-register "sell back" SFX
    /// (`VOC_CASHTURN`, `building.cpp:3840` `Mission_Deconstruction`).
    Sell,
    /// EVA "Structure sold" voice line, played once when a **player**-owned
    /// building finishes selling (`VOX_STRUCTURE_SOLD`, `building.cpp:3972`).
    StructureSold,
    /// Repair toggled **on** for an own building — the repair/click SFX
    /// (`VOC_CLICK`, `BuildingClass::Repair` → `Sound_Effect(soundid)`,
    /// `building.cpp:2770`). RA plays no EVA line for building self-repair
    /// (`VOX_REPAIRING` is the service-depot path only, `building.cpp:4313`).
    Repair,
    /// A nuclear missile was launched — EVA "Nuclear weapon launched"
    /// (`VOX_ABOMB_LAUNCH` = `ALAUNCH1.AUD`, `house.cpp` special-blast dispatch).
    NukeLaunch,
    /// The nuclear strike detonated — the heavy explosion SFX (`KABOOM25.AUD`).
    NukeImpact,
    /// The iron curtain was applied to a unit/building — the device SFX
    /// (`VOC_IRON1` = `IRONCUR9.AUD`, `house.cpp:2950`).
    IronCurtain,
    /// The chronosphere warped a unit — the teleport SFX (`VOC_CHRONO` =
    /// `CHRONO2.AUD`, `house.cpp:3053`).
    Chronosphere,
}

/// The logical mouse-cursor the UI wants shown, derived from the armed action
/// mode and the object under the pointer (DESIGN.md §4.2: cosmetic, never fed
/// back into the sim). The shell (or `compose_game`) maps each to a cursor
/// glyph. Mirrors the original's `MouseType` states for the sidebar cursor
/// (`mouse.cpp:346` `MouseControl`): the sell/repair cursors and their
/// "prohibited" variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    /// Default pointer (no action mode armed, or over the sidebar/UI).
    /// `MOUSE_NORMAL` (MOUSE.SHP frame 0).
    Normal,
    /// Sell mode, hovering a sellable own building — the "$"/dollar cursor.
    /// `MOUSE_SELL_BACK` (MOUSE.SHP frames 68..79, `mouse.cpp:358`).
    Sell,
    /// Sell mode, but not over a sellable own building — the prohibited "$".
    /// `MOUSE_NO_SELL_BACK` (MOUSE.SHP frame 119, `mouse.cpp:362`).
    NoSell,
    /// Repair mode, hovering a repairable own building — the wrench cursor.
    /// `MOUSE_REPAIR` (MOUSE.SHP frames 35..58, `mouse.cpp:360`).
    Repair,
    /// Repair mode, but not over a repairable own building — the prohibited
    /// wrench. `MOUSE_NO_REPAIR` (MOUSE.SHP frame 120, `mouse.cpp:361`).
    NoRepair,
    /// Superweapon target-select mode — the targeting reticle (marquee arc).
    /// Stands in for the original's special-weapon cursors (`MOUSE_NUCLEAR_BOMB`
    /// / `MOUSE_IRON_CURTAIN` / `MOUSE_CHRONO*`, `mouse.cpp`), rendered as a
    /// crosshair since our SHP decoder doesn't read the variable-size MOUSE.SHP.
    SuperTarget,
}

/// The windowless client core: terrain raster + camera + the sim view.
pub struct AppCore {
    raster: IndexedImage,
    palette: Palette,

    // Camera position (top-left of the viewport) in map pixels. Float because
    // this is presentation state, never simulation state.
    cam_x: f32,
    cam_y: f32,
    viewport_w: u32,
    viewport_h: u32,

    // Held scroll keys.
    left: bool,
    right: bool,
    up: bool,
    down: bool,

    // Last known pointer position (viewport pixels) and whether it is inside.
    mouse_x: i32,
    mouse_y: i32,
    mouse_inside: bool,

    scroll_speed: f32,

    // --- Simulation view (M3) ---
    world: World,
    /// Unit body sprites, indexed by `Unit::type_id`.
    sprites: Vec<UnitSprite>,
    /// House-colour remap LUTs, indexed by house; falls back to identity.
    remaps: Vec<RemapTable>,

    /// Virtual time accumulated for sim stepping, in milliseconds.
    virtual_ms: u64,
    /// Fraction (0..1000) into the current tick, for render interpolation.
    tick_frac: u32,
    /// Previous-tick unit positions (handle index → coord) for interpolation.
    prev_coords: BTreeMap<u32, WorldCoord>,

    /// Currently-selected unit handle indices.
    selected: Vec<Handle>,
    /// Active left-drag selection box, if any.
    drag: Option<DragBox>,

    /// The command transport (DESIGN.md §4.6): single player runs the
    /// zero-delay loopback, so a command submitted during tick `T` executes at
    /// `T` in submission order — byte-identical to the pre-M8 `pending` drain.
    transport: LocalTransport,
    /// Commands emitted since the last [`AppCore::drain_commands`] (for the net
    /// layer / tests to observe).
    emitted: Vec<Command>,

    // --- Build UI (M5) ---
    /// Whether the build sidebar is drawn and interactive (game mode). Off by
    /// default so terrain/combat test paths are unaffected.
    sidebar_enabled: bool,
    /// The controlled ("player") house. Selection and orders gate to it once the
    /// sidebar is enabled (fixes the M3 open question); `None` = the legacy
    /// "command whatever you select" behaviour used by the headless harnesses.
    player_house: Option<u8>,
    /// Building idle sprites, indexed by building type id.
    building_sprites: Vec<UnitSprite>,
    building_overlays: Vec<Option<UnitSprite>>,
    /// Per-unit-type infantry animation layout, indexed by `Unit::type_id`
    /// (`None` = a vehicle). Drives the Do-table frame selection in `draw_units`.
    infantry_anim: Vec<Option<InfantryAnim>>,
    /// The buildable items the sidebar lists, in display order.
    buildables: Vec<BuildItem>,
    /// Active placement mode: a completed building type id awaiting a map click.
    placing: Option<u32>,

    // --- Onboarding / cosmetic (M7) ---
    /// Whether the F1 controls-hint overlay is shown (toggled by `Key::Help`).
    /// Purely presentation; the shell shows it briefly at boot then hides it.
    show_help: bool,

    // --- Cosmetic animation layer (M7) ---
    /// The cosmetic animation clock (ms). Advances with `dt_ms` like `virtual_ms`
    /// but drives only presentation — animation frame selection and lifetime.
    /// It is *never* read by the sim, so anims on/off yields identical sim hashes.
    anim_ms: u64,
    /// Live cosmetic effects (explosions, buildups). Pruned as they expire.
    effects: Vec<Effect>,
    /// Death/impact explosion animation frames (e.g. FBALL1). Empty = no art.
    explosion_sprite: Vec<UnitSprite>,
    /// Shared helicopter rotor-blade sprite (`RROTOR.SHP`, `aircraft.cpp:521`
    /// `Draw_Rotors`). 12 frames: 0..4 spin fast (airborne), 4..12 idle slow
    /// (landed). `None` = no rotor art (the aircraft body still draws).
    rotor_sprite: Option<UnitSprite>,
    /// Construction buildup frames per building type id (`<NAME>MAKE.SHP`). A
    /// `None` entry means that building has no buildup art (skip the anim).
    buildup_sprites: Vec<Option<UnitSprite>>,
    /// Ore (gold) overlay tiles GOLD01..04 — frame = density stage. Empty = the
    /// flat-rectangle fallback is used.
    ore_gold_sprites: Vec<UnitSprite>,
    /// Gem overlay tiles GEM01..04 — frame = density stage.
    ore_gem_sprites: Vec<UnitSprite>,
    /// Sidebar cameo icons per buildable (parallel to `buildables`). Empty = the
    /// text-only sidebar rows are used.
    cameo_sprites: Vec<Option<UnitSprite>>,
    /// Whether the radar minimap panel is drawn in the sidebar (M7).
    radar_enabled: bool,
    /// "Classic radar rules" override (M7.8 skirmish option). When `true` the
    /// radar bypasses DOME power-gating and is always on (as long as the sidebar
    /// radar is enabled) — the "OFF = always-on radar" setup choice. Default
    /// `false` keeps the authentic DOME gating (QUIRKS Q10).
    radar_always_on: bool,
    /// Per-column scroll offset (`TopIndex`, `sidebar.h`) for the two-strip
    /// sidebar (M7.7 P6): `[structures, units]`. Index of the first visible row
    /// in each column's item list; clamped so a column never scrolls past its
    /// last page.
    sidebar_scroll: [usize; 2],

    // --- Audio cue queue (M7, cosmetic) ---
    /// Logical sound cues awaiting playback, drained by the shell each frame.
    sounds: Vec<SoundEvent>,
    /// Previous game-over state (to fire the win/lose cue on transition).
    prev_game_over: GameOver,
    /// Previous player low-power state (to fire the low-power cue on transition).
    prev_low_power: bool,

    // --- Sell / repair mode (M7.9 P1) ---
    /// Sell mode is armed (SELL button toggled): the next tactical left-click on
    /// an **own** building sells it (`Command::Sell`). Mirrors the original's
    /// sidebar SELL cursor mode (`sidebar.cpp`). Mutually exclusive with
    /// `repair_mode` and placement. Never emits a command for an enemy building or
    /// any unit (monkey/scripted-drive safe).
    sell_mode: bool,
    /// Repair mode is armed (REPAIR button toggled): the next tactical left-click
    /// on an own building toggles its repair (`Command::Repair`).
    repair_mode: bool,
    /// Original SELL button art (`SELL.SHP` from hires.mix): frame 0 = up,
    /// frame 1 = pressed, frame 2 = disabled. `None` = text fallback ("SELL").
    sell_button_art: Option<UnitSprite>,
    /// Original REPAIR button art (`REPAIR.SHP` from hires.mix), same frame
    /// convention. `None` = text fallback ("REP").
    repair_button_art: Option<UnitSprite>,
    /// Selection-overlay art (`SELECT.SHP` from conquer.mix). Frame 2 is
    /// `SELECT_WRENCH` (`defines.h:2525`), the pulsing wrench drawn over a
    /// repairing building (`building.cpp:520`) and reused as the repair-mode
    /// cursor glyph. `None` = a synthetic wrench primitive is drawn instead.
    wrench_art: Option<UnitSprite>,

    // --- Superweapon fire UI (marquee arc P1) ---
    /// The superweapon the player has armed for target-select, if any (clicking a
    /// ready SW indicator enters this mode; a tactical click fires it). Mutually
    /// exclusive with sell/repair/placement. `None` = no SW targeting in progress.
    sw_fire_mode: Option<SuperKind>,
    /// Chronosphere two-click gather: the unit picked by the first click, awaiting
    /// a destination click. Only meaningful while `sw_fire_mode == Chronosphere`.
    sw_chrono_source: Option<Handle>,
}

/// Frame index of the wrench in `SELECT.SHP` (`SELECT_WRENCH`, `defines.h:2525`:
/// `SELECT_INFANTRY=0, SELECT_UNIT=1, SELECT_WRENCH=2`).
const SELECT_WRENCH_FRAME: usize = 2;

impl AppCore {
    /// Build a terrain-only core (no units), exactly as M2. Camera starts at the
    /// map origin with a default viewport size.
    pub fn new(raster: IndexedImage, palette: Palette) -> AppCore {
        AppCore::with_sim(
            raster,
            palette,
            World::new(Passability::all_passable(), 0),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Build a core with a populated simulation: `world` already holds spawned
    /// units, `sprites` are indexed by `Unit::type_id`, and `remaps` by house.
    pub fn with_sim(
        raster: IndexedImage,
        palette: Palette,
        world: World,
        sprites: Vec<UnitSprite>,
        remaps: Vec<RemapTable>,
    ) -> AppCore {
        AppCore {
            raster,
            palette,
            cam_x: 0.0,
            cam_y: 0.0,
            viewport_w: 640,
            viewport_h: 400,
            left: false,
            right: false,
            up: false,
            down: false,
            mouse_x: -1,
            mouse_y: -1,
            mouse_inside: false,
            scroll_speed: DEFAULT_SCROLL_SPEED,
            world,
            sprites,
            remaps,
            virtual_ms: 0,
            tick_frac: 0,
            prev_coords: BTreeMap::new(),
            selected: Vec::new(),
            drag: None,
            transport: LocalTransport::new(),
            emitted: Vec::new(),
            sidebar_enabled: false,
            player_house: None,
            building_sprites: Vec::new(),
            building_overlays: Vec::new(),
            infantry_anim: Vec::new(),
            buildables: Vec::new(),
            placing: None,
            show_help: false,
            anim_ms: 0,
            effects: Vec::new(),
            explosion_sprite: Vec::new(),
            rotor_sprite: None,
            buildup_sprites: Vec::new(),
            ore_gold_sprites: Vec::new(),
            ore_gem_sprites: Vec::new(),
            cameo_sprites: Vec::new(),
            sidebar_scroll: [0, 0],
            radar_enabled: false,
            radar_always_on: false,
            sounds: Vec::new(),
            prev_game_over: GameOver::Ongoing,
            prev_low_power: false,
            sell_mode: false,
            repair_mode: false,
            sell_button_art: None,
            repair_button_art: None,
            wrench_art: None,
            sw_fire_mode: None,
            sw_chrono_source: None,
        }
    }

    /// Drain queued logical sound cues (for the shell's audio device). A headless
    /// build simply never calls this. Emitting/draining cues is pure presentation
    /// and never touches the sim, so audio on/off yields identical sim hashes.
    pub fn drain_sounds(&mut self) -> Vec<SoundEvent> {
        std::mem::take(&mut self.sounds)
    }

    /// Install the cosmetic animation art: a shared explosion SHP (`FBALL1`) and
    /// per-building-type buildup SHPs (`<NAME>MAKE.SHP`, indexed by building type
    /// id). Optional — with none installed, deaths/placements simply play no anim.
    pub fn set_effect_art(
        &mut self,
        explosion: Vec<UnitSprite>,
        buildups: Vec<Option<UnitSprite>>,
    ) {
        self.explosion_sprite = explosion;
        self.buildup_sprites = buildups;
    }

    /// Install the shared helicopter rotor-blade sprite (`RROTOR.SHP`). Optional —
    /// without it, airborne helis still draw (lifted body + shadow) but with no
    /// spinning blades.
    pub fn set_rotor_art(&mut self, rotor: Option<UnitSprite>) {
        self.rotor_sprite = rotor;
    }

    /// Install ore/gem overlay tile art (GOLD01..04 / GEM01..04). Frame index is
    /// the density stage (`bails - 1`). Optional — falls back to flat rectangles.
    pub fn set_ore_art(&mut self, gold: Vec<UnitSprite>, gem: Vec<UnitSprite>) {
        self.ore_gold_sprites = gold;
        self.ore_gem_sprites = gem;
    }

    /// Install sidebar cameo icons, parallel to the `buildables` list. Optional.
    pub fn set_cameo_art(&mut self, cameos: Vec<Option<UnitSprite>>) {
        self.cameo_sprites = cameos;
    }

    /// Install the original SELL / REPAIR sidebar button art (`SELL.SHP` /
    /// `REPAIR.SHP`, hires.mix). Each is a 3-frame SHP (up / pressed / disabled).
    /// When present the header draws these icon buttons at their native size
    /// (M7.9 P1 art pass, `sidebar.cpp:303-321`); when absent the text buttons
    /// ("SELL"/"REP") stay, so a build with no assets is unaffected.
    pub fn set_mode_button_art(&mut self, sell: Option<UnitSprite>, repair: Option<UnitSprite>) {
        self.sell_button_art = sell;
        self.repair_button_art = repair;
    }

    /// Install the selection-overlay art (`SELECT.SHP`, conquer.mix). Frame
    /// [`SELECT_WRENCH_FRAME`] is drawn (pulsing) over a repairing building and
    /// reused as the repair-mode cursor glyph. Optional — a synthetic wrench
    /// primitive is drawn when this is absent, so no-asset paths still show the
    /// repair indicator/cursor.
    pub fn set_indicator_art(&mut self, wrench: Option<UnitSprite>) {
        self.wrench_art = wrench;
    }

    /// Turn the radar minimap panel on (drawn at the top of the sidebar strip).
    pub fn enable_radar(&mut self) {
        self.radar_enabled = true;
    }

    // ---- Sell / repair mode (M7.9 P1) ----

    /// Whether sell mode is armed.
    pub fn sell_mode(&self) -> bool {
        self.sell_mode
    }

    /// Whether repair mode is armed.
    pub fn repair_mode(&self) -> bool {
        self.repair_mode
    }

    /// The logical cursor the UI should show right now (the sell/repair-mode
    /// cursor and its prohibited variants, or the default pointer). Derived
    /// purely from the armed mode plus what sits under the pointer — the *same*
    /// own-building gate that decides whether a click would act
    /// ([`Self::own_building_at_map`]), so the cursor can never imply an illegal
    /// action. Over the sidebar/UI or with no mode armed it is
    /// [`CursorKind::Normal`]. This is the shell-facing accessor the shell maps
    /// to a cursor shape (and that `compose_game` draws), pinned by tests.
    pub fn cursor_kind(&self) -> CursorKind {
        if !self.sell_mode && !self.repair_mode && self.sw_fire_mode.is_none() {
            return CursorKind::Normal;
        }
        // Over the sidebar strip or off-screen: the mode cursor reverts to the
        // default pointer (the original shows the sidebar/normal cursor there).
        let tw = self.tactical_width() as i32;
        if !self.mouse_inside || self.mouse_x < 0 || self.mouse_x >= tw {
            return CursorKind::Normal;
        }
        // Superweapon target-select shows the targeting reticle everywhere over the
        // tactical area (any cell/unit is a candidate target, per kind).
        if self.sw_fire_mode.is_some() {
            return CursorKind::SuperTarget;
        }
        let over_own = self.player_house.is_some_and(|house| {
            let (mx, my) = self.viewport_to_map(self.mouse_x, self.mouse_y);
            self.own_building_at_map(mx, my, house).is_some()
        });
        match (self.sell_mode, over_own) {
            (true, true) => CursorKind::Sell,
            (true, false) => CursorKind::NoSell,
            (false, true) => CursorKind::Repair,
            (false, false) => CursorKind::NoRepair,
        }
    }

    /// Arm/disarm sell mode. Arming it clears repair mode and any active
    /// placement (the three tactical-click modes are mutually exclusive), like
    /// the original's single sidebar cursor state.
    pub fn set_sell_mode(&mut self, on: bool) {
        self.sell_mode = on;
        if on {
            self.repair_mode = false;
            self.placing = None;
            self.sw_fire_mode = None;
            self.sw_chrono_source = None;
        }
    }

    /// Arm/disarm repair mode (mutually exclusive with sell mode / placement).
    pub fn set_repair_mode(&mut self, on: bool) {
        self.repair_mode = on;
        if on {
            self.sell_mode = false;
            self.placing = None;
            self.sw_fire_mode = None;
            self.sw_chrono_source = None;
        }
    }

    /// Toggle sell mode (the SELL button action).
    pub fn toggle_sell_mode(&mut self) {
        self.set_sell_mode(!self.sell_mode);
    }

    /// Toggle repair mode (the REPAIR button action).
    pub fn toggle_repair_mode(&mut self) {
        self.set_repair_mode(!self.repair_mode);
    }

    /// Cancel every tactical action mode (Esc / right-click while armed):
    /// sell, repair, and superweapon target-select (including a half-finished
    /// chronosphere two-click gather).
    fn cancel_action_modes(&mut self) {
        self.sell_mode = false;
        self.repair_mode = false;
        self.sw_fire_mode = None;
        self.sw_chrono_source = None;
    }

    // ---- Superweapon fire UI (marquee arc P1) ----

    /// The superweapon the player has armed for target-select, if any. Drives the
    /// targeting cursor + banner and the fire-on-click behaviour.
    pub fn superweapon_fire_mode(&self) -> Option<SuperKind> {
        self.sw_fire_mode
    }

    /// The chronosphere source unit picked by the first click, awaiting a
    /// destination click (test/observation seam).
    pub fn chrono_pending_source(&self) -> Option<Handle> {
        self.sw_chrono_source
    }

    /// Whether any tactical action mode is armed (sell / repair / superweapon
    /// target-select). The `App` layer uses this to decide whether Esc cancels a
    /// mode or opens the pause menu, and the shell to hide the OS cursor while our
    /// drawn mode cursor takes over.
    pub fn action_mode_armed(&self) -> bool {
        self.sell_mode || self.repair_mode || self.sw_fire_mode.is_some()
    }

    /// Arm a superweapon for target-select (the ready-indicator click). No-op
    /// unless the player owns that superweapon and it is fully charged
    /// ([`World::superweapon_ready`]); arming clears the other tactical modes.
    pub fn arm_superweapon(&mut self, kind: SuperKind) {
        let Some(house) = self.player_house else {
            return;
        };
        if !self.world.superweapon_ready(house, kind) {
            return;
        }
        self.sell_mode = false;
        self.repair_mode = false;
        self.placing = None;
        self.sw_chrono_source = None;
        self.sw_fire_mode = Some(kind);
    }

    /// Set the "classic radar rules" mode (M7.8 skirmish option). `true` keeps the
    /// authentic DOME power-gating (default); `false` makes the radar always-on
    /// (bypasses [`Self::has_radar`]'s DOME check). Cosmetic — never touches the
    /// sim, so it leaves the hash chain identical.
    pub fn set_classic_radar(&mut self, classic: bool) {
        self.radar_always_on = !classic;
    }

    /// Replace the house-colour remap table for a single house (M7.8 player-colour
    /// choice). Grows the remap vector with identity tables as needed so the index
    /// is always valid.
    pub fn set_house_remap(&mut self, house: u8, table: RemapTable) {
        let i = house as usize;
        if self.remaps.len() <= i {
            self.remaps.resize(i + 1, identity_remap());
        }
        self.remaps[i] = table;
    }

    /// Install building idle sprites, indexed by building type id.
    pub fn set_building_sprites(&mut self, sprites: Vec<UnitSprite>) {
        self.building_sprites = sprites;
    }

    /// Install the per-unit-type infantry animation layouts (indexed by
    /// `Unit::type_id`; `None` for vehicles). Enables the infantry Do-table frame
    /// selection in the unit renderer.
    pub fn set_infantry_anim(&mut self, anim: Vec<Option<InfantryAnim>>) {
        self.infantry_anim = anim;
    }

    /// Optional overlay shapes drawn over the base building sprite (the war
    /// factory's WEAP2 roof/door; building.cpp:513). Indexed like the sprites.
    pub fn set_building_overlays(&mut self, overlays: Vec<Option<UnitSprite>>) {
        self.building_overlays = overlays;
    }

    /// Enable the build sidebar for the controlled `player_house`, listing
    /// `buildables` (in display order). Turns on player-house gating of
    /// selection/orders.
    pub fn enable_sidebar(&mut self, player_house: u8, buildables: Vec<BuildItem>) {
        self.sidebar_enabled = true;
        self.player_house = Some(player_house);
        self.buildables = buildables;
    }

    /// Whether the F1 controls-hint overlay is currently visible. Exposed for
    /// tests and the shell.
    pub fn help_visible(&self) -> bool {
        self.show_help
    }

    /// Set the controls-hint overlay visibility (the shell shows it briefly at
    /// boot, then hides it; F1 toggles it thereafter).
    pub fn set_help_visible(&mut self, on: bool) {
        self.show_help = on;
    }

    /// The controlled house, if one is set.
    pub fn player_house(&self) -> Option<u8> {
        self.player_house
    }

    /// The current terminal game state (Ongoing / Victory / Defeat), surfaced for
    /// the overlay, the shell, and tests.
    pub fn game_over(&self) -> GameOver {
        self.world.game_over()
    }

    /// Whether the UI still accepts player orders — false once the game is over
    /// (§4.9 M6: "stops accepting orders").
    fn accepting_orders(&self) -> bool {
        self.world.game_over() == GameOver::Ongoing
    }

    /// The controlled house's spendable credits (0 if none / no house).
    pub fn credits(&self) -> i32 {
        self.player_house
            .map(|h| self.world.house_credits(h))
            .unwrap_or(0)
    }

    /// The controlled house's `(power_output, power_drain)`.
    pub fn power(&self) -> (i32, i32) {
        self.player_house
            .and_then(|h| self.world.house(h))
            .map(|hs| (hs.power_output, hs.power_drain))
            .unwrap_or((0, 0))
    }

    /// The sidebar's buildable entries, with live buildable/progress/ready
    /// state — the queryable surface tests drive the build UI through.
    pub fn sidebar_items(&self) -> Vec<SidebarItem> {
        let Some(house) = self.player_house else {
            return Vec::new();
        };
        let hs = self.world.house(house);
        self.buildables
            .iter()
            .filter_map(|&item| self.describe_buildable(house, hs, item))
            .collect()
    }

    fn describe_buildable(
        &self,
        house: u8,
        hs: Option<&ra_sim::House>,
        item: BuildItem,
    ) -> Option<SidebarItem> {
        let cat = &self.world.catalog;
        let (name, cost, prereq) = match item {
            BuildItem::Building(id) => {
                let p = cat.building(id)?;
                (p.name.clone(), p.cost, &p.prereq)
            }
            BuildItem::Unit(id) => {
                let p = cat.unit(id)?;
                (p.name.clone(), p.cost, &p.prereq)
            }
        };

        // In production?
        let (progress, ready) = match (item, hs) {
            (BuildItem::Building(id), Some(h)) => {
                let ready = h.ready_building == Some(id);
                let prog = h
                    .building_prod
                    .filter(|p| p.item == item)
                    .map(|p| p.progress_permille());
                (prog, ready)
            }
            (BuildItem::Unit(id), Some(h)) => {
                // Infantry build on their own barracks strip (infantry_prod);
                // vehicles on the war-factory lane (unit_prod).
                let lane = if cat.unit(id).map(|p| p.is_infantry).unwrap_or(false) {
                    &h.infantry_prod
                } else {
                    &h.unit_prod
                };
                let prog = lane
                    .filter(|p| p.item == item)
                    .map(|p| p.progress_permille());
                (prog, false)
            }
            _ => (None, false),
        };

        // Buildable now? Prereqs owned + the producing factory present + funds +
        // the lane free.
        let owns = |id: u32| hs.map(|h| h.owns_building(id)).unwrap_or(false);
        let prereqs_ok = prereq.iter().all(|&id| owns(id));
        let (factory_ok, lane_free) = match item {
            BuildItem::Building(_) => {
                let yard = self
                    .world
                    .buildings
                    .iter()
                    .any(|(_, b)| b.house == house && b.is_construction_yard && b.is_alive());
                let free = hs
                    .map(|h| h.building_prod.is_none() && h.ready_building.is_none())
                    .unwrap_or(false);
                (yard, free)
            }
            BuildItem::Unit(id) => {
                let is_inf = cat.unit(id).map(|p| p.is_infantry).unwrap_or(false);
                if is_inf {
                    let fac = self
                        .world
                        .buildings
                        .iter()
                        .any(|(_, b)| b.house == house && b.is_barracks && b.is_alive());
                    let free = hs.map(|h| h.infantry_prod.is_none()).unwrap_or(false);
                    (fac, free)
                } else {
                    let fac = self
                        .world
                        .buildings
                        .iter()
                        .any(|(_, b)| b.house == house && b.is_war_factory && b.is_alive());
                    let free = hs.map(|h| h.unit_prod.is_none()).unwrap_or(false);
                    (fac, free)
                }
            }
        };
        let funds = self.world.house_credits(house) > 0;
        let buildable =
            prereqs_ok && factory_ok && lane_free && funds && progress.is_none() && !ready;

        Some(SidebarItem {
            item,
            name,
            cost,
            buildable,
            progress,
            ready,
        })
    }

    /// Number of live cosmetic effects (explosions / buildups / deconstructs).
    /// Debug/observation seam so tests can confirm a visual effect spawned (e.g.
    /// the sell-back deconstruct anim) without reaching into private state.
    pub fn cosmetic_effect_count(&self) -> usize {
        self.effects.len()
    }

    /// Full map width in pixels.
    pub fn map_width(&self) -> u32 {
        self.raster.width
    }
    /// Full map height in pixels.
    pub fn map_height(&self) -> u32 {
        self.raster.height
    }

    /// The current viewport size in pixels.
    pub fn viewport_size(&self) -> (u32, u32) {
        (self.viewport_w, self.viewport_h)
    }

    /// Borrow the simulation world (read-only view for tests/tools).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Harness/verification hook (§4.8 scripted drives): mutable access to the
    /// sim world for constructing a controlled scenario (spawning test units and
    /// buildings) before driving it. The game shell never calls this.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Harness hook: queue a raw sim [`Command`] for the next tick, exactly as the
    /// input layer would — so a scripted drive can issue Move/Attack/Deploy
    /// without synthesizing pixel-accurate mouse events.
    pub fn inject_command(&mut self, cmd: Command) {
        self.transport.submit(cmd);
    }

    /// The current sim state hash — the determinism backbone surfaced through
    /// the seam so drives can assert same-seed-twice equality.
    pub fn sim_hash(&self) -> u64 {
        self.world.state_hash()
    }

    /// Directly set the selection to `handles` (only those the player may
    /// command are kept). A deterministic selection seam for the verification
    /// harness / tests — equivalent to a box-select but independent of the
    /// camera position.
    pub fn select_units(&mut self, handles: &[Handle]) {
        self.selected = handles
            .iter()
            .copied()
            .filter(|&h| {
                self.world
                    .units
                    .get(h)
                    .map(|u| self.selectable(u.house))
                    .unwrap_or(false)
            })
            .collect();
    }

    /// The handles of currently-selected units (ascending slot order).
    pub fn selected_handles(&self) -> Vec<Handle> {
        self.world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .collect()
    }

    /// Handle one input event.
    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::KeyDown(Key::Deploy) => self.deploy_selected(),
            InputEvent::KeyUp(Key::Deploy) => {}
            InputEvent::KeyDown(Key::Help) => self.show_help = !self.show_help,
            InputEvent::KeyUp(Key::Help) => {}
            // Esc cancels an armed sell/repair mode (the App layer only forwards
            // it here while a mode is active; otherwise it opens the pause menu).
            InputEvent::KeyDown(Key::Menu) => self.cancel_action_modes(),
            InputEvent::KeyDown(k) => self.set_key(k, true),
            InputEvent::KeyUp(k) => self.set_key(k, false),
            InputEvent::MouseMoved { x, y } => {
                self.mouse_x = x;
                self.mouse_y = y;
                self.mouse_inside = true;
                if let Some(d) = &mut self.drag {
                    d.cur = (x, y);
                }
            }
            InputEvent::MouseLeft => self.mouse_inside = false,
            InputEvent::MouseDown { button, x, y } => match button {
                MouseButton::Left => {
                    // Sidebar click? (game mode, click in the right strip)
                    if self.sidebar_enabled && x >= self.tactical_width() as i32 {
                        self.sidebar_click(x, y);
                    } else if self.sw_fire_mode.is_some() {
                        // Superweapon target-select: a tactical click picks the
                        // target (cell / unit / building, or the chrono two-click
                        // source→dest) and fires through `Command::FireSuperWeapon`.
                        self.try_fire_super_at(x, y);
                    } else if self.sell_mode {
                        // Sell mode: a tactical click sells the own building under
                        // it (no-op on enemy buildings, units, or empty ground).
                        self.try_sell_at(x, y);
                    } else if self.repair_mode {
                        // Repair mode: toggle repair on the own building clicked.
                        self.try_repair_at(x, y);
                    } else if self.placing.is_some() {
                        // Placement mode: a tactical click drops the building.
                        self.place_at(x, y);
                    } else {
                        self.drag = Some(DragBox {
                            start: (x, y),
                            cur: (x, y),
                        })
                    }
                }
                MouseButton::Right => {
                    if self.sell_mode || self.repair_mode || self.sw_fire_mode.is_some() {
                        // Right-click cancels an armed sell/repair/superweapon mode
                        // (like the original's cursor-mode cancel).
                        self.cancel_action_modes();
                    } else if self.placing.take().is_some() {
                        // Right-click cancels placement (keeps the ready building).
                    } else {
                        self.issue_order(x, y);
                    }
                }
            },
            InputEvent::MouseUp { button, x, y } => {
                if button == MouseButton::Left {
                    if let Some(d) = self.drag.take() {
                        self.finish_selection(d.start, (x, y));
                    }
                }
            }
            InputEvent::Resize { width, height } => {
                self.viewport_w = width.clamp(1, MAX_VIEWPORT_DIM);
                self.viewport_h = height.clamp(1, MAX_VIEWPORT_DIM);
                self.clamp_camera();
                self.clamp_sidebar_scroll();
            }
            InputEvent::SidebarScroll { column, up } => self.scroll_sidebar(column, up),
        }
    }

    fn set_key(&mut self, k: Key, down: bool) {
        match k {
            Key::Left => self.left = down,
            Key::Right => self.right = down,
            Key::Up => self.up = down,
            Key::Down => self.down = down,
            Key::Deploy => {} // handled at the event layer (edge-triggered)
            Key::Help => {}   // handled at the event layer (edge-triggered)
            Key::Menu | Key::Confirm => {} // menu/pause keys — handled by the App layer
        }
    }

    /// Advance the camera and the simulation by `dt_ms` of virtual time.
    pub fn update(&mut self, dt_ms: u32) {
        // Camera scroll (unchanged from M2).
        let (dx, dy) = self.scroll_direction();
        if dx != 0.0 || dy != 0.0 {
            let dt = dt_ms as f32 / 1000.0;
            self.cam_x += dx * self.scroll_speed * dt;
            self.cam_y += dy * self.scroll_speed * dt;
            self.clamp_camera();
        }

        // Cosmetic animation clock + expiry pruning (presentation only).
        self.anim_ms = self.anim_ms.saturating_add(dt_ms as u64);
        let now = self.anim_ms;
        // Disjoint field borrows: prune `effects` while reading the sprite tables.
        let expl_frames = self
            .explosion_sprite
            .first()
            .map(|s| s.frames.len() as u64)
            .unwrap_or(0);
        let buildups = &self.buildup_sprites;
        self.effects.retain(|e| {
            let frames = match e.kind {
                EffectKind::Explosion => expl_frames,
                EffectKind::Buildup(id) | EffectKind::Deconstruct(id) => buildups
                    .get(id as usize)
                    .and_then(|o| o.as_ref())
                    .map(|s| s.frames.len() as u64)
                    .unwrap_or(0),
                EffectKind::Warp => WARP_FRAMES,
            };
            frames > 0 && now.saturating_sub(e.start_ms) < frames * FX_FRAME_MS
        });

        // Fixed-timestep sim stepping on virtual time.
        self.virtual_ms = self.virtual_ms.saturating_add(dt_ms as u64);
        let target = (self.virtual_ms.saturating_mul(TICKS_PER_SECOND) / 1000) as u32;
        let mut steps = 0;
        while self.world.tick_count() < target && steps < MAX_CATCHUP_TICKS {
            self.step_tick();
            steps += 1;
        }
        if self.world.tick_count() < target {
            // Giant dt: snap the clock to the current tick so we neither spin
            // nor perpetually lag (see MAX_CATCHUP_TICKS).
            self.virtual_ms = (self.world.tick_count() as u64) * 1000 / TICKS_PER_SECOND;
        }
        self.tick_frac = (self.virtual_ms.saturating_mul(TICKS_PER_SECOND) % 1000) as u32;

        // Transition-driven audio cues (win/lose, low power) — cosmetic.
        let go = self.world.game_over();
        if go != self.prev_game_over {
            match go {
                GameOver::Victory => self.push_sound(SoundEvent::Victory),
                GameOver::Defeat => self.push_sound(SoundEvent::Defeat),
                GameOver::Ongoing => {}
            }
            self.prev_game_over = go;
        }
        let low = self
            .player_house
            .and_then(|h| self.world.house(h))
            .map(|hs| hs.low_power())
            .unwrap_or(false);
        if low && !self.prev_low_power {
            self.push_sound(SoundEvent::LowPower);
        }
        self.prev_low_power = low;
    }

    /// Snapshot positions for interpolation, then apply one tick's commands and
    /// run the sim's systems. Afterwards, diff sim state to spawn cosmetic
    /// effects (explosions on death, buildups on placement) — read-only over the
    /// world, so this never perturbs the sim or its RNG.
    fn step_tick(&mut self) {
        // Poll the transport for this tick's full command bundle *first*: on a
        // stall or desync the sim must simply not advance (never happens for
        // the zero-delay LocalTransport; this seam is where M8-B's
        // LanTransport will stall at the tick barrier).
        let tick = self.world.tick_count();
        let cmds = match self.transport.poll(tick) {
            PollResult::Ready(bundle) => bundle.flatten(),
            PollResult::Waiting | PollResult::Desync(_) => return,
        };

        self.prev_coords.clear();
        // Handle + coord + airborne altitude-lift (pixels) so a crashing aircraft
        // explodes at its flight height, not on the ground beneath it.
        let mut prev_units: Vec<(Handle, WorldCoord, i32)> = Vec::new();
        for (h, u) in self.world.units.iter() {
            self.prev_coords.insert(h.index, u.coord);
            let lift = if u.is_airborne() {
                leptons_to_pixel(u.altitude)
            } else {
                0
            };
            prev_units.push((h, u.coord, lift));
        }
        // Pre-tick building snapshot: handle + centre coord + top-left cell +
        // type/house/repair state (for the sell-back and repair feedback below).
        let prev_buildings: Vec<PrevBuilding> = self
            .world
            .buildings
            .iter()
            .map(|(h, b)| PrevBuilding {
                handle: h,
                center: b.center_cell().center(),
                top_left: b.cell.center(),
                type_id: b.type_id,
                house: b.house,
                is_repairing: b.is_repairing,
            })
            .collect();
        let prev_bullets: Vec<Handle> = self.world.bullets.iter().map(|(h, _)| h).collect();
        // Superweapon effect snapshots (marquee arc P2): pending nuke ground-zeros,
        // and the set of units/buildings already under an iron curtain, so the
        // post-tick diff can fire the launch/impact/curtain cosmetics. Cheap and
        // empty in every world without a superweapon (so nothing changes there).
        let prev_nuke_cells: Vec<CellCoord> =
            self.world.nuke_strikes().iter().map(|n| n.cell).collect();
        let prev_iron_units: Vec<Handle> = self
            .world
            .units
            .iter()
            .filter(|(_, u)| u.iron_curtain > 0)
            .map(|(h, _)| h)
            .collect();
        let prev_iron_buildings: Vec<Handle> = self
            .world
            .buildings
            .iter()
            .filter(|(_, b)| b.iron_curtain > 0)
            .map(|(h, _)| h)
            .collect();

        // Buildings the player asked to sell this tick: a building that vanishes
        // and was a Sell target deconstructs (reverse buildup + cash SFX) rather
        // than exploding like a combat death.
        let sold: Vec<Handle> = cmds
            .iter()
            .filter_map(|c| match c {
                Command::Sell { building, .. } => Some(*building),
                _ => None,
            })
            .collect();
        let hash = self.world.tick(&cmds);
        // Feed the hash chain back to the transport (§4.6: hashes are chained
        // even in single player, so stages 2/3 inherit a battle-tested core).
        self.transport.report_hash(tick, hash);

        // New projectiles → a fire cue (covers visible cannon shots; hitscan
        // weapons are represented by the muzzle flash instead).
        let fired = self
            .world
            .bullets
            .iter()
            .any(|(h, _)| !prev_bullets.contains(&h));
        if fired {
            self.push_sound(SoundEvent::Fire);
        }

        let player = self.player_house;

        // Deaths → explosions (visual + audio).
        let mut any_death = false;
        for &(h, coord, lift) in &prev_units {
            if !self.world.units.contains(h) {
                self.spawn_effect_lifted(EffectKind::Explosion, coord, lift);
                any_death = true;
            }
        }
        // Superweapon effect diffs (marquee arc P2) — all derived from sim state,
        // spawned into the cosmetic layer only (never written back to the sim).
        self.diff_superweapon_effects(
            &prev_units,
            &prev_nuke_cells,
            &prev_iron_units,
            &prev_iron_buildings,
        );
        // Buildings that vanished this tick: a *sold* one deconstructs (reverse
        // buildup + cash-register SFX + EVA "Structure sold" for the player); any
        // other loss is a combat death → explosion. Faithful to the split between
        // `Mission_Deconstruction` (building.cpp:3722) and combat destruction.
        let mut any_sold = false;
        for pb in &prev_buildings {
            if self.world.buildings.contains(pb.handle) {
                continue;
            }
            if sold.contains(&pb.handle) {
                // Reverse-buildup deconstruct at the building's top-left; falls
                // back to a plain explosion (centre-anchored) if the type has no
                // MAKE art, so a sale is never fully invisible.
                if self.effect_frame_count(EffectKind::Deconstruct(pb.type_id)) > 0 {
                    self.spawn_effect(EffectKind::Deconstruct(pb.type_id), pb.top_left);
                } else {
                    self.spawn_effect(EffectKind::Explosion, pb.center);
                }
                any_sold = true;
                if Some(pb.house) == player {
                    self.push_sound(SoundEvent::StructureSold);
                }
            } else {
                self.spawn_effect(EffectKind::Explosion, pb.center);
                any_death = true;
            }
        }
        if any_death {
            self.push_sound(SoundEvent::Explosion);
        }
        if any_sold {
            self.push_sound(SoundEvent::Sell);
        }

        // Repair toggled **on** for an own building this tick → the repair SFX
        // (`VOC_CLICK`, building.cpp:2770). Detected as a false→true transition
        // of `is_repairing` on a still-live building the player owns.
        let repair_started = prev_buildings.iter().any(|pb| {
            !pb.is_repairing
                && Some(pb.house) == player
                && self
                    .world
                    .buildings
                    .get(pb.handle)
                    .is_some_and(|b| b.is_repairing)
        });
        if repair_started {
            self.push_sound(SoundEvent::Repair);
        }

        // New buildings → construction buildup (anchored at the building
        // top-left); a new *player* building also plays the EVA cue.
        let fresh: Vec<(u32, WorldCoord, u8)> = self
            .world
            .buildings
            .iter()
            .filter(|(h, _)| !prev_buildings.iter().any(|pb| pb.handle == *h))
            .map(|(_, b)| (b.type_id, b.cell.center(), b.house))
            .collect();
        let mut player_built = false;
        for (type_id, anchor, house) in fresh {
            self.spawn_effect(EffectKind::Buildup(type_id), anchor);
            if Some(house) == player {
                player_built = true;
            }
        }
        if player_built {
            self.push_sound(SoundEvent::ConstructionComplete);
        }
    }

    /// Diff superweapon sim state across a tick and spawn the cosmetic feedback
    /// (marquee arc P2): the nuke launch EVA + mushroom cluster + impact SFX, the
    /// iron-curtain SFX, and the chronosphere warp flashes + SFX. Everything here
    /// reads sim state and writes only the cosmetic effect/sound queues — it never
    /// touches `world`, so effects on/off leaves the sim hash chain identical.
    fn diff_superweapon_effects(
        &mut self,
        prev_units: &[(Handle, WorldCoord, i32)],
        prev_nuke_cells: &[CellCoord],
        prev_iron_units: &[Handle],
        prev_iron_buildings: &[Handle],
    ) {
        // Gather everything from the (immutable) world first, then mutate the
        // cosmetic queues — keeps the borrows disjoint.
        let now_cells: Vec<CellCoord> = self.world.nuke_strikes().iter().map(|n| n.cell).collect();
        let launched = now_cells.iter().any(|c| !prev_nuke_cells.contains(c));
        let detonated: Vec<CellCoord> = prev_nuke_cells
            .iter()
            .copied()
            .filter(|c| !now_cells.contains(c))
            .collect();
        let iron_applied = self
            .world
            .units
            .iter()
            .any(|(h, u)| u.iron_curtain > 0 && !prev_iron_units.contains(&h))
            || self
                .world
                .buildings
                .iter()
                .any(|(h, b)| b.iron_curtain > 0 && !prev_iron_buildings.contains(&h));
        // Chronosphere: a *surviving* unit whose position jumped more than a few
        // cells in one tick — only the warp moves a unit that far per tick.
        let warp_thresh = (3 * LEPTONS_PER_CELL) as i64;
        let mut warps: Vec<WorldCoord> = Vec::new();
        for &(h, old, _) in prev_units {
            let Some(u) = self.world.units.get(h) else {
                continue;
            };
            let dx = (u.coord.x.0 - old.x.0) as i64;
            let dy = (u.coord.y.0 - old.y.0) as i64;
            if dx * dx + dy * dy > warp_thresh * warp_thresh {
                warps.push(old);
                warps.push(u.coord);
            }
        }

        if launched {
            self.push_sound(SoundEvent::NukeLaunch);
        }
        for cell in detonated {
            self.spawn_nuke_blast(cell);
            self.push_sound(SoundEvent::NukeImpact);
        }
        if iron_applied {
            self.push_sound(SoundEvent::IronCurtain);
        }
        if !warps.is_empty() {
            for c in warps {
                self.spawn_warp(c);
            }
            self.push_sound(SoundEvent::Chronosphere);
        }
    }

    /// Spawn the nuclear-detonation cosmetic: a **cluster** of explosion fireballs
    /// across the blast radius (a scaled-up "mushroom" — bigger than a single
    /// combat blast), plus a couple lifted skyward for the rising column. Uses the
    /// shared explosion SHP (`FBALL1`); no-op if that art isn't installed, so the
    /// off/no-asset paths are unaffected (the ATOMICEXP/NUKE dedicated shape is not
    /// in the freeware set — the coordinator-sanctioned scaled-FBALL fallback).
    fn spawn_nuke_blast(&mut self, cell: CellCoord) {
        let c = cell.center();
        let step = LEPTONS_PER_CELL;
        // Ground fireball ring across the ~3-cell radius.
        for (ox, oy) in [
            (0, 0),
            (step, 0),
            (-step, 0),
            (0, step),
            (0, -step),
            (step, step),
            (-step, -step),
            (step, -step),
            (-step, step),
        ] {
            let anchor = WorldCoord::new(c.x.0 + ox, c.y.0 + oy);
            self.spawn_effect(EffectKind::Explosion, anchor);
        }
        // Rising column: two fireballs lifted above ground-zero (the mushroom stalk).
        self.spawn_effect_lifted(EffectKind::Explosion, c, CELL_PIXELS);
        self.spawn_effect_lifted(EffectKind::Explosion, c, CELL_PIXELS * 2);
    }

    /// Spawn a chronosphere warp flash (synthetic rings, no art) at `anchor`.
    fn spawn_warp(&mut self, anchor: WorldCoord) {
        self.effects.push(Effect {
            kind: EffectKind::Warp,
            anchor,
            lift_px: 0,
            start_ms: self.anim_ms,
        });
    }

    /// Queue a cosmetic sound cue (deduped against the current frame's queue so a
    /// burst of same-type events plays once).
    fn push_sound(&mut self, ev: SoundEvent) {
        if !self.sounds.contains(&ev) {
            self.sounds.push(ev);
        }
    }

    /// Queue a cosmetic effect if it has art to play (else no-op — an explosion
    /// with no SHP installed, or a buildup for a building with no MAKE art).
    fn spawn_effect(&mut self, kind: EffectKind, anchor: WorldCoord) {
        self.spawn_effect_lifted(kind, anchor, 0);
    }

    /// Like [`Self::spawn_effect`] but lifts the animation up by `lift_px` pixels
    /// (an airborne aircraft's crash fireball appears at its flight altitude).
    fn spawn_effect_lifted(&mut self, kind: EffectKind, anchor: WorldCoord, lift_px: i32) {
        if self.effect_frame_count(kind) == 0 {
            return;
        }
        self.effects.push(Effect {
            kind,
            anchor,
            lift_px,
            start_ms: self.anim_ms,
        });
    }

    /// Number of animation frames the given effect kind has (0 = no art).
    fn effect_frame_count(&self, kind: EffectKind) -> u32 {
        match kind {
            EffectKind::Explosion => self
                .explosion_sprite
                .first()
                .map(|s| s.frames.len() as u32)
                .unwrap_or(0),
            EffectKind::Buildup(id) | EffectKind::Deconstruct(id) => self
                .buildup_sprites
                .get(id as usize)
                .and_then(|o| o.as_ref())
                .map(|s| s.frames.len() as u32)
                .unwrap_or(0),
            // Synthetic warp flash needs no art — a fixed frame budget.
            EffectKind::Warp => WARP_FRAMES as u32,
        }
    }

    /// Unit-ish scroll direction from held keys plus pointer edge scrolling.
    fn scroll_direction(&self) -> (f32, f32) {
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if self.left {
            dx -= 1.0;
        }
        if self.right {
            dx += 1.0;
        }
        if self.up {
            dy -= 1.0;
        }
        if self.down {
            dy += 1.0;
        }
        // Edge scroll only over the tactical area (never from inside the
        // sidebar strip).
        let tw = self.tactical_width() as i32;
        if self.mouse_inside && self.mouse_x < tw {
            if self.mouse_x >= 0 && self.mouse_x < EDGE_MARGIN {
                dx -= 1.0;
            } else if self.mouse_x >= tw - EDGE_MARGIN {
                dx += 1.0;
            }
            if self.mouse_y >= 0 && self.mouse_y < EDGE_MARGIN {
                dy -= 1.0;
            } else if self.mouse_y >= self.viewport_h as i32 - EDGE_MARGIN {
                dy += 1.0;
            }
        }
        (dx.clamp(-1.0, 1.0), dy.clamp(-1.0, 1.0))
    }

    /// Width of the tactical (map) area in viewport pixels — the full viewport
    /// unless the build sidebar is enabled, in which case the sidebar strip is
    /// reserved on the right.
    pub fn tactical_width(&self) -> u32 {
        if self.sidebar_enabled {
            self.viewport_w.saturating_sub(SIDEBAR_W).max(1)
        } else {
            self.viewport_w
        }
    }

    /// Clamp the camera so the tactical viewport stays within the map.
    fn clamp_camera(&mut self) {
        let max_x = (self.raster.width as f32 - self.tactical_width() as f32).max(0.0);
        let max_y = (self.raster.height as f32 - self.viewport_h as f32).max(0.0);
        self.cam_x = self.cam_x.clamp(0.0, max_x);
        self.cam_y = self.cam_y.clamp(0.0, max_y);
    }

    /// Directly set the camera top-left (map pixels); clamped. For tests/CLI.
    pub fn set_camera(&mut self, x: f32, y: f32) {
        self.cam_x = x;
        self.cam_y = y;
        self.clamp_camera();
    }

    /// The clamped tactical viewport rectangle at the current camera position.
    /// Its width excludes the sidebar strip when the sidebar is enabled, so
    /// viewport→map click mapping stays correct for the visible tactical area.
    pub fn camera_rect(&self) -> Rect {
        Rect {
            x: self.cam_x.round() as i64,
            y: self.cam_y.round() as i64,
            width: self.tactical_width(),
            height: self.viewport_h,
        }
    }

    /// Composite an arbitrary map-space rectangle to RGBA: terrain, then units
    /// at interpolated positions with their house remap, then selection markers.
    /// Pure over `self` — camera state is not read, so tests can sweep the whole
    /// map independently of the camera.
    pub fn compose(&self, viewport: Rect) -> Frame {
        let mut frame = viewport_rgba(
            &self.raster,
            &self.palette,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
        );
        self.draw_units(&mut frame, viewport);
        frame
    }

    /// Composite at the current camera position, plus the transient drag-select
    /// box (which is viewport-relative, so it only belongs on the camera view).
    /// In game mode (sidebar enabled) this delegates to [`AppCore::compose_game`].
    pub fn compose_camera(&self) -> Frame {
        if self.sidebar_enabled {
            return self.compose_game();
        }
        let rect = self.camera_rect();
        let mut frame = self.compose(rect);
        if let Some(d) = &self.drag {
            draw_rect_outline(
                &mut frame, d.start.0, d.start.1, d.cur.0, d.cur.1, SELECT_RGB,
            );
        }
        self.draw_help_overlay(&mut frame);
        frame
    }

    /// The full game view: a `viewport_w × viewport_h` frame with the tactical
    /// map (terrain + ore + buildings + units) on the left, the build sidebar on
    /// the right, plus the placement preview and drag box. Used by the windowed
    /// shell and the M5 economy verification.
    pub fn compose_game(&self) -> Frame {
        let tw = self.tactical_width();
        // Tactical area: render terrain + entities for the camera rect into a
        // full-viewport-sized frame (its right strip stays black, then the
        // sidebar overpaints it).
        let cam = self.camera_rect();
        let mut frame = viewport_rgba(
            &self.raster,
            &self.palette,
            cam.x,
            cam.y,
            self.viewport_w,
            self.viewport_h,
        );
        self.draw_ore(&mut frame, cam);
        self.draw_buildings(&mut frame, cam);
        // Units draw over terrain/buildings; anything spilling into the sidebar
        // strip is overpainted by `draw_sidebar` below.
        self.draw_units(&mut frame, cam);
        // Cosmetic animation layer (explosions, buildups) over the entities.
        self.draw_effects(&mut frame, cam);
        // Shroud: paint unexplored cells black, hiding whatever sits under them.
        self.draw_shroud(&mut frame, cam);
        self.draw_placement_preview(&mut frame, cam, tw);
        // Sell/repair-mode hover tint over the own building under the cursor.
        self.draw_action_hover(&mut frame, cam, tw);
        // Pulsing wrench over every building currently repairing (own or not).
        self.draw_repair_indicators(&mut frame, cam);
        if let Some(d) = &self.drag {
            draw_rect_outline(
                &mut frame, d.start.0, d.start.1, d.cur.0, d.cur.1, SELECT_RGB,
            );
        }
        self.draw_sidebar(&mut frame);
        self.draw_game_over(&mut frame);
        self.draw_help_overlay(&mut frame);
        // Sell/repair mode reminders, drawn topmost: a mode banner near the top
        // of the tactical area and the mode cursor glyph at the pointer.
        self.draw_mode_banner(&mut frame);
        self.draw_mode_cursor(&mut frame);
        frame
    }

    /// Draw the F1 controls-hint overlay: a dim panel of one-line hints over the
    /// tactical area. Shown for the first few seconds of play and whenever F1 is
    /// toggled on. Text uses the built-in bitmap font (uppercase + basic
    /// punctuation only), so hints stay within the supported glyph set.
    fn draw_help_overlay(&self, frame: &mut RgbaImage) {
        if !self.help_visible() {
            return;
        }
        const HINTS: [&str; 7] = [
            "CONTROLS  -  F1 TO HIDE",
            "ARROWS / SCREEN EDGE: SCROLL MAP",
            "LEFT-DRAG: SELECT   LEFT-CLICK: PICK",
            "RIGHT-CLICK: MOVE / ATTACK",
            "D: DEPLOY MCV INTO A BASE",
            "SIDEBAR: CLICK TO BUILD / PLACE / SELL",
            "GOAL: DESTROY THE ENEMY BASE",
        ];
        let pad = 6;
        let line_h = font::GLYPH_H + 3;
        let panel_w = (HINTS.iter().map(|s| font::text_width(s)).max().unwrap_or(0)) + pad * 2;
        let panel_h = line_h * HINTS.len() as i32 + pad * 2;
        let x0 = 8;
        let y0 = 8;
        // Dim backing panel (clipped inside fill_rect).
        fill_rect(frame, x0, y0, x0 + panel_w, y0 + panel_h, [10, 12, 20]);
        draw_rect_outline(frame, x0, y0, x0 + panel_w, y0 + panel_h, [70, 90, 130]);
        let mut ty = y0 + pad;
        for (i, line) in HINTS.iter().enumerate() {
            let col = if i == 0 {
                [240, 220, 120]
            } else {
                [210, 215, 225]
            };
            font::draw_text(frame, x0 + pad, ty, line, col);
            ty += line_h;
        }
    }

    /// Paint a solid-black overlay over cells the player house has not explored
    /// (M6 shroud). No-op when the shroud is disabled (non-skirmish worlds), so
    /// terrain/econ views are unchanged. Only the tactical strip is shrouded.
    fn draw_shroud(&self, frame: &mut RgbaImage, cam: Rect) {
        if !self.world.shroud.is_enabled() {
            return;
        }
        let house = self.player_house.unwrap_or(0);
        let tw = self.tactical_width() as i32;
        let cx0 = (cam.x.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cy0 = (cam.y.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cx1 = cx0 + (tw / CELL_PIXELS) + 3;
        let cy1 = cy0 + (self.viewport_h as i32 / CELL_PIXELS) + 3;
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                let c = CellCoord::new(cx, cy);
                if self.world.shroud.is_explored(house, c) {
                    continue;
                }
                let px = (cx * CELL_PIXELS) as i64 - cam.x;
                let py = (cy * CELL_PIXELS) as i64 - cam.y;
                // Clip the black fill to the tactical strip (leave the sidebar).
                let x0 = (px as i32).max(0);
                let x1 = ((px + CELL_PIXELS as i64) as i32).min(tw);
                let y0 = (py as i32).max(0);
                let y1 = ((py + CELL_PIXELS as i64) as i32).min(self.viewport_h as i32);
                if x1 > x0 && y1 > y0 {
                    fill_rect(frame, x0, y0, x1 - 1, y1 - 1, [0, 0, 0]);
                }
            }
        }
    }

    /// Draw the centred VICTORY / DEFEAT banner once the skirmish resolves.
    fn draw_game_over(&self, frame: &mut RgbaImage) {
        let (text, rgb) = match self.world.game_over() {
            GameOver::Ongoing => return,
            GameOver::Victory => ("VICTORY", [120, 240, 120]),
            GameOver::Defeat => ("DEFEAT", [240, 90, 90]),
        };
        let scale = 6;
        let tw = font::text_width(text) * scale;
        let th = font::GLYPH_H * scale;
        let cx = (self.tactical_width() as i32 - tw) / 2;
        let cy = (self.viewport_h as i32 - th) / 2;
        // Dim backing band for legibility.
        fill_rect(
            frame,
            0,
            cy - 12,
            self.tactical_width() as i32 - 1,
            cy + th + 12,
            [12, 12, 16],
        );
        font::draw_text_scaled(frame, cx, cy, text, rgb, scale);
    }

    /// Interpolated render coordinate of a unit this frame.
    fn render_coord(&self, h: Handle, cur: WorldCoord) -> WorldCoord {
        match self.prev_coords.get(&h.index) {
            Some(prev) => {
                let f = self.tick_frac as i64;
                let x = prev.x.0 as i64 + (cur.x.0 as i64 - prev.x.0 as i64) * f / 1000;
                let y = prev.y.0 as i64 + (cur.y.0 as i64 - prev.y.0 as i64) * f / 1000;
                WorldCoord::new(x as i32, y as i32)
            }
            None => cur,
        }
    }

    /// Draw all units (and selection markers) whose sprite overlaps `viewport`.
    fn draw_units(&self, frame: &mut RgbaImage, viewport: Rect) {
        for (h, unit) in self.world.units.iter() {
            let coord = self.render_coord(h, unit.coord);
            let map_px = leptons_to_pixel(coord.x.0);
            let map_py = leptons_to_pixel(coord.y.0);
            let sx = (map_px as i64 - viewport.x) as i32;
            let sy = (map_py as i64 - viewport.y) as i32;

            let remap = self
                .remaps
                .get(unit.house as usize)
                .copied()
                .unwrap_or_else(identity_remap);

            let is_inf = unit.is_infantry();
            let is_air = unit.is_aircraft();
            // Aircraft float above the ground by their sim `altitude` (leptons →
            // pixels, `FLIGHT_LEVEL`=256=one cell=`CELL_PIXELS`px lift), casting a
            // shadow on the cell below (`AircraftClass::Draw_It`, aircraft.cpp:408:
            // body at `y - Lepton_To_Pixel(Height)`, shadow at ground `y`). Every
            // sprite/marker for an aircraft is drawn at the lifted `sy_d`; the
            // shadow silhouette stays at ground `sy`.
            let lift = if is_air {
                leptons_to_pixel(unit.altitude)
            } else {
                0
            };
            let sy_d = sy - lift;
            // Ground shadow: the darkened body silhouette at the cell below, offset
            // like the original's `x+1, y+2` SHAPE_FADING shadow. Only when lifted.
            if is_air && lift > 1 {
                if let Some(sprite) = self.sprites.get(unit.type_id as usize) {
                    if let Some(sframe) = sprite.frame_for(unit.facing) {
                        draw_sprite_shadow(frame, sx + 1, sy + 2, sframe);
                    }
                }
            }
            if let Some(sprite) = self.sprites.get(unit.type_id as usize) {
                if is_inf {
                    // Infantry: pick the Do-table band (idle / walk / fire) and the
                    // animation stage from the cosmetic clock (sim-independent), then
                    // index the SHP by facing (`Shape_Number`, infantry.cpp:524).
                    let anim = self
                        .infantry_anim
                        .get(unit.type_id as usize)
                        .and_then(|a| *a)
                        .unwrap_or_else(|| InfantryAnim::for_name(""));
                    let firing = unit.has_target()
                        && unit.weapon.is_some()
                        && unit.arm + 3 >= { unit.weapon.map(|w| w.rof).unwrap_or(0) }
                        && unit.arm != 0;
                    let action = if firing {
                        InfAction::Fire
                    } else if unit.is_moving() {
                        InfAction::Walk
                    } else {
                        InfAction::Idle
                    };
                    // ~8 fps animation stage from the cosmetic clock.
                    let stage = (self.anim_ms / 120) as u32;
                    let fi = infantry_frame(&anim, unit.facing, action, stage);
                    if let Some(sframe) = sprite.frame_at(fi) {
                        draw_sprite_centered(frame, sx, sy_d, sframe, &remap, &self.palette);
                    }
                } else {
                    // Vehicle / aircraft body sprite (drawn at the lifted `sy_d`;
                    // for a ground vehicle `lift==0` so `sy_d == sy`).
                    if let Some(sframe) = sprite.frame_for(unit.facing) {
                        draw_sprite_centered(frame, sx, sy_d, sframe, &remap, &self.palette);
                    }
                    // Turret overlay (turreted vehicles whose SHP has turret frames).
                    if unit.has_turret {
                        if let Some(tframe) = sprite.turret_frame_for(unit.turret_facing) {
                            draw_sprite_centered(frame, sx, sy_d, tframe, &remap, &self.palette);
                        }
                    }
                }
            }

            // Helicopter rotor blades, drawn spinning over the lifted body
            // (`AircraftClass::Draw_Rotors`, aircraft.cpp:521). The stage advances
            // on the cosmetic clock (sim-inert): airborne blades spin fast through
            // frames 0..4 (`Fetch_Stage()%4`); a landed heli (altitude 0) idles
            // slowly through frames 4..12 (`(Fetch_Stage()%8)+4`). Single rotor,
            // centred on the body, offset up 2px like the original.
            if is_air {
                if let Some(rotor) = &self.rotor_sprite {
                    let stage = (self.anim_ms / 60) as usize;
                    let ri = if unit.altitude > 0 {
                        stage % 4
                    } else {
                        (stage % 8) + 4
                    };
                    if let Some(rframe) = rotor.frames.get(ri) {
                        draw_sprite_centered(frame, sx, sy_d - 2, rframe, &remap, &self.palette);
                    }
                }
            }

            // Iron-curtain tint: a pulsing blue/metallic overlay over an
            // invulnerable unit for the curtain's duration (marquee arc P2,
            // cosmetic — derived from the sim's `iron_curtain` countdown, never
            // written back). The pulse is on the cosmetic clock (sim-inert).
            if unit.iron_curtain > 0 {
                let half = if is_inf {
                    CELL_PIXELS / 4
                } else {
                    CELL_PIXELS / 2
                };
                self.draw_iron_tint(frame, sx, sy_d, half);
            }

            // Muzzle flash: a brief bright spot at the barrel tip the tick(s)
            // right after a shot (arm just reset to ROF). Covers hitscan
            // weapons whose bullet never persists in the arena.
            if let Some(w) = &unit.weapon {
                if unit.has_target() && unit.arm + 2 >= w.rof && unit.arm != 0 {
                    let aim = if unit.has_turret {
                        unit.turret_facing
                    } else {
                        unit.facing
                    };
                    let (fx, fy) = offset_pixels(sx, sy_d, aim, CELL_PIXELS / 2);
                    fill_rect(frame, fx - 1, fy - 1, fx + 1, fy + 1, [255, 230, 120]);
                }
            }

            let selected = self.selected.contains(&h);
            // Infantry are small targets: their selection box and health bar are
            // scaled to roughly a sub-cell footprint rather than a full cell.
            let marker_half = if is_inf {
                CELL_PIXELS / 4
            } else {
                CELL_PIXELS / 2
            };
            if selected {
                draw_rect_outline(
                    frame,
                    sx - marker_half,
                    sy_d - marker_half,
                    sx + marker_half,
                    sy_d + marker_half,
                    SELECT_RGB,
                );
            }
            // Health bar on selected or damaged units (over the lifted body).
            if selected || unit.health < unit.max_health {
                let bar_w = if is_inf { CELL_PIXELS / 2 } else { CELL_PIXELS };
                draw_health_bar(
                    frame,
                    sx,
                    sy_d - marker_half - 4,
                    bar_w,
                    unit.health_permille(),
                );
            }
        }

        self.draw_bullets(frame, viewport);
    }

    /// Draw projectiles in flight as short bright tracers. Invisible/hitscan
    /// projectiles are represented by the muzzle flash instead, so they are
    /// skipped here.
    fn draw_bullets(&self, frame: &mut RgbaImage, viewport: Rect) {
        for (_h, b) in self.world.bullets.iter() {
            if b.invisible {
                continue;
            }
            let px = (leptons_to_pixel(b.pos.x.0) as i64 - viewport.x) as i32;
            let py_ground = (leptons_to_pixel(b.pos.y.0) as i64 - viewport.y) as i32;
            // Arcing lob (artillery/grenade): lift the projectile off the ground
            // by its sim `height` (leptons → pixels) and draw a small shadow at the
            // ground point, so the shell visibly arcs. Straight shots have height 0.
            let lift = leptons_to_pixel(b.height);
            let py = py_ground - lift;
            if b.arcing && lift > 1 {
                fill_rect(
                    frame,
                    px - 1,
                    py_ground,
                    px + 1,
                    py_ground + 1,
                    [20, 20, 24],
                );
            }
            // Small tracer: a couple of pixels back along the flight direction.
            let (tx, ty) = offset_pixels(px, py, Facing(b.facing.0.wrapping_add(128)), 4);
            draw_line(frame, tx, ty, px, py, [255, 240, 160]);
            fill_rect(frame, px - 1, py - 1, px + 1, py + 1, [255, 255, 200]);
        }
        self.draw_defense_effects(frame, viewport);
    }

    /// Draw defense-building firing effects (M7.7 Chunk B): the tesla coil's
    /// charge glow and its instant zap bolt (`TeslaZap` is an invisible hitscan
    /// weapon, so it never leaves a persistent bullet — it is drawn here at the
    /// firing tick), plus a muzzle flash for the gun/pillbox emplacements whose
    /// hitscan shots likewise leave no bullet.
    fn draw_defense_effects(&self, frame: &mut RgbaImage, viewport: Rect) {
        for (_h, b) in self.world.buildings.iter() {
            let Some(w) = &b.weapon else { continue };
            if !b.is_alive() {
                continue;
            }
            // Coil/emplacement top, in screen pixels.
            let cx =
                (b.cell.x * CELL_PIXELS + b.foot_w as i32 * CELL_PIXELS / 2) as i64 - viewport.x;
            let cy =
                (b.cell.y * CELL_PIXELS + b.foot_h as i32 * CELL_PIXELS / 2) as i64 - viewport.y;
            let (cx, cy) = (cx as i32, cy as i32);

            // Tesla charge glow: brightens as the charge builds.
            if b.charges && b.charge > 0 {
                let t = (b.charge as i32 * 200 / TESLA_CHARGE_MAX).clamp(40, 220) as u8;
                fill_rect(frame, cx - 2, cy - 2, cx + 2, cy + 2, [t, t, 255]);
            }

            // The target's screen position (for the zap line / flash aim).
            let tpos = b.target.and_then(|t| self.target_screen_pos(t, viewport));

            // Firing tick: arm has just reset toward ROF (same detection as the
            // unit muzzle flash). For the tesla, draw a bright zap line to the
            // target; for gun/pillbox, a muzzle flash at the barrel.
            let firing = w.rof > 0 && b.arm + 2 >= w.rof && b.arm != 0;
            if firing {
                if b.charges {
                    if let Some((tx, ty)) = tpos {
                        // A jagged-ish bright bolt (two segments via a midpoint kink).
                        let mx = (cx + tx) / 2;
                        let my = (cy + ty) / 2 - 3;
                        draw_line(frame, cx, cy, mx, my, [180, 210, 255]);
                        draw_line(frame, mx, my, tx, ty, [220, 235, 255]);
                        fill_rect(frame, tx - 2, ty - 2, tx + 2, ty + 2, [235, 245, 255]);
                    }
                } else if b.has_turret {
                    // GUN: flash at the barrel tip in the turret direction.
                    let (fx, fy) = offset_pixels(cx, cy, b.turret_facing, CELL_PIXELS / 2);
                    fill_rect(frame, fx - 1, fy - 1, fx + 1, fy + 1, [255, 230, 120]);
                } else if let Some((tx, ty)) = tpos {
                    // Fixed emplacement (PBOX/HBOX/FTUR): flash partway to target.
                    let fx = cx + (tx - cx) / 3;
                    let fy = cy + (ty - cy) / 3;
                    fill_rect(frame, fx - 1, fy - 1, fx + 1, fy + 1, [255, 210, 120]);
                }
            }
        }
    }

    /// Screen-pixel position of a combat target (unit/building centre), if live.
    fn target_screen_pos(&self, target: Target, viewport: Rect) -> Option<(i32, i32)> {
        // Airborne aircraft float above their cell — lift the aim point so an AA
        // tracer/flash reaches the flying heli, not the ground under it.
        let mut lift = 0;
        let coord = match target {
            Target::Unit(t) => {
                let u = self.world.units.get(t).filter(|u| u.is_alive())?;
                if u.is_airborne() {
                    lift = leptons_to_pixel(u.altitude);
                }
                u.coord
            }
            Target::Building(t) => self
                .world
                .buildings
                .get(t)
                .filter(|b| b.is_alive())
                .map(|b| b.center_cell().center())?,
            Target::Cell(c) => c.center(),
        };
        let x = (leptons_to_pixel(coord.x.0) as i64 - viewport.x) as i32;
        let y = (leptons_to_pixel(coord.y.0) as i64 - viewport.y) as i32 - lift;
        Some((x, y))
    }

    /// Convert a viewport pixel to a map-pixel position at the current camera.
    fn viewport_to_map(&self, x: i32, y: i32) -> (i64, i64) {
        let r = self.camera_rect();
        (r.x + x as i64, r.y + y as i64)
    }

    /// Finish a left-drag: box-select owned units inside the rectangle, or on a
    /// near-zero drag treat it as a click that picks the nearest unit.
    fn finish_selection(&mut self, start: (i32, i32), end: (i32, i32)) {
        self.selected.clear();
        let (sx0, sy0) = self.viewport_to_map(start.0, start.1);
        let (sx1, sy1) = self.viewport_to_map(end.0, end.1);

        if (end.0 - start.0).abs() <= CLICK_SLOP && (end.1 - start.1).abs() <= CLICK_SLOP {
            // Click: pick the nearest selectable unit within PICK_RADIUS.
            let mut best: Option<(i64, Handle)> = None;
            for (h, unit) in self.world.units.iter() {
                if !self.selectable(unit.house) {
                    continue;
                }
                let px = leptons_to_pixel(unit.coord.x.0) as i64;
                let py = leptons_to_pixel(unit.coord.y.0) as i64;
                let d2 = (px - sx0) * (px - sx0) + (py - sy0) * (py - sy0);
                let r = pick_radius(unit.is_infantry()) as i64;
                if d2 <= r * r && best.map(|(bd, _)| d2 < bd).unwrap_or(true) {
                    best = Some((d2, h));
                }
            }
            if let Some((_, handle)) = best {
                self.selected.push(handle);
                self.push_sound(SoundEvent::Select);
            }
            return;
        }

        let (xa, xb) = (sx0.min(sx1), sx0.max(sx1));
        let (ya, yb) = (sy0.min(sy1), sy0.max(sy1));
        for (h, unit) in self.world.units.iter() {
            if !self.selectable(unit.house) {
                continue;
            }
            let px = leptons_to_pixel(unit.coord.x.0) as i64;
            let py = leptons_to_pixel(unit.coord.y.0) as i64;
            if px >= xa && px <= xb && py >= ya && py <= yb {
                self.selected.push(h);
            }
        }
        if !self.selected.is_empty() {
            self.push_sound(SoundEvent::Select);
        }
    }

    /// Whether the controlled player may select/command a unit of `house`. With
    /// no controlled house set (headless harnesses), everything is selectable —
    /// the legacy behaviour; once a player house is set (game mode) it gates to
    /// exactly that house.
    fn selectable(&self, house: u8) -> bool {
        match self.player_house {
            Some(ph) => ph == house,
            None => true,
        }
    }

    /// Right-click order: if the click lands on an **enemy** unit, every
    /// selected owned unit attacks it; otherwise it is a move to the clicked
    /// cell. Ownership: you cannot attack your own units (ground force-fire is
    /// deferred — the task's simplification). Emits `Command::Attack` or
    /// `Command::Move` through the deterministic pipeline.
    fn issue_order(&mut self, x: i32, y: i32) {
        if self.selected.is_empty() || !self.accepting_orders() {
            return;
        }
        let (mx, my) = self.viewport_to_map(x, y);
        // Collect live selected handles + houses first (borrow discipline).
        let orders: Vec<(Handle, u8)> = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .filter_map(|h| self.world.units.get(h).map(|u| (h, u.house)))
            .collect();
        if orders.is_empty() {
            return;
        }
        // The controlling house is that of the selected units (single-house
        // player). A click on a unit of a different house is an attack order.
        let player_house = orders[0].1;
        let picked = self.unit_at_map(mx, my);
        let enemy = picked.and_then(|h| {
            self.world
                .units
                .get(h)
                .filter(|u| u.house != player_house)
                .map(|_| h)
        });
        // Load: right-clicking an **own transport** (a unit with passenger
        // capacity) with infantry selected orders those infantry to board it
        // (M7.5-B P1). Vehicles in the selection ignore the load order.
        let own_transport = picked.filter(|&h| {
            self.world
                .units
                .get(h)
                .map(|u| u.house == player_house && u.capacity > 0)
                .unwrap_or(false)
        });
        if let Some(transport) = own_transport {
            let mut loaded_any = false;
            for (unit, house) in &orders {
                if *unit == transport {
                    continue;
                }
                let is_inf = self
                    .world
                    .units
                    .get(*unit)
                    .map(|u| u.is_infantry())
                    .unwrap_or(false);
                if !is_inf {
                    continue;
                }
                let cmd = Command::Load {
                    passenger: *unit,
                    transport,
                    house: *house,
                };
                self.emit(cmd);
                loaded_any = true;
            }
            if loaded_any {
                return;
            }
        }

        // An enemy building under the cursor is an attack target too (M6). Own
        // buildings are ignored here — a sell mode over own buildings is deferred
        // (noted): the sim already has `Command::Sell`, but no UI affordance yet.
        let enemy_target = enemy.map(Target::Unit).or_else(|| {
            self.enemy_building_at_map(mx, my, player_house)
                .map(Target::Building)
        });

        if let Some(target) = enemy_target {
            for (unit, house) in orders {
                // Only armed units get an attack order; unarmed selected units
                // are simply left idle (the sim also rejects unarmed attackers).
                let armed = self
                    .world
                    .units
                    .get(unit)
                    .map(|u| u.weapon.is_some())
                    .unwrap_or(false);
                if !armed {
                    continue;
                }
                let cmd = Command::Attack {
                    unit,
                    target,
                    house,
                };
                self.emit(cmd);
            }
        } else {
            let dest = CellCoord::new(
                (mx / CELL_PIXELS as i64) as i32,
                (my / CELL_PIXELS as i64) as i32,
            );
            for (unit, house) in orders {
                let cmd = Command::Move { unit, dest, house };
                self.emit(cmd);
            }
        }
    }

    /// The unit whose sprite is nearest a map-pixel point within the pick
    /// radius, if any (slot order breaks ties deterministically).
    fn unit_at_map(&self, mx: i64, my: i64) -> Option<Handle> {
        let mut best: Option<(i64, Handle)> = None;
        for (h, unit) in self.world.units.iter() {
            let px = leptons_to_pixel(unit.coord.x.0) as i64;
            let py = leptons_to_pixel(unit.coord.y.0) as i64;
            let d2 = (px - mx) * (px - mx) + (py - my) * (py - my);
            let r = pick_radius(unit.is_infantry()) as i64;
            if d2 <= r * r && best.map(|(bd, _)| d2 < bd).unwrap_or(true) {
                best = Some((d2, h));
            }
        }
        best.map(|(_, h)| h)
    }

    /// The enemy building whose footprint covers a map-pixel point, if any (for
    /// the enemy-building attack click). Own buildings return `None`.
    fn enemy_building_at_map(&self, mx: i64, my: i64, player_house: u8) -> Option<Handle> {
        let cell = CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        );
        self.world
            .buildings
            .iter()
            .find(|(_, b)| b.house != player_house && b.is_alive() && b.covers(cell))
            .map(|(h, _)| h)
    }

    /// The player's **own** building whose footprint covers a map-pixel point, if
    /// any — excluding walls (which cannot be sold or repaired). The sell/repair
    /// modes gate strictly on this, so a click on an enemy building, a unit, or
    /// empty ground can never emit a command (monkey/scripted-drive safe).
    fn own_building_at_map(&self, mx: i64, my: i64, player_house: u8) -> Option<Handle> {
        let cell = CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        );
        self.world
            .buildings
            .iter()
            .find(|(_, b)| b.house == player_house && b.is_alive() && !b.is_wall && b.covers(cell))
            .map(|(h, _)| h)
    }

    /// Sell the own building under a tactical viewport point (sell mode). Emits
    /// `Command::Sell` only for a live, own, non-wall building; stays in sell mode
    /// so several buildings can be sold in a row (like the original).
    fn try_sell_at(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let Some(house) = self.player_house else {
            return;
        };
        let (mx, my) = self.viewport_to_map(x, y);
        if let Some(building) = self.own_building_at_map(mx, my, house) {
            self.emit(Command::Sell { house, building });
        }
    }

    /// Toggle repair on the own building under a tactical viewport point (repair
    /// mode). Emits `Command::Repair` only for a live, own, non-wall building.
    fn try_repair_at(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let Some(house) = self.player_house else {
            return;
        };
        let (mx, my) = self.viewport_to_map(x, y);
        if let Some(building) = self.own_building_at_map(mx, my, house) {
            self.emit(Command::Repair { house, building });
        }
    }

    /// Any live building (of any house) whose footprint covers a map-pixel point —
    /// for the iron-curtain target pick, which may protect any unit or building.
    fn building_at_map(&self, mx: i64, my: i64) -> Option<Handle> {
        let cell = CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        );
        self.world
            .buildings
            .iter()
            .find(|(_, b)| b.is_alive() && b.covers(cell))
            .map(|(h, _)| h)
    }

    /// Fire the armed superweapon at the tactical viewport point (marquee arc P1).
    /// Emits `Command::FireSuperWeapon` per kind:
    /// - **Nuclear** → the clicked cell (one click).
    /// - **IronCurtain** → the unit (preferred) or building under the cursor (one
    ///   click; a click on empty ground stays armed for a retry).
    /// - **Chronosphere** → a two-click gather: the first click on a unit stores
    ///   the warp source, the second click sets the destination cell and fires.
    ///
    /// Re-validates readiness each click (the granting building may have been
    /// destroyed while the mode was armed); a click never emits for an unready
    /// weapon. The mode exits on a successful fire (Esc/right-click cancels).
    fn try_fire_super_at(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let (Some(house), Some(kind)) = (self.player_house, self.sw_fire_mode) else {
            return;
        };
        if !self.world.superweapon_ready(house, kind) {
            self.cancel_action_modes();
            return;
        }
        let (mx, my) = self.viewport_to_map(x, y);
        let cell = CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        );
        match kind {
            SuperKind::Nuclear => {
                self.emit(Command::FireSuperWeapon {
                    house,
                    kind,
                    target: Target::Cell(cell),
                    dest: None,
                });
                self.cancel_action_modes();
            }
            SuperKind::IronCurtain => {
                let target = self
                    .unit_at_map(mx, my)
                    .map(Target::Unit)
                    .or_else(|| self.building_at_map(mx, my).map(Target::Building));
                if let Some(target) = target {
                    self.emit(Command::FireSuperWeapon {
                        house,
                        kind,
                        target,
                        dest: None,
                    });
                    self.cancel_action_modes();
                }
                // No unit/building under the cursor → stay armed for a retry.
            }
            SuperKind::Chronosphere => match self.sw_chrono_source {
                None => {
                    // First click: pick the vehicle to warp (stay armed for dest).
                    if let Some(u) = self.unit_at_map(mx, my) {
                        self.sw_chrono_source = Some(u);
                    }
                }
                Some(src) => {
                    // Second click: the destination cell → fire and exit.
                    self.emit(Command::FireSuperWeapon {
                        house,
                        kind,
                        target: Target::Unit(src),
                        dest: Some(cell),
                    });
                    self.cancel_action_modes();
                }
            },
        }
    }

    /// The SELL and REPAIR mode-button rects `(x0,y0,x1,y1)` in the sidebar header
    /// (only meaningful when the sidebar is enabled). Stacked at the header's
    /// right edge over blank background, so no other sidebar geometry moves.
    ///
    /// When the original icon art is installed the two buttons sit **side by
    /// side** at their native SHP size (repair left of sell, matching
    /// `sidebar.cpp`'s `Repair.X < Upgrade.X`); with no art they keep the
    /// original stacked text-button geometry (so the text-fallback / no-asset
    /// goldens are byte-identical).
    fn mode_btn_art_dims(&self) -> Option<(i32, i32)> {
        // Prefer the sell art's frame 0 size; fall back to repair's. Both SHPs
        // are the same size in the real asset (34×28 hires).
        let art = self
            .sell_button_art
            .as_ref()
            .or(self.repair_button_art.as_ref())?;
        let f = art.frames.first()?;
        Some((f.width as i32, f.height as i32))
    }
    fn sell_button_rect(&self) -> (i32, i32, i32, i32) {
        let x1 = self.viewport_w as i32 - 2;
        match self.mode_btn_art_dims() {
            Some((w, h)) => (x1 - w, 1, x1, 1 + h),
            None => (x1 - MODE_BTN_W, 1, x1, 1 + MODE_BTN_H),
        }
    }
    fn repair_button_rect(&self) -> (i32, i32, i32, i32) {
        match self.mode_btn_art_dims() {
            Some((w, h)) => {
                // Left of the sell button, same top edge.
                let (sx0, _, _, _) = self.sell_button_rect();
                let x1 = sx0 - 1;
                (x1 - w, 1, x1, 1 + h)
            }
            None => {
                let (x0, _, x1, _) = self.sell_button_rect();
                let y0 = 1 + MODE_BTN_H + 1;
                (x0, y0, x1, y0 + MODE_BTN_H)
            }
        }
    }

    /// The superweapon kinds the player house currently owns (present in the sim,
    /// charging or ready), in a stable kind order — the set the sidebar surfaces as
    /// ready/charge indicators. Empty (and hence the whole SW indicator strip is
    /// absent) unless the player owns a superweapon structure, so no pinned frame
    /// is affected.
    fn sw_owned_kinds(&self) -> Vec<SuperKind> {
        let Some(house) = self.player_house else {
            return Vec::new();
        };
        let mut kinds: Vec<SuperKind> = self
            .world
            .superweapons()
            .iter()
            .filter(|s| s.house == house)
            .map(|s| s.kind)
            .collect();
        kinds.sort_by_key(|k| sw_kind_index(*k));
        kinds.dedup();
        kinds
    }

    /// The stacked ready/charge indicator buttons `(kind, (x0,y0,x1,y1))` at the
    /// bottom of the sidebar strip (marquee arc P1) — one per owned superweapon.
    fn sw_buttons(&self) -> Vec<(SuperKind, (i32, i32, i32, i32))> {
        let kinds = self.sw_owned_kinds();
        if kinds.is_empty() {
            return Vec::new();
        }
        let x0 = self.tactical_width() as i32 + 1;
        let x1 = self.viewport_w as i32 - 1;
        let n = kinds.len() as i32;
        kinds
            .into_iter()
            .enumerate()
            .map(|(i, k)| {
                let y0 = self.viewport_h as i32 - (n - i as i32) * SW_BTN_H;
                (k, (x0, y0, x1, y0 + SW_BTN_H - 1))
            })
            .collect()
    }

    /// The superweapon indicator button under a sidebar point, if any.
    fn sw_button_at(&self, x: i32, y: i32) -> Option<SuperKind> {
        self.sw_buttons()
            .into_iter()
            .find(|(_, (x0, y0, x1, y1))| x >= *x0 && x < *x1 && y >= *y0 && y <= *y1)
            .map(|(k, _)| k)
    }

    // ---- Build UI actions (public so tests / the verification drive them) ----

    /// Queue a command into the loopback transport and record it as emitted.
    fn emit(&mut self, cmd: Command) {
        self.transport.submit(cmd);
        self.emitted.push(cmd);
    }

    /// Deploy the currently-selected MCV (if any) into a construction yard.
    pub fn deploy_selected(&mut self) {
        if !self.accepting_orders() {
            return;
        }
        let Some(house) = self.player_house.or(Some(0)) else {
            return;
        };
        // Find a selected unit that is an MCV (a proto with `deploys_to`).
        let mcv = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .find(|&h| {
                self.world
                    .units
                    .get(h)
                    .map(|u| self.is_mcv(u.type_id))
                    .unwrap_or(false)
            });
        if let Some(unit) = mcv {
            self.emit(Command::Deploy { unit, house });
            return;
        }
        // Deploy also unloads a selected loaded transport (M7.5-B P1): the APC
        // disgorges its passengers to free adjacent spots.
        let loaded = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .find(|&h| {
                self.world
                    .units
                    .get(h)
                    .map(|u| u.house == house && !u.cargo.is_empty())
                    .unwrap_or(false)
            });
        if let Some(transport) = loaded {
            self.emit(Command::Unload { transport, house });
        }
    }

    /// Whether unit sprite `type_id` belongs to a deployable MCV proto.
    fn is_mcv(&self, type_id: u32) -> bool {
        self.world
            .catalog
            .units
            .iter()
            .any(|p| p.sprite_id == type_id && p.deploys_to.is_some())
    }

    /// Start producing `item` for the controlled house (validated by the sim).
    pub fn start_production(&mut self, item: BuildItem) {
        if let Some(house) = self.player_house {
            self.emit(Command::StartProduction { house, item });
        }
    }

    /// Cancel the controlled house's production of `kind`.
    pub fn cancel_production(&mut self, kind: ProdKind) {
        if let Some(house) = self.player_house {
            self.emit(Command::CancelProduction { house, kind });
        }
    }

    /// Enter placement mode for a ready building type id (green/red preview
    /// follows the cursor until a map click).
    pub fn begin_placement(&mut self, building_id: u32) {
        self.placing = Some(building_id);
        self.sw_fire_mode = None;
        self.sw_chrono_source = None;
    }

    /// Whether a building is currently being placed.
    pub fn placing(&self) -> Option<u32> {
        self.placing
    }

    /// The footprint top-left cell for a tactical viewport pixel.
    fn cell_at_viewport(&self, x: i32, y: i32) -> CellCoord {
        let (mx, my) = self.viewport_to_map(x, y);
        CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        )
    }

    /// Attempt to place the building being placed at the clicked tactical pixel.
    /// Emits `PlaceBuilding` only when the spot is valid, so an errant click
    /// keeps placement mode active for a retry.
    fn place_at(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let (Some(building), Some(house)) = (self.placing, self.player_house) else {
            return;
        };
        let cell = self.cell_at_viewport(x, y);
        if self.world.can_place_building(house, building, cell) {
            self.emit(Command::PlaceBuilding {
                house,
                building,
                cell,
            });
            self.placing = None;
        }
    }

    /// Directly place a ready building (for tests / the verification harness).
    pub fn place_building(&mut self, building: u32, cell: CellCoord) {
        if let Some(house) = self.player_house {
            self.emit(Command::PlaceBuilding {
                house,
                building,
                cell,
            });
            self.placing = None;
        }
    }

    /// Y offset (px) where the buildable rows begin (below the readout header).
    /// Header height (credits + power lines) before the radar / rows.
    // `pub`: read-only accessor for the radar panel's `y0` so UI layout tests
    // assert real geometry instead of re-deriving it (ra-tester, M7.12 audit).
    pub fn sidebar_header_h(&self) -> i32 {
        // Text header: the credits + power readout lines (font-derived, = 22).
        let text_h = 2 + (font::GLYPH_H + 2) + font::GLYPH_H + 4;
        // When real SELL/REPAIR button *art* is installed (Q14 M7.12 art pass)
        // it renders at native SHP height (34×28 hires) side-by-side in the
        // top-right of the header. The header band — whose height is the radar
        // panel's `y0` (`radar_rect`) and gates `sidebar_rows_top` — must clear
        // the buttons so the radar sits *below* them, never under them (matching
        // the original sidebar, where Repair/Sell sit above the radar). With no
        // art the fallback text buttons are 9px and already fit inside the text
        // header, so this `max` is a no-op and every no-asset golden is
        // byte-identical; only the real-asset (`hires`) frames move (radar +
        // cameo rows shift down by the extra band — a rendering-only re-pin).
        match self.mode_btn_art_dims() {
            Some((_, h)) => text_h.max(1 + h + 1),
            None => text_h,
        }
    }

    /// The radar panel rectangle `(x0, y0, size)` in viewport pixels, if the
    /// minimap is currently active (M7.7 Chunk C: gated on owning a **powered
    /// radar dome**).
    fn radar_rect(&self) -> Option<(i32, i32, i32)> {
        if !self.has_radar() {
            return None;
        }
        let x0 = self.tactical_width() as i32 + 2;
        let y0 = self.sidebar_header_h();
        Some((x0, y0, RADAR_SIZE))
    }

    /// Whether the radar minimap is active. Requires the sidebar radar to be
    /// enabled and — when the catalog models a radar dome (DOME) — the player to
    /// own a **live, powered** one (`RadarClass::Radar_Activate`, gated on
    /// `IsRadarActive` + `House->Power_Fraction()`). A catalog with no DOME
    /// concept (synthetic test fixtures) keeps the radar always-on, so those
    /// goldens are unaffected.
    pub fn has_radar(&self) -> bool {
        if !self.radar_enabled {
            return false;
        }
        // Classic-radar-rules OFF: always on (skip DOME gating).
        if self.radar_always_on {
            return true;
        }
        let Some(house) = self.player_house else {
            return false;
        };
        let dome_id = self
            .world
            .catalog
            .buildings
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case("DOME"));
        match dome_id {
            None => true, // no radar-dome concept modeled → always on
            Some(id) => {
                let owns_live = self
                    .world
                    .buildings
                    .iter()
                    .any(|(_, b)| b.house == house && b.type_id == id as u32 && b.is_alive());
                let powered = self
                    .world
                    .house(house)
                    .map(|h| !h.low_power())
                    .unwrap_or(true);
                owns_live && powered
            }
        }
    }

    /// Per-row height — taller when cameo art is drawn.
    fn sidebar_row_h(&self) -> i32 {
        if self.cameo_sprites.iter().any(|c| c.is_some()) {
            SIDEBAR_ROW_H_CAMEO
        } else {
            SIDEBAR_ROW_H
        }
    }

    fn sidebar_rows_top(&self) -> i32 {
        match self.radar_rect() {
            Some((_, y0, size)) => y0 + size + 4,
            None => font::GLYPH_H * 3 + 12,
        }
    }

    /// Which build column a buildable belongs to: `0` = structures (left),
    /// `1` = units/other (right). Mirrors `SidebarClass::Which_Column`
    /// (`sidebar.cpp`: `RTTI_BUILDING* -> 0`, else `1`).
    fn which_column(item: BuildItem) -> usize {
        match item {
            BuildItem::Building(_) => 0,
            BuildItem::Unit(_) => 1,
        }
    }

    /// The live [`SidebarItem`]s in build column `col`, in list order. The flat
    /// [`Self::sidebar_items`] is exactly `column_items(0)` followed by
    /// `column_items(1)` (buildables are declared structures-first), so name
    /// lookups and column lookups stay consistent.
    fn column_items(&self, col: usize) -> Vec<SidebarItem> {
        self.sidebar_items()
            .into_iter()
            .filter(|it| Self::which_column(it.item) == col)
            .collect()
    }

    /// The number of buildables in column `col` (cheap; avoids materialising the
    /// `SidebarItem`s just to count).
    fn column_len(&self, col: usize) -> usize {
        let Some(house) = self.player_house else {
            return 0;
        };
        let hs = self.world.house(house);
        self.buildables
            .iter()
            .filter(|&&item| Self::which_column(item) == col)
            .filter(|&&item| self.describe_buildable(house, hs, item).is_some())
            .count()
    }

    /// Left viewport x of build column `col`.
    fn column_x(&self, col: usize) -> i32 {
        self.tactical_width() as i32 + 1 + col as i32 * COLUMN_W
    }

    /// How many cameo rows are visible per column at the current viewport height
    /// (`StripClass::MAX_VISIBLE` is fixed at 4 in the original's 200px sidebar;
    /// we derive it from the actual height so it adapts, reserving the
    /// scroll-button row at the bottom). At least one row is always shown.
    fn sidebar_visible_rows(&self) -> usize {
        let top = self.sidebar_rows_top();
        let avail = self.viewport_h as i32 - top - SCROLL_BTN_H;
        (avail / self.sidebar_row_h()).max(1) as usize
    }

    /// Whether the build sidebar is enabled (game mode). For the shell's input
    /// routing (e.g. mouse-wheel scroll only over the sidebar).
    pub fn sidebar_enabled(&self) -> bool {
        self.sidebar_enabled
    }

    /// Which build column a sidebar viewport x lands in (`0` structures, `1`
    /// units), clamped to a valid column. For the shell's wheel-scroll routing.
    pub fn sidebar_column_at_x(&self, x: i32) -> u8 {
        let rel = x - self.column_x(0);
        let col = (rel / COLUMN_W).clamp(0, SIDEBAR_COLUMNS as i32 - 1);
        col.max(0) as u8
    }

    /// The scroll offset (`TopIndex`) of column `col`, clamped to a valid page.
    pub fn sidebar_scroll(&self, col: usize) -> usize {
        let max = self.max_scroll(col);
        self.sidebar_scroll.get(col).copied().unwrap_or(0).min(max)
    }

    /// The maximum scroll offset for a column (0 when it fits without scrolling).
    fn max_scroll(&self, col: usize) -> usize {
        self.column_len(col)
            .saturating_sub(self.sidebar_visible_rows())
    }

    /// Whether column `col` has more items than fit (so its scroll arrows show).
    fn column_overflows(&self, col: usize) -> bool {
        self.max_scroll(col) > 0
    }

    /// Scroll a build column by one row (`StripClass::Scroll(up, column)`).
    /// A no-op past either end. Public so scripted drives / the shell drive it.
    pub fn scroll_sidebar(&mut self, column: u8, up: bool) {
        let col = column as usize;
        if col >= SIDEBAR_COLUMNS {
            return;
        }
        let max = self.max_scroll(col);
        let cur = self.sidebar_scroll[col].min(max);
        self.sidebar_scroll[col] = if up {
            cur.saturating_sub(1)
        } else {
            (cur + 1).min(max)
        };
    }

    /// Re-clamp both columns' scroll after a resize (fewer visible rows may make
    /// a previously-valid offset overshoot).
    fn clamp_sidebar_scroll(&mut self) {
        for col in 0..SIDEBAR_COLUMNS {
            self.sidebar_scroll[col] = self.sidebar_scroll[col].min(self.max_scroll(col));
        }
    }

    /// The up/down scroll-arrow button rects for column `col`, as
    /// `(up_rect, down_rect)` in `(x0,y0,x1,y1)` viewport pixels — `None` when
    /// the column does not overflow (no arrows shown).
    #[allow(clippy::type_complexity)]
    fn scroll_buttons(&self, col: usize) -> Option<((i32, i32, i32, i32), (i32, i32, i32, i32))> {
        if !self.column_overflows(col) {
            return None;
        }
        let top = self.sidebar_rows_top();
        let by = top + self.sidebar_visible_rows() as i32 * self.sidebar_row_h();
        let cx = self.column_x(col);
        let up = (cx + 1, by, cx + 1 + SCROLL_BTN_W, by + SCROLL_BTN_H);
        let dx = cx + COLUMN_W - 1 - SCROLL_BTN_W;
        let down = (dx, by, dx + SCROLL_BTN_W, by + SCROLL_BTN_H);
        Some((up, down))
    }

    /// If `(x,y)` lands on a scroll arrow, the `(column, up)` it triggers.
    fn scroll_button_at(&self, x: i32, y: i32) -> Option<(u8, bool)> {
        let hit = |(x0, y0, x1, y1): (i32, i32, i32, i32)| x >= x0 && x < x1 && y >= y0 && y < y1;
        for col in 0..SIDEBAR_COLUMNS {
            if let Some((up, down)) = self.scroll_buttons(col) {
                if hit(up) {
                    return Some((col as u8, true));
                }
                if hit(down) {
                    return Some((col as u8, false));
                }
            }
        }
        None
    }

    /// The buildable's **flat** [`Self::sidebar_items`] index for a sidebar
    /// viewport point, if it lands on a visible cameo row. Maps the 2D
    /// (column, visible-row) hit through each column's scroll offset back to the
    /// flat index (structures block first, then units) so `sidebar_click` and the
    /// name-based test lookups agree.
    fn sidebar_row_at(&self, x: i32, y: i32) -> Option<usize> {
        let x0 = self.tactical_width() as i32;
        if x < x0 {
            return None;
        }
        // Which column? (past the right edge of column 1 → miss)
        let col = ((x - self.column_x(0)) / COLUMN_W) as usize;
        if col >= SIDEBAR_COLUMNS || x < self.column_x(0) {
            return None;
        }
        let top = self.sidebar_rows_top();
        if y < top {
            return None;
        }
        let row = ((y - top) / self.sidebar_row_h()) as usize;
        if row >= self.sidebar_visible_rows() {
            return None; // in the scroll-button band or below
        }
        let pos = self.sidebar_scroll(col) + row;
        if pos >= self.column_len(col) {
            return None;
        }
        // Flat index: all of column 0 precedes column 1 in `sidebar_items`.
        let base = if col == 0 { 0 } else { self.column_len(0) };
        Some(base + pos)
    }

    /// Whether a viewport point lands on the radar panel; returns the map cell it
    /// corresponds to (for click-to-jump).
    fn radar_cell_at(&self, x: i32, y: i32) -> Option<CellCoord> {
        let (rx, ry, size) = self.radar_rect()?;
        if x < rx || x >= rx + size || y < ry || y >= ry + size {
            return None;
        }
        let (mw, mh) = (self.map_cells_w().max(1), self.map_cells_h().max(1));
        let cx = ((x - rx) as i64 * mw as i64 / size as i64) as i32;
        let cy = ((y - ry) as i64 * mh as i64 / size as i64) as i32;
        Some(CellCoord::new(cx, cy))
    }

    /// Map width/height in cells (from the terrain raster).
    fn map_cells_w(&self) -> i32 {
        self.raster.width as i32 / CELL_PIXELS
    }
    fn map_cells_h(&self) -> i32 {
        self.raster.height as i32 / CELL_PIXELS
    }

    /// Handle a left-click inside the sidebar strip: the radar jumps the camera,
    /// ready buildings enter placement mode, buildable rows start production.
    fn sidebar_click(&mut self, x: i32, y: i32) {
        // Radar click-to-jump works even after the game is over (navigation only).
        if let Some(cell) = self.radar_cell_at(x, y) {
            let px = (cell.x * CELL_PIXELS - self.tactical_width() as i32 / 2) as f32;
            let py = (cell.y * CELL_PIXELS - self.viewport_h as i32 / 2) as f32;
            self.set_camera(px, py);
            return;
        }
        // Scroll arrows work regardless of order-acceptance (pure UI navigation).
        if let Some((col, up)) = self.scroll_button_at(x, y) {
            self.scroll_sidebar(col, up);
            return;
        }
        if !self.accepting_orders() {
            return;
        }
        // SELL / REPAIR mode buttons (M7.9 P1): toggle the corresponding tactical
        // click mode. A pure UI action — the actual command is only emitted later,
        // on a tactical click over an own building.
        let hit = |(x0, y0, x1, y1): (i32, i32, i32, i32)| x >= x0 && x < x1 && y >= y0 && y < y1;
        if hit(self.sell_button_rect()) {
            self.toggle_sell_mode();
            return;
        }
        if hit(self.repair_button_rect()) {
            self.toggle_repair_mode();
            return;
        }
        // Superweapon ready-indicator buttons (marquee arc P1): clicking a *ready*
        // one enters its target-select mode; clicking one that is still charging
        // is a no-op (the clock shows the progress).
        if let Some(kind) = self.sw_button_at(x, y) {
            self.arm_superweapon(kind);
            return;
        }
        let Some(idx) = self.sidebar_row_at(x, y) else {
            return;
        };
        let items = self.sidebar_items();
        let item = &items[idx];
        if item.ready {
            if let BuildItem::Building(id) = item.item {
                self.begin_placement(id);
            }
        } else if item.buildable {
            let it = item.item;
            self.start_production(it);
        }
    }

    // ---- Build UI rendering ----

    /// Draw ore cells as small coloured squares over the tactical terrain.
    fn draw_ore(&self, frame: &mut RgbaImage, cam: Rect) {
        let ore = &self.world.ore;
        // Iterate only the visible cell band.
        let cx0 = (cam.x.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cy0 = (cam.y.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cx1 = cx0 + (self.tactical_width() as i32 / CELL_PIXELS) + 3;
        let cy1 = cy0 + (self.viewport_h as i32 / CELL_PIXELS) + 3;
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                let c = CellCoord::new(cx, cy);
                let cell = ore.at(c);
                if cell.bails == 0 {
                    continue;
                }
                let px = (cx * CELL_PIXELS) as i64 - cam.x;
                let py = (cy * CELL_PIXELS) as i64 - cam.y;

                // Real overlay art when installed: pick a tile variant from the
                // cell coordinates (cosmetic-only variety — the sim tracks only
                // density+kind, the original stored the GOLD01..04 variant in the
                // overlay byte) and the density stage (`bails - 1`) as the frame.
                let tiles = if cell.gem {
                    &self.ore_gem_sprites
                } else {
                    &self.ore_gold_sprites
                };
                if !tiles.is_empty() {
                    let variant = (cx.wrapping_mul(7) ^ cy.wrapping_mul(13))
                        .rem_euclid(tiles.len() as i32) as usize;
                    let sprite = &tiles[variant];
                    let nframes = sprite.frames.len().max(1);
                    let stage = ((cell.bails as usize).saturating_sub(1)).min(nframes - 1);
                    if let Some(sframe) = sprite.frames.get(stage) {
                        draw_sprite_topleft(
                            frame,
                            px as i32,
                            py as i32,
                            sframe,
                            &identity_remap(),
                            &self.palette,
                        );
                        continue;
                    }
                }

                // Fallback: flat rectangle whose size grows with density.
                let rgb = if cell.gem { ORE_GEM_RGB } else { ORE_GOLD_RGB };
                let inset = (CELL_PIXELS - 4 - (cell.bails as i32).min(CELL_PIXELS - 6)).max(2) / 2;
                fill_rect(
                    frame,
                    px as i32 + inset,
                    py as i32 + inset,
                    px as i32 + CELL_PIXELS - inset,
                    py as i32 + CELL_PIXELS - inset,
                    rgb,
                );
            }
        }
    }

    /// Draw the live cosmetic effects at their current animation frame. Explosions
    /// are centre-anchored; buildups are top-left-anchored (like the building).
    /// House-neutral art, so the identity remap is used.
    fn draw_effects(&self, frame: &mut RgbaImage, cam: Rect) {
        let remap = identity_remap();
        for e in &self.effects {
            let elapsed = self.anim_ms.saturating_sub(e.start_ms);
            let fi = (elapsed / FX_FRAME_MS) as usize;
            // Synthetic chronosphere warp flash: expanding cyan rings (no art).
            if matches!(e.kind, EffectKind::Warp) {
                let px = (leptons_to_pixel(e.anchor.x.0) as i64 - cam.x) as i32;
                let py = (leptons_to_pixel(e.anchor.y.0) as i64 - cam.y) as i32 - e.lift_px;
                let stage = (elapsed / FX_FRAME_MS) as i32;
                let r = 3 + stage * 2;
                let fade = 255u8.saturating_sub((stage * 26).min(230) as u8);
                for a in 0..40 {
                    let t = a as f32 * std::f32::consts::PI / 20.0;
                    let rx = px + (r as f32 * t.cos()) as i32;
                    let ry = py + (r as f32 * t.sin()) as i32;
                    put_pixel(frame, rx, ry, [120, 230, fade.max(160)]);
                }
                continue;
            }
            let (sprite, centered) = match e.kind {
                EffectKind::Explosion => (self.explosion_sprite.first(), true),
                EffectKind::Buildup(id) | EffectKind::Deconstruct(id) => (
                    self.buildup_sprites
                        .get(id as usize)
                        .and_then(|o| o.as_ref()),
                    false,
                ),
                EffectKind::Warp => (None, true), // handled above
            };
            let Some(sprite) = sprite else { continue };
            // Deconstruct plays the buildup band in *reverse* (the original's
            // BSTATE_CONSTRUCTION reverse sequence, building.cpp:602-606).
            let fi = match e.kind {
                EffectKind::Deconstruct(_) => sprite.frames.len().saturating_sub(1 + fi),
                _ => fi,
            };
            let Some(sframe) = sprite.frames.get(fi) else {
                continue;
            };
            let px = (leptons_to_pixel(e.anchor.x.0) as i64 - cam.x) as i32;
            let py = (leptons_to_pixel(e.anchor.y.0) as i64 - cam.y) as i32 - e.lift_px;
            if centered {
                draw_sprite_centered(frame, px, py, sframe, &remap, &self.palette);
            } else {
                draw_sprite_topleft(frame, px, py, sframe, &remap, &self.palette);
            }
        }
    }

    /// Draw all buildings from their SHP idle frame (top-left anchored).
    fn draw_buildings(&self, frame: &mut RgbaImage, cam: Rect) {
        for (_h, b) in self.world.buildings.iter() {
            let remap = self
                .remaps
                .get(b.house as usize)
                .copied()
                .unwrap_or_else(identity_remap);
            let px = (b.cell.x * CELL_PIXELS) as i64 - cam.x;
            let py = (b.cell.y * CELL_PIXELS) as i64 - cam.y;
            let drawn = self.building_sprites.get(b.type_id as usize).and_then(|s| {
                s.frames.first().map(|f| {
                    draw_sprite_topleft(frame, px as i32, py as i32, f, &remap, &self.palette);
                })
            });
            // Two-part buildings: overlay shape on top of the base (WEAP2 roof/
            // door for the war factory, building.cpp:513).
            if let Some(Some(ov)) = self.building_overlays.get(b.type_id as usize) {
                if let Some(f) = ov.frames.first() {
                    draw_sprite_topleft(frame, px as i32, py as i32, f, &remap, &self.palette);
                }
            }
            if drawn.is_none() {
                // No sprite: fall back to a filled footprint so it is visible.
                // Walls get a distinct darker fill so they read as barriers.
                let fill = if b.is_wall {
                    [120, 110, 80]
                } else {
                    [90, 90, 110]
                };
                fill_rect(
                    frame,
                    px as i32,
                    py as i32,
                    px as i32 + b.foot_w as i32 * CELL_PIXELS,
                    py as i32 + b.foot_h as i32 * CELL_PIXELS,
                    fill,
                );
            }
            // GUN turret barrel: a short line from the emplacement centre in the
            // (sim-tracked) turret facing, so the rotating turret is visible even
            // without dedicated turret SHP frames.
            if b.has_turret {
                let ccx = px as i32 + b.foot_w as i32 * CELL_PIXELS / 2;
                let ccy = py as i32 + b.foot_h as i32 * CELL_PIXELS / 2;
                let (bx, by) = offset_pixels(ccx, ccy, b.turret_facing, CELL_PIXELS / 2);
                draw_line(frame, ccx, ccy, bx, by, [40, 40, 48]);
                fill_rect(frame, bx - 1, by - 1, bx + 1, by + 1, [30, 30, 36]);
            }
            // Iron-curtain tint over an invulnerable building (marquee arc P2).
            if b.iron_curtain > 0 {
                let cxp = px as i32 + b.foot_w as i32 * CELL_PIXELS / 2;
                let cyp = py as i32 + b.foot_h as i32 * CELL_PIXELS / 2;
                let hw = b.foot_w as i32 * CELL_PIXELS / 2;
                let hh = b.foot_h as i32 * CELL_PIXELS / 2;
                self.draw_iron_tint_box(frame, cxp - hw, cyp - hh, cxp + hw, cyp + hh);
            }
            // Damage bar.
            if b.health < b.max_health {
                draw_health_bar(
                    frame,
                    px as i32 + b.foot_w as i32 * CELL_PIXELS / 2,
                    py as i32 - 4,
                    b.foot_w as i32 * CELL_PIXELS,
                    b.health_permille(),
                );
            }
        }
    }

    /// Iron-curtain tint centred at `(cx, cy)` with half-extent `half` — a pulsing
    /// blue/metallic wash (marquee arc P2). Pulse amplitude rides the cosmetic
    /// clock, so it never couples to the sim.
    fn draw_iron_tint(&self, frame: &mut RgbaImage, cx: i32, cy: i32, half: i32) {
        self.draw_iron_tint_box(frame, cx - half, cy - half, cx + half, cy + half);
    }

    /// Iron-curtain tint over an arbitrary box (shared by units and buildings).
    fn draw_iron_tint_box(&self, frame: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32) {
        // Pulse the alpha 70..150 on a ~0.5s cosmetic cycle for a "shimmer".
        let phase = (self.anim_ms / 90) % 8;
        let a = 70 + (phase.abs_diff(4) as u8) * 18;
        fill_rect_alpha(frame, x0, y0, x1, y1, [120, 170, 235], a);
    }

    /// Draw the green/red footprint preview while placing a building.
    fn draw_placement_preview(&self, frame: &mut RgbaImage, cam: Rect, tw: u32) {
        let (Some(building), Some(house)) = (self.placing, self.player_house) else {
            return;
        };
        if !self.mouse_inside || self.mouse_x >= tw as i32 {
            return;
        }
        let Some(proto) = self.world.catalog.building(building) else {
            return;
        };
        let cell = self.cell_at_viewport(self.mouse_x, self.mouse_y);
        let ok = self.world.can_place_building(house, building, cell);
        let rgb = if ok { [0, 220, 0] } else { [220, 0, 0] };
        for dy in 0..proto.foot_h as i32 {
            for dx in 0..proto.foot_w as i32 {
                let px = ((cell.x + dx) * CELL_PIXELS) as i64 - cam.x;
                let py = ((cell.y + dy) * CELL_PIXELS) as i64 - cam.y;
                draw_rect_outline(
                    frame,
                    px as i32,
                    py as i32,
                    px as i32 + CELL_PIXELS - 1,
                    py as i32 + CELL_PIXELS - 1,
                    rgb,
                );
            }
        }
    }

    /// Sell/repair-mode indicator: tint the footprint of the own building under
    /// the cursor (red for sell, green for repair) so the player sees what a click
    /// would act on. Nothing is tinted over enemy buildings / units / empty ground
    /// — the same gate as the command emission, so the visual can't imply an
    /// illegal action.
    fn draw_action_hover(&self, frame: &mut RgbaImage, cam: Rect, tw: u32) {
        if !(self.sell_mode || self.repair_mode) {
            return;
        }
        let Some(house) = self.player_house else {
            return;
        };
        if !self.mouse_inside || self.mouse_x >= tw as i32 {
            return;
        }
        let (mx, my) = self.viewport_to_map(self.mouse_x, self.mouse_y);
        let Some(h) = self.own_building_at_map(mx, my, house) else {
            return;
        };
        let Some(b) = self.world.buildings.get(h) else {
            return;
        };
        let rgb = if self.sell_mode {
            [230, 60, 50]
        } else {
            [70, 210, 70]
        };
        for c in b.footprint() {
            let px = (c.x * CELL_PIXELS) as i64 - cam.x;
            let py = (c.y * CELL_PIXELS) as i64 - cam.y;
            fill_rect_alpha(
                frame,
                px as i32,
                py as i32,
                px as i32 + CELL_PIXELS - 1,
                py as i32 + CELL_PIXELS - 1,
                rgb,
                110,
            );
        }
    }

    /// Pulsing wrench over every building currently repairing — the original's
    /// `IsRepairing && IsWrenchVisible` overlay (`building.cpp:520`,
    /// `CC_Draw_Shape(SelectShapes, SELECT_WRENCH, ...)`). The wrench blinks (the
    /// original toggles `IsWrenchVisible` each repair step); we blink it on the
    /// cosmetic clock so it draws without any sim coupling.
    fn draw_repair_indicators(&self, frame: &mut RgbaImage, cam: Rect) {
        // Blink: on for two of every three ~180ms phases (a clear pulse).
        if (self.anim_ms / WRENCH_BLINK_MS) % 3 == 2 {
            return;
        }
        let tw = self.tactical_width() as i32;
        for (_h, b) in self.world.buildings.iter() {
            if !b.is_repairing || !b.is_alive() {
                continue;
            }
            let cx = ((b.cell.x * CELL_PIXELS + b.foot_w as i32 * CELL_PIXELS / 2) as i64 - cam.x)
                as i32;
            let cy = ((b.cell.y * CELL_PIXELS + b.foot_h as i32 * CELL_PIXELS / 2) as i64 - cam.y)
                as i32;
            if cx < 0 || cx >= tw {
                continue;
            }
            self.draw_wrench(frame, cx, cy);
        }
    }

    /// Draw the wrench glyph centred at `(cx, cy)`: the real `SELECT.SHP`
    /// wrench frame when installed, else a synthetic spanner primitive.
    fn draw_wrench(&self, frame: &mut RgbaImage, cx: i32, cy: i32) {
        if let Some(w) = self
            .wrench_art
            .as_ref()
            .and_then(|s| s.frames.get(SELECT_WRENCH_FRAME))
        {
            draw_sprite_centered(frame, cx, cy, w, &identity_remap(), &self.palette);
            return;
        }
        // Synthetic spanner: a light diagonal shaft with a blob at each end,
        // dark-outlined for legibility over any background.
        let outline = [20, 20, 24];
        let steel = [210, 215, 225];
        draw_line(frame, cx - 6, cy + 6, cx + 6, cy - 6, outline);
        draw_line(frame, cx - 5, cy + 6, cx + 7, cy - 6, steel);
        for (ex, ey) in [(cx - 6, cy + 6), (cx + 6, cy - 6)] {
            fill_rect(frame, ex - 2, ey - 2, ex + 2, ey + 2, outline);
            fill_rect(frame, ex - 1, ey - 1, ex + 1, ey + 1, steel);
        }
    }

    /// Draw the "SELL MODE" / "REPAIR MODE" reminder banner near the top of the
    /// tactical area while a mode is armed. The explicit state reminder the
    /// player asked for (the cursor is the primary signal); nothing draws when no
    /// mode is armed, so no existing frame is affected.
    fn draw_mode_banner(&self, frame: &mut RgbaImage) {
        let sw_text = self.sw_fire_mode.map(|k| match k {
            SuperKind::Nuclear => "SELECT NUKE TARGET",
            SuperKind::IronCurtain => "SELECT IRON CURTAIN TARGET",
            SuperKind::Chronosphere => {
                if self.sw_chrono_source.is_some() {
                    "SELECT CHRONO DESTINATION"
                } else {
                    "SELECT CHRONO UNIT"
                }
            }
        });
        let (text, rgb) = if self.sell_mode {
            ("SELL MODE", [235, 80, 70])
        } else if self.repair_mode {
            ("REPAIR MODE", [80, 205, 95])
        } else if let Some(t) = sw_text {
            (t, [245, 210, 80])
        } else {
            return;
        };
        let scale = 2;
        let tw = self.tactical_width() as i32;
        let pad = 4;
        let text_w = font::text_width(text) * scale;
        let text_h = font::GLYPH_H * scale;
        let bx = ((tw - text_w) / 2 - pad).max(0);
        let by = 6;
        fill_rect(
            frame,
            bx,
            by,
            bx + text_w + pad * 2,
            by + text_h + pad * 2,
            [14, 14, 18],
        );
        draw_rect_outline(
            frame,
            bx,
            by,
            bx + text_w + pad * 2,
            by + text_h + pad * 2,
            rgb,
        );
        font::draw_text_scaled(frame, bx + pad, by + pad, text, rgb, scale);
    }

    /// Draw the mode cursor glyph at the pointer — the primary sell/repair-mode
    /// signal. Sell shows a gold "$" (`MOUSE_SELL_BACK`), repair shows the wrench
    /// (`MOUSE_REPAIR`); the "no" variants overlay a red prohibition slash when
    /// the pointer is not over a valid own building. Nothing draws when no mode is
    /// armed (`CursorKind::Normal`), so existing frames are untouched.
    ///
    /// **Deviation (documented):** the original's cursor art lives in `MOUSE.SHP`,
    /// a legacy variable-size shape container our `SHP` decoder does not read
    /// (each frame carries its own dimensions; no global width/height). The frame
    /// indices are cited on [`CursorKind`]. We render a faithful stand-in: the
    /// real `SELECT.SHP` wrench for repair and a bitmap-font "$" for sell.
    fn draw_mode_cursor(&self, frame: &mut RgbaImage) {
        let kind = self.cursor_kind();
        let (cx, cy) = (self.mouse_x, self.mouse_y);
        match kind {
            CursorKind::Normal => {}
            CursorKind::Sell | CursorKind::NoSell => {
                self.draw_glyph_cursor(frame, cx, cy, "$", [245, 210, 70]);
                if kind == CursorKind::NoSell {
                    draw_prohibit(frame, cx, cy);
                }
            }
            CursorKind::Repair | CursorKind::NoRepair => {
                self.draw_wrench(frame, cx, cy);
                if kind == CursorKind::NoRepair {
                    draw_prohibit(frame, cx, cy);
                }
            }
            CursorKind::SuperTarget => self.draw_target_reticle(frame, cx, cy),
        }
    }

    /// Draw the superweapon targeting reticle at `(cx, cy)`: a crosshair inside a
    /// ring, dark-outlined for legibility over any background. The chronosphere's
    /// second step (destination pick) tints it cyan; other picks are red.
    fn draw_target_reticle(&self, frame: &mut RgbaImage, cx: i32, cy: i32) {
        let rgb = if matches!(self.sw_fire_mode, Some(SuperKind::Chronosphere))
            && self.sw_chrono_source.is_some()
        {
            [90, 220, 245] // chrono destination step
        } else {
            [245, 70, 60]
        };
        let r = 8;
        // Ring (double outline for contrast).
        draw_rect_outline(
            frame,
            cx - r - 1,
            cy - r - 1,
            cx + r + 1,
            cy + r + 1,
            [12, 12, 14],
        );
        for a in 0..48 {
            let t = a as f32 * std::f32::consts::PI / 24.0;
            let px = cx + (r as f32 * t.cos()) as i32;
            let py = cy + (r as f32 * t.sin()) as i32;
            put_pixel(frame, px, py, rgb);
        }
        // Crosshair arms.
        for d in -(r + 3)..=(r + 3) {
            put_pixel(
                frame,
                cx + d,
                cy,
                if d.abs() <= 2 { [12, 12, 14] } else { rgb },
            );
            put_pixel(
                frame,
                cx,
                cy + d,
                if d.abs() <= 2 { [12, 12, 14] } else { rgb },
            );
        }
    }

    /// Draw a scale-2 bitmap-font glyph centred at `(cx, cy)` with a dark
    /// outline, so it reads as a cursor over any background.
    fn draw_glyph_cursor(&self, frame: &mut RgbaImage, cx: i32, cy: i32, text: &str, rgb: [u8; 3]) {
        let scale = 2;
        let x = cx - font::text_width(text) * scale / 2;
        let y = cy - font::GLYPH_H * scale / 2;
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            font::draw_text_scaled(frame, x + dx, y + dy, text, [12, 12, 14], scale);
        }
        font::draw_text_scaled(frame, x, y, text, rgb, scale);
    }

    /// Draw the build sidebar: credits + power header, then buildable rows with
    /// cost, a build-progress bar, or a READY tag.
    fn draw_sidebar(&self, frame: &mut RgbaImage) {
        let x0 = self.tactical_width() as i32;
        let w = self.viewport_w as i32;
        // Background panel.
        fill_rect(frame, x0, 0, w - 1, self.viewport_h as i32 - 1, SIDEBAR_BG);

        let pad = 4;
        let tx = x0 + pad;
        // Header: credits + power.
        let credits = self.credits();
        let (out, drain) = self.power();
        font::draw_text(frame, tx, 2, &format!("$ {credits}"), [240, 220, 80]);
        let low = drain > out;
        let pcol = if low { [230, 80, 80] } else { [120, 220, 120] };
        font::draw_text(
            frame,
            tx,
            2 + font::GLYPH_H + 2,
            &format!("PWR {out}/{drain}"),
            pcol,
        );

        // SELL / REPAIR mode buttons in the header's right edge.
        self.draw_mode_buttons(frame);

        // Radar minimap panel (top of the strip, under the header).
        self.draw_radar(frame);

        // Two build strips: structures (col 0, left) then units (col 1, right),
        // each scrolled independently through its own `TopIndex`.
        for col in 0..SIDEBAR_COLUMNS {
            self.draw_sidebar_column(frame, col);
        }

        // Superweapon ready/charge indicators at the bottom of the strip.
        self.draw_sw_buttons(frame);
    }

    /// Draw the superweapon ready/charge indicator buttons (marquee arc P1) —
    /// one per owned superweapon, stacked at the bottom of the sidebar. Each shows
    /// the SW's cameo (or a text label), a **recharge clock** whose lit sweep
    /// reflects [`World::superweapon_charge_permille`], and a bright "READY" state
    /// when fully charged (the original's pie/clock over the special-weapon button,
    /// `sidebar.cpp` `SuperClass`/`Flash_Clock`). Nothing draws when the player
    /// owns no superweapon, so no existing frame is touched.
    fn draw_sw_buttons(&self, frame: &mut RgbaImage) {
        let Some(house) = self.player_house else {
            return;
        };
        for (kind, (x0, y0, x1, y1)) in self.sw_buttons() {
            let ready = self.world.superweapon_ready(house, kind);
            let permille = self
                .world
                .superweapon_charge_permille(house, kind)
                .unwrap_or(0);
            // Panel: green when ready, dim slate while charging.
            let bg = if ready { [26, 72, 30] } else { [34, 34, 42] };
            fill_rect(frame, x0, y0, x1, y1, bg);
            let border = if ready { [120, 240, 120] } else { [80, 80, 96] };
            draw_rect_outline(frame, x0, y0, x1, y1, border);

            // Recharge clock at the right edge (radius from the button height).
            let r = (SW_BTN_H / 2 - 2).max(4);
            let ccx = x1 - r - 2;
            let ccy = (y0 + y1) / 2;
            self.draw_charge_clock(frame, ccx, ccy, r, permille, ready);

            // Label at the left: the short SW name (the full cameo already shows in
            // the build strip; this compact indicator is text + clock).
            let tcol = if ready {
                [200, 245, 200]
            } else {
                [200, 200, 210]
            };
            let label = if ready {
                match kind {
                    SuperKind::Nuclear => "NUKE RDY",
                    SuperKind::IronCurtain => "IRON RDY",
                    SuperKind::Chronosphere => "CHRONO RDY",
                }
            } else {
                sw_short_label(kind)
            };
            font::draw_text(frame, x0 + 3, ccy - font::GLYPH_H / 2, label, tcol);
            if !ready {
                // Percent charged, under the label.
                font::draw_text(
                    frame,
                    x0 + 3,
                    ccy + font::GLYPH_H / 2 + 1,
                    &format!("{}%", permille / 10),
                    [150, 150, 165],
                );
            }
        }
    }

    /// Draw a recharge "clock": a filled disc whose lit sweep (clockwise from the
    /// top) covers `permille`/1000 of the circle, the rest darkened — the original
    /// `Flash_Clock`/`Draw_Clock` pie over a special-weapon button. Fully lit (and
    /// a small tick highlight) when `ready`.
    fn draw_charge_clock(
        &self,
        frame: &mut RgbaImage,
        cx: i32,
        cy: i32,
        r: i32,
        permille: i32,
        ready: bool,
    ) {
        let frac = (permille.clamp(0, 1000) as f32) / 1000.0;
        let lit = if ready {
            [130, 245, 130]
        } else {
            [90, 170, 240]
        };
        let dark = [20, 22, 30];
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                // Angle clockwise from straight up (12 o'clock), in [0,1).
                let ang = (dx as f32).atan2(-dy as f32); // 0 at top, +→clockwise
                let mut t = ang / (2.0 * std::f32::consts::PI);
                if t < 0.0 {
                    t += 1.0;
                }
                let rgb = if ready || t <= frac { lit } else { dark };
                put_pixel(frame, cx + dx, cy + dy, rgb);
            }
        }
        // Rim.
        draw_rect_outline(frame, cx - r, cy - r, cx + r, cy + r, [10, 12, 16]);
    }

    /// Draw the SELL and REPAIR mode buttons (M7.9 P1). Each shows highlighted
    /// (bright fill) when its mode is armed, dim otherwise — the sell-mode
    /// indicator the task calls for.
    fn draw_mode_buttons(&self, frame: &mut RgbaImage) {
        // Icon button: draw the original SHP at the rect's top-left — frame 1
        // (pressed) while the mode is armed, else frame 0 (up). This mirrors the
        // original `ShapeButtonClass` with `IsToggleType`/`ReflectButtonState`
        // (`sidebar.cpp:303-321`): a toggled button shows its pressed frame.
        let icon = |frame: &mut RgbaImage,
                    (x0, y0, _, _): (i32, i32, i32, i32),
                    art: &UnitSprite,
                    armed: bool| {
            let idx = if armed { 1 } else { 0 };
            if let Some(f) = art.frames.get(idx).or_else(|| art.frames.first()) {
                draw_sprite_topleft(frame, x0, y0, f, &identity_remap(), &self.palette);
            }
        };
        // Text fallback button (no art installed).
        let text_btn = |frame: &mut RgbaImage,
                        (x0, y0, x1, y1): (i32, i32, i32, i32),
                        label: &str,
                        armed: bool,
                        armed_rgb: [u8; 3]| {
            let bg = if armed { armed_rgb } else { [46, 46, 54] };
            fill_rect(frame, x0, y0, x1 - 1, y1 - 1, bg);
            draw_rect_outline(frame, x0, y0, x1 - 1, y1 - 1, [90, 90, 100]);
            let tcol = if armed { [12, 12, 12] } else { [200, 200, 210] };
            font::draw_text(frame, x0 + 2, y0 + 1, label, tcol);
        };

        match &self.sell_button_art {
            Some(art) => icon(frame, self.sell_button_rect(), art, self.sell_mode),
            None => text_btn(
                frame,
                self.sell_button_rect(),
                "SELL",
                self.sell_mode,
                [230, 90, 80],
            ),
        }
        match &self.repair_button_art {
            Some(art) => icon(frame, self.repair_button_rect(), art, self.repair_mode),
            None => text_btn(
                frame,
                self.repair_button_rect(),
                "REP",
                self.repair_mode,
                [120, 200, 120],
            ),
        }
    }

    /// Draw one build strip (column) of cameo rows plus, when it overflows, its
    /// up/down scroll arrows. Only the visible window `[scroll .. scroll+rows]`
    /// is drawn.
    fn draw_sidebar_column(&self, frame: &mut RgbaImage, col: usize) {
        let cx0 = self.column_x(col);
        let row_h = self.sidebar_row_h();
        let rows = self.sidebar_visible_rows();
        let scroll = self.sidebar_scroll(col);
        let items = self.column_items(col);
        let mut ry = self.sidebar_rows_top();

        for slot in 0..rows {
            let Some(item) = items.get(scroll + slot) else {
                break;
            };
            let row_bg = if item.ready {
                [30, 70, 30]
            } else if item.buildable {
                [40, 40, 52]
            } else {
                [30, 30, 34]
            };
            fill_rect(frame, cx0, ry, cx0 + COLUMN_W - 1, ry + row_h - 2, row_bg);
            let active = item.buildable || item.progress.is_some() || item.ready;
            let name_col = if active {
                [230, 230, 230]
            } else {
                [110, 110, 120]
            };

            // Cameo art when installed; else the item's short name (text fallback).
            let label_y = if let Some(sprite) = self.cameo_for(item.item) {
                if let Some(f) = sprite.frames.first() {
                    draw_sprite_topleft(frame, cx0, ry + 2, f, &identity_remap(), &self.palette);
                    if !active {
                        fill_rect_alpha(
                            frame,
                            cx0,
                            ry + 2,
                            cx0 + CAMEO_W,
                            ry + 2 + CAMEO_H,
                            [10, 10, 14],
                            140,
                        );
                    }
                }
                ry + CAMEO_H + 2
            } else {
                font::draw_text(frame, cx0 + 2, ry + 2, &item.name, name_col);
                ry + 2 + font::GLYPH_H + 1
            };

            // Cost line, and a ready tag or a progress bar under the cameo.
            font::draw_text(
                frame,
                cx0 + 2,
                label_y,
                &format!("${}", item.cost),
                [180, 180, 140],
            );
            if item.ready {
                font::draw_text(
                    frame,
                    cx0 + 2,
                    label_y + font::GLYPH_H,
                    "RDY",
                    [120, 240, 120],
                );
            } else if let Some(pm) = item.progress {
                let bx0 = cx0 + 2;
                let bx1 = cx0 + COLUMN_W - 3;
                fill_rect(
                    frame,
                    bx0,
                    label_y + font::GLYPH_H,
                    bx1,
                    label_y + font::GLYPH_H + 4,
                    [20, 20, 24],
                );
                let fill = bx0 + (bx1 - bx0) * pm / 1000;
                fill_rect(
                    frame,
                    bx0,
                    label_y + font::GLYPH_H,
                    fill,
                    label_y + font::GLYPH_H + 4,
                    [80, 160, 240],
                );
            }
            ry += row_h;
        }

        // Scroll arrows (only when the column overflows its visible window).
        if let Some((up, down)) = self.scroll_buttons(col) {
            let at_top = scroll == 0;
            let at_bottom = scroll >= self.max_scroll(col);
            self.draw_scroll_arrow(frame, up, true, !at_top);
            self.draw_scroll_arrow(frame, down, false, !at_bottom);
        }
    }

    /// Draw a single up/down scroll arrow in `rect`. `enabled` brightens it.
    fn draw_scroll_arrow(
        &self,
        frame: &mut RgbaImage,
        rect: (i32, i32, i32, i32),
        up: bool,
        enabled: bool,
    ) {
        let (x0, y0, x1, y1) = rect;
        let bg = if enabled { [70, 70, 84] } else { [38, 38, 44] };
        fill_rect(frame, x0, y0, x1 - 1, y1 - 1, bg);
        let fg = if enabled {
            [230, 230, 240]
        } else {
            [90, 90, 100]
        };
        // A little triangle: rows of a centred run of pixels.
        let cx = (x0 + x1) / 2;
        let h = (y1 - y0 - 4).max(3);
        for i in 0..h {
            // up: widen toward the bottom; down: widen toward the top.
            let spread = if up { i } else { h - 1 - i };
            let yy = y0 + 2 + i;
            fill_rect(frame, cx - spread, yy, cx + spread, yy, fg);
        }
    }

    /// Look up the cameo sprite for a buildable (parallel to `buildables`).
    fn cameo_for(&self, item: BuildItem) -> Option<&UnitSprite> {
        let idx = self.buildables.iter().position(|&b| b == item)?;
        self.cameo_sprites.get(idx).and_then(|o| o.as_ref())
    }

    /// Draw the radar minimap: explored terrain (dim), ore tint, house-coloured
    /// building/unit markers, and the current camera view box. Reads sim state
    /// only. No-op if the radar is disabled.
    fn draw_radar(&self, frame: &mut RgbaImage) {
        let Some((rx, ry, size)) = self.radar_rect() else {
            return;
        };
        let (mw, mh) = (self.map_cells_w().max(1), self.map_cells_h().max(1));
        // Panel backing + frame.
        fill_rect(frame, rx - 1, ry - 1, rx + size, ry + size, [8, 8, 12]);

        let house = self.player_house.unwrap_or(0);
        let shroud = &self.world.shroud;
        // Terrain / shroud / ore, one radar pixel per scaled cell.
        for py in 0..size {
            let cy = (py as i64 * mh as i64 / size as i64) as i32;
            for px in 0..size {
                let cx = (px as i64 * mw as i64 / size as i64) as i32;
                let c = CellCoord::new(cx, cy);
                let explored = shroud.is_explored(house, c);
                let rgb = if !explored {
                    [0, 0, 0]
                } else {
                    let ore = self.world.ore.at(c);
                    if ore.bails > 0 {
                        if ore.gem {
                            [60, 110, 160]
                        } else {
                            [150, 125, 40]
                        }
                    } else {
                        [40, 52, 40]
                    }
                };
                put_pixel(frame, rx + px, ry + py, rgb);
            }
        }
        // Building footprints (house colour) — only where explored.
        for (_h, b) in self.world.buildings.iter() {
            let col = house_dot(b.house);
            for fc in b.footprint() {
                if !shroud.is_explored(house, fc) {
                    continue;
                }
                let px = rx + (fc.x as i64 * size as i64 / mw as i64) as i32;
                let py = ry + (fc.y as i64 * size as i64 / mh as i64) as i32;
                put_pixel(frame, px, py, col);
            }
        }
        // Unit dots (house colour) — only where explored.
        for (_h, u) in self.world.units.iter() {
            let c = u.cell();
            if !shroud.is_explored(house, c) {
                continue;
            }
            let px = rx + (c.x as i64 * size as i64 / mw as i64) as i32;
            let py = ry + (c.y as i64 * size as i64 / mh as i64) as i32;
            put_pixel(frame, px, py, house_dot(u.house));
        }
        // Camera view box.
        let cam = self.camera_rect();
        let vx0 = rx + (cam.x * size as i64 / (mw as i64 * CELL_PIXELS as i64)) as i32;
        let vy0 = ry + (cam.y * size as i64 / (mh as i64 * CELL_PIXELS as i64)) as i32;
        let vx1 = rx
            + ((cam.x + cam.width as i64) * size as i64 / (mw as i64 * CELL_PIXELS as i64)) as i32;
        let vy1 = ry
            + ((cam.y + cam.height as i64) * size as i64 / (mh as i64 * CELL_PIXELS as i64)) as i32;
        draw_rect_outline(
            frame,
            vx0,
            vy0,
            vx1.min(rx + size),
            vy1.min(ry + size),
            [230, 230, 230],
        );
    }

    /// Drain queued sim commands emitted since the last call (for the transport
    /// / tests). Terrain-only cores never emit any.
    pub fn drain_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.emitted)
    }
}

/// Stable ordering index for a superweapon kind (for the sidebar indicator stack
/// + owned-kind dedup). Mirrors the sim's `super_kind_tag`.
fn sw_kind_index(kind: SuperKind) -> usize {
    match kind {
        SuperKind::Nuclear => 0,
        SuperKind::IronCurtain => 1,
        SuperKind::Chronosphere => 2,
    }
}

/// Short font-safe label for a superweapon kind (the charging-indicator caption).
fn sw_short_label(kind: SuperKind) -> &'static str {
    match kind {
        SuperKind::Nuclear => "NUKE",
        SuperKind::IronCurtain => "IRON",
        SuperKind::Chronosphere => "CHRONO",
    }
}

/// Set a single clipped opaque pixel.
fn put_pixel(dst: &mut RgbaImage, x: i32, y: i32, rgb: [u8; 3]) {
    if x < 0 || y < 0 || x as u32 >= dst.width || y as u32 >= dst.height {
        return;
    }
    let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
    dst.pixels[di] = rgb[0];
    dst.pixels[di + 1] = rgb[1];
    dst.pixels[di + 2] = rgb[2];
    dst.pixels[di + 3] = 255;
}

/// Alpha-blend a solid colour over a clipped rectangle (`alpha` 0..=255).
fn fill_rect_alpha(
    dst: &mut RgbaImage,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    rgb: [u8; 3],
    alpha: u8,
) {
    let (xa, xb) = (x0.min(x1).max(0), x1.max(x0).min(dst.width as i32 - 1));
    let (ya, yb) = (y0.min(y1).max(0), y1.max(y0).min(dst.height as i32 - 1));
    let a = alpha as u32;
    for y in ya..=yb {
        for x in xa..=xb {
            let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
            for (k, &c) in rgb.iter().enumerate() {
                let bg = dst.pixels[di + k] as u32;
                dst.pixels[di + k] = ((c as u32 * a + bg * (255 - a)) / 255) as u8;
            }
        }
    }
}

/// Convert a lepton coordinate to a terrain-raster pixel coordinate
/// (`CELL_PIXELS` per `LEPTONS_PER_CELL`).
fn leptons_to_pixel(leptons: i32) -> i32 {
    (leptons as i64 * CELL_PIXELS as i64 / LEPTONS_PER_CELL as i64) as i32
}

/// Offset a pixel point by `dist` pixels along a binary-angle facing (for muzzle
/// tips / tracer tails). Uses the sim's own [`ra_sim::coords::coord_move`] on a
/// scaled lepton point so the direction matches the sim exactly — this is
/// presentation only, never fed back into the sim.
fn offset_pixels(x: i32, y: i32, dir: Facing, dist: i32) -> (i32, i32) {
    let leptons = dist * LEPTONS_PER_CELL / CELL_PIXELS;
    let moved = ra_sim::coords::coord_move(WorldCoord::new(0, 0), dir, leptons);
    (
        x + leptons_to_pixel(moved.x.0),
        y + leptons_to_pixel(moved.y.0),
    )
}

/// Draw a bright line between two pixel points (Bresenham), clipped to `dst`.
/// Draw a red "prohibited" mark centred at `(cx, cy)` — a ring outline plus a
/// diagonal slash — over a cursor glyph to signal an invalid target (the
/// original's `MOUSE_NO_SELL_BACK` / `MOUSE_NO_REPAIR` cursors).
fn draw_prohibit(dst: &mut RgbaImage, cx: i32, cy: i32) {
    const R: i32 = 9;
    const RED: [u8; 3] = [235, 45, 45];
    draw_rect_outline(dst, cx - R, cy - R, cx + R, cy + R, RED);
    draw_line(dst, cx - R, cy - R, cx + R, cy + R, RED);
    draw_line(dst, cx - R, cy - R + 1, cx + R - 1, cy + R, RED);
}

fn draw_line(dst: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, rgb: [u8; 3]) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if x >= 0 && y >= 0 && (x as u32) < dst.width && (y as u32) < dst.height {
            let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
            dst.pixels[di] = rgb[0];
            dst.pixels[di + 1] = rgb[1];
            dst.pixels[di + 2] = rgb[2];
            dst.pixels[di + 3] = 255;
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> AppCore {
        // 100x80 px map (not cell-aligned; the camera math is pixel-based).
        let raster = IndexedImage::filled(100, 80, 7);
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [10, 20, 30];
        let mut c = AppCore::new(raster, pal);
        c.handle(InputEvent::Resize {
            width: 40,
            height: 30,
        });
        c
    }

    #[test]
    fn arrow_scroll_moves_and_clamps() {
        let mut c = core();
        c.handle(InputEvent::KeyDown(Key::Right));
        c.update(1000); // 1s * 640px/s, but clamped to map-viewport = 60
        assert_eq!(c.camera_rect().x, 60); // 100 - 40
        c.handle(InputEvent::KeyUp(Key::Right));
        c.handle(InputEvent::KeyDown(Key::Left));
        c.update(1000);
        assert_eq!(c.camera_rect().x, 0);
    }

    #[test]
    fn edge_scroll_triggers_on_margin() {
        let mut c = core();
        c.handle(InputEvent::MouseMoved { x: 2, y: 15 }); // near left edge
        c.update(100);
        assert_eq!(c.camera_rect().x, 0); // already at left, clamped
        c.handle(InputEvent::MouseMoved { x: 39, y: 15 }); // near right edge
        c.update(1000);
        assert_eq!(c.camera_rect().x, 60);
    }

    #[test]
    fn compose_size_matches_viewport() {
        let c = core();
        let f = c.compose(Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        });
        assert_eq!(f.width, 40);
        assert_eq!(f.height, 30);
        assert_eq!(f.pixels.len(), 40 * 30 * 4);
        assert_eq!(&f.pixels[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn compose_is_deterministic() {
        let c = core();
        let r = Rect {
            x: 5,
            y: 5,
            width: 40,
            height: 30,
        };
        assert_eq!(c.compose(r).pixels, c.compose(r).pixels);
    }

    #[test]
    fn no_commands_without_units() {
        let mut c = core();
        // Right-clicking with nothing selected emits nothing.
        c.handle(InputEvent::MouseDown {
            button: MouseButton::Right,
            x: 10,
            y: 10,
        });
        assert!(c.drain_commands().is_empty());
    }

    #[test]
    fn resize_is_bounded() {
        let mut c = core();
        c.handle(InputEvent::Resize {
            width: 100_000,
            height: 100_000,
        });
        let (w, h) = c.viewport_size();
        assert!(w <= MAX_VIEWPORT_DIM && h <= MAX_VIEWPORT_DIM);
    }
}
