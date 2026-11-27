//! Vertex Launcher v3 profile (`%LOCALAPPDATA%\Vertex\launcher.json`) and
//! environment builder for the Vertex-AXX engine (AnyXboxX + Vertex +
//! VIctory-NVIDIA).
//!
//! Settings are split cleanly into:
//! - **Live settings** (Controls, Mouse Sensitivity/Invert Y, In-Game Hotkey,
//!   Launcher Audio): take effect immediately while the game runs.
//! - **Restart-required settings** (Graphics Mode, Resolution, DLSS, Texture
//!   Sharpness, Reflections, Engine Performance/Memory tuning): applied on
//!   launch; changing them while playing prompts to Save & Restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

include!(concat!(env!("OUT_DIR"), "/engine_switches.rs"));

/// The console model the title sees (`vertex-axx.exe run --console`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Console {
    #[default]
    SeriesX,
    SeriesS,
}

impl Console {
    pub fn arg(self) -> &'static str {
        match self {
            Console::SeriesX => "series-x",
            Console::SeriesS => "series-s",
        }
    }
}

/// Top-level Graphics / Performance mode backed by measured Vertex-AXX runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    #[serde(alias = "low")]
    Performance,
    #[default]
    Balanced,
    #[serde(alias = "high")]
    HighQuality,
    /// High Quality's settings at 3840x2160 rendered at full size (DLAA).
    #[serde(rename = "native-4k")]
    Native4k,
}

/// NVIDIA DLSS Super Resolution mode (intercepts the game's FidelityFX upscale).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DlssMode {
    Off,
    #[default]
    Quality,
    Balanced,
    Performance,
    Dlaa,
}

impl DlssMode {
    pub fn ffx_scale(self) -> Option<&'static str> {
        match self {
            DlssMode::Off => None,
            DlssMode::Quality => Some("0.6667"),
            DlssMode::Balanced => Some("0.58"),
            DlssMode::Performance => Some("0.5"),
            DlssMode::Dlaa => Some("1.0"),
        }
    }

    pub fn default_lod_bias(self) -> Option<&'static str> {
        match self {
            DlssMode::Off => None,
            DlssMode::Quality => Some("-0.58"),
            DlssMode::Balanced => Some("-0.79"),
            DlssMode::Performance => Some("-1.0"),
            DlssMode::Dlaa => Some("0"),
        }
    }
}

/// Legacy compatibility enum kept for CLI/best.json parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Work {
    Cpu,
    #[default]
    Balanced,
    Gpu,
}

impl Work {
    pub fn arg(self) -> &'static str {
        match self {
            Work::Cpu => "cpu",
            Work::Balanced => "balanced",
            Work::Gpu => "gpu",
        }
    }
}

/// Lossless Scaling profile fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lossless {
    pub start_with_game: bool,
    pub multiplier: u32,
    pub flow_scale: u32,
    pub auto_scale: bool,
    pub auto_scale_delay: u32,
    pub max_frame_latency: u32,
    pub draw_fps: bool,
}

impl Default for Lossless {
    fn default() -> Self {
        Lossless {
            start_with_game: false,
            multiplier: 2,
            flow_scale: 75,
            auto_scale: true,
            auto_scale_delay: 10,
            max_frame_latency: 1,
            draw_fps: true,
        }
    }
}

impl Lossless {
    pub fn clamped(&self) -> Lossless {
        Lossless {
            multiplier: self.multiplier.clamp(2, 4),
            flow_scale: self.flow_scale.clamp(25, 100),
            auto_scale_delay: self.auto_scale_delay.min(120),
            max_frame_latency: self.max_frame_latency.clamp(1, 3),
            ..self.clone()
        }
    }
}

pub const PROFILE_VERSION: u32 = 3;

fn old_version() -> u32 {
    1
}

/// Valid output resolutions supported by the title's resolution patch (`VICTORY_OUTPUT`).
pub const OUTPUT_RESOLUTIONS: &[&str] = &[
    "1280x720",
    "1664x936",
    "1920x1080",
    "2560x1440",
    "3840x2160",
];

