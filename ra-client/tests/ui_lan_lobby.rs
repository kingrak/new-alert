//! M8-B proof test (f): the LAN lobby flow driven headlessly through the
//! menu/AppCore seam — two complete `App` instances in one process, real UDP
//! sockets on 127.0.0.1 with OS-assigned ports (the fixed discovery port is
//! never bound: the host's announcements are aimed at the joiner's
//! OS-assigned browser port via the test seam in `DiscoveryConfig`).
//!
//! Covered: host announce → joiner browse/see session → join → WELCOME
//! (host-authoritative settings cross) → READY (both-confirm) → START →
//! both build the same seed/scenario through their factories → both reach
//! `InGame` → the two sims advance in lockstep hash-identically → a clean
//! quit on one side surfaces "PLAYER LEFT THE GAME" on the other.
//! Plus the lobby unhappy paths: host-cancel and joiner-leave.
//!
//! All interaction goes through `handle(InputEvent)` clicks on the real
//! item geometry (the same path a mouse takes), with the public hook
//! methods used only for assertions.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ra_client::compositor::{IndexedImage, RgbaImage};
use ra_client::input::{InputEvent, MouseButton};
use ra_client::menu::{
    App, AppState, GameFactory, LanGameFactory, LanGameSpec, MapEntry, MapSource, ResolvedSkirmish,
};
use ra_client::AppCore;
use ra_net::DiscoveryConfig;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{MoveStats, Passability, World};

const WALL_SECS: u64 = 60;

/// Skirmish factory stub — required by `App::new`, never invoked here.
struct NoSkirmish;
impl GameFactory for NoSkirmish {
    fn build(&self, _res: &ResolvedSkirmish) -> Result<(AppCore, CellCoord), String> {
        Err("skirmish factory must not be used by the LAN flow".to_string())
    }
}

/// Synthetic LAN factory: builds a small deterministic world purely from the
/// spec (seed/credits/seats) — the world must be byte-identical on both
/// sides — and records every spec it was asked to build (the test's proof
/// that host and joiner resolved the same session).
struct SynthLanFactory {
    record: Rc<RefCell<Vec<LanGameSpec>>>,
}

impl LanGameFactory for SynthLanFactory {
    fn build(&self, spec: &LanGameSpec) -> Result<(AppCore, CellCoord), String> {
        self.record.borrow_mut().push(spec.clone());
        let mut world = World::new(Passability::all_passable(), spec.seed);
        world.init_houses(8, spec.credits);
        let stats = MoveStats {
            max_speed: 20,
            rot: 8,
        };
        // Spawn order keyed to the HOST seat on both sides — identical arena
        // handles, identical world hash (the same rule the real loader uses).
        let join_house = if spec.host_house == spec.local_house {
            spec.remote_house
        } else {
            spec.local_house
        };
        world.spawn_unit(
            0,
            spec.host_house,
            CellCoord::new(10, 10),
            Facing(0),
            100,
            stats,
        );
        world.spawn_unit(0, join_house, CellCoord::new(50, 50), Facing(0), 100, stats);
        let raster = IndexedImage {
            width: 16,
            height: 16,
            pixels: vec![0u8; 16 * 16],
        };
        let mut core = AppCore::with_sim(raster, [[0u8; 3]; 256], world, Vec::new(), Vec::new());
        core.enable_sidebar(spec.local_house, Vec::new());
        let start = if spec.local_house == spec.host_house {
            CellCoord::new(10, 10)
        } else {
            CellCoord::new(50, 50)
        };
        Ok((core, start))
    }
}

fn synth_maps() -> Vec<MapEntry> {
    vec![MapEntry {
        name: "Arena".to_string(),
        filename: "arena.ini".to_string(),
        players: 2,
        width: 64,
        height: 64,
        source: MapSource::Archive,
        preview: RgbaImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        },
    }]
}

fn lan_app(record: &Rc<RefCell<Vec<LanGameSpec>>>, name: &str) -> App {
    let factory = SynthLanFactory {
        record: Rc::clone(record),
    };
    let mut a = App::new(synth_maps(), Box::new(NoSkirmish)).with_lan(
        Box::new(factory),
        DiscoveryConfig {
            announce_targets: Vec::new(), // aimed explicitly by each test
            listen_port: 0,               // OS-assigned — never a fixed port
        },
        name,
    );
    a.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    a
}

fn click(a: &mut App, x: i32, y: i32) {
    a.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    a.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x,
        y,
    });
}

