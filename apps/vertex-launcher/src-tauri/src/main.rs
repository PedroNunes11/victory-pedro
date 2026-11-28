//! Vertex Launcher: unified desktop frontend for the Vertex engine with
//! Performance / Balanced / High Quality / 4K Native modes, GTA VI PC
//! Controls & Scaleform vector prompt glyphs, shader cache warmer, and a
//! global `Win+Alt+V` (customizable) In-Game Menu overlay.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod best;
mod cli;
mod hotkey;
mod keys;
mod lossless;
mod native_keys;
mod runner;
mod settings;
mod system;
mod tune;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use settings::{Preset, Profile};

struct App {
    profile: Mutex<Profile>,
    run: runner::Shared,
    system: Mutex<Option<system::SystemInfo>>,
    launch: Mutex<Option<cli::Launch>>,
    cli_applied: AtomicBool,
    hotkey: Mutex<Option<hotkey::HotkeyManager>>,
    overlay_open: AtomicBool,
}

impl App {
    fn profile(&self) -> Profile {
        self.profile.lock().unwrap().clone()
    }

    fn system(&self) -> system::SystemInfo {
        let mut cached = self.system.lock().unwrap();
        cached.get_or_insert_with(system::read).clone()
    }

    fn cpu_threads(&self) -> u32 {
        std::thread::available_parallelism().map_or(16, |n| n.get() as u32)
    }

    fn victory(&self) -> Result<PathBuf, String> {
        runner::find_victory(&self.profile().victory_exe)
            .path
            .map(PathBuf::from)
            .ok_or_else(|| "no_victory".into())
    }
}

type R<T> = Result<T, String>;

#[derive(Serialize)]
struct Paths {
    profile: String,
    keys: String,
    native_keys: String,
    log: String,
    lossless_settings: String,
    cache: settings::Folder,
    data: settings::Folder,
}

#[derive(Serialize)]
struct Defaults {
    cpu: Vec<(&'static str, &'static str)>,
    balanced: Vec<(&'static str, &'static str)>,
    gpu: Vec<(&'static str, &'static str)>,
}

#[derive(Serialize)]
struct HotkeyStatus {
    spec: String,
    error: Option<String>,
}

#[derive(Serialize)]
struct Start {
    profile: Profile,
    victory: runner::Found,
    dlss_available: bool,
    paths: Paths,
    defaults: Defaults,
    key_names: Vec<String>,
    native_key_names: Vec<String>,
    hotkey: HotkeyStatus,
}

fn workdir(app: &App) -> Option<PathBuf> {
    app.victory().ok().map(|v| runner::workdir(&v))
}

fn resolved_data_dir(app: &App) -> PathBuf {
    let profile = app.profile();
    let wd = workdir(app);
    PathBuf::from(profile.data_folder(wd.as_deref()).path)
}

fn keys_path(app: &App) -> PathBuf {
    let data = app.profile().data_override().map(|(dir, _)| dir);
    keys::file_path(data.as_deref(), workdir(app).as_deref())
}

fn paths(app: &App) -> Paths {
    let profile = app.profile();
    let wd = workdir(app);
    let data = profile.data_folder(wd.as_deref());
    let native = native_keys::native_keys_path(Path::new(&data.path));
    Paths {
        profile: settings::profile_path().display().to_string(),
        keys: keys_path(app).display().to_string(),
        native_keys: native.display().to_string(),
        log: runner::log_path().display().to_string(),
        lossless_settings: lossless::settings_path().display().to_string(),
        cache: profile.cache_folder(wd.as_deref()),
        data,
    }
}

fn hotkey_status(app: &App) -> HotkeyStatus {
    if let Some(mgr) = app.hotkey.lock().unwrap().as_ref() {
        let (spec, error) = mgr.status();
        HotkeyStatus { spec, error }
    } else {
        HotkeyStatus {
            spec: app.profile().ingame_hotkey,
            error: None,
        }
    }
}