/// Everything the launcher remembers (`%LOCALAPPDATA%\Vertex\launcher.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    #[serde(default = "old_version")]
    pub version: u32,
    pub victory_exe: String,
    pub game_exe: String,
    pub console: Console,
    /// Active graphics mode (Performance / Balanced / HighQuality).
    pub preset: Preset,
    /// Output resolution patched into the title (`VICTORY_OUTPUT`).
    pub output_resolution: String,
    /// Launch fullscreen (`VICTORY_FULLSCREEN=1`) vs windowed.
    pub fullscreen: bool,
    /// Optional client window resize when windowed (`"WxH"`).
    pub window_size: String,
    /// Show the built-in performance overlay (`VICTORY_PANEL=1`).
    pub perf_panel: bool,
    /// NVIDIA DLSS Super Resolution mode.
    pub dlss: DlssMode,
    /// Optional explicit texture LOD bias (`VICTORY_LOD_BIAS`, e.g. `-0.8`).
    pub lod_bias: Option<f32>,
    /// Native GTA V-style keyboard & mouse controls (`native-keys.txt`).
    pub native_controls: bool,
    /// Experimental ray-traced reflection passes (`VICTORY_BVH_MISS` + resolve).
    pub reflections: bool,

    // --- Measured Texture Streaming & CPU/GPU Bottleneck Settings ---
    /// Share of DXGI VRAM budget available to textures (`VICTORY_TEXTURE_ROOM_PERCENT`, 50..97).
    pub texture_room_percent: u32,
    /// Cap on idle 64 MiB texture pool blocks (`VICTORY_TEXTURE_POOL_SPARES`, 2..64).
    pub texture_pool_spares: u32,
    /// Partial-mip recheck cadence in frames (`VICTORY_TEXTURE_PARTIAL_FRAMES`, 1..60).
    pub texture_partial_frames: u32,
    /// Report honest title physical memory to `XMemGetWorkingSetStatistics` (`VICTORY_TITLE_USED_MB=title`).
    pub honest_title_memory: bool,
    /// Parallelize large 8+ MiB title copies across worker threads (`VICTORY_WIDE_COPY=1`).
    pub wide_copy: bool,
    /// 64-shard watch journal (`VICTORY_PORT_JOURNAL=1`, cuts lock waits).
    pub port_journal: bool,
    /// Presents allowed in flight (`VICTORY_SUBMIT_FRAMES`, 1..3; 2 removes ~10 ms stall).
    pub submit_frames: u32,
    /// VRAM budget for card buffer copies (`VICTORY_CARD_BUFFERS_MB`).
    pub card_buffers_mb: u64,
    /// VRAM budget for card memory windows (`VICTORY_CARD_WINDOWS_MB`).
    pub card_windows_mb: u64,
    /// Max ms a frame waits for shaders still compiling (`VICTORY_PIPELINE_WAIT_MS`).
    pub pipeline_wait_ms: Option<u64>,

    // --- Live-Editable Settings (No Restart Needed) ---
    /// Global Windows hotkey to toggle the In-Game Menu (`Win+Alt+V`).
    pub ingame_hotkey: String,
    /// Mouse camera sensitivity (written live to `keys.txt` `[mouse]`).
    pub mouse_sensitivity: Option<f32>,
    /// Mouse Y-axis inversion (written live to `keys.txt` `[mouse]`).
    pub mouse_invert_y: bool,
    pub music_volume: u32,
    pub muted: bool,
    pub show_log: bool,

    // --- Folders & Extras ---
    pub prebuild_all: bool,
    pub cache_dir: String,
    pub data_dir: String,
    pub dlss_sdk_dir: String,
    pub extra_env: BTreeMap<String, String>,
    pub best_seen: String,
    pub lossless: Lossless,
    // Legacy compatibility field (kept for CLI `--work`).
    pub work: Work,
}

impl Default for Profile {
    fn default() -> Self {
        let mut p = Profile {
            version: PROFILE_VERSION,
            victory_exe: String::new(),
            game_exe: default_game_exe(),
            console: Console::SeriesX,
            preset: Preset::Balanced,
            output_resolution: "2560x1440".into(),
            fullscreen: true,
            window_size: String::new(),
            perf_panel: true,
            dlss: DlssMode::Quality,
            lod_bias: None,
            native_controls: true,
            reflections: false,
            texture_room_percent: 95,
            texture_pool_spares: 8,
            texture_partial_frames: 6,
            honest_title_memory: true,
            wide_copy: true,
            port_journal: true,
            submit_frames: 2,
            card_buffers_mb: 1536,
            card_windows_mb: 1024,
            pipeline_wait_ms: None,
            ingame_hotkey: crate::hotkey::DEFAULT_HOTKEY.into(),
            mouse_sensitivity: Some(1.0),
            mouse_invert_y: false,
            music_volume: 35,
            muted: false,
            show_log: false,
            prebuild_all: false,
            cache_dir: String::new(),
            data_dir: String::new(),
            dlss_sdk_dir: r"D:\sdk\dlss".into(),
            extra_env: BTreeMap::new(),
            best_seen: String::new(),
            lossless: Lossless::default(),
            work: Work::Balanced,
        };
        p.apply_preset(Preset::Balanced);
        p
    }
}

