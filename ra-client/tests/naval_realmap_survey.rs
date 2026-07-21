//! SURVEY (ignored): enumerate every shipped scenario INI and rank by water
//! coverage, to pick the best REAL coastal map for the naval end-to-end suite.
//! Run: `cargo test -p ra-client --test naval_realmap_survey -- --ignored --nocapture`

mod support;

use ra_client::assets;
use ra_data::landtype::LandType;
use ra_data::scenario::{MAP_CELL_H, MAP_CELL_W};

#[test]
#[ignore = "survey utility; needs real assets"]
fn survey_coastal_scenarios() {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() {
        eprintln!("SKIP: no assets");
        return;
    }
    let main_bytes = std::fs::read(dir.join("main.mix")).unwrap();
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).unwrap();
    let players = ['G', 'U', 'M', 'S', 'A'];
    let dirs = ['E', 'W'];
    let vars = ['A', 'B', 'C', 'D'];
    let mut rows: Vec<(usize, usize, String, String)> = Vec::new();
    for &p in &players {
        for sc in 1..=60u32 {
            for &d in &dirs {
                for &v in &vars {
                    let fname = format!("sc{}{:02}{}{}.ini", p.to_ascii_lowercase(), sc, d, v);
                    let terrain =
                        match assets::load_from_bytes(&main_bytes, &redalert_bytes, &fname) {
                            Ok(t) => t,
                            Err(_) => continue,
                        };
                    // count water within playable rect
                    let sx = terrain.scenario.map_x as u32;
                    let sy = terrain.scenario.map_y as u32;
                    let ex = (sx + terrain.scenario.map_width as u32).min(MAP_CELL_W);
                    let ey = (sy + terrain.scenario.map_height as u32).min(MAP_CELL_H);
                    let mut water = 0usize;
                    let mut total = 0usize;
                    for cy in sy..ey {
                        for cx in sx..ex {
                            let c = terrain.scenario.cell(cx, cy);
                            total += 1;
                            if terrain.tiles.land_type(c.template, c.icon) == LandType::Water {
                                water += 1;
                            }
                        }
                    }
                    let theater = format!("{:?}", terrain.scenario.theater);
                    rows.push((water, total, fname, theater));
                }
            }
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!("=== scenarios ranked by water cells (in playable rect) ===");
    for (water, total, name, theater) in rows.iter().take(40) {
        let pct = if *total > 0 { water * 100 / total } else { 0 };
        eprintln!("{name:14} {theater:10} water={water:5} / {total:5} ({pct:2}%)");
    }
    eprintln!("total scenarios found: {}", rows.len());
}