// Click coordinates derived from the layout code (1024x768 viewport).
const CX: i32 = 512;
/// Main menu: SKIRMISH y=364, CAMPAIGN-disabled +50, MULTIPLAYER +100.
fn click_multiplayer(a: &mut App) {
    click(a, CX, 364 + 100 + 18);
    assert_eq!(a.state(), AppState::MultiplayerMenu, "MULTIPLAYER click");
}
/// Multiplayer menu rows start at 768/2 - 60 = 324: HOST, +50 JOIN, +100 BACK.
fn click_host_game(a: &mut App) {
    click(a, CX, 324 + 18);
    assert_eq!(a.state(), AppState::LanHostSetup, "HOST GAME click");
}
fn click_join_game(a: &mut App) {
    click(a, CX, 324 + 50 + 18);
    assert_eq!(a.state(), AppState::LanJoinBrowse, "JOIN GAME click");
}
/// Host setup: CREATE GAME at y = 84 + 8*16 + 32 + 36 = 280.
fn click_create_game(a: &mut App) {
    click(a, 100, 280 + 16);
    assert_eq!(a.state(), AppState::LanHostLobby, "CREATE GAME click");
}
/// Lobby action row (host START / joiner READY) at y = 768 - 80.
fn click_lobby_primary(a: &mut App) {
    click(a, 100, 768 - 80 + 16);
}
/// Lobby secondary (host CANCEL / joiner LEAVE) at x offset 240.
fn click_lobby_secondary(a: &mut App) {
    click(a, 300, 768 - 80 + 16);
}
/// First session row in the browse list (y = 90..114).
fn click_first_session(a: &mut App) {
    click(a, 100, 100);
}