#[cfg(not(test))]
fn default_game_exe() -> String {
    let candidate = Path::new(r"D:\V6\payload\V6\master.exe");
    if candidate.is_file() {
        candidate.display().to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
fn default_game_exe() -> String {
    String::new()
}

/// The Vertex-AXX test/install root used for default cache/data discovery.
#[cfg(not(test))]
pub const FOUND_ROOT: &str = r"D:\dev\Vertex-AXX";
#[cfg(test)]
pub const FOUND_ROOT: &str = r"Z:\vertex-test\nowhere";

/// Managed env keys exposed for UI defaults and CLI validation.
pub const MANAGED: &[&str] = &[
    "VICTORY_OUTPUT",
    "VICTORY_FULLSCREEN",
    "VICTORY_PANEL",
    "VICTORY_DLSS_PLUGIN",
    "VICTORY_DLSS_SDK",
    "VICTORY_UPSCALER",
    "VICTORY_FFX_SCALE",
    "VICTORY_LOD_BIAS",
    "VICTORY_TEXTURE_ROOM_PERCENT",
    "VICTORY_TEXTURE_POOL_SPARES",
    "VICTORY_TEXTURE_PARTIAL_FRAMES",
    "VICTORY_TITLE_USED_MB",
    "VICTORY_WIDE_COPY",
    "VICTORY_PORT_JOURNAL",
    "VICTORY_SUBMIT_FRAMES",
    "VICTORY_CARD_BUFFERS_MB",
    "VICTORY_CARD_WINDOWS_MB",
    "VICTORY_PIPELINE_WAIT_MS",
    "VICTORY_KEYBOARD_EVENTS",
    "VICTORY_NATIVE_INPUT",
    "VICTORY_NATIVE_KEYS",
    "VICTORY_BVH_MISS",
    "VICTORY_RT_RESOLVE",
    "VICTORY_ACTIVATE_RAYS",
    "VICTORY_MOUSE_SENSITIVITY",
    "VICTORY_CACHE_DIR",
    "VICTORY_DATA_DIR",
];

pub fn pick_dir(chosen: &str, found: &Path) -> Option<(PathBuf, &'static str)> {
    let chosen = chosen.trim();
    if !chosen.is_empty() {
        return Some((PathBuf::from(chosen), "chosen"));
    }
    found.is_dir().then(|| (found.to_path_buf(), "found"))
}

pub fn engine_dir(workdir: Option<&Path>, key: &str, name: &str) -> PathBuf {
    workdir
        .and_then(|w| toml_path(&w.join("victory.toml"), key))
        .unwrap_or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join("victory")
                .join(name)
        })
}

pub fn toml_path(file: &Path, key: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut in_paths = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_paths = line == "[paths]";
            continue;
        }
        if !in_paths {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let value = if let Some(s) = v.strip_prefix('\'') {
            s.split('\'').next()?.to_string()
        } else if let Some(s) = v.strip_prefix('"') {
            s.split('"').next()?.replace("\\\\", "\\")
        } else {
            continue;
        };
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    None
}

#[derive(Clone, Debug, Serialize)]
pub struct Folder {
    pub path: String,
    pub how: &'static str,
}

pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

pub fn default_of(name: &str, _work: Work) -> Option<&'static str> {
    Some(match name {
        "VICTORY_OUTPUT" => "2560x1440",
        "VICTORY_TEXTURE_ROOM_PERCENT" => "95",
        "VICTORY_TEXTURE_POOL_SPARES" => "8",
        "VICTORY_TEXTURE_PARTIAL_FRAMES" => "6",
        "VICTORY_SUBMIT_FRAMES" => "2",
        "VICTORY_CARD_BUFFERS_MB" => "1536",
        "VICTORY_CARD_WINDOWS_MB" => "1024",
        "VICTORY_PIPELINE_WAIT_MS" => "24",
        "VICTORY_MOUSE_SENSITIVITY" => "1.00",
        _ => return None,
    })
}

/// Discovers `victory_dlss.dll` next to the engine binary or in `D:\dev\Vertex-AXX\bin`.
pub fn find_dlss_plugin(victory_exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = victory_exe.and_then(Path::parent) {
        let beside = dir.join("victory_dlss.dll");
        if beside.is_file() {
            return Some(beside);
        }
    }
    let fallback = Path::new(r"D:\dev\Vertex-AXX\bin\victory_dlss.dll");
    fallback.is_file().then(|| fallback.to_path_buf())
}

impl Profile {
    /// Fingerprint of ONLY restart-required settings (excludes live settings
    /// like controls, mouse sensitivity/invert, hotkey, launcher audio/log).
    pub fn restart_signature(&self) -> String {
        serde_json::json!({
            "victory_exe": self.victory_exe,
            "game_exe": self.game_exe,
            "console": self.console,
            "preset": self.preset,
            "output_resolution": self.output_resolution,
            "fullscreen": self.fullscreen,
            "window_size": self.window_size,
            "perf_panel": self.perf_panel,
            "dlss": self.dlss,
            "lod_bias": self.lod_bias,
            "native_controls": self.native_controls,
            "reflections": self.reflections,
            "texture_room_percent": self.texture_room_percent,
            "texture_pool_spares": self.texture_pool_spares,
            "texture_partial_frames": self.texture_partial_frames,
            "honest_title_memory": self.honest_title_memory,
            "wide_copy": self.wide_copy,
            "port_journal": self.port_journal,
            "submit_frames": self.submit_frames,
            "card_buffers_mb": self.card_buffers_mb,
            "card_windows_mb": self.card_windows_mb,
            "pipeline_wait_ms": self.pipeline_wait_ms,
            "cache_dir": self.cache_dir,
            "data_dir": self.data_dir,
            "dlss_sdk_dir": self.dlss_sdk_dir,
            "extra_env": self.extra_env,
        })
        .to_string()
    }

