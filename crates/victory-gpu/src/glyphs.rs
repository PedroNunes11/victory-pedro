//! Keyboard key caps in place of the title's pad button glyphs, by a plugin
//! (`plugins/glyphs`), and a dump of the textures they could be in.
//! `docs/GLYPHS.md` has the files and the steps.
//!
//! Off unless one of these is set; then each small title texture, when its
//! memory is detiled for the card ([`crate::texture`]), is hashed once per
//! content (the texels of the largest level held, with its format family and
//! size), and:
//!
//! - `VICTORY_GLYPHS_PLUGIN=<full path of victory_glyphs.dll>`: the plugin
//!   is asked whether it repaints that content; if so it is handed each
//!   level and may change it in place, and what it hands back is what the
//!   card gets. Its answers are kept per content (and per count of levels)
//!   until its files change ([`Api::generation`]), so a texture the title
//!   writes again costs a lookup.
//! - `VICTORY_GLYPHS_DUMP=<folder>`: each distinct content is written once,
//!   as `<hash>_<w>x<h>_<format>.png` (block formats decoded), on a thread
//!   of its own, and a line for it is added to `index.txt` with the seconds
//!   since the first one, so that the textures on screen at a moment can be
//!   found.
//! - `VICTORY_GLYPHS_MAX_SIDE=<texels>`: the largest width or height looked
//!   at (1024; at least 4). Smaller sides cost less and dump less.
//!
//! The textures looked at: 2D, one slice, one sample, formats the plugin
//! and the dump know (RGBA8, BGRA8, BGRX8, R8, A8, BC1-BC5, BC7), not
//! levels-on-demand ones (`crate::tiles`). Anything the plugin cannot do is
//! told once in the log and the texture goes up as the title wrote it.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use victory_bc::Format;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
};
use windows::core::{HSTRING, PCSTR};

/// The plugin's interface this was written against.
const INTERFACE: u32 = 1;
/// Bytes of plugin answers kept at most (all are forgotten past it).
const KEPT: usize = 64 << 20;
/// Bytes of dumps waiting to be written at most (past it, later contents
/// wait for their next upload).
const QUEUED: u64 = 256 << 20;

/// The plugin's `Init`.
#[repr(C)]
struct Init {
    data: *const u16,
    log: Option<extern "C" fn(*const c_char)>,
}

/// The plugin's `Texture`: the content as it was hashed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Texture {
    hash: u64,
    format: u32,
    width: u32,
    height: u32,
    levels: u32,
}

#[derive(Clone, Copy)]
struct Api {
    /// Changes when its files were read again.
    generation: unsafe extern "C" fn() -> u32,
    wants: unsafe extern "C" fn(*const Texture) -> u32,
    replace: unsafe extern "C" fn(*const Texture, u32, *mut u8, usize, u32) -> u32,
}

/// Levels as the plugin left them (`None`: unchanged), by content.
type Answer = Option<Arc<Vec<Option<Vec<u8>>>>>;

struct Plugin {
    api: Api,
    answers: Mutex<Answers>,
}

#[derive(Default)]
struct Answers {
    generation: u32,
    by: HashMap<(u64, u32), Answer>,
    bytes: usize,
}

struct Dump {
    seen: Mutex<HashSet<u64>>,
    jobs: Mutex<mpsc::Sender<Job>>,
    queued: AtomicU64,
    since: std::time::Instant,
}

struct Job {
    hash: u64,
    format: Format,
    size: [u32; 2],
    levels: u32,
    bytes: Vec<u8>,
    seconds: f64,
}

struct Settings {
    plugin: Option<Plugin>,
    dump: Option<Dump>,
    max_side: u32,
}

fn settings() -> &'static Settings {
    static SETTINGS: OnceLock<Settings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty() && v != "0");
        let plugin = var("VICTORY_GLYPHS_PLUGIN").and_then(|file| start(Path::new(&file)));
        let dump = var("VICTORY_GLYPHS_DUMP").and_then(|folder| dump(PathBuf::from(folder)));
        let max_side = std::env::var("VICTORY_GLYPHS_MAX_SIDE")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(1024)
            .max(4);
        if plugin.is_some() || dump.is_some() {
            tracing::info!(max_side, "glyphs: title textures up to {max_side} texels a side are looked at");
        }
        Settings { plugin, dump, max_side }
    })
}

