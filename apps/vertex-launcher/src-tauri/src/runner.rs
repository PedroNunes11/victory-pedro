//! Runs `vertex-axx.exe` / `vertex.exe` / `victory.exe` for Play, Pipeline
//! Warming (`warm --used`), Shader Pre-Translation (`shaders`), and Config
//! check (`config`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::Serialize;

use crate::settings::Profile;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TAIL_LINES: usize = 200;
const TAIL_BYTES: u64 = 48 * 1024;
const FALLBACK_DIRS: &[&str] = &[
    r"D:\dev\Vertex-AXX\bin",
    r"D:\dev\unify\target-integ\diag",
    r"D:\dev\unify\target\release",
];
pub const WARM_PIPELINES_MB: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Play,
    Prebuild,
    TranslateShaders,
}

#[derive(Default)]
pub struct Run {
    pub kind: Option<Kind>,
    pub pid: Option<u32>,
    pub started: Option<Instant>,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub window_size: Option<(u32, u32)>,
    pub resized: bool,
    pub window_seen: bool,
    pub game_hwnd: Option<isize>,
    /// Launch-time signature of restart-required settings.
    pub launch_signature: Option<String>,
}

pub type Shared = Arc<Mutex<Run>>;

#[derive(Clone, Debug, Serialize)]
pub struct Found {
    pub path: Option<String>,
    pub how: &'static str,
}

pub const ENGINE_EXES: [&str; 3] = ["vertex-axx.exe", "vertex.exe", "victory.exe"];

/// The engine in `dir`: the first of [`ENGINE_EXES`] there, by that order alone. Not the newest
/// file: a test build copied beside the engine (or a touched backup) must never take over a run.
fn engine_in(dir: &Path) -> Option<PathBuf> {
    ENGINE_EXES.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

pub fn find_victory(chosen: &str) -> Found {
    let chosen = chosen.trim();
    if !chosen.is_empty() && Path::new(chosen).is_file() {
        return Found {
            path: Some(chosen.to_string()),
            how: "chosen",
        };
    }
    let here = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    search_victory(here.as_deref())
}

fn search_victory(here: Option<&Path>) -> Found {
    let found = |p: PathBuf, how| Found {
        path: Some(p.display().to_string()),
        how,
    };
    if let Some(here) = here {
        if let Some(p) = engine_in(here) {
            return found(p, "beside");
        }
        if let Some(p) = engine_in(&here.join("bin")) {
            return found(p, "beside");
        }
        for dir in here.ancestors() {
            if let Some(p) = engine_in(&dir.join("target-integ").join("diag")) {
                return found(p, "release");
            }
            if let Some(p) = engine_in(&dir.join("target").join("release")) {
                return found(p, "release");
            }
        }
    }
    for dir in FALLBACK_DIRS {
        if let Some(p) = engine_in(Path::new(dir)) {
            return found(p, "fallback");
        }
    }
    Found {
        path: None,
        how: "none",
    }
}

pub fn workdir(victory: &Path) -> PathBuf {
    // If inside `Vertex-AXX\bin\vertex-axx.exe`, workdir is `Vertex-AXX`.
    if let Some(parent) = victory.parent()
        && parent.file_name().is_some_and(|n| n.eq_ignore_ascii_case("bin"))
        && let Some(root) = parent.parent()
    {
        return root.to_path_buf();
    }
    for dir in victory.ancestors().skip(1) {
        if dir.join("Cargo.toml").is_file() {
            return dir.to_path_buf();
        }
    }
    victory.parent().map(Path::to_path_buf).unwrap_or_default()
}

pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

pub fn log_path() -> PathBuf {
    crate::settings::launcher_dir().join("last-run.log")
}

fn command(
    victory: &Path,
    args: &[String],
    profile: &Profile,
    game: Option<&Path>,
    data_dir: &Path,
    cpu_threads: u32,
) -> Command {
    let mut cmd = Command::new(victory);
    cmd.args(args)
        .current_dir(workdir(victory))
        .creation_flags(CREATE_NO_WINDOW);
    // Clear inherited VICTORY_* so stale host env never leaks into a run.
    for (k, _) in std::env::vars() {
        if k.starts_with("VICTORY_") {
            cmd.env_remove(&k);
        }
    }
    for (name, value) in profile.env_for(Some(victory), Some(data_dir), cpu_threads) {
        cmd.env(name, value);
    }
    if let Some(dir) = game.and_then(Path::parent) {
        cmd.env("VICTORY_GAME_DIR", dir);
    }
    cmd.env("NO_COLOR", "1");
    cmd
}

pub fn describe(
    victory: &Path,
    args: &[String],
    profile: &Profile,
    game: Option<&Path>,
    data_dir: &Path,
    cpu_threads: u32,
) -> String {
    let quote = |s: &str| {
        if s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s.to_string()
        }
    };
    let mut out = String::new();
    if let Some(dir) = game.and_then(Path::parent) {
        out.push_str(&format!("VICTORY_GAME_DIR={}\n", dir.display()));
    }
    for (name, value) in profile.env_for(Some(victory), Some(data_dir), cpu_threads) {
        out.push_str(&format!("{name}={value}\n"));
    }
    out.push_str(&quote(&victory.display().to_string()));
    for a in args {
        out.push(' ');
        out.push_str(&quote(a));
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn start(
    shared: &Shared,
    kind: Kind,
    victory: &Path,
    args: Vec<String>,
    profile: &Profile,
    game: &Path,
    data_dir: &Path,
    cpu_threads: u32,
) -> Result<(), String> {
    start_then(shared, kind, victory, args, profile, game, data_dir, cpu_threads, |_| {})
}

/// [`start`], and `done` called with the exit code once the run has ended (before anyone can
/// start another run).
#[allow(clippy::too_many_arguments)]
pub fn start_then(
    shared: &Shared,
    kind: Kind,
    victory: &Path,
    args: Vec<String>,
    profile: &Profile,
    game: &Path,
    data_dir: &Path,
    cpu_threads: u32,
    done: impl FnOnce(Option<i32>) + Send + 'static,
) -> Result<(), String> {
    {
        let run = shared.lock().unwrap();
        if run.running {
            return Err("busy".into());
        }
    }
    let dir = crate::settings::launcher_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = log_path();
    let log_err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut log = std::fs::File::create(&path).map_err(log_err)?;
    writeln!(
        log,
        "{}\n",
        describe(victory, &args, profile, Some(game), data_dir, cpu_threads)
    )
    .map_err(log_err)?;
    let out = log.try_clone().map_err(log_err)?;
    let err = log.try_clone().map_err(log_err)?;
    drop(log);
    let mut cmd = command(victory, &args, profile, Some(game), data_dir, cpu_threads);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", victory.display()))?;
    {
        let mut run = shared.lock().unwrap();
        *run = Run {
            kind: Some(kind),
            pid: Some(child.id()),
            started: Some(Instant::now()),
            running: true,
            window_size: (kind == Kind::Play && !profile.fullscreen)
                .then(|| crate::settings::parse_size(&profile.window_size))
                .flatten(),
            launch_signature: (kind == Kind::Play).then(|| profile.restart_signature()),
            ..Run::default()
        };
    }
    let shared = shared.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        let mut run = shared.lock().unwrap();
        let code = status.ok().and_then(|s| s.code());
        // Under the lock: nothing starts another run until this is done.
        done(code);
        run.running = false;
        run.game_hwnd = None;
        run.launch_signature = None;
        run.exit_code = code;
    });
    Ok(())
}