    /// Complete Vertex-AXX environment map for launching or warming pipelines.
    pub fn env_for(
        &self,
        victory_exe: Option<&Path>,
        data_dir: Option<&Path>,
        cpu_threads: u32,
    ) -> Vec<(String, String)> {
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        let mut put = |k: &str, v: &str| {
            map.insert(k.to_string(), v.to_string());
        };

        // --- AnyXboxX / Vice-Tay measured NVIDIA baseline ---
        put("VICTORY_DISCORD", "0");
        put("VICTORY_TEXTURE_NO_WAIT", "1");
        put("VICTORY_RELAY", "2");
        put("VICTORY_WIDE_BACK", "1");
        put("VICTORY_CARD_REHASH_RUNS", "1");
        put("VICTORY_JOIN_COPIES", "1");
        put("VICTORY_INDIRECT_WINDOW", "1");
        put("VICTORY_RELAY_COPY", "1");
        put("VICTORY_CARD_STEPS", "2");
        put("VICTORY_TITLE_PRIORITIES", "0");
        put("VICTORY_SHOW_AFTER", "1");
        put("VICTORY_AMBIENT", "0.8");
        put("VICTORY_D3D12_DRED", "light");
        put("VICTORY_THIN", "1");
        put("VICTORY_JOIN_PARTS", "8");
        put("VICTORY_UPLOAD_AHEAD", "1");
        put("VICTORY_INDIRECT_BATCH", "2");
        put("VICTORY_PLACED_ORDER", "1");
        put("VICTORY_PIPELINE_BOOK", "1");
        put("VICTORY_CARD_RESERVE_BUDGETS", "1");
        put("VICTORY_CARD_BUFFERS_MIN_KB", "64");
        put(
            "VICTORY_WAVE_WIDE_ONLY",
            "6f225b641c5b0708,5dc3b25986622949,369cbe906ea72764,2cc4c197ee29e291",
        );

        // --- Thread pools scaled to CPU (9950X3D 32 threads -> 12/8/8/8; AnyXboxX #469: 8 landing threads
        // on 24+ logical CPUs, heavy scenes 17.1 -> 22.0 fps) ---
        let (thin, under, land, pipe) = if cpu_threads >= 24 {
            ("12", "8", "8", "8")
        } else if cpu_threads >= 16 {
            ("10", "6", "5", "6")
        } else {
            ("6", "4", "3", "4")
        };
        put("VICTORY_THIN_THREADS", thin);
        put("VICTORY_UNDER_THREADS", under);
        put("VICTORY_LAND_THREADS", land);
        put("VICTORY_MAP_THREADS", "auto");
        put("VICTORY_PIPELINE_WORKERS", pipe);

        // --- Resolution, Fullscreen, Panel ---
        let output = if OUTPUT_RESOLUTIONS.contains(&self.output_resolution.as_str()) {
            self.output_resolution.as_str()
        } else {
            "2560x1440"
        };
        put("VICTORY_OUTPUT", output);
        if self.fullscreen {
            put("VICTORY_FULLSCREEN", "1");
        }
        if self.perf_panel {
            put("VICTORY_PANEL", "1");
        }

        // --- DLSS Super Resolution & Texture Sharpness (LOD Bias) ---
        if self.dlss != DlssMode::Off {
            if let Some(plugin) = find_dlss_plugin(victory_exe) {
                let s = plugin.display().to_string();
                put("VICTORY_DLSS_PLUGIN", &s);
            }
            let sdk = self.dlss_sdk_dir.trim();
            if !sdk.is_empty() && Path::new(sdk).is_dir() {
                put("VICTORY_DLSS_SDK", sdk);
            }
            if self.console == Console::SeriesX {
                put("VICTORY_UPSCALER", "7");
                if let Some(scale) = self.dlss.ffx_scale() {
                    put("VICTORY_FFX_SCALE", scale);
                }
            }
            if let Some(bias) = self.lod_bias {
                let s = format!("{:.2}", bias.clamp(-3.0, 2.0));
                put("VICTORY_LOD_BIAS", &s);
            } else if let Some(def_bias) = self.dlss.default_lod_bias() {
                put("VICTORY_LOD_BIAS", def_bias);
            }
        } else if let Some(bias) = self.lod_bias {
            let s = format!("{:.2}", bias.clamp(-3.0, 2.0));
            put("VICTORY_LOD_BIAS", &s);
        }

        // --- Texture Streaming & CPU/GPU Bottleneck Fixes ---
        let room_s = self.texture_room_percent.clamp(50, 97).to_string();
        put("VICTORY_TEXTURE_ROOM_PERCENT", &room_s);
        let spares_s = self.texture_pool_spares.clamp(2, 64).to_string();
        put("VICTORY_TEXTURE_POOL_SPARES", &spares_s);
        let partial_s = self.texture_partial_frames.clamp(1, 60).to_string();
        put("VICTORY_TEXTURE_PARTIAL_FRAMES", &partial_s);
        let bufs_s = self.card_buffers_mb.min(8192).to_string();
        put("VICTORY_CARD_BUFFERS_MB", &bufs_s);
        let wins_s = self.card_windows_mb.min(8192).to_string();
        put("VICTORY_CARD_WINDOWS_MB", &wins_s);
        if self.honest_title_memory {
            put("VICTORY_TITLE_USED_MB", "title");
        }
        if self.wide_copy {
            put("VICTORY_WIDE_COPY", "1");
        }
        if self.port_journal {
            put("VICTORY_PORT_JOURNAL", "1");
        }
        let sub_s = self.submit_frames.clamp(1, 3).to_string();
        put("VICTORY_SUBMIT_FRAMES", &sub_s);
        if let Some(ms) = self.pipeline_wait_ms {
            let s = ms.min(5_000).to_string();
            put("VICTORY_PIPELINE_WAIT_MS", &s);
        }

        // --- Best of VIctory-NVIDIA, AnyXboxX and Vertex (2026-11-28; any can be turned off in
        // the extra switches, e.g. VICTORY_WC_NO_BACK=0) ---
        // Shader writes into big write-combined copies stay on the card (VNV 50c15a1/2f81d0c,
        // 4070 SUPER: gameplay 38.3 -> 33.3 ms, cutscenes 47.3 -> 36.3; under 1 MB they go back,
        // or cutscenes go near black).
        put("VICTORY_WC_NO_BACK", "1");
        put("VICTORY_WC_NO_BACK_MIN_KB", "1024");
        // Page-mapping copies kept in the books until the title's CPU touches them: no freezes at
        // scene cuts (VNV fb838e8: 20 s of map and 33 s of unmap calls in 143 s before).
        put("VICTORY_SOFT_VIEWS", "copies");
        // No 64 MiB blocks made ahead: 768 MiB of the card back for textures (VNV).
        put("VICTORY_RIPE_BLOCKS", "0");
        // The texture pass stops at 16 ms a frame and does not catch up (a pass could hold a
        // frame for up to 750 ms), as VNV runs it.
        put("VICTORY_TEXTURE_STAGE_MS", "16");
        put("VICTORY_TEXTURE_CATCHUP", "0");
        put("VICTORY_CARD_AWAKE", "1");
        put("VICTORY_SYNC_FINE", "1");
        // The heat-haze pass smears blocks of pixels (water too) and spoils the motion vectors
        // DLSS reads (VNV 672442b).
        put("VICTORY_LEAVE_OUT", "731a39171b568298");
        // Dynamic descriptor slots on NVIDIA's capped heap: this base dropped ~115 draws a frame
        // for want of them (Vertex 5a291f4d; 393216 first, 524288 if the log still says refused).
        put("VICTORY_DYNAMIC_SLOTS", "393216");
        // Floors, glass and eyes without the frosty sky glare (Whiplash floor fix): reflections held
        // to half the light around them, after the screen-space resolve (needs VICTORY_AMBIENT).
        if !self.reflections {
            put("VICTORY_REFLECT_SCREEN", "1");
            put("VICTORY_AMBIENT_REFLECT", "0.5");
        }

        // --- Native KB/M Controls ---
        if self.native_controls {
            put("VICTORY_KEYBOARD_EVENTS", "1");
            put("VICTORY_NATIVE_INPUT", "1");
            if let Some(dir) = data_dir {
                let s = crate::native_keys::native_keys_path(dir)
                    .display()
                    .to_string();
                put("VICTORY_NATIVE_KEYS", &s);
            }
        }

        // --- Experimental Reflections ---
        if self.reflections {
            put("VICTORY_BVH_MISS", "1");
            put("VICTORY_RT_RESOLVE", "1");
            put("VICTORY_ACTIVATE_RAYS", "1");
        }

        if let Some(s) = self.mouse_sensitivity
            && s.is_finite()
            && s > 0.0
        {
            let v = format!("{:.2}", s.clamp(0.1, 10.0));
            put("VICTORY_MOUSE_SENSITIVITY", &v);
        }

        if let Some((dir, _)) = self.cache_override() {
            let s = dir.display().to_string();
            put("VICTORY_CACHE_DIR", &s);
        }
        if let Some((dir, _)) = self.data_override() {
            let s = dir.display().to_string();
            put("VICTORY_DATA_DIR", &s);
        }

        // User-supplied extra switches override everything above.
        for (name, value) in &self.extra_env {
            put(name, value);
        }

        map.into_iter().collect()
    }