/// Brings the launcher window up over the game (In-Game Menu) or dismisses it
/// back to the game window. Dispatches window activation onto the Tauri main
/// thread so `AttachThreadInput` + `HWND_TOPMOST` reliably lifts over fullscreen.
fn set_ingame_overlay(handle: &tauri::AppHandle, open: bool) {
    let state = handle.state::<App>();
    state.overlay_open.store(open, Ordering::Relaxed);
    let (tracked_hwnd, tracked_running) = {
        let r = state.run.lock().unwrap();
        (r.game_hwnd, r.running && r.kind == Some(runner::Kind::Play))
    };
    let game_hwnd = tracked_hwnd.or_else(system::find_running_game_hwnd);
    let game_running = tracked_running || game_hwnd.is_some();

    let h2 = handle.clone();
    let _ = handle.run_on_main_thread(move || {
        if let Some(w) = h2.get_webview_window("main") {
            if open {
                system::release_cursor_clip();
                let _ = w.set_always_on_top(true);
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
                if let Ok(hwnd) = w.hwnd() {
                    system::focus_window_with_z(hwnd.0 as isize, Some(true));
                }
            } else {
                let _ = w.set_always_on_top(false);
                if game_running {
                    let _ = w.minimize();
                    if let Some(gh) = game_hwnd {
                        system::focus_window(gh);
                    }
                }
            }
        }
    });
    let _ = handle.emit(
        "ingame-overlay",
        serde_json::json!({ "open": open, "game_running": game_running }),
    );
}

#[tauri::command]
fn get_start(app: State<App>) -> Start {
    let names = settings::MANAGED;
    let table = |work| {
        names
            .iter()
            .filter_map(|&n| settings::default_of(n, work).map(|v| (n, v)))
            .collect()
    };
    let victory = runner::find_victory(&app.profile().victory_exe);
    let dlss_available = settings::find_dlss_plugin(victory.path.as_deref().map(Path::new)).is_some();
    let mut native_key_names: Vec<String> = ('A'..='Z')
        .chain('0'..='9')
        .map(|c| c.to_string())
        .collect();
    native_key_names.extend(native_keys::NATIVE_KEY_NAMES.iter().map(|s| (*s).to_string()));
    Start {
        profile: app.profile(),
        victory,
        dlss_available,
        paths: paths(&app),
        defaults: Defaults {
            cpu: table(settings::Work::Cpu),
            balanced: table(settings::Work::Balanced),
            gpu: table(settings::Work::Gpu),
        },
        key_names: keys::all_key_names(),
        native_key_names,
        hotkey: hotkey_status(&app),
    }
}

fn handle_line(app: &App, args: Vec<String>, base: &Path) -> cli::Launch {
    let mut launch = cli::Launch::default();
    let line = match cli::parse(args) {
        Ok(line) => line,
        Err(e) => {
            launch.error = Some(e);
            return launch;
        }
    };
    if line.changes_profile() {
        let mut p = app.profile();
        match cli::apply(&line, &mut p, base, || best::load().file, || app.system()) {
            Ok(notes) => {
                launch.notes = notes;
                launch.applied = true;
                if !line.no_save {
                    match settings::save(&p) {
                        Ok(()) => launch.saved = true,
                        Err(e) => launch.error = Some(e),
                    }
                }
                *app.profile.lock().unwrap() = p;
                app.cli_applied.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                launch.error = Some(e);
                return launch;
            }
        }
    }
    (launch.start, launch.prebuild, launch.no_music) = (line.start, line.prebuild, line.no_music);
    launch
}

#[tauri::command]
fn take_launch(app: State<App>) -> Option<cli::Launch> {
    app.launch.lock().unwrap().take()
}

#[derive(Serialize)]
struct Offer {
    pick: best::Pick,
    source: &'static str,
    problem: Option<String>,
    updated: String,
    due: bool,
}

#[tauri::command]
async fn best_offer(app: State<'_, App>) -> R<Offer> {
    let loaded = best::load();
    let pick = best::pick(&loaded.file, &app.system());
    let due = !app.cli_applied.load(Ordering::Relaxed)
        && app.profile().best_seen.as_str() < loaded.file.updated.as_str();
    Ok(Offer {
        pick,
        source: loaded.source,
        problem: loaded.problem,
        updated: loaded.file.updated,
        due,
    })
}