/// Whether the plugin or the dump is on.
pub(crate) fn on() -> bool {
    let s = settings();
    s.plugin.is_some() || s.dump.is_some()
}

/// The format of a texture to look at: a `DXGI_FORMAT` known here whose
/// elements are `element_bytes` (as the title's layout has them), and a
/// size within bounds.
pub(crate) fn candidate(dxgi: u32, width: u32, height: u32, element_bytes: u32) -> Option<Format> {
    let f = Format::from_dxgi(dxgi)?;
    let max = settings().max_side;
    (f.element_bytes() == element_bytes as usize && (4..=max).contains(&width) && (4..=max).contains(&height))
        .then_some(f)
}

/// The content hash: format family, size and texels of the level.
pub(crate) fn hash(format: Format, size: [u32; 2], texels: &[u8]) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(format.name().as_bytes());
    h.update(&size[0].to_le_bytes());
    h.update(&size[1].to_le_bytes());
    h.update(texels);
    let d = h.finalize();
    u64::from_le_bytes(d.as_bytes()[..8].try_into().expect("8 bytes"))
}

/// A texture was detiled for the card. Its levels `0..levels` are those the
/// staging buffer holds (level 0 the largest, `size` texels, `dxgi` its
/// format). `read(i, out, pitch)` detiles level `i` into `out`, rows of
/// elements `pitch` bytes apart, tightly packed; `write(i, bytes, pitch)`
/// puts level `i` into the staging buffer.
pub(crate) fn detiled(
    format: Format,
    dxgi: u32,
    size: [u32; 2],
    levels: u32,
    read: &mut dyn FnMut(u32, &mut [u8], usize) -> bool,
    write: &mut dyn FnMut(u32, &[u8], usize),
) {
    let s = settings();
    let (row, rows) = victory_bc::level_shape(format, size[0], size[1]);
    let mut base = vec![0u8; row * rows];
    if !read(0, &mut base, row) {
        return;
    }
    let hash = hash(format, size, &base);
    if let Some(d) = &s.dump {
        d.offer(hash, format, size, levels, &base);
    }
    if let Some(p) = &s.plugin {
        p.apply(Texture { hash, format: dxgi, width: size[0], height: size[1], levels }, format, base, read, write);
    }
}

impl Plugin {
    fn apply(
        &self,
        t: Texture,
        format: Format,
        base: Vec<u8>,
        read: &mut dyn FnMut(u32, &mut [u8], usize) -> bool,
        write: &mut dyn FnMut(u32, &[u8], usize),
    ) {
        // SAFETY: a function of the plugin's interface.
        let generation = unsafe { (self.api.generation)() };
        let key = (t.hash, t.levels);
        {
            let mut a = self.answers.lock().unwrap_or_else(|e| e.into_inner());
            if a.generation != generation {
                *a = Answers { generation, ..Answers::default() };
            }
            if let Some(answer) = a.by.get(&key) {
                let answer = answer.clone();
                drop(a);
                put(&answer, format, [t.width, t.height], write);
                return;
            }
        }
        // SAFETY: a live structure of the interface's layout.
        let answer: Answer = if unsafe { (self.api.wants)(&t) } == 0 {
            None
        } else {
            let mut levels = Vec::with_capacity(t.levels as usize);
            let mut base = Some(base);
            for i in 0..t.levels {
                let (lw, lh) = ((t.width >> i).max(1), (t.height >> i).max(1));
                let (row, rows) = victory_bc::level_shape(format, lw, lh);
                let mut bytes = match base.take() {
                    Some(b) => b,
                    None => vec![0u8; row * rows],
                };
                if i > 0 && !read(i, &mut bytes, row) {
                    levels.push(None);
                    continue;
                }
                // SAFETY: `bytes` is writable for its length; the plugin
                // keeps nothing of it past the call.
                let changed = unsafe { (self.api.replace)(&t, i, bytes.as_mut_ptr(), bytes.len(), row as u32) } != 0;
                levels.push(changed.then_some(bytes));
            }
            Some(Arc::new(levels))
        };
        put(&answer, format, [t.width, t.height], write);
        let mut a = self.answers.lock().unwrap_or_else(|e| e.into_inner());
        if a.generation == generation {
            let bytes = answer.as_ref().map_or(0, |l| l.iter().flatten().map(Vec::len).sum());
            if a.bytes + bytes > KEPT {
                a.by.clear();
                a.bytes = 0;
            }
            a.bytes += bytes;
            a.by.insert(key, answer);
        }
    }
}