    /// Compatibility helper used by tests & CLI.
    #[allow(dead_code)]
    pub fn env(&self) -> Vec<(&'static str, Option<String>)> {
        let built: BTreeMap<String, String> = self.env_for(None, None, 16).into_iter().collect();
        MANAGED
            .iter()
            .map(|&name| (name, built.get(name).cloned()))
            .collect()
    }

    pub fn extra(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extra_env
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
    }

    pub fn set_env(&mut self, name: &str, value: &str) -> Result<(), String> {
        let name = name.trim();
        let value = value.trim();
        let clear = value.is_empty() || value.eq_ignore_ascii_case("default");
        if name == "VICTORY_GAME_DIR" {
            return Err("VICTORY_GAME_DIR comes from the game's path: use --game".into());
        }
        if !ENGINE_SWITCHES.contains(&name) && !MANAGED.contains(&name) {
            return Err(format!("{name} is not a switch the engine reads"));
        }
        let bad = || format!("{name}={value}: not a value the engine takes");
        let flag = || -> Result<bool, String> {
            match value.to_ascii_lowercase().as_str() {
                "1" | "on" | "true" | "yes" => Ok(true),
                "0" | "off" | "false" | "no" => Ok(false),
                _ => Err(bad()),
            }
        };
        match name {
            "VICTORY_OUTPUT" => {
                if clear {
                    self.output_resolution = "2560x1440".into();
                } else if OUTPUT_RESOLUTIONS.contains(&value) {
                    self.output_resolution = value.into();
                } else {
                    return Err(bad());
                }
            }
            "VICTORY_FULLSCREEN" => self.fullscreen = clear || flag()?,
            "VICTORY_PANEL" => self.perf_panel = clear || flag()?,
            "VICTORY_TEXTURE_ROOM_PERCENT" => {
                self.texture_room_percent = if clear {
                    95
                } else {
                    value.parse::<u32>().map_err(|_| bad())?.clamp(50, 97)
                };
            }
            "VICTORY_TEXTURE_POOL_SPARES" => {
                self.texture_pool_spares = if clear {
                    8
                } else {
                    value.parse::<u32>().map_err(|_| bad())?.clamp(2, 64)
                };
            }
            "VICTORY_TEXTURE_PARTIAL_FRAMES" => {
                self.texture_partial_frames = if clear {
                    6
                } else {
                    value.parse::<u32>().map_err(|_| bad())?.clamp(1, 60)
                };
            }
            "VICTORY_TITLE_USED_MB" => {
                self.honest_title_memory = clear || value.eq_ignore_ascii_case("title");
            }
            "VICTORY_WIDE_COPY" => self.wide_copy = clear || flag()?,
            "VICTORY_PORT_JOURNAL" => self.port_journal = clear || flag()?,
            "VICTORY_SUBMIT_FRAMES" => {
                self.submit_frames = if clear {
                    2
                } else {
                    value.parse::<u32>().map_err(|_| bad())?.clamp(1, 3)
                };
            }
            "VICTORY_CARD_BUFFERS_MB" => {
                self.card_buffers_mb = if clear {
                    1536
                } else {
                    value.parse::<u64>().map_err(|_| bad())?.min(8192)
                };
            }
            "VICTORY_CARD_WINDOWS_MB" => {
                self.card_windows_mb = if clear {
                    1024
                } else {
                    value.parse::<u64>().map_err(|_| bad())?.min(8192)
                };
            }
            "VICTORY_PIPELINE_WAIT_MS" => {
                self.pipeline_wait_ms = if clear {
                    None
                } else {
                    Some(value.parse::<u64>().map_err(|_| bad())?.min(5000))
                };
            }
            "VICTORY_LOD_BIAS" => {
                self.lod_bias = if clear {
                    None
                } else {
                    let v: f32 = value.parse().map_err(|_| bad())?;
                    if !v.is_finite() {
                        return Err(bad());
                    }
                    Some(v.clamp(-3.0, 2.0))
                };
            }
            "VICTORY_MOUSE_SENSITIVITY" => {
                self.mouse_sensitivity = if clear {
                    None
                } else {
                    let v: f32 = value.parse().map_err(|_| bad())?;
                    if !(v.is_finite() && v > 0.0) {
                        return Err(bad());
                    }
                    Some(v.clamp(0.1, 10.0))
                };
            }
            "VICTORY_CACHE_DIR" => {
                self.cache_dir = if clear { String::new() } else { value.into() };
            }
            "VICTORY_DATA_DIR" => {
                self.data_dir = if clear { String::new() } else { value.into() };
            }
            _ => {
                if clear {
                    self.extra_env.remove(name);
                } else {
                    self.extra_env.insert(name.to_string(), value.to_string());
                }
            }
        }
        Ok(())
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() || key == "version" {
            return Err(format!("{key}: not a setting that can be set"));
        }
        let mut doc = serde_json::to_value(&*self).map_err(|e| e.to_string())?;
        let mut slot = &mut doc;
        for part in key.split('.') {
            slot = slot
                .as_object_mut()
                .and_then(|o| o.get_mut(part))
                .ok_or_else(|| format!("{key} is not a launcher setting"))?;
        }
        let parsed = serde_json::from_str(value.trim())
            .unwrap_or_else(|_| serde_json::Value::String(value.trim().to_string()));
        *slot = parsed;
        let next: Profile =
            serde_json::from_value(doc).map_err(|e| format!("{key}={value}: {e}"))?;
        *self = next;
        Ok(())
    }

