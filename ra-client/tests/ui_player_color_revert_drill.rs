//! M7.22 audit: the Germany/France `HOUSE_PCOLOR` revert drill, run for real.
//!
//! `ui_player_color_suite.rs::germany_grey_france_blue_not_swapped` asserts
//! the CURRENT (fixed) behaviour. This file proves the assertions are
//! actually sensitive to the regression they claim to guard against: it
//! rebuilds the house remap tables using the naive pre-fix `row == house
//! index` mapping (identity, via the same public `build_color_remap` the
//! real `build_house_remaps` calls, just with `h` instead of
//! `HOUSE_PCOLOR[h]`) and shows the fixed suite's own assertions FAIL against
//! it — Germany renders blue and France renders grey, the exact reported bug.
//! No production code is modified; this only proves the pin has teeth.

mod support;

use ra_client::compositor::{Palette, RgbaImage};
use ra_client::unit_render::{draw_sprite_centered, SpriteFrame};
use ra_data::house::{build_color_remap, build_color_remaps, build_house_remaps, HOUSE_PCOLOR};
use ra_formats::cps::Cps;
use ra_formats::mix::MixArchive;

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

fn unity_band(cps: &Cps) -> [u8; 16] {
    core::array::from_fn(|i| cps.pixel(i, 0))
}

fn ramp_palette() -> Palette {
    core::array::from_fn(|i| [i as u8, i as u8, i as u8])
}

fn band_sprite(src: u8) -> SpriteFrame {
    SpriteFrame {
        width: 4,
        height: 4,
        pixels: vec![src; 16],
    }
}

fn rendered_index(src: u8, remap: &[u8; 256], palette: &Palette) -> u8 {
    let mut dst = RgbaImage {
        width: 4,
        height: 4,
        pixels: vec![0u8; 4 * 4 * 4],
    };
    draw_sprite_centered(&mut dst, 2, 2, &band_sprite(src), remap, palette);
    dst.pixels[((2 * 4 + 2) * 4) as usize]
}

/// Sanity: `HOUSE_PCOLOR` is not the identity permutation at exactly the two
/// positions the bug report was about (else this whole drill is vacuous).
#[test]
fn house_pcolor_is_not_identity_at_germany_and_france() {
    assert_ne!(
        HOUSE_PCOLOR[5], 5,
        "Germany (house 5) must not map to row 5"
    );
    assert_ne!(HOUSE_PCOLOR[6], 6, "France (house 6) must not map to row 6");
}

/// The revert drill: swap `HOUSE_PCOLOR` for the naive identity mapping
/// (`row == house index`, the pre-fix behaviour) and show the fixed suite's
/// own "not swapped" assertion fails against it.
#[test]
fn identity_mapping_reproduces_the_reported_swap_bug() {
    let Some(cps_bytes) = load_palette_cps() else {
        return;
    };
    let cps = Cps::parse(&cps_bytes).expect("palette.cps parses");
    let color_remaps = build_color_remaps(&cps);
    let real_house_remaps = build_house_remaps(&cps); // the fix
    let pal = ramp_palette();
    let probe = unity_band(&cps)[8];

    // The pre-fix table: house h -> CPS row h (identity), built with the same
    // primitive `build_house_remaps` itself uses.
    let buggy_house_remaps: [[u8; 256]; 8] =
        core::array::from_fn(|h| build_color_remap(&cps, h as u8));

    let real_germany = rendered_index(probe, &real_house_remaps[5], &pal);
    let real_france = rendered_index(probe, &real_house_remaps[6], &pal);
    let buggy_germany = rendered_index(probe, &buggy_house_remaps[5], &pal);
    let buggy_france = rendered_index(probe, &buggy_house_remaps[6], &pal);
    let grey = rendered_index(probe, &color_remaps[6], &pal);
    let blue = rendered_index(probe, &color_remaps[5], &pal);

    // The fix: Germany=grey, France=blue (already covered by
    // `ui_player_color_suite.rs`; re-asserted here as the baseline this
    // drill diffs against).
    assert_eq!(real_germany, grey);
    assert_eq!(real_france, blue);

    // The bug, reproduced: under identity mapping Germany gets row 5 (blue)
    // and France gets row 6 (grey) — precisely swapped from the fix, and
    // precisely what `germany_grey_france_blue_not_swapped`'s
    // `assert_ne!(germany, blue)` / `assert_ne!(france, grey)` guard against.
    assert_eq!(
        buggy_germany, blue,
        "identity mapping must reproduce Germany-renders-blue"
    );
    assert_eq!(
        buggy_france, grey,
        "identity mapping must reproduce France-renders-grey"
    );
    assert_ne!(
        buggy_germany, real_germany,
        "the buggy and fixed Germany renders must differ — else this drill is vacuous"
    );
    assert_ne!(
        buggy_france, real_france,
        "the buggy and fixed France renders must differ — else this drill is vacuous"
    );
}