/// Puts the levels the plugin changed into the staging buffer.
fn put(answer: &Answer, format: Format, size: [u32; 2], write: &mut dyn FnMut(u32, &[u8], usize)) {
    for (i, level) in answer.iter().flat_map(|l| l.iter()).enumerate() {
        if let Some(bytes) = level {
            let (row, _) = victory_bc::level_shape(format, (size[0] >> i).max(1), (size[1] >> i).max(1));
            write(i as u32, bytes, row);
        }
    }
}

impl Dump {
    fn offer(&self, hash: u64, format: Format, size: [u32; 2], levels: u32, texels: &[u8]) {
        if !self.seen.lock().unwrap_or_else(|e| e.into_inner()).insert(hash) {
            return;
        }
        if self.queued.load(Relaxed) > QUEUED {
            // Not now: the next upload of this content offers it again.
            self.seen.lock().unwrap_or_else(|e| e.into_inner()).remove(&hash);
            static TOLD: std::sync::Once = std::sync::Once::new();
            TOLD.call_once(|| tracing::warn!("glyphs: the dump is behind; some textures are written later"));
            return;
        }
        self.queued.fetch_add(texels.len() as u64, Relaxed);
        let job = Job {
            hash,
            format,
            size,
            levels,
            bytes: texels.to_vec(),
            seconds: self.since.elapsed().as_secs_f64(),
        };
        let _ = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).send(job);
    }
}

/// The file name of a dumped content.
pub(crate) fn file_name(hash: u64, format: Format, size: [u32; 2]) -> String {
    format!("{hash:016x}_{}x{}_{}.png", size[0], size[1], format.name())
}

fn dump(folder: PathBuf) -> Option<Dump> {
    if let Err(e) = std::fs::create_dir_all(&folder) {
        tracing::warn!(folder = %folder.display(), "glyphs: no dump folder: {e}");
        return None;
    }
    let (send, receive) = mpsc::channel::<Job>();
    let to = folder.clone();
    std::thread::Builder::new()
        .name("glyph dump".into())
        .spawn(move || {
            let Ok(mut index) = std::fs::OpenOptions::new().create(true).append(true).open(to.join("index.txt")) else {
                return;
            };
            let _ = writeln!(index, "# run of {:?}: hash size format levels seconds file", std::time::SystemTime::now());
            for job in receive {
                let name = file_name(job.hash, job.format, job.size);
                let path = to.join(&name);
                let (row, _) = victory_bc::level_shape(job.format, job.size[0], job.size[1]);
                if !path.exists()
                    && let Some(rgba) = victory_bc::decode(job.format, job.size[0], job.size[1], &job.bytes, row)
                    && let Err(e) = std::fs::write(&path, png(job.size[0], job.size[1], &rgba))
                {
                    tracing::warn!(path = %path.display(), "glyphs: not dumped: {e}");
                }
                let _ = writeln!(
                    index,
                    "{:016x} {}x{} {} {} {:.1} {name}",
                    job.hash,
                    job.size[0],
                    job.size[1],
                    job.format.name(),
                    job.levels,
                    job.seconds
                );
                if let Some(d) = &settings().dump {
                    d.queued.fetch_sub(job.bytes.len() as u64, Relaxed);
                }
            }
        })
        .ok()?;
    tracing::info!(folder = %folder.display(), "glyphs: every distinct small title texture is dumped there (VICTORY_GLYPHS_DUMP)");
    Some(Dump {
        seen: Mutex::new(HashSet::new()),
        jobs: Mutex::new(send),
        queued: AtomicU64::new(0),
        since: std::time::Instant::now(),
    })
}

