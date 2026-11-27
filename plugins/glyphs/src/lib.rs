//! `victory_glyphs.dll`: keyboard key caps in place of the title's pad button
//! glyphs. The engine (`victory-gpu`, `VICTORY_GLYPHS_PLUGIN=<this dll>`)
//! hands over each small title texture when it is first uploaded, with a
//! hash of its texels; for the textures `glyphs.txt` names, this paints a
//! key cap over each glyph, in the texture's own format, showing the key
//! that stands for that pad button in `native-keys.txt`.
//!
//! Five C functions ([`victory_glyphs_version`] and those after it). The
//! files are read again when they change (looked at at most once a second);
//! the engine asks [`victory_glyphs_generation`] and forgets what it was told
//! when that changes.
//!
//! - `glyphs.txt`: `VICTORY_GLYPHS_FILE`, else `<data>\glyphs\glyphs.txt`
//!   (`<data>`: the engine's data folder, `VICTORY_DATA_DIR`).
//! - The keys: `VICTORY_NATIVE_KEYS`, else `<data>\native-keys.txt`.

mod actions;
pub mod cap;
pub mod config;
pub mod font;
pub mod keys;
pub mod paint;

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use victory_bc::Format;

/// The interface this plugin has.
pub const INTERFACE: u32 = 1;

/// [`victory_glyphs_init`]'s argument.
#[repr(C)]
pub struct Init {
    /// The engine's data folder, a NUL-terminated wide string (may be null).
    pub data: *const u16,
    /// Where to tell things (a NUL-terminated line); may be absent.
    pub log: Option<extern "C" fn(*const c_char)>,
}

/// A title texture as the engine hashed it: its level 0 (the largest level
/// the engine holds) is `width * height` in `format` (a `DXGI_FORMAT`), and
/// `levels` levels follow from there.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Texture {
    pub hash: u64,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub levels: u32,
}

type Log = extern "C" fn(*const c_char);

struct State {
    glyphs_file: PathBuf,
    keys_file: PathBuf,
    stamps: [Option<SystemTime>; 2],
    looked: Option<Instant>,
    generation: u32,
    config: config::Config,
    by_hash: HashMap<u64, Vec<usize>>,
    layout: keys::Layout,
    log: Option<Log>,
    /// Textures told about since the files were read.
    told: HashSet<u64>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn modified(file: &Path) -> Option<SystemTime> {
    std::fs::metadata(file).and_then(|m| m.modified()).ok()
}

impl State {
    fn new(glyphs_file: PathBuf, keys_file: PathBuf, log: Option<Log>) -> State {
        let mut s = State {
            glyphs_file,
            keys_file,
            stamps: [None, None],
            looked: None,
            generation: 0,
            config: config::Config::default(),
            by_hash: HashMap::new(),
            layout: keys::Layout::default(),
            log,
            told: HashSet::new(),
        };
        s.read();
        s
    }

    fn say(&self, line: &str) {
        if let Some(log) = self.log
            && let Ok(c) = CString::new(line.replace('\0', " "))
        {
            log(c.as_ptr());
        }
    }

    /// Reads both files again if either changed (looked at at most once a
    /// second).
    fn refresh(&mut self) {
        let now = Instant::now();
        if self.looked.is_some_and(|l| now.duration_since(l) < Duration::from_secs(1)) {
            return;
        }
        self.looked = Some(now);
        if [modified(&self.glyphs_file), modified(&self.keys_file)] != self.stamps {
            self.read();
        }
    }

    fn read(&mut self) {
        self.stamps = [modified(&self.glyphs_file), modified(&self.keys_file)];
        self.config = match std::fs::read_to_string(&self.glyphs_file) {
            Ok(text) => config::parse(&text),
            Err(e) => {
                self.say(&format!("glyphs: {} not read ({e}): no texture is changed", self.glyphs_file.display()));
                config::Config::default()
            }
        };
        self.layout = match std::fs::read_to_string(&self.keys_file) {
            Ok(text) => keys::parse_layout(&text),
            Err(e) => {
                self.say(&format!("glyphs: {} not read ({e}): only key= and label= lines draw", self.keys_file.display()));
                keys::Layout::default()
            }
        };
        self.by_hash.clear();
        for (i, e) in self.config.entries.iter().enumerate() {
            self.by_hash.entry(e.hash).or_default().push(i);
        }
        self.told.clear();
        self.generation = self.generation.wrapping_add(1);
        for (line, why) in &self.config.errors {
            self.say(&format!("glyphs: {} line {line}: {why}", self.glyphs_file.display()));
        }
        let summary = |menu: bool| -> String {
            keys::GLYPHS
                .iter()
                .map(|g| format!("{g}={}", self.labels(&config::What::Glyph(g.to_string()), menu).map_or("-".into(), |l| l[0].clone())))
                .collect::<Vec<_>>()
                .join(" ")
        };
        self.say(&format!(
            "glyphs: {} lines for {} textures from {}; keys from {} ({} keys)",
            self.config.entries.len(),
            self.by_hash.len(),
            self.glyphs_file.display(),
            self.keys_file.display(),
            self.layout.lines.len()
        ));
        self.say(&format!("glyphs: in play {}", summary(false)));
        self.say(&format!("glyphs: in menus {}", summary(true)));
    }