    pub fn cache_override(&self) -> Option<(PathBuf, &'static str)> {
        pick_dir(&self.cache_dir, &Path::new(FOUND_ROOT).join("cache"))
    }

    pub fn data_override(&self) -> Option<(PathBuf, &'static str)> {
        pick_dir(&self.data_dir, &Path::new(FOUND_ROOT).join("data"))
    }

    pub fn cache_folder(&self, workdir: Option<&Path>) -> Folder {
        let (path, how) = self
            .cache_override()
            .unwrap_or_else(|| (engine_dir(workdir, "cache_dir", "cache"), "engine"));
        Folder {
            path: path.display().to_string(),
            how,
        }
    }

    pub fn data_folder(&self, workdir: Option<&Path>) -> Folder {
        let (path, how) = self
            .data_override()
            .unwrap_or_else(|| (engine_dir(workdir, "data_dir", "data"), "engine"));
        Folder {
            path: path.display().to_string(),
            how,
        }
    }

    /// Migrates v1/v2 profiles cleanly to v3 (Vertex-AXX defaults).
    pub fn upgrade(&mut self) -> bool {
        if self.version >= PROFILE_VERSION {
            return false;
        }
        let keep_game = self.game_exe.clone();
        let keep_sens = self.mouse_sensitivity;
        let keep_vol = self.music_volume;
        let keep_muted = self.muted;
        let keep_lossless = self.lossless.clone();
        *self = Profile::default();
        if !keep_game.trim().is_empty() && Path::new(&keep_game).is_file() {
            self.game_exe = keep_game;
        }
        self.mouse_sensitivity = keep_sens.or(Some(1.0));
        self.music_volume = keep_vol;
        self.muted = keep_muted;
        self.lossless = keep_lossless;
        self.lossless.start_with_game = false;
        self.version = PROFILE_VERSION;
        true
    }

