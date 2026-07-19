# new-alert

A from-scratch, deterministic Rust reproduction of Command & Conquer: Red Alert
(1996). Architecture and rationale live in [`docs/DESIGN.md`](docs/DESIGN.md);
bug-for-bug behavioral notes in [`docs/QUIRKS.md`](docs/QUIRKS.md).

## Assets

The game loads the original **freeware** RA assets (`main.mix`, `redalert.mix`).
Point the client at them with `--assets DIR`, the `RA_ASSETS_DIR` env var, or a
local `./assets/` directory (searched in that order). Assets are never committed.

## Running

```sh
# Windowed: boots the main menu → skirmish setup → in-game (M7.8).
cargo run -p ra-client -- window --assets assets

# Headless verifications (PNG evidence + determinism checks), e.g.:
cargo run -p ra-client -- verify-m78 --assets assets --out-dir /tmp/out
```

From the main menu: **Skirmish** opens the setup screen (map list + minimap
preview, AI difficulty, player house/colour, starting credits, classic-radar
toggle); **Start** builds the game. In game, **Esc** opens the pause overlay
(Resume / Quit to Menu); on Victory/Defeat, **Continue** returns to the menu.

## User maps folder

Custom skirmish maps (`*.ini` / `*.mpr`) placed in the per-OS user maps folder
are listed on the setup screen alongside the built-in multiplayer maps. The folder
is created on first run:

- **Linux:** `$XDG_DATA_HOME/new-alert/maps` or `~/.local/share/new-alert/maps`
- **macOS:** `~/Library/Application Support/new-alert/maps`
- **Windows:** `%APPDATA%\new-alert\maps`

(Override the parent directory with `RA_DATA_DIR` for testing.)
