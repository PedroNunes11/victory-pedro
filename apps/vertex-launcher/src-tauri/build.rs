//! Copies the window's art, music and sounds into `../ui/assets`, makes the
//! window's icon (`icons/icon.ico`) from the logo, then runs Tauri's build
//! step, which embeds both in the program.
//!
//! The art is the owner's own (made for the launcher, no game material), kept
//! outside the repository because it holds text files only
//! (docs/asset-policy.md): in `VERTEX_LAUNCHER_ASSETS`, else in
//! `launcher-assets` next to the source folder. Without it the window still
//! works: a drawn background, no sound, and a drawn icon.

use std::path::{Path, PathBuf};

/// The files the window uses: where they are in the assets folder, and
/// their name in `ui/assets`.
const ASSETS: &[(&str, &str)] = &[
    ("art/bg-city-night.webp", "bg-city-night.webp"),
    ("art/bg-abstract.webp", "bg-abstract.webp"),
    ("art/logo-v-256.png", "logo-v-256.png"),
    ("music/lobby-theme.mp3", "lobby-theme.mp3"),
    ("sfx/hover.mp3", "hover.mp3"),
    ("sfx/click.mp3", "click.mp3"),
    ("sfx/toggle.mp3", "toggle.mp3"),
    ("sfx/page.mp3", "page.mp3"),
    ("sfx/start.mp3", "start.mp3"),
    ("sfx/success.mp3", "success.mp3"),
    ("sfx/error.mp3", "error.mp3"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VERTEX_LAUNCHER_ASSETS");
    engine_switches();
    let copied = copy_assets();
    let icon = Path::new("icons").join("icon.ico");
    let logo = copied.then(|| std::fs::read("../ui/assets/logo-v-256.png").ok());
    let bytes = match logo.flatten() {
        Some(png) => ico_png(&png),
        None => ico(&[16, 32, 48, 256]),
    };
    if std::fs::read(&icon).ok().as_deref() != Some(&bytes[..]) {
        std::fs::create_dir_all("icons").expect("making icons/");
        std::fs::write(&icon, &bytes).expect("writing icons/icon.ico");
    }
    tauri_build::build();
}

/// Helpers through which the engine reads its `VICTORY_*` switches:
/// `std::env::var("VICTORY_X")` and the small wrappers some files use.
const READERS: &[&str] = &[
    "var",
    "var_os",
    "env_path",
    "env",
    "mb",
    "ms",
    "number",
    "count",
    "seconds",
    "env_number",
    "is",
    "file",
];

/// Every switch name the engine reads, from its own source (`crates` and
/// `apps/victory`), into `$OUT_DIR/engine_switches.rs`. The launcher refuses
/// any other name on its command line and in best.json. `.env("VICTORY_X")`
/// (the engine setting a child process's environment) does not count.
fn engine_switches() {
    let root = Path::new("..").join("..").join("..");
    let mut names = std::collections::BTreeSet::new();
    for dir in [root.join("crates"), root.join("apps").join("victory")] {
        println!("cargo:rerun-if-changed={}", dir.display());
        scan(&dir, &mut names);
    }
    let mut out = String::from(
        "/// Every `VICTORY_*` switch the engine reads (made by build.rs from its source).\n\
         pub const ENGINE_SWITCHES: &[&str] = &[\n",
    );
    for n in &names {
        out.push_str(&format!("    \"{n}\",\n"));
    }
    out.push_str("];\n");
    let file =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("engine_switches.rs");
    if std::fs::read_to_string(&file).ok().as_deref() != Some(out.as_str()) {
        std::fs::write(&file, out).expect("writing engine_switches.rs");
    }
}

fn scan(dir: &Path, names: &mut std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                scan(&path, names);
            }
            continue;
        }
        if path.extension().is_none_or(|x| x != "rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut rest = text.as_str();
        while let Some(at) = rest.find("(\"VICTORY_") {
            let before = &rest[..at];
            let after = &rest[at + 2..];
            rest = &rest[at + 2..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if !after[name.len()..].starts_with('"') {
                continue;
            }
            let helper: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let dotted = before[..before.len() - helper.len()].ends_with('.');
            if READERS.contains(&helper.as_str()) && !(dotted && helper == "env") {
                names.insert(name);
            }
        }
    }
}