#[tauri::command]
async fn best_apply(app: State<'_, App>) -> R<Profile> {
    let loaded = best::load();
    let pick = best::pick(&loaded.file, &app.system());
    let mut p = app.profile();
    best::apply(&mut p, &pick.entry)?;
    p.best_seen = loaded.file.updated;
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p.clone();
    Ok(p)
}

#[tauri::command]
fn best_decline(app: State<App>) -> R<Profile> {
    let mut p = app.profile();
    p.best_seen = best::load().file.updated;
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p.clone();
    Ok(p)
}

#[tauri::command]
async fn get_system(app: State<'_, App>) -> R<system::SystemInfo> {
    Ok(app.system())
}

#[derive(Serialize)]
struct Saved {
    victory: runner::Found,
    dlss_available: bool,
    paths: Paths,
    restart_needed: bool,
}

#[tauri::command]
fn save_profile(app: State<App>, profile: Profile) -> R<Saved> {
    settings::save(&profile)?;
    let victory = runner::find_victory(&profile.victory_exe);
    let dlss_available = settings::find_dlss_plugin(victory.path.as_deref().map(Path::new)).is_some();
    let restart_needed = {
        let run = app.run.lock().unwrap();
        run.running
            && run.kind == Some(runner::Kind::Play)
            && run
                .launch_signature
                .as_deref()
                .is_some_and(|sig| sig != profile.restart_signature())
    };
    *app.profile.lock().unwrap() = profile;
    Ok(Saved {
        victory,
        dlss_available,
        paths: paths(&app),
        restart_needed,
    })
}

#[tauri::command]
fn apply_preset(app: State<App>, preset: Preset) -> R<Profile> {
    let mut p = app.profile();
    p.apply_preset(preset);
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p.clone();
    Ok(p)
}

#[tauri::command]
fn reset_profile(app: State<App>) -> R<Profile> {
    let mut p = app.profile();
    p.reset();
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p.clone();
    Ok(p)
}

#[tauri::command]
fn set_ingame_hotkey(app: State<App>, spec: String) -> R<HotkeyStatus> {
    let canonical = {
        let guard = app.hotkey.lock().unwrap();
        match guard.as_ref() {
            Some(mgr) => mgr.update(&spec)?,
            None => hotkey::parse_hotkey(&spec)?.1,
        }
    };
    let mut p = app.profile();
    p.ingame_hotkey = canonical;
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p;
    // Brief moment for the hotkey thread to finish RegisterHotKey.
    std::thread::sleep(Duration::from_millis(25));
    Ok(hotkey_status(&app))
}

#[tauri::command]
fn resume_game(handle: tauri::AppHandle) {
    set_ingame_overlay(&handle, false);
}

#[tauri::command]
async fn pick_exe(window: tauri::Window, label: String) -> Option<String> {
    window
        .dialog()
        .file()
        .add_filter(label, &["exe"])
        .blocking_pick_file()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.display().to_string())
}

#[tauri::command]
async fn pick_folder(window: tauri::Window) -> Option<String> {
    window
        .dialog()
        .file()
        .blocking_pick_folder()
        .and_then(|f| f.into_path().ok())
        .map(|p| p.display().to_string())
}

#[tauri::command]
async fn cache_check(app: State<'_, App>) -> R<runner::CacheCheck> {
    let profile = app.profile();
    let victory = app.victory().ok();
    let wd = victory.as_deref().map(runner::workdir);
    let cache = PathBuf::from(profile.cache_folder(wd.as_deref()).path);
    Ok(runner::check_cache(&cache, victory.as_deref()))
}

fn game_of(profile: &Profile) -> R<PathBuf> {
    let game = PathBuf::from(profile.game_exe.trim());
    if profile.game_exe.trim().is_empty() || !game.is_file() {
        return Err("no_game".into());
    }
    Ok(game)
}

#[derive(Serialize)]
struct Started {
    command: String,
    lossless_error: Option<String>,
}