    /// The labels of a cap for `what`.
    fn labels(&self, what: &config::What, menu: bool) -> Option<Vec<String>> {
        match what {
            config::What::Label(s) => Some(vec![s.clone()]),
            config::What::Key(k) => Some(keys::labels(k)),
            config::What::Glyph(g) => {
                let g = keys::canonical_glyph(g)?;
                let mapped = self
                    .config
                    .maps
                    .iter()
                    .find(|m| m.glyph == g && m.menu == menu)
                    .or_else(|| self.config.maps.iter().find(|m| m.glyph == g && !m.menu));
                match mapped {
                    Some(m) => Some(keys::labels(&m.key)),
                    None => self.layout.glyph_labels(g, menu),
                }
            }
        }
    }

    fn jobs(&self, hash: u64) -> Vec<paint::Job> {
        self.by_hash
            .get(&hash)
            .into_iter()
            .flatten()
            .filter_map(|&i| {
                let e = &self.config.entries[i];
                Some(paint::Job { rect: e.rect, labels: self.labels(&e.what, e.menu)?, premultiplied: e.premultiplied })
            })
            .collect()
    }

    fn wants(&mut self, t: &Texture) -> bool {
        self.refresh();
        if !self.by_hash.contains_key(&t.hash) {
            return false;
        }
        let format = Format::from_dxgi(t.format);
        if self.told.insert(t.hash) {
            let jobs = self.jobs(t.hash);
            let caps = jobs.iter().map(|j| j.labels[0].as_str()).collect::<Vec<_>>().join(", ");
            match format {
                Some(f) if f.encodable() => self.say(&format!(
                    "glyphs: {:016x} ({}x{} {}, {} levels): caps {caps}",
                    t.hash,
                    t.width,
                    t.height,
                    f.name(),
                    t.levels
                )),
                _ => self.say(&format!("glyphs: {:016x}: format {} cannot be written, left as it is", t.hash, t.format)),
            }
        }
        format.is_some_and(Format::encodable) && !self.jobs(t.hash).is_empty()
    }
}

fn locked<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_mut().map(f)
}

/// Where the files are: `VICTORY_GLYPHS_FILE` / `VICTORY_NATIVE_KEYS` if
/// set (`env`), else in `data`.
fn files(data: &Path, env: impl Fn(&str) -> Option<OsString>) -> (PathBuf, PathBuf) {
    let path = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    (
        path("VICTORY_GLYPHS_FILE").unwrap_or_else(|| data.join("glyphs").join("glyphs.txt")),
        path("VICTORY_NATIVE_KEYS").unwrap_or_else(|| data.join("native-keys.txt")),
    )
}

/// A NUL-terminated wide string.
///
/// # Safety
/// `p` is null or points at one.
unsafe fn wide(p: *const u16) -> Option<OsString> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: up to the terminating NUL, as the caller promises.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: the `n` units just read.
    let units = unsafe { std::slice::from_raw_parts(p, n) };
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Some(OsString::from_wide(units))
    }
    #[cfg(not(windows))]
    {
        Some(OsString::from(String::from_utf16_lossy(units)))
    }
}

/// The interface version: [`INTERFACE`].
#[unsafe(no_mangle)]
pub extern "C" fn victory_glyphs_version() -> u32 {
    INTERFACE
}

/// Starts the plugin: finds and reads its files. 0 when started.
///
/// # Safety
/// `init` points at an [`Init`] whose strings live through the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn victory_glyphs_init(init: *const Init) -> u32 {
    if init.is_null() {
        return 1;
    }
    // SAFETY: as the caller promises.
    let init = unsafe { &*init };
    // SAFETY: as the caller promises.
    let data = unsafe { wide(init.data) }.map(PathBuf::from).unwrap_or_default();
    let log = init.log;
    catch_unwind(AssertUnwindSafe(|| {
        let (glyphs, keys) = files(&data, |k| std::env::var_os(k));
        let state = State::new(glyphs, keys, log);
        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
        0
    }))
    .unwrap_or(2)
}

