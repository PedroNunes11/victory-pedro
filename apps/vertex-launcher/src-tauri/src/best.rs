//! Best measured settings (`profiles/best.json` or `%LOCALAPPDATA%\Vertex\best.json`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::{Console, Preset, Profile, Work};
use crate::system::SystemInfo;

const BUILT_IN: &str = include_str!("../../profiles/best.json");

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Match {
    pub gpu: Option<String>,
    pub vram_gb: Option<u64>,
    pub ram_gb: Option<u64>,
    pub cpu: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BestLossless {
    pub multiplier: Option<u32>,
    pub flow_scale: Option<u32>,
    pub max_frame_latency: Option<u32>,
    pub auto_scale: Option<bool>,
    pub auto_scale_delay: Option<u32>,
    pub draw_fps: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub run: u32,
    pub fps_median: f32,
    pub pc: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub pc: String,
    #[serde(rename = "match", default)]
    pub matches: Match,
    pub console: Console,
    #[serde(default)]
    pub work: Work,
    #[serde(default)]
    pub preset: Preset,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub cache_dir: Option<String>,
    pub data_dir: Option<String>,
    #[serde(default)]
    pub lossless: BestLossless,
    pub measured: Measured,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BestFile {
    pub updated: String,
    pub entries: Vec<Entry>,
}

pub fn parse(text: &str) -> Result<BestFile, String> {
    let file: BestFile = serde_json::from_str(text).map_err(|e| format!("best.json: {e}"))?;
    if file.entries.is_empty() {
        return Err("best.json: no entries".into());
    }
    for e in &file.entries {
        apply(&mut Profile::default(), e).map_err(|err| format!("best.json, {}: {err}", e.id))?;
    }
    Ok(file)
}

pub fn override_path() -> PathBuf {
    crate::settings::launcher_dir().join("best.json")
}

#[derive(Clone, Debug, Serialize)]
pub struct Loaded {
    pub file: BestFile,
    pub source: &'static str,
    pub problem: Option<String>,
}

pub fn load() -> Loaded {
    load_from(&override_path())
}

fn load_from(path: &Path) -> Loaded {
    let built_in = || parse(BUILT_IN).expect("the built-in best.json is checked by a test");
    match std::fs::read_to_string(path) {
        Err(_) => Loaded {
            file: built_in(),
            source: "built-in",
            problem: None,
        },
        Ok(text) => match parse(&text) {
            Ok(file) => Loaded {
                file,
                source: "override",
                problem: None,
            },
            Err(e) => Loaded {
                file: built_in(),
                source: "built-in",
                problem: Some(format!("{}: {e}", path.display())),
            },
        },
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Pick {
    pub entry: Entry,
    pub exact: bool,
}

fn has(text: &str, part: &Option<String>) -> bool {
    part.as_ref()
        .is_none_or(|p| text.to_ascii_lowercase().contains(&p.to_ascii_lowercase()))
}

pub fn pick(file: &BestFile, info: &SystemInfo) -> Pick {
    let gpu_name = info.gpu.as_ref().map_or("", |g| g.name.as_str());
    let vram = info
        .gpu
        .as_ref()
        .map_or(0, |g| crate::tune::nominal_gb(g.vram_mb));
    let ram = crate::tune::nominal_gb(info.ram_mb);
    let exact = |m: &Match| {
        has(gpu_name, &m.gpu)
            && has(&info.cpu_name, &m.cpu)
            && m.vram_gb.is_none_or(|v| v == vram)
            && m.ram_gb.is_none_or(|r| r.abs_diff(ram) <= 2)
    };
    if let Some(e) = file.entries.iter().find(|e| exact(&e.matches)) {
        return Pick {
            entry: e.clone(),
            exact: true,
        };
    }
    let distance = |m: &Match| {
        m.vram_gb.map_or(50, |v| v.abs_diff(vram) * 10) + m.ram_gb.map_or(20, |r| r.abs_diff(ram))
    };
    let entry = file
        .entries
        .iter()
        .min_by_key(|e| distance(&e.matches))
        .expect("parse refuses a file with no entries")
        .clone();
    Pick {
        entry,
        exact: false,
    }
}

pub fn apply(profile: &mut Profile, entry: &Entry) -> Result<(), String> {
    let mut p = profile.clone();
    let keep = (
        p.window_size.clone(),
        p.mouse_sensitivity,
        p.mouse_invert_y,
        p.ingame_hotkey.clone(),
        p.prebuild_all,
        p.lossless.start_with_game,
    );
    p.reset();
    (
        p.window_size,
        p.mouse_sensitivity,
        p.mouse_invert_y,
        p.ingame_hotkey,
        p.prebuild_all,
        p.lossless.start_with_game,
    ) = keep;
    p.apply_preset(entry.preset);
    p.console = entry.console;
    p.work = entry.work;
    for (name, value) in &entry.env {
        p.set_env(name, value)?;
    }
    for (dir, slot) in [
        (&entry.cache_dir, &mut p.cache_dir),
        (&entry.data_dir, &mut p.data_dir),
    ] {
        if let Some(d) = dir.as_ref().filter(|d| Path::new(d).is_dir()) {
            *slot = d.clone();
        }
    }
    let l = &entry.lossless;
    let ll = &mut p.lossless;
    ll.multiplier = l.multiplier.unwrap_or(ll.multiplier);
    ll.flow_scale = l.flow_scale.unwrap_or(ll.flow_scale);
    ll.max_frame_latency = l.max_frame_latency.unwrap_or(ll.max_frame_latency);
    ll.auto_scale = l.auto_scale.unwrap_or(ll.auto_scale);
    ll.auto_scale_delay = l.auto_scale_delay.unwrap_or(ll.auto_scale_delay);
    ll.draw_fps = l.draw_fps.unwrap_or(ll.draw_fps);
    p.lossless = p.lossless.clamped();
    *profile = p;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Adapter;

    fn pc(gpu: &str, vram_mb: u64, ram_mb: u64, cpu: &str) -> SystemInfo {
        SystemInfo {
            adapters: Vec::new(),
            gpu: Some(Adapter {
                name: gpu.into(),
                vendor_id: 0x10de,
                vram_mb,
                shared_mb: 0,
                driver: String::new(),
                driver_nvidia: None,
                tiled_tier: Some(3),
                software: false,
            }),
            ram_mb,
            cpu_name: cpu.into(),
            cpu_threads: 32,
            avx2: true,
            display_width: 2560,
            display_height: 1440,
            display_hz: 144,
        }
    }

    #[test]
    fn built_in_best_matches_rtx5070_9950x3d() {
        let file = parse(BUILT_IN).unwrap();
        let p = pick(
            &file,
            &pc(
                "NVIDIA GeForce RTX 5070",
                12_078,
                32_000,
                "AMD Ryzen 9 9950X3D 16-Core Processor",
            ),
        );
        assert!(p.exact);
        assert_eq!(p.entry.id, "rtx5070-12gb-9950x3d");
        let mut prof = Profile::default();
        apply(&mut prof, &p.entry).unwrap();
        assert_eq!(prof.output_resolution, "2560x1440");
        assert!(prof.honest_title_memory);
        assert_eq!(prof.submit_frames, 2);
    }
}