fn launch_play_now(app: &App) -> R<Started> {
    let profile = app.profile();
    let game = game_of(&profile)?;
    let victory = app.victory()?;
    let data_dir = resolved_data_dir(app);
    // Ensure native-keys.txt exists before launching in native controls mode.
    if profile.native_controls && !native_keys::native_keys_path(&data_dir).is_file() {
        let _ = native_keys::reset(&data_dir);
    }
    let mut lossless_error = None;
    if profile.lossless.start_with_game
        && lossless::find_exe().is_some()
        && let Err(e) = lossless::start()
    {
        lossless_error = Some(e);
    }
    let args = profile.run_args(&game);
    let threads = app.cpu_threads();
    runner::start(
        &app.run,
        runner::Kind::Play,
        &victory,
        args.clone(),
        &profile,
        &game,
        &data_dir,
        threads,
    )?;
    Ok(Started {
        command: runner::describe(&victory, &args, &profile, Some(&game), &data_dir, threads),
        lossless_error,
    })
}

#[tauri::command]
async fn start_play(app: State<'_, App>) -> R<Started> {
    launch_play_now(&app)
}

#[tauri::command]
async fn restart_game(handle: tauri::AppHandle, app: State<'_, App>) -> R<Started> {
    set_ingame_overlay(&handle, false);
    let _ = runner::stop(&app.run);
    for _ in 0..40 {
        if !app.run.lock().unwrap().running {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    launch_play_now(&app)
}

/// The settings a warm is made for: the graphics mode and what changes render sizes, the upscaler
/// or the passes run, and the engine it was made with (its size and time: a new engine translates
/// shaders anew). Play warms again first whenever these differ from the last warm's.
fn warm_signature(p: &Profile, victory: &std::path::Path) -> String {
    let engine = std::fs::metadata(victory)
        .map(|m| {
            let secs = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            format!("{}:{secs}", m.len())
        })
        .unwrap_or_default();
    serde_json::json!({
        "engine": engine,
        "preset": p.preset,
        "output_resolution": p.output_resolution,
        "dlss": p.dlss,
        "render_scale": p.render_scale,
        "console": p.console,
        "reflections": p.reflections,
    })
    .to_string()
}

/// `<cache>\pipelines\warm-mode.txt`: the signature of the last warm that ended well.
fn warm_mark(profile: &Profile, victory: &std::path::Path) -> PathBuf {
    let wd = runner::workdir(victory);
    PathBuf::from(profile.cache_folder(Some(&wd)).path)
        .join("pipelines")
        .join("warm-mode.txt")
}

/// Whether the current mode has not been warmed yet (the last warm was for another mode, or none).
#[tauri::command]
async fn warm_needed(app: State<'_, App>) -> R<bool> {
    let profile = app.profile();
    let victory = app.victory()?;
    let last = std::fs::read_to_string(warm_mark(&profile, &victory)).unwrap_or_default();
    Ok(last.trim() != warm_signature(&profile, &victory))
}

#[tauri::command]
async fn start_prebuild(app: State<'_, App>) -> R<Started> {
    let profile = app.profile();
    let game = game_of(&profile)?;
    let victory = app.victory()?;
    let data_dir = resolved_data_dir(&app);
    let args = profile.prebuild_args(&game);
    let threads = app.cpu_threads();
    // A warm that ends well is written down for its mode.
    let (mark, signature) = (warm_mark(&profile, &victory), warm_signature(&profile, &victory));
    runner::start_then(
        &app.run,
        runner::Kind::Prebuild,
        &victory,
        args.clone(),
        &profile,
        &game,
        &data_dir,
        threads,
        move |code| {
            if code == Some(0) {
                let _ = std::fs::write(&mark, &signature);
            }
        },
    )?;
    Ok(Started {
        command: runner::describe(&victory, &args, &profile, Some(&game), &data_dir, threads),
        lossless_error: None,
    })
}

#[tauri::command]
async fn start_translate_shaders(app: State<'_, App>) -> R<Started> {
    let profile = app.profile();
    let game = game_of(&profile)?;
    let victory = app.victory()?;
    let data_dir = resolved_data_dir(&app);
    let args = profile.shader_prebuild_args(&game);
    let threads = app.cpu_threads();
    runner::start(
        &app.run,
        runner::Kind::TranslateShaders,
        &victory,
        args.clone(),
        &profile,
        &game,
        &data_dir,
        threads,
    )?;
    Ok(Started {
        command: runner::describe(&victory, &args, &profile, Some(&game), &data_dir, threads),
        lossless_error: None,
    })
}

#[tauri::command]
fn stop_run(app: State<App>) -> R<()> {
    runner::stop(&app.run)
}

#[tauri::command]
async fn run_status(app: State<'_, App>, since: u64) -> R<runner::Status> {
    Ok(runner::status(&app.run, since))
}

#[tauri::command]
fn command_preview(app: State<App>) -> R<String> {
    let profile = app.profile();
    let victory = app
        .victory()
        .unwrap_or_else(|_| PathBuf::from("victory.exe"));
    let data_dir = resolved_data_dir(&app);
    let game = PathBuf::from(if profile.game_exe.trim().is_empty() {
        "master.exe"
    } else {
        profile.game_exe.trim()
    });
    Ok(runner::describe(
        &victory,
        &profile.run_args(&game),
        &profile,
        Some(&game),
        &data_dir,
        app.cpu_threads(),
    ))
}

#[derive(Serialize)]
struct Doctor {
    ok: bool,
    text: String,
}

#[tauri::command]
async fn doctor(app: State<'_, App>) -> R<Doctor> {
    let victory = app.victory()?;
    let data_dir = resolved_data_dir(&app);
    let (ok, text) = runner::doctor(&victory, &app.profile(), &data_dir, app.cpu_threads())?;
    Ok(Doctor { ok, text })
}

// --- Native Controls (live reload in game within 1 s) ---

#[tauri::command]
fn read_native_keys(app: State<App>) -> native_keys::NativeControlsData {
    native_keys::load(&resolved_data_dir(&app))
}

#[tauri::command]
fn write_native_keys(
    app: State<App>,
    actions: Vec<native_keys::NativeAction>,
) -> R<String> {
    native_keys::save(&resolved_data_dir(&app), &actions)
}

#[tauri::command]
fn preview_prompt_glyphs(
    actions: Vec<native_keys::NativeAction>,
) -> Vec<native_keys::PromptPreviewItem> {
    native_keys::compute_prompt_preview(&actions)
}

#[tauri::command]
fn reset_native_keys(app: State<App>) -> R<native_keys::NativeControlsData> {
    native_keys::reset(&resolved_data_dir(&app))
}

#[tauri::command]
fn write_mouse_live(app: State<App>, sensitivity: f32, invert_y: bool) -> R<()> {
    let data_dir = resolved_data_dir(&app);
    native_keys::write_mouse_to_keys_txt(&data_dir, sensitivity, invert_y)?;
    let mut p = app.profile();
    p.mouse_sensitivity = Some(sensitivity.clamp(0.1, 10.0));
    p.mouse_invert_y = invert_y;
    settings::save(&p)?;
    *app.profile.lock().unwrap() = p;
    Ok(())
}

// --- Legacy keys.txt commands (retained for optional fallback mode) ---

#[derive(Serialize)]
struct KeysRead {
    file: keys::KeyFile,
    problems: Vec<keys::Problem>,
    path: String,
    exists: bool,
}

#[tauri::command]
fn read_keys(app: State<App>) -> KeysRead {
    let path = keys_path(&app);
    let text = std::fs::read_to_string(&path).ok();
    let (file, problems) = match &text {
        Some(t) => keys::parse(t),
        None => (keys::KeyFile::defaults(), Vec::new()),
    };
    KeysRead {
        file,
        problems,
        path: path.display().to_string(),
        exists: text.is_some(),
    }
}

#[tauri::command]
fn default_keys() -> keys::KeyFile {
    keys::KeyFile::defaults()
}

#[tauri::command]
fn write_keys(app: State<App>, file: keys::KeyFile) -> R<String> {
    let path = keys_path(&app);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = keys::write(&file);
    let (_, problems) = keys::parse(&text);
    if !problems.is_empty() {
        return Err(format!("{problems:?}"));
    }
    let temp = path.with_extension("txt.vertex-new");
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

#[tauri::command]
async fn auto_tune(app: State<'_, App>) -> R<tune::Tune> {
    Ok(tune::tune(&app.system(), &app.profile()))
}

#[tauri::command]
async fn lossless_status(app: State<'_, App>) -> R<lossless::Status> {
    let victory = app.victory().ok();
    Ok(lossless::status(victory.as_deref()))
}

#[tauri::command]
fn lossless_write(app: State<App>) -> R<lossless::Written> {
    let victory = app.victory()?;
    lossless::write_profile(&victory, &app.profile().lossless)
}

#[tauri::command]
fn lossless_start() -> R<()> {
    lossless::start()
}

#[tauri::command]
fn open_place(app: State<App>, what: String) -> R<()> {
    let p = paths(&app);
    let target = match what.as_str() {
        "keys-folder" => Path::new(&p.native_keys).parent().map(Path::to_path_buf),
        "log" => Some(PathBuf::from(&p.log)),
        "launcher-folder" => Some(settings::launcher_dir()),
        "game-folder" => Path::new(app.profile().game_exe.trim())
            .parent()
            .map(Path::to_path_buf),
        "victory-folder" => app
            .victory()
            .ok()
            .and_then(|v| v.parent().map(Path::to_path_buf)),
        _ => None,
    }
    .ok_or("missing")?;
    if !target.exists() {
        return Err("missing".into());
    }
    system::open(&target.display().to_string())
}

fn main() {
    let profile = settings::load();
    let initial_hotkey = profile.ingame_hotkey.clone();
    let app = App {
        profile: Mutex::new(profile),
        run: Arc::new(Mutex::new(runner::Run::default())),
        system: Mutex::new(None),
        launch: Mutex::new(None),
        cli_applied: AtomicBool::new(false),
        hotkey: Mutex::new(None),
        overlay_open: AtomicBool::new(false),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let base = std::env::current_dir().unwrap_or_default();
        let launch = handle_line(&app, args, &base);
        if let Some(e) = &launch.error {
            eprintln!("vertex-launcher: {e}");
        }
        *app.launch.lock().unwrap() = Some(launch);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|handle, argv, cwd| {
            let state = handle.state::<App>();
            if argv.len() > 1 {
                let launch =
                    handle_line(&state, argv.into_iter().skip(1).collect(), Path::new(&cwd));
                *state.launch.lock().unwrap() = Some(launch);
                let _ = handle.emit("launch", ());
            }
            if let Some(w) = handle.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(app)
        .setup(move |tauri_app| {
            let handle = tauri_app.handle().clone();
            let toggle_handle = handle.clone();
            let mgr = hotkey::HotkeyManager::start(&initial_hotkey, move || {
                let state = toggle_handle.state::<App>();
                let launcher_focused = toggle_handle
                    .get_webview_window("main")
                    .and_then(|w| w.hwnd().ok())
                    .is_some_and(|h| system::is_foreground(h.0 as isize));
                let game_running = {
                    let r = state.run.lock().unwrap();
                    r.running && r.kind == Some(runner::Kind::Play)
                } || system::find_running_game_hwnd().is_some();
                if game_running {
                    // Always open when in-game; close back to game only if launcher is focused.
                    set_ingame_overlay(&toggle_handle, !launcher_focused);
                } else {
                    let h = toggle_handle.clone();
                    let _ = toggle_handle.run_on_main_thread(move || {
                        if let Some(w) = h.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                            if let Ok(hwnd) = w.hwnd() {
                                system::focus_window(hwnd.0 as isize);
                            }
                        }
                    });
                }
            });
            *handle.state::<App>().hotkey.lock().unwrap() = Some(mgr);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            warm_needed,
            get_start,
            get_system,
            save_profile,
            apply_preset,
            reset_profile,
            set_ingame_hotkey,
            resume_game,
            pick_exe,
            pick_folder,
            cache_check,
            start_play,
            restart_game,
            start_prebuild,
            start_translate_shaders,
            stop_run,
            run_status,
            command_preview,
            doctor,
            read_native_keys,
            write_native_keys,
            preview_prompt_glyphs,
            reset_native_keys,
            write_mouse_live,
            read_keys,
            default_keys,
            write_keys,
            auto_tune,
            lossless_status,
            lossless_write,
            lossless_start,
            open_place,
            take_launch,
            best_offer,
            best_apply,
            best_decline,
        ])
        .run(tauri::generate_context!())
        .expect("the launcher window could not start");
}