/// A number that changes each time the files were read again; what the
/// engine was told before belongs to an older one.
#[unsafe(no_mangle)]
pub extern "C" fn victory_glyphs_generation() -> u32 {
    catch_unwind(|| {
        locked(|s| {
            s.refresh();
            s.generation
        })
        .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// 1 if caps are to be painted over `texture` (then the engine calls
/// [`victory_glyphs_replace`] for its levels), else 0.
///
/// # Safety
/// `texture` points at a [`Texture`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn victory_glyphs_wants(texture: *const Texture) -> u32 {
    if texture.is_null() {
        return 0;
    }
    // SAFETY: as the caller promises.
    let t = unsafe { *texture };
    catch_unwind(AssertUnwindSafe(|| u32::from(locked(|s| s.wants(&t)).unwrap_or(false)))).unwrap_or(0)
}

/// Paints the caps into level `level` of `texture`: `bytes` holds that
/// level's elements as the title wrote them, rows `pitch` bytes apart, and
/// is changed in place. 1 if it was changed.
///
/// # Safety
/// `texture` points at a [`Texture`], `bytes` at `len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn victory_glyphs_replace(texture: *const Texture, level: u32, bytes: *mut u8, len: usize, pitch: u32) -> u32 {
    if texture.is_null() || bytes.is_null() {
        return 0;
    }
    // SAFETY: as the caller promises.
    let t = unsafe { *texture };
    // SAFETY: as the caller promises.
    let bytes = unsafe { std::slice::from_raw_parts_mut(bytes, len) };
    catch_unwind(AssertUnwindSafe(|| {
        let Some(format) = Format::from_dxgi(t.format) else { return 0 };
        if level >= t.levels.max(1) || level >= 32 {
            return 0;
        }
        let jobs = locked(|s| s.jobs(t.hash)).unwrap_or_default();
        u32::from(paint::paint(format, [t.width, t.height], level, bytes, pitch as usize, &jobs))
    }))
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_default_to_the_data_folder() {
        let (g, k) = files(Path::new(r"D:\data"), |_| None);
        assert_eq!(g, Path::new(r"D:\data\glyphs\glyphs.txt"));
        assert_eq!(k, Path::new(r"D:\data\native-keys.txt"));
        let (g, k) = files(Path::new(r"D:\data"), |key| match key {
            "VICTORY_GLYPHS_FILE" => Some(r"E:\g.txt".into()),
            "VICTORY_NATIVE_KEYS" => Some("".into()),
            _ => None,
        });
        assert_eq!(g, Path::new(r"E:\g.txt"));
        assert_eq!(k, Path::new(r"D:\data\native-keys.txt"), "empty is unset");
    }

    #[test]
    fn the_c_interface_end_to_end() {
        let dir = std::env::temp_dir().join(format!("victory-glyphs-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("glyphs")).unwrap();
        std::fs::write(dir.join("native-keys.txt"), "Space = INPUT_JUMP\nE = INPUT_CONTEXT\n").unwrap();
        std::fs::write(
            dir.join("glyphs").join("glyphs.txt"),
            "00000000000000aa A\n00000000000000bb DRight 0 0 16 16\n00000000000000bb key=Tab 16 0 16 16\n",
        )
        .unwrap();
        let wide: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
        extern "C" fn quiet(_: *const c_char) {}
        let init = Init { data: wide.as_ptr(), log: Some(quiet) };
        assert_eq!(victory_glyphs_version(), 1);
        // SAFETY: a live Init.
        assert_eq!(unsafe { victory_glyphs_init(&init) }, 0);
        let generation = victory_glyphs_generation();
        assert_ne!(generation, 0);

        let atlas = Texture { hash: 0xbb, format: 28, width: 32, height: 16, levels: 2 };
        let other = Texture { hash: 0xcc, ..atlas };
        let bc5 = Texture { hash: 0xaa, format: 83, ..atlas };
        // SAFETY (all below): live arguments of the sizes given.
        unsafe {
            assert_eq!(victory_glyphs_wants(&atlas), 1);
            assert_eq!(victory_glyphs_wants(&other), 0);
            assert_eq!(victory_glyphs_wants(&bc5), 0, "BC5 cannot be written");
            let mut level0 = vec![0u8; 32 * 16 * 4];
            assert_eq!(victory_glyphs_replace(&atlas, 0, level0.as_mut_ptr(), level0.len(), 32 * 4), 1);
            assert!(level0.iter().any(|&b| b != 0));
            let mut level1 = vec![0u8; 16 * 8 * 4];
            assert_eq!(victory_glyphs_replace(&atlas, 1, level1.as_mut_ptr(), level1.len(), 16 * 4), 1);
            let mut level2 = vec![0u8; 8 * 4 * 4];
            assert_eq!(victory_glyphs_replace(&atlas, 2, level2.as_mut_ptr(), level2.len(), 8 * 4), 0, "past its levels");
            assert_eq!(victory_glyphs_replace(&atlas, 0, level0.as_mut_ptr(), 10, 32 * 4), 0, "too short");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maps_override_the_layout() {
        let mut s = State::new(PathBuf::from("nowhere/glyphs.txt"), PathBuf::from("nowhere/keys.txt"), None);
        s.layout = keys::parse_layout("Space = INPUT_JUMP | INPUT_FRONTEND_ACCEPT\n");
        s.config = config::parse("map A Enter menu\n");
        let a = config::What::Glyph("A".into());
        assert_eq!(s.labels(&a, false).unwrap()[0], "SPACE");
        assert_eq!(s.labels(&a, true).unwrap()[0], "ENTER");
        s.config = config::parse("map A G\n");
        assert_eq!(s.labels(&a, true).unwrap()[0], "G", "a play map serves menus too");
        assert_eq!(s.labels(&config::What::Label("HOLD".into()), false).unwrap(), ["HOLD"]);
    }
}