    /// Arguments for `vertex-axx.exe run` (exact Vice-Tay/AnyXboxX CLI).
    pub fn run_args(&self, game: &Path) -> Vec<String> {
        vec![
            "run".into(),
            game.display().to_string(),
            "--execute".into(),
            "--console".into(),
            self.console.arg().into(),
        ]
    }

    /// Arguments for `vertex-axx.exe warm` (pre-builds recorded host pipelines).
    pub fn prebuild_args(&self, _game: &Path) -> Vec<String> {
        let mut args = vec!["warm".into()];
        if !self.prebuild_all {
            args.push("--used".into());
        }
        args
    }

    /// Arguments for `vertex-axx.exe shaders <game_dir>` (pre-translates all shaders on disk).
    pub fn shader_prebuild_args(&self, game: &Path) -> Vec<String> {
        let dir = game.parent().unwrap_or(game);
        vec!["shaders".into(), dir.display().to_string()]
    }

    pub fn reset(&mut self) {
        let keep = self.clone();
        *self = Profile {
            victory_exe: keep.victory_exe,
            game_exe: keep.game_exe,
            lossless: keep.lossless,
            cache_dir: keep.cache_dir,
            data_dir: keep.data_dir,
            dlss_sdk_dir: keep.dlss_sdk_dir,
            ingame_hotkey: keep.ingame_hotkey,
            music_volume: keep.music_volume,
            muted: keep.muted,
            show_log: keep.show_log,
            best_seen: keep.best_seen,
            ..Profile::default()
        };
    }

