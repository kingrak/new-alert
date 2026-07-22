//! M7.22 Fix 1 pins — player/house colour remaps assert on **rendered pixels**,
//! never on enum labels (the reported bug was "I selected blue, I got green":
//! the menu label was correct but the row index behind it was wrong).
//!
//! Reference: `Init_Color_Remaps` (`INIT.CPP:2639-2650`) builds `ColorRemaps[p]`
//! by iterating `PlayerColorType` in enum order and remapping `PALETTE.CPS` row 0
//! (the unity band) to row `p` — so the CPS row index IS the `PlayerColorType`.
//! House colours come from each `HouseTypeClass`'s `RemapColor` (`HDATA.CPP`),
//! which for Germany (grey=6) and France (blue=5) does **not** equal the house
//! index.
//!
//! Needs the real `redalert.mix` (for `palette.cps`); skips cleanly otherwise.

mod support;

use ra_client::compositor::{Palette, RgbaImage};
use ra_client::menu::COLORS;
use ra_client::unit_render::{draw_sprite_centered, SpriteFrame};
use ra_data::house::{build_color_remaps, build_house_remaps, HOUSE_PCOLOR};
use ra_formats::cps::Cps;
use ra_formats::mix::MixArchive;

/// Decode the real `palette.cps` out of `redalert.mix` -> `local.mix`.
fn load_palette_cps() -> Option<Vec<u8>> {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (see support::real_assets_available)");
        return None;
    }
    let bytes = std::fs::read(support::assets_dir().join("redalert.mix")).unwrap();
    let redalert = MixArchive::parse(&bytes).ok()?;
    let local = redalert.open_nested("local.mix").ok()?;
    local.get("palette.cps").map(|b| b.to_vec())
}

/// The 16 "unity" source indices (`PALETTE.CPS` row 0) a sprite is drawn in.
fn unity_band(cps: &Cps) -> [u8; 16] {
    core::array::from_fn(|i| cps.pixel(i, 0))
}

/// A grayscale palette where index `i` -> RGB `[i,i,i]`, so a remapped palette
/// index shows up directly as a distinct rendered byte.
fn ramp_palette() -> Palette {
    core::array::from_fn(|i| [i as u8, i as u8, i as u8])
}

/// A 4x4 sprite frame filled with unity-band index `src` (never 0, so it draws).
fn band_sprite(src: u8) -> SpriteFrame {
    SpriteFrame {
        width: 4,
        height: 4,
        pixels: vec![src; 16],
    }
}

/// Render `src` through `remap`+`palette` and return the centre pixel's R byte
/// (== the remapped palette index under [`ramp_palette`]).
fn rendered_index(src: u8, remap: &[u8; 256], palette: &Palette) -> u8 {
    let mut dst = RgbaImage {
        width: 4,
        height: 4,
        pixels: vec![0u8; 4 * 4 * 4],
    };
    draw_sprite_centered(&mut dst, 2, 2, &band_sprite(src), remap, palette);
    // Centre-ish pixel (2,2).
    dst.pixels[((2 * 4 + 2) * 4) as usize]
}

/// Every one of the eight selectable colours renders a distinguishable unit, and
/// the `COLORS` table's rows are the real `PlayerColorType` enum order.
#[test]
fn each_selectable_colour_renders_distinct_pixels() {
    let Some(cps_bytes) = load_palette_cps() else {
        return;
    };
    let cps = Cps::parse(&cps_bytes).expect("palette.cps parses");
    let color_remaps = build_color_remaps(&cps);
    let pal = ramp_palette();
    let band = unity_band(&cps);

    // The `COLORS` table must be exactly the eight PlayerColorTypes, in order.
    let expected_labels = [
        "GOLD", "LTBLUE", "RED", "GREEN", "ORANGE", "BLUE", "GREY", "BROWN",
    ];
    assert_eq!(COLORS.len(), 8);
    for (i, (label, row)) in COLORS.iter().enumerate() {
        assert_eq!(*label, expected_labels[i], "colour {i} label drifted");
        assert_eq!(
            *row as usize, i,
            "colour {label} must map to PlayerColorType/CPS row {i}"
        );
    }

    // Render band index 8 (a position where all eight rows differ — position 0
    // collides GREEN/ORANGE) for each colour; all eight rendered bytes distinct.
    let probe = band[8];
    let mut rendered: Vec<u8> = Vec::new();
    for (label, row) in COLORS.iter() {
        let px = rendered_index(probe, &color_remaps[*row as usize], &pal);
        assert!(
            !rendered.contains(&px),
            "colour {label} rendered pixel {px} collides with an earlier colour"
        );
        rendered.push(px);
    }

    // Pin the exact rendered bytes (revert-drill: any row re-shuffle moves these).
    // Derived from the shipped redalert.mix palette.cps, probe = unity index 88.
    assert_eq!(
        rendered,
        vec![88u8, 168, 236, 150, 216, 119, 187, 206],
        "per-colour rendered pixels changed — palette.cps or COLORS rows drifted"
    );
}