/// A PNG of `width * height` RGBA8 texels.
pub(crate) fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = width as usize * 4;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgba.chunks_exact(row.max(1)).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    header.extend([8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &header);
    let mut compressed = vec![0u8; zlib_rs::compress_bound(raw.len())];
    let (idat, _) = zlib_rs::compress_slice(&mut compressed, &raw, zlib_rs::DeflateConfig::new(3));
    chunk(&mut out, b"IDAT", idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend((data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend(kind);
    png.extend(data);
    let mut crc = !0u32;
    for &b in &png[start..] {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    png.extend((!crc).to_be_bytes());
}

extern "C" fn log(line: *const c_char) {
    if line.is_null() {
        return;
    }
    // SAFETY: the plugin passes a string that lives for the call.
    let line = unsafe { CStr::from_ptr(line) }.to_string_lossy();
    tracing::info!("{}", line.trim_end());
}

/// The engine's data folder, as `victory-core` finds it without a
/// victory.toml: `VICTORY_DATA_DIR`, else `%LOCALAPPDATA%\victory\data`.
fn data_folder() -> PathBuf {
    std::env::var_os("VICTORY_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("victory").join("data")))
        .unwrap_or_default()
}

/// Loads and starts the plugin; `None` (told once) if it does not.
fn start(file: &Path) -> Option<Plugin> {
    let api = match load(file) {
        Ok(api) => api,
        Err(why) => {
            tracing::warn!(file = %file.display(), "glyphs: no plugin: {why}");
            return None;
        }
    };
    let data = HSTRING::from(data_folder().as_os_str());
    let init = Init { data: data.as_ptr(), log: Some(log) };
    // SAFETY: a structure of the plugin's layout whose string lives through
    // the call.
    let code = unsafe { (api.init)(&init) };
    if code != 0 {
        tracing::warn!(file = %file.display(), code, "glyphs: the plugin did not start");
        return None;
    }
    tracing::info!(file = %file.display(), "glyphs: the plugin is loaded (VICTORY_GLYPHS_PLUGIN)");
    Some(Plugin {
        api: Api { generation: api.generation, wants: api.wants, replace: api.replace },
        answers: Mutex::new(Answers::default()),
    })
}

struct Loaded {
    init: unsafe extern "C" fn(*const Init) -> u32,
    generation: unsafe extern "C" fn() -> u32,
    wants: unsafe extern "C" fn(*const Texture) -> u32,
    replace: unsafe extern "C" fn(*const Texture, u32, *mut u8, usize, u32) -> u32,
}

fn load(file: &Path) -> Result<Loaded, String> {
    if !file.is_absolute() {
        return Err("VICTORY_GLYPHS_PLUGIN is not a full path".into());
    }
    // SAFETY: loads a named library; its own folder and the system's are
    // searched for what it needs, never the current folder.
    let module = unsafe {
        LoadLibraryExW(
            &HSTRING::from(file.as_os_str()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        )
    }
    .map_err(|e| e.to_string())?;
    let get = |name: &str| {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: a name with its end, and a module that stays loaded.
        unsafe { GetProcAddress(module, PCSTR(c.as_ptr().cast())) }.ok_or_else(|| format!("it lacks {name}"))
    };
    type Any = unsafe extern "system" fn() -> isize;
    // SAFETY: the functions have the signatures of the interface whose
    // version is asked first.
    unsafe {
        let version = std::mem::transmute::<Any, unsafe extern "C" fn() -> u32>(get("victory_glyphs_version")?)();
        if version != INTERFACE {
            return Err(format!("its interface is version {version}, not {INTERFACE}"));
        }
        Ok(Loaded {
            init: std::mem::transmute::<Any, unsafe extern "C" fn(*const Init) -> u32>(get("victory_glyphs_init")?),
            generation: std::mem::transmute::<Any, unsafe extern "C" fn() -> u32>(get("victory_glyphs_generation")?),
            wants: std::mem::transmute::<Any, unsafe extern "C" fn(*const Texture) -> u32>(get("victory_glyphs_wants")?),
            replace: std::mem::transmute::<Any, unsafe extern "C" fn(*const Texture, u32, *mut u8, usize, u32) -> u32>(
                get("victory_glyphs_replace")?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_tell_contents_apart_and_ignore_the_format_variant() {
        let a = hash(Format::Bc3, [64, 64], &[1, 2, 3]);
        assert_eq!(a, hash(Format::Bc3, [64, 64], &[1, 2, 3]), "stable");
        assert_ne!(a, hash(Format::Bc3, [64, 64], &[1, 2, 4]));
        assert_ne!(a, hash(Format::Bc7, [64, 64], &[1, 2, 3]));
        assert_ne!(a, hash(Format::Bc3, [32, 128], &[1, 2, 3]));
        // UNORM and sRGB of one family hash alike (the family is hashed).
        assert_eq!(Format::from_dxgi(77), Format::from_dxgi(78));
    }

    #[test]
    fn dump_names_carry_hash_size_and_format() {
        assert_eq!(file_name(0x3fa2c9d1e0b4a785, Format::Bc7, [128, 64]), "3fa2c9d1e0b4a785_128x64_BC7.png");
        assert_eq!(file_name(0xab, Format::Rgba8, [4, 4]), "00000000000000ab_4x4_RGBA8.png");
    }

    #[test]
    fn png_is_well_formed() {
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| i as u8).collect();
        let p = png(4, 3, &rgba);
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(p[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(p[20..24].try_into().unwrap()), 3);
        assert_eq!(p[25], 6, "RGBA");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        // The IEND chunk's CRC is the well-known constant.
        assert_eq!(&p[p.len() - 4..], &[0xae, 0x42, 0x60, 0x82]);
    }

    #[test]
    fn put_writes_only_changed_levels_at_their_own_pitch() {
        let answer: Answer = Some(Arc::new(vec![None, Some(vec![7u8; 4 * 4 * 16]), None]));
        let mut written = Vec::new();
        put(&answer, Format::Bc7, [32, 32], &mut |i, bytes, pitch| written.push((i, bytes.len(), pitch)));
        // Level 1 of 32x32 BC7 is 16x16: 4 blocks a row of 16 bytes.
        assert_eq!(written, [(1, 256, 64)]);
        put(&None, Format::Bc7, [32, 32], &mut |_, _, _| panic!("nothing to write"));
    }

    /// The built plugin through this file's loader and interface:
    /// `VICTORY_GLYPHS_TEST_PLUGIN=<full path of victory_glyphs.dll>`.
    #[test]
    #[ignore = "needs VICTORY_GLYPHS_TEST_PLUGIN (a built victory_glyphs.dll)"]
    fn the_built_plugin_paints_a_glyph() {
        let file = PathBuf::from(std::env::var_os("VICTORY_GLYPHS_TEST_PLUGIN").expect("VICTORY_GLYPHS_TEST_PLUGIN"));
        let api = load(&file).unwrap();
        // A 64x64 BC7 texture of one flat colour, as a title glyph might be.
        let (w, h) = (64u32, 64u32);
        let flat = vec![[200u8, 30, 30, 255]; (w * h) as usize].concat();
        let texels = victory_bc::encode(Format::Bc7, w, h, &flat).unwrap();
        let hash = hash(Format::Bc7, [w, h], &texels);
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("glyphs")).unwrap();
        std::fs::write(
            dir.path().join("glyphs").join("glyphs.txt"),
            format!("{} A\n", file_name(hash, Format::Bc7, [w, h])),
        )
        .unwrap();
        std::fs::write(dir.path().join("native-keys.txt"), "Space = INPUT_JUMP\n").unwrap();
        let data = HSTRING::from(dir.path().as_os_str());
        let init = Init { data: data.as_ptr(), log: Some(log) };
        // SAFETY (all below): the interface's functions with live arguments.
        unsafe {
            assert_eq!((api.init)(&init), 0);
            assert_ne!((api.generation)(), 0);
            let t = Texture { hash, format: 98, width: w, height: h, levels: 1 };
            assert_eq!((api.wants)(&t), 1);
            assert_eq!((api.wants)(&Texture { hash: hash ^ 1, ..t }), 0);
            let mut level = texels.clone();
            assert_eq!((api.replace)(&t, 0, level.as_mut_ptr(), level.len(), 16 * 16), 1);
            let rgba = victory_bc::decode(Format::Bc7, w, h, &level, 16 * 16).unwrap();
            // Red is gone: the corner is clear and the rim grey.
            assert!(rgba[3] < 16, "corner {:?}", &rgba[..4]);
            let rim = &rgba[((2 * w + 32) * 4) as usize..][..4];
            assert!(rim[0] > 150 && rim[1] > 150, "rim {rim:?}");
        }
    }
}