pub fn tail(path: &Path, lines: usize) -> (u64, Vec<String>) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return (0, Vec::new());
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = size.saturating_sub(TAIL_BYTES);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(from)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return (size, Vec::new());
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.split('\n').collect();
    if from > 0 && !all.is_empty() {
        all.remove(0);
    }
    while all.last().is_some_and(|l| l.trim().is_empty()) {
        all.pop();
    }
    let skip = all.len().saturating_sub(lines);
    let out = all[skip..]
        .iter()
        .map(|l| strip_ansi(l.trim_end_matches('\r')))
        .collect();
    (size, out)
}

pub fn stop(shared: &Shared) -> Result<(), String> {
    let pid = {
        let run = shared.lock().unwrap();
        if !run.running {
            return Ok(());
        }
        run.pid
    };
    let Some(pid) = pid else { return Ok(()) };
    Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn doctor(
    victory: &Path,
    profile: &Profile,
    data_dir: &Path,
    cpu_threads: u32,
) -> Result<(bool, String), String> {
    let out = command(
        victory,
        &["config".to_string()],
        profile,
        None,
        data_dir,
        cpu_threads,
    )
    .stdin(Stdio::null())
    .output()
    .map_err(|e| format!("{}: {e}", victory.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push('\n');
        text.push_str(&err);
    }
    Ok((out.status.success(), strip_ansi(&text)))
}

#[derive(Clone, Debug, Serialize)]
pub struct CacheCheck {
    pub cold: Option<&'static str>,
    pub dir: String,
    pub pipelines_mb: u64,
    pub known_pipelines: usize,
    pub warmed_pipelines: usize,
}

fn newest_change(cache: &Path, library: &Path) -> Option<SystemTime> {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let shaders = cache.join("shaders");
    let pipes = cache.join("pipelines");
    let mut times: Vec<SystemTime> = [
        modified(library),
        modified(&pipes.join("warm-done.txt")),
        modified(&shaders),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Ok(entries) = std::fs::read_dir(&shaders) {
        for e in entries.flatten().take(32) {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                times.push(t);
            }
        }
    }
    times.into_iter().max()
}

pub fn check_cache(cache: &Path, victory: Option<&Path>) -> CacheCheck {
    let pipes = cache.join("pipelines");
    let library = pipes.join("host-pipelines.bin");
    let size = std::fs::metadata(&library).ok().map(|m| m.len());
    let mb = size.unwrap_or(0) / (1024 * 1024);
    let warmed = std::fs::read_to_string(pipes.join("warm-done.txt"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let known = std::fs::read_to_string(pipes.join("early-pipelines.txt"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let built = victory.and_then(|v| std::fs::metadata(v).and_then(|m| m.modified()).ok());
    let has_cache = size.is_some() || warmed > 100;
    let cold = if !has_cache {
        Some("missing")
    } else if size.is_some() && mb < WARM_PIPELINES_MB && warmed < 200 {
        Some("small")
    } else {
        match (newest_change(cache, &library), built) {
            (Some(cache_time), Some(exe_time)) if cache_time < exe_time => Some("old"),
            _ => None,
        }
    };
    CacheCheck {
        cold,
        dir: cache.display().to_string(),
        pipelines_mb: mb,
        known_pipelines: known,
        warmed_pipelines: warmed,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub kind: Option<Kind>,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub seconds: u64,
    pub log_size: u64,
    pub lines: Option<Vec<String>>,
    pub window_seen: bool,
    pub fps: Option<f32>,
    pub ms: Option<f32>,
    pub launch_signature: Option<String>,
}

/// Reads FPS & frame time from either `" | Vertex "` or `" | VIctory "` title.
pub fn meter(title: &str) -> (Option<f32>, Option<f32>) {
    let at = title
        .rfind("| Vertex ")
        .map(|i| i + 9)
        .or_else(|| title.rfind("| VIctory ").map(|i| i + 10));
    let Some(start) = at else {
        return (None, None);
    };
    let words: Vec<&str> = title[start..].split_whitespace().collect();
    let num = |i: usize, unit: &str| {
        (words.get(i + 1) == Some(&unit))
            .then(|| words.get(i)?.parse::<f32>().ok())
            .flatten()
    };
    (num(0, "fps"), num(2, "ms/frame"))
}

pub fn status(shared: &Shared, since: u64) -> Status {
    let (pid, kind, size, resized) = {
        let run = shared.lock().unwrap();
        (
            run.running.then_some(run.pid).flatten(),
            run.kind,
            run.window_size,
            run.resized,
        )
    };
    let mut fps = None;
    let mut ms = None;
    let mut seen = false;
    let mut found_hwnd = None;
    if let (Some(pid), Some(Kind::Play)) = (pid, kind) {
        let tree = crate::system::process_tree(pid);
        if let Some((hwnd, title)) = crate::system::window_of(&tree) {
            seen = true;
            found_hwnd = Some(hwnd);
            (fps, ms) = meter(&title);
            if let (Some((w, h)), false) = (size, resized) {
                let _ = crate::system::resize_window(hwnd, w, h);
                shared.lock().unwrap().resized = true;
            }
        }
    }
    let log = log_path();
    let log_size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let lines = (log_size != since).then(|| tail(&log, TAIL_LINES).1);
    let mut run = shared.lock().unwrap();
    run.window_seen |= seen;
    if found_hwnd.is_some() {
        run.game_hwnd = found_hwnd;
    }
    Status {
        kind: run.kind,
        running: run.running,
        exit_code: run.exit_code,
        seconds: run.started.map_or(0, |s| s.elapsed().as_secs()),
        log_size,
        lines,
        window_seen: run.window_seen,
        fps,
        ms,
        launch_signature: run.launch_signature.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_reads_both_vertex_and_victory_titles() {
        assert_eq!(
            meter("Title | Vertex 22 fps 45.1 ms/frame 6200 draws/frame"),
            (Some(22.0), Some(45.1))
        );
        assert_eq!(
            meter("Title | VIctory 24 fps 41.7 ms/frame 6500 draws/frame"),
            (Some(24.0), Some(41.7))
        );
        assert_eq!(meter("Title"), (None, None));
    }

    #[test]
    fn engine_writes_the_meter_the_launcher_reads() {
        let host = include_str!("../../../../crates/victory-gpu/src/host.rs");
        assert!(host.contains("| VIctory {fps:.0} fps {ms:.1} ms/frame"));
    }
}