/// The reported bug, pinned directly: selecting BLUE must NOT render GREEN's
/// pixels. The old table had `("BLUE", 3)` — row 3 is GREEN — so BLUE and GREEN
/// rendered identically. Assert they now differ, and that BLUE == row 5.
#[test]
fn blue_is_not_green() {
    let Some(cps_bytes) = load_palette_cps() else {
        return;
    };
    let cps = Cps::parse(&cps_bytes).expect("palette.cps parses");
    let color_remaps = build_color_remaps(&cps);
    let pal = ramp_palette();
    let probe = unity_band(&cps)[8];

    let blue_row = COLORS.iter().find(|(l, _)| *l == "BLUE").unwrap().1;
    let green_row = COLORS.iter().find(|(l, _)| *l == "GREEN").unwrap().1;
    assert_eq!(blue_row, 5, "BLUE must be PlayerColorType 5");
    assert_eq!(green_row, 3, "GREEN must be PlayerColorType 3");

    let blue_px = rendered_index(probe, &color_remaps[blue_row as usize], &pal);
    let green_px = rendered_index(probe, &color_remaps[green_row as usize], &pal);
    assert_ne!(
        blue_px, green_px,
        "selecting BLUE rendered GREEN's pixels — the reported bug"
    );
}

/// Germany (house 5) wears grey (row 6) and France (house 6) wears blue (row 5),
/// NOT the blue/grey a naive `row == house index` mapping gives. Assert the
/// house-keyed remap for each equals the correct colour-keyed remap, and that
/// the two are not swapped.
#[test]
fn germany_grey_france_blue_not_swapped() {
    let Some(cps_bytes) = load_palette_cps() else {
        return;
    };
    let cps = Cps::parse(&cps_bytes).expect("palette.cps parses");
    let house_remaps = build_house_remaps(&cps);
    let color_remaps = build_color_remaps(&cps);
    let pal = ramp_palette();
    let probe = unity_band(&cps)[8];

    // HOUSE_PCOLOR pins the whole table; spot-check the two that don't coincide.
    assert_eq!(HOUSE_PCOLOR, [0, 1, 2, 3, 4, 6, 5, 7]);

    let germany = rendered_index(probe, &house_remaps[5], &pal); // house 5
    let france = rendered_index(probe, &house_remaps[6], &pal); // house 6
    let grey = rendered_index(probe, &color_remaps[6], &pal); // PCOLOR_GREY
    let blue = rendered_index(probe, &color_remaps[5], &pal); // PCOLOR_BLUE

    assert_eq!(
        germany, grey,
        "Germany must render grey (PlayerColorType 6)"
    );
    assert_eq!(france, blue, "France must render blue (PlayerColorType 5)");
    assert_ne!(
        germany, france,
        "Germany and France must not share a colour"
    );
    // Revert-drill: the pre-fix `row == house` mapping would have made Germany
    // render `color_remaps[5]` (blue) and France `color_remaps[6]` (grey).
    assert_ne!(germany, blue, "Germany must not render blue (the old bug)");
    assert_ne!(france, grey, "France must not render grey (the old bug)");
}