    /// Applies one of the 3 real, measured Vertex-AXX modes.
    pub fn apply_preset(&mut self, preset: Preset) {
        self.preset = preset;
        // Common proven bottleneck fixes across all modes:
        self.honest_title_memory = true;
        self.wide_copy = true;
        self.port_journal = true;
        self.submit_frames = 2;
        self.reflections = false;
        self.extra_env.clear();
        match preset {
            Preset::Performance => {
                // Highest FPS (~25-30 fps): 1080p output + DLSS Balanced + fast texture budget.
                self.console = Console::SeriesX;
                self.output_resolution = "1920x1080".into();
                self.dlss = DlssMode::Balanced;
                self.lod_bias = Some(-0.79);
                self.texture_room_percent = 94;
                self.texture_pool_spares = 6;
                self.texture_partial_frames = 10;
                self.card_buffers_mb = 1536;
                self.card_windows_mb = 1024;
            }
            Preset::Balanced => {
                // Best all-round for RTX 5070 12 GB + 1440p (~20-25+ fps): native 1440p + DLSS Quality + sharp textures.
                self.console = Console::SeriesX;
                self.output_resolution = "2560x1440".into();
                self.dlss = DlssMode::Quality;
                self.lod_bias = Some(-0.75);
                self.texture_room_percent = 95;
                self.texture_pool_spares = 8;
                self.texture_partial_frames = 6;
                self.card_buffers_mb = 1536;
                self.card_windows_mb = 1024;
            }
            Preset::HighQuality | Preset::Native4k => {
                // Sharpest picture & textures: DLAA (native 1.0x render scale) + aggressive mip sharpening.
                // 4K Native is the same at 3840x2160: the engine grows the title's render target heaps
                // and pool for a render size past the console's 2560x1440 (resolution.rs).
                self.console = Console::SeriesX;
                self.output_resolution = if preset == Preset::Native4k { "3840x2160" } else { "2560x1440" }.into();
                self.dlss = DlssMode::Dlaa;
                self.lod_bias = Some(-0.50);
                self.texture_room_percent = 96;
                self.texture_pool_spares = 8;
                self.texture_partial_frames = 4;
                self.card_buffers_mb = 1536;
                self.card_windows_mb = 1024;
            }
        }
    }
}

pub fn launcher_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Vertex")
}

pub fn profile_path() -> PathBuf {
    launcher_dir().join("launcher.json")
}

pub fn load() -> Profile {
    let mut p: Profile = std::fs::read_to_string(profile_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if p.upgrade() {
        let _ = save(&p);
    }
    p
}

pub fn save(profile: &Profile) -> Result<(), String> {
    let dir = launcher_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    let path = profile_path();
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(p: &Profile, name: &str) -> Option<String> {
        p.env_for(None, None, 32)
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    #[test]
    fn presets_produce_distinct_working_configs() {
        let mut p = Profile::default();
        p.apply_preset(Preset::Performance);
        assert_eq!(val(&p, "VICTORY_OUTPUT").as_deref(), Some("1920x1080"));
        assert_eq!(val(&p, "VICTORY_FFX_SCALE").as_deref(), Some("0.58"));
        assert_eq!(val(&p, "VICTORY_SUBMIT_FRAMES").as_deref(), Some("2"));
        assert_eq!(val(&p, "VICTORY_TITLE_USED_MB").as_deref(), Some("title"));
        assert_eq!(val(&p, "VICTORY_WIDE_COPY").as_deref(), Some("1"));
        assert_eq!(val(&p, "VICTORY_PORT_JOURNAL").as_deref(), Some("1"));

        p.apply_preset(Preset::Balanced);
        assert_eq!(val(&p, "VICTORY_OUTPUT").as_deref(), Some("2560x1440"));
        assert_eq!(val(&p, "VICTORY_FFX_SCALE").as_deref(), Some("0.6667"));

        p.apply_preset(Preset::HighQuality);
        assert_eq!(val(&p, "VICTORY_OUTPUT").as_deref(), Some("2560x1440"));
        assert_eq!(val(&p, "VICTORY_FFX_SCALE").as_deref(), Some("1.0"));
        assert_eq!(val(&p, "VICTORY_TEXTURE_PARTIAL_FRAMES").as_deref(), Some("4"));

        p.apply_preset(Preset::Native4k);
        assert_eq!(val(&p, "VICTORY_OUTPUT").as_deref(), Some("3840x2160"));
        assert_eq!(val(&p, "VICTORY_FFX_SCALE").as_deref(), Some("1.0"));
        assert_eq!(val(&p, "VICTORY_UPSCALER").as_deref(), Some("7"));
        assert_eq!(val(&p, "VICTORY_LOD_BIAS").as_deref(), Some("-0.50"));
        assert_eq!(val(&p, "VICTORY_TEXTURE_ROOM_PERCENT").as_deref(), Some("96"));
        assert_eq!(serde_json::to_string(&Preset::Native4k).unwrap(), "\"native-4k\"");
    }

    #[test]
    fn restart_signature_ignores_live_settings() {
        let mut p = Profile::default();
        let before = p.restart_signature();
        p.mouse_sensitivity = Some(2.5);
        p.mouse_invert_y = true;
        p.ingame_hotkey = "Win+F1".into();
        p.muted = true;
        assert_eq!(before, p.restart_signature());

        p.output_resolution = "1920x1080".into();
        assert_ne!(before, p.restart_signature());
    }

    #[test]
    fn run_and_warm_args_match_vertex_axx_cli() {
        let p = Profile::default();
        let game = Path::new(r"D:\V6\payload\V6\master.exe");
        assert_eq!(
            p.run_args(game),
            ["run", r"D:\V6\payload\V6\master.exe", "--execute", "--console", "series-x"]
        );
        assert_eq!(p.prebuild_args(game), ["warm", "--used"]);
    }
}
