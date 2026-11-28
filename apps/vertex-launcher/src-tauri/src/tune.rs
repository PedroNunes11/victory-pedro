//! Hardware-aware Auto Tune for Vertex: picks real resolution, render scale,
//! DLSS mode, VRAM pool limits, and texture sharpness based on the detected
//! GPU, VRAM, RAM, CPU threads, and monitor resolution.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::settings::{Console, DlssMode, Preset, Profile};
use crate::system::SystemInfo;

#[derive(Clone, Debug, Serialize)]
pub struct Reason {
    pub code: &'static str,
    pub args: Map<String, Value>,
}

fn reason(code: &'static str, args: &[(&str, String)]) -> Reason {
    Reason {
        code,
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String(v.clone())))
            .collect(),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Tune {
    pub basis: &'static str,
    pub nearest: &'static str,
    pub profile: Profile,
    pub picks: Vec<Reason>,
    pub warnings: Vec<Reason>,
    pub notes: Vec<Reason>,
}

pub fn nominal_gb(mb: u64) -> u64 {
    (mb + 512) / 1024
}

pub fn tune(info: &SystemInfo, current: &Profile) -> Tune {
    let mut picks = Vec::new();
    let mut warnings = Vec::new();
    let mut notes = Vec::new();
    let mut profile = current.clone();

    let gpu = info.gpu.as_ref();
    let name = gpu.map(|g| g.name.clone()).unwrap_or_default();
    let upper = name.to_ascii_uppercase();
    let vram = gpu.map_or(0, |g| nominal_gb(g.vram_mb));
    let vendor = gpu.map_or(0, |g| g.vendor_id);
    let is_nvidia = vendor == 0x10de;

    let (basis, nearest) = if upper.contains("RTX 5070") && !upper.contains("TI") && vram == 12 {
        ("measured", "a")
    } else if is_nvidia {
        ("guess", "a")
    } else {
        ("guess", "b")
    };

    if gpu.is_none() {
        warnings.push(reason("no_gpu", &[]));
    }

    // Choose base mode from VRAM + monitor size.
    if gpu.is_some() && vram < 10 {
        profile.apply_preset(Preset::Performance);
        profile.console = if vram < 8 {
            Console::SeriesS
        } else {
            Console::SeriesX
        };
        profile.texture_room_percent = 85;
        profile.texture_pool_spares = 4;
        picks.push(reason(
            "mode_performance",
            &[("vram", vram.to_string())],
        ));
    } else if vram >= 16 {
        profile.apply_preset(Preset::HighQuality);
        profile.output_resolution = if info.display_width >= 3840 {
            "3840x2160".into()
        } else {
            "2560x1440".into()
        };
        profile.texture_pool_spares = 16;
        picks.push(reason(
            "mode_high",
            &[
                ("vram", vram.to_string()),
                ("res", profile.output_resolution.clone()),
            ],
        ));
    } else {
        profile.apply_preset(Preset::Balanced);
        profile.output_resolution = if info.display_width >= 2560 {
            "2560x1440".into()
        } else {
            "1920x1080".into()
        };
        picks.push(reason(
            "mode_balanced",
            &[
                ("vram", vram.to_string()),
                ("res", profile.output_resolution.clone()),
            ],
        ));
    }

    if is_nvidia {
        picks.push(reason("dlss_on", &[]));
    } else {
        profile.dlss = DlssMode::Off;
        notes.push(reason("dlss_non_nvidia", &[]));
    }
    picks.push(reason("streaming_fixes", &[]));

    let ram = nominal_gb(info.ram_mb);
    if info.ram_mb > 0 && info.ram_mb < 30 * 1024 {
        warnings.push(reason("ram_low", &[("ram", ram.to_string())]));
    }
    if info.cpu_threads < 12 {
        warnings.push(reason(
            "threads_low",
            &[("threads", info.cpu_threads.to_string())],
        ));
    }
    if !info.avx2 {
        warnings.push(reason("no_avx2", &[]));
    }
    if let Some(g) = gpu {
        let driver = g.driver_nvidia.clone().unwrap_or_else(|| g.driver.clone());
        notes.push(reason("driver", &[("driver", driver)]));
    }
    notes.push(reason("warm_recommended", &[]));

    Tune {
        basis,
        nearest,
        profile,
        picks,
        warnings,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::Adapter;

    fn pc(name: &str, vendor: u32, vram_mb: u64, ram_mb: u64, threads: u32) -> SystemInfo {
        let gpu = Adapter {
            name: name.into(),
            vendor_id: vendor,
            vram_mb,
            shared_mb: 0,
            driver: "32.0.15.8157".into(),
            driver_nvidia: Some("621.40".into()),
            tiled_tier: Some(3),
            software: false,
        };
        SystemInfo {
            adapters: vec![gpu.clone()],
            gpu: Some(gpu),
            ram_mb,
            cpu_name: "AMD Ryzen 9 9950X3D".into(),
            cpu_threads: threads,
            avx2: true,
            display_width: 2560,
            display_height: 1440,
            display_hz: 144,
        }
    }

    #[test]
    fn rtx5070_gets_balanced_1440p_dlss_quality() {
        let t = tune(
            &pc("NVIDIA GeForce RTX 5070", 0x10de, 12_050, 32_000, 32),
            &Profile::default(),
        );
        assert_eq!(t.basis, "measured");
        assert_eq!(t.profile.preset, Preset::Balanced);
        assert_eq!(t.profile.output_resolution, "2560x1440");
        assert_eq!(t.profile.dlss, DlssMode::Quality);
        assert!(t.profile.honest_title_memory);
        assert_eq!(t.profile.submit_frames, 2);
    }
}