/// Copies [`ASSETS`] into `../ui/assets` (only what changed). False if the
/// assets folder is not there.
fn copy_assets() -> bool {
    let source = std::env::var_os("VERTEX_LAUNCHER_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new("..")
                .join("..")
                .join("..")
                .join("..")
                .join("launcher-assets")
        });
    if !source.join("README.md").is_file() {
        println!(
            "cargo:warning=no launcher assets at {} (set VERTEX_LAUNCHER_ASSETS): the window has no art or sound",
            source.display()
        );
        return false;
    }
    let dest = Path::new("..").join("ui").join("assets");
    std::fs::create_dir_all(&dest).expect("making ui/assets");
    for (from, to) in ASSETS {
        let from = source.join(from);
        println!("cargo:rerun-if-changed={}", from.display());
        let Ok(bytes) = std::fs::read(&from) else {
            println!("cargo:warning=missing launcher asset {}", from.display());
            continue;
        };
        let to = dest.join(to);
        if std::fs::read(&to).ok().as_deref() != Some(&bytes[..]) {
            std::fs::write(&to, &bytes).expect("copying a launcher asset");
        }
    }
    true
}

/// An .ico file holding one 256x256 PNG image (Windows Vista and later
/// read these, and scale them for the smaller sizes).
fn ico_png(png: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes());
    out.extend_from_slice(png);
    out
}

/// One pixel of the icon at (x, y) in 0..1, as straight RGBA in 0..1.
fn shade(x: f32, y: f32) -> [f32; 4] {
    // Rounded square.
    let r = 0.22;
    let (cx, cy) = (x.clamp(r, 1.0 - r), y.clamp(r, 1.0 - r));
    let outside = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() > r;
    if outside || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return [0.0; 4];
    }
    // Diagonal gradient #2a1352 -> #7c3aed.
    let t = ((x + y) / 2.0).clamp(0.0, 1.0);
    let from = [0x2a as f32, 0x13 as f32, 0x52 as f32];
    let to = [0x7c as f32, 0x3a as f32, 0xed as f32];
    let mut c = [0.0f32; 4];
    for i in 0..3 {
        c[i] = (from[i] + (to[i] - from[i]) * t) / 255.0;
    }
    c[3] = 1.0;
    // The V: two strokes meeting at the bottom.
    let stroke = |ax: f32, ay: f32, bx: f32, by: f32| {
        let (dx, dy) = (bx - ax, by - ay);
        let k = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - ax - k * dx).powi(2) + (y - ay - k * dy).powi(2)).sqrt()
    };
    let d = stroke(0.27, 0.28, 0.5, 0.74).min(stroke(0.73, 0.28, 0.5, 0.74));
    if d < 0.075 {
        c = [1.0, 1.0, 1.0, 1.0];
    }
    c
}

/// A 32-bit BGRA image of `size` pixels a side, bottom row first, 4x4
/// samples a pixel.
fn image(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for row in (0..size).rev() {
        for col in 0..size {
            let mut sum = [0.0f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (col as f32 + (sx as f32 + 0.5) / 4.0) / size as f32;
                    let y = (row as f32 + (sy as f32 + 0.5) / 4.0) / size as f32;
                    let p = shade(x, y);
                    for i in 0..3 {
                        sum[i] += p[i] * p[3];
                    }
                    sum[3] += p[3];
                }
            }
            let a = sum[3] / 16.0;
            let un = |v: f32| {
                if sum[3] > 0.0 {
                    (v / sum[3] * 255.0).round() as u8
                } else {
                    0
                }
            };
            out.extend_from_slice(&[
                un(sum[2]),
                un(sum[1]),
                un(sum[0]),
                (a * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// An .ico file with one uncompressed 32-bit image of each size.
fn ico(sizes: &[u32]) -> Vec<u8> {
    let entries: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| {
            let mut dib = Vec::new();
            // BITMAPINFOHEADER; the height counts the colour and mask halves.
            dib.extend_from_slice(&40u32.to_le_bytes());
            dib.extend_from_slice(&(s as i32).to_le_bytes());
            dib.extend_from_slice(&((s * 2) as i32).to_le_bytes());
            dib.extend_from_slice(&1u16.to_le_bytes());
            dib.extend_from_slice(&32u16.to_le_bytes());
            dib.extend_from_slice(&[0u8; 24]);
            dib.extend_from_slice(&image(s));
            // The AND mask: rows of whole 32-bit words, all clear.
            let row = s.div_ceil(32) * 4;
            dib.extend(std::iter::repeat_n(0u8, (row * s) as usize));
            dib
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, dib) in sizes.iter().zip(&entries) {
        let side = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += dib.len() as u32;
    }
    for dib in entries {
        out.extend_from_slice(&dib);
    }
    out
}
