//! `radump` — a small CLI for inspecting Red Alert data archives using
//! [`ra_formats`]. Subcommands:
//!
//! - `list   <file.mix> [--in NESTED]...`
//!   List a MIX's entries (id, offset, size, and name when known).
//! - `extract <file.mix> <entry> <out> [--in NESTED]...`
//!   Extract one entry (or nested-mix entry) to a file.
//! - `render  <file.mix> <shp> <frame> <out.ppm> --pal <entry> [--in NESTED]...
//!   [--pal-file PATH] [--pal-in NESTED]...`
//!   Decode an SHP frame with a palette and write a binary PPM (P6) image.
//!
//! `--in` (repeatable) descends into nested MIX archives before resolving the
//! target entry; `--pal-file` / `--pal-in` locate the palette in a separate
//! archive when it does not live alongside the shape.

use std::process::ExitCode;

use ra_formats::mix::MixArchive;
use ra_formats::names;
use ra_formats::pal::Palette;
use ra_formats::shp::Shp;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("radump: {e}");
            ExitCode::FAILURE
        }
    }
}

type BoxErr = Box<dyn std::error::Error>;

/// Collect repeated `--flag VALUE` occurrences, leaving positionals behind.
fn take_flag(args: &mut Vec<String>, flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if i + 1 < args.len() {
                values.push(args.remove(i + 1));
            }
            args.remove(i);
        } else {
            i += 1;
        }
    }
    values
}

fn run(args: &[String]) -> Result<(), BoxErr> {
    let mut args = args.to_vec();
    let cmd = if args.is_empty() {
        String::new()
    } else {
        args.remove(0)
    };

    match cmd.as_str() {
        "list" => cmd_list(args),
        "extract" => cmd_extract(args),
        "render" => cmd_render(args),
        _ => {
            eprintln!(
                "usage:\n  \
                 radump list <file.mix> [--in NESTED]...\n  \
                 radump extract <file.mix> <entry> <out> [--in NESTED]...\n  \
                 radump render <file.mix> <shp> <frame> <out.ppm> --pal <entry> \
                 [--in NESTED]... [--pal-file PATH] [--pal-in NESTED]..."
            );
            Err("unknown or missing subcommand".into())
        }
    }
}

/// Open a disk MIX file and descend through the given chain of nested archives.
/// The returned archive borrows `root`, so keep `root` alive.
fn descend<'a>(root: &'a [u8], nested: &[String]) -> Result<MixArchive<'a>, BoxErr> {
    let mut arch = MixArchive::parse(root)?;
    for name in nested {
        arch = arch.open_nested(name)?;
    }
    Ok(arch)
}

fn cmd_list(mut args: Vec<String>) -> Result<(), BoxErr> {
    let nested = take_flag(&mut args, "--in");
    let path = args.first().ok_or("list: missing <file.mix>")?;
    let root = std::fs::read(path)?;
    let arch = descend(&root, &nested)?;

    println!(
        "archive: {path}{}  (encrypted={}, digest={}, entries={}, data_start={})",
        nested
            .iter()
            .map(|n| format!(" -> {n}"))
            .collect::<String>(),
        arch.encrypted,
        arch.has_digest,
        arch.entries().len(),
        arch.data_start(),
    );
    println!("{:>10}  {:>12}  {:>12}  NAME", "ID", "OFFSET", "SIZE");
    for e in arch.entries() {
        let name = names::lookup(e.id).unwrap_or("");
        println!("0x{:08X}  {:>12}  {:>12}  {}", e.id, e.offset, e.size, name);
    }
    Ok(())
}

fn cmd_extract(mut args: Vec<String>) -> Result<(), BoxErr> {
    let nested = take_flag(&mut args, "--in");
    if args.len() < 3 {
        return Err("extract: expected <file.mix> <entry> <out>".into());
    }
    let path = &args[0];
    let entry = &args[1];
    let out = &args[2];

    let root = std::fs::read(path)?;
    let arch = descend(&root, &nested)?;
    let bytes = arch
        .get(entry)
        .ok_or_else(|| format!("entry '{entry}' not found"))?;
    std::fs::write(out, bytes)?;
    println!("extracted '{entry}' ({} bytes) -> {out}", bytes.len());
    Ok(())
}

fn cmd_render(mut args: Vec<String>) -> Result<(), BoxErr> {
    let nested = take_flag(&mut args, "--in");
    let pal_nested = take_flag(&mut args, "--pal-in");
    let pal_file = take_flag(&mut args, "--pal-file").pop();
    let pal_entry = take_flag(&mut args, "--pal")
        .pop()
        .ok_or("render: missing --pal <entry>")?;

    if args.len() < 4 {
        return Err("render: expected <file.mix> <shp> <frame> <out.ppm>".into());
    }
    let path = &args[0];
    let shp_name = &args[1];
    let frame: usize = args[2]
        .parse()
        .map_err(|_| "render: <frame> must be a number")?;
    let out = &args[3];

    let root = std::fs::read(path)?;
    let arch = descend(&root, &nested)?;
    let shp_bytes = arch
        .get(shp_name)
        .ok_or_else(|| format!("shape '{shp_name}' not found"))?;
    let shp = Shp::parse(shp_bytes)?;
    let hdr = shp.header();
    let f = shp.decode_frame(frame)?;

    // Locate the palette (possibly in a different file / nesting).
    let pal_root: Vec<u8>;
    let pal_arch = if let Some(pf) = &pal_file {
        pal_root = std::fs::read(pf)?;
        descend(&pal_root, &pal_nested)?
    } else {
        descend(&root, &pal_nested)?
    };
    let pal_bytes = pal_arch
        .get(&pal_entry)
        .ok_or_else(|| format!("palette '{pal_entry}' not found"))?;
    let pal = Palette::parse(pal_bytes)?;

    write_ppm(out, &f.pixels, f.width, f.height, &pal)?;

    // Report a quick plausibility summary.
    let non_zero = f.pixels.iter().filter(|&&p| p != 0).count();
    let max_index = f.pixels.iter().copied().max().unwrap_or(0);
    println!(
        "shape '{shp_name}': {} frames, header {}x{}, flags=0x{:04X}",
        shp.frame_count(),
        hdr.width,
        hdr.height,
        hdr.flags
    );
    println!(
        "frame {frame}: {}x{} ({} px), {} non-zero, max palette index {}",
        f.width,
        f.height,
        f.pixels.len(),
        non_zero,
        max_index
    );
    println!("wrote {out}");
    Ok(())
}

fn write_ppm(path: &str, pixels: &[u8], w: u16, h: u16, pal: &Palette) -> Result<(), BoxErr> {
    let mut buf = Vec::with_capacity(pixels.len() * 3 + 32);
    buf.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for &idx in pixels {
        buf.extend_from_slice(&pal.colors[idx as usize]);
    }
    std::fs::write(path, buf)?;
    Ok(())
}