/// Pump both apps until `cond` holds (wall-guarded).
fn pump_until(
    host: &mut App,
    join: &mut App,
    what: &str,
    mut cond: impl FnMut(&App, &App) -> bool,
) {
    let start = Instant::now();
    while !cond(host, join) {
        host.update(16);
        join.update(16);
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "wall guard: {what} never happened"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wire a host and joiner together: joiner opens the browser first (binding
/// an OS port), then the host's announcements are aimed at it.
fn wired_pair(
    record_h: &Rc<RefCell<Vec<LanGameSpec>>>,
    record_j: &Rc<RefCell<Vec<LanGameSpec>>>,
) -> (App, App) {
    let mut host = lan_app(record_h, "HOSTP");
    let mut join = lan_app(record_j, "JOINP");
    click_multiplayer(&mut join);
    click_join_game(&mut join);
    let port = join.browser_port().expect("browser must be bound");
    host.lan_config_mut().announce_targets = vec![format!("127.0.0.1:{port}").parse().unwrap()];
    click_multiplayer(&mut host);
    click_host_game(&mut host);
    click_create_game(&mut host);
    assert!(host.host_lobby().is_some());
    (host, join)
}

/// The full happy path, end to end, plus lockstep identity and the clean
/// quit → "player left" end screen.
#[test]
fn lobby_flow_host_join_ready_start_reaches_ingame_with_same_seed_and_stays_locked() {
    let record_h = Rc::new(RefCell::new(Vec::new()));
    let record_j = Rc::new(RefCell::new(Vec::new()));
    let (mut host, mut join) = wired_pair(&record_h, &record_j);

    // Discovery: the joiner's list fills from the host's announcements.
    pump_until(&mut host, &mut join, "session discovered", |_, j| {
        !j.lan_sessions().is_empty()
    });
    let s = &join.lan_sessions()[0];
    assert!(s.compatible);
    assert_eq!(s.name, "HOSTP");
    assert_eq!(s.map, "arena.ini");

    // Join → WELCOME (host-authoritative settings cross the wire).
    click_first_session(&mut join);
    assert_eq!(join.state(), AppState::LanJoinLobby);
    pump_until(&mut host, &mut join, "welcome received", |_, j| {
        j.join_lobby()
            .map(|l| l.welcome().is_some())
            .unwrap_or(false)
    });
    {
        let w = join.join_lobby().unwrap().welcome().unwrap();
        assert_eq!(w.map, "arena.ini");
        assert_eq!(w.host_name, "HOSTP");
        assert_eq!(w.seat, App::LAN_JOIN_HOUSE);
        assert_eq!(w.host_seat, App::LAN_HOST_HOUSE);
    }
    pump_until(&mut host, &mut join, "host sees joiner", |h, _| {
        h.host_lobby()
            .map(|l| l.joiner_name() == Some("JOINP"))
            .unwrap_or(false)
    });
    assert!(
        !host.host_lobby().unwrap().can_start(),
        "START must be gated on the joiner's READY (both-confirm)"
    );

    // READY → host sees it → START → both build and enter.
    click_lobby_primary(&mut join); // READY
    pump_until(&mut host, &mut join, "host sees ready", |h, _| {
        h.host_lobby().map(|l| l.joiner_ready()).unwrap_or(false)
    });
    click_lobby_primary(&mut host); // START GAME
    assert_eq!(host.state(), AppState::InGame, "host enters on START");
    pump_until(&mut host, &mut join, "joiner enters game", |_, j| {
        j.state() == AppState::InGame
    });

    // Both factories were asked to build exactly the same session.
    let rh = record_h.borrow();
    let rj = record_j.borrow();
    assert_eq!(rh.len(), 1, "host built exactly one game");
    assert_eq!(rj.len(), 1, "joiner built exactly one game");
    assert_eq!(rh[0].map_filename, rj[0].map_filename);
    assert_eq!(rh[0].seed, rj[0].seed, "seed must be host-authoritative");
    assert_eq!(rh[0].credits, rj[0].credits);
    assert_eq!(rh[0].local_house, App::LAN_HOST_HOUSE);
    assert_eq!(rh[0].remote_house, App::LAN_JOIN_HOUSE);
    assert_eq!(rj[0].local_house, App::LAN_JOIN_HOUSE);
    assert_eq!(rj[0].remote_house, App::LAN_HOST_HOUSE);
    assert!(host.in_lan_game() && join.in_lan_game());
    drop(rh);
    drop(rj);

    // Lockstep: both sims advance and stay hash-identical.
    let start = Instant::now();
    loop {
        host.update(67);
        join.update(67);
        let th = host.core().unwrap().world().tick_count();
        let tj = join.core().unwrap().world().tick_count();
        if th >= 30 && tj >= 30 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "lockstep never reached tick 30 (host {th}, join {tj})"
        );
    }
    // Align tick counts exactly (the barrier keeps them within the delay
    // window; step only the laggard).
    let start = Instant::now();
    loop {
        let th = host.core().unwrap().world().tick_count();
        let tj = join.core().unwrap().world().tick_count();
        match th.cmp(&tj) {
            std::cmp::Ordering::Less => host.update(67),
            std::cmp::Ordering::Greater => join.update(67),
            std::cmp::Ordering::Equal => break,
        }
        assert!(start.elapsed() < Duration::from_secs(WALL_SECS));
    }
    assert_eq!(
        host.core().unwrap().sim_hash(),
        join.core().unwrap().sim_hash(),
        "the two menu-driven instances diverged"
    );

    // Clean quit: the joiner leaves; the host's client shows "player left".
    join.quit_to_menu();
    assert_eq!(join.state(), AppState::MainMenu);
    let start = Instant::now();
    while host.state() != AppState::NetEnded {
        host.update(16);
        assert!(
            start.elapsed() < Duration::from_secs(WALL_SECS),
            "host never saw the peer quit"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host.net_end_message(), "PLAYER LEFT THE GAME");
    // Continue returns to the main menu.
    click(&mut host, CX, 768 - 120 + 18);
    assert_eq!(host.state(), AppState::MainMenu);
}

/// Host-cancel: the joiner in the lobby is bounced back to the browse
/// screen with a clear error.
#[test]
fn host_cancel_bounces_joiner_back_to_browse() {
    let record_h = Rc::new(RefCell::new(Vec::new()));
    let record_j = Rc::new(RefCell::new(Vec::new()));
    let (mut host, mut join) = wired_pair(&record_h, &record_j);

    pump_until(&mut host, &mut join, "session discovered", |_, j| {
        !j.lan_sessions().is_empty()
    });
    click_first_session(&mut join);
    pump_until(&mut host, &mut join, "welcome received", |_, j| {
        j.join_lobby()
            .map(|l| l.welcome().is_some())
            .unwrap_or(false)
    });

    click_lobby_secondary(&mut host); // CANCEL
    assert_eq!(host.state(), AppState::MultiplayerMenu);
    pump_until(&mut host, &mut join, "joiner bounced", |_, j| {
        j.state() == AppState::LanJoinBrowse
    });
    assert!(
        join.lan_error().unwrap_or("").contains("cancelled"),
        "joiner must learn why: {:?}",
        join.lan_error()
    );
    assert_eq!(record_h.borrow().len(), 0, "no game may have been built");
    assert_eq!(record_j.borrow().len(), 0);
}

/// Joiner-leave: the host lobby reverts to waiting (seat open again) and
/// surfaces the departure.
#[test]
fn joiner_leave_reopens_the_host_seat() {
    let record_h = Rc::new(RefCell::new(Vec::new()));
    let record_j = Rc::new(RefCell::new(Vec::new()));
    let (mut host, mut join) = wired_pair(&record_h, &record_j);

    pump_until(&mut host, &mut join, "session discovered", |_, j| {
        !j.lan_sessions().is_empty()
    });
    click_first_session(&mut join);
    pump_until(&mut host, &mut join, "host sees joiner", |h, _| {
        h.host_lobby()
            .map(|l| l.joiner_name().is_some())
            .unwrap_or(false)
    });

    click_lobby_secondary(&mut join); // LEAVE
    assert_eq!(join.state(), AppState::LanJoinBrowse);
    pump_until(&mut host, &mut join, "host seat reopens", |h, _| {
        h.host_lobby()
            .map(|l| l.joiner_name().is_none())
            .unwrap_or(false)
    });
    assert_eq!(host.state(), AppState::LanHostLobby, "host keeps hosting");
    assert!(
        host.lan_error().unwrap_or("").contains("left"),
        "host must surface the departure: {:?}",
        host.lan_error()
    );
}
