//! The launcher's command line, so another tool can open it with settings
//! applied:
//!
//! ```text
//! vertex-launcher.exe [--profile best|<file.json>] [--set name=value ...]
//!     [--env VICTORY_X=value ...] [--console series-x|series-s]
//!     [--work cpu|balanced|gpu] [--cache-dir P] [--data-dir P] [--game P]
//!     [--start] [--prebuild] [--no-music] [--no-save]
//! ```
//!
//! Everything is checked before anything is applied: an unknown option, a
//! switch name the engine does not read or a value it would not take refuses
//! the whole line. A second start while the window is open hands its line to
//! that window (main.rs).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::best::{self, BestFile};
use crate::settings::{Console, Profile, Work};
use crate::system::SystemInfo;

pub const USAGE: &str = "vertex-launcher.exe [--profile best|<file.json>] [--set name=value ...] \
[--env VICTORY_X=value ...] [--console series-x|series-s] [--work cpu|balanced|gpu] \
[--cache-dir P] [--data-dir P] [--game P] [--start] [--prebuild] [--no-music] [--no-save]";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cli {
    /// "best", or a profile or best.json file.
    pub profile: Option<String>,
    /// Launcher settings by their launcher.json name.
    pub sets: Vec<(String, String)>,
    /// Engine switches.
    pub envs: Vec<(String, String)>,
    pub console: Option<Console>,
    pub work: Option<Work>,
    pub cache_dir: Option<String>,
    pub data_dir: Option<String>,
    pub game: Option<String>,
    pub start: bool,
    pub prebuild: bool,
    pub no_music: bool,
    pub no_save: bool,
}

impl Cli {
    /// True if the line changes the profile.
    pub fn changes_profile(&self) -> bool {
        self.profile.is_some()
            || !self.sets.is_empty()
            || !self.envs.is_empty()
            || self.console.is_some()
            || self.work.is_some()
            || self.cache_dir.is_some()
            || self.data_dir.is_some()
            || self.game.is_some()
    }
}

fn pair(text: &str, option: &str) -> Result<(String, String), String> {
    let (k, v) = text
        .split_once('=')
        .ok_or_else(|| format!("{option} {text}: expected name=value"))?;
    if k.trim().is_empty() {
        return Err(format!("{option} {text}: expected name=value"));
    }
    Ok((k.trim().to_string(), v.trim().to_string()))
}

/// Reads the arguments (without the program's own name).
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (option, inline) = match arg.split_once('=') {
            Some((o, v)) if o.starts_with("--") => (o.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => args
                    .next()
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| format!("{name} needs a value")),
            }
        };
        let flag = |name: &str| -> Result<bool, String> {
            if inline.is_some() {
                Err(format!("{name} takes no value"))
            } else {
                Ok(true)
            }
        };
        match option.as_str() {
            "--profile" => cli.profile = Some(value("--profile")?),
            "--set" => cli.sets.push(pair(&value("--set")?, "--set")?),
            "--env" => cli.envs.push(pair(&value("--env")?, "--env")?),
            "--console" => {
                cli.console = Some(match value("--console")?.as_str() {
                    "series-x" => Console::SeriesX,
                    "series-s" => Console::SeriesS,
                    v => return Err(format!("--console {v}: series-x or series-s")),
                })
            }
            "--work" => {
                cli.work = Some(match value("--work")?.as_str() {
                    "cpu" => Work::Cpu,
                    "balanced" => Work::Balanced,
                    "gpu" => Work::Gpu,
                    v => return Err(format!("--work {v}: cpu, balanced or gpu")),
                })
            }
            "--cache-dir" => cli.cache_dir = Some(value("--cache-dir")?),
            "--data-dir" => cli.data_dir = Some(value("--data-dir")?),
            "--game" => cli.game = Some(value("--game")?),
            "--start" => cli.start = flag("--start")?,
            "--prebuild" => cli.prebuild = flag("--prebuild")?,
            "--no-music" => cli.no_music = flag("--no-music")?,
            "--no-save" => cli.no_save = flag("--no-save")?,
            "--help" | "-h" | "/?" => return Err(format!("usage: {USAGE}")),
            other => return Err(format!("unknown option {other}. Usage: {USAGE}")),
        }
    }
    Ok(cli)
}

fn resolve(base: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path.trim());
    if p.is_absolute() { p } else { base.join(p) }
}

/// What a command line did, for the window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Launch {
    pub start: bool,
    pub prebuild: bool,
    pub no_music: bool,
    /// The profile was changed by the line.
    pub applied: bool,
    pub saved: bool,
    /// One line for each thing set.
    pub notes: Vec<String>,
    pub error: Option<String>,
}

/// Applies `cli` to `profile` (all or nothing). Relative paths are taken
/// from `base` (the folder the command was given in). `best` gives the best
/// measured settings and `info` this PC, read only for `--profile`.
pub fn apply(
    cli: &Cli,
    profile: &mut Profile,
    base: &Path,
    best: impl FnOnce() -> BestFile,
    info: impl FnOnce() -> SystemInfo,
) -> Result<Vec<String>, String> {
    let mut p = profile.clone();
    let mut notes = Vec::new();
    if let Some(source) = &cli.profile {
        if source == "best" {
            let pick = best::pick(&best(), &info());
            best::apply(&mut p, &pick.entry)?;
            notes.push(note_best(&pick));
        } else {
            let path = resolve(base, source);
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let json: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if json.get("entries").is_some() {
                let file = best::parse(&text)?;
                let pick = best::pick(&file, &info());
                best::apply(&mut p, &pick.entry)?;
                notes.push(note_best(&pick));
            } else {
                let mut loaded: Profile =
                    serde_json::from_value(json).map_err(|e| format!("{}: {e}", path.display()))?;
                loaded.version = crate::settings::PROFILE_VERSION;
                if loaded.game_exe.trim().is_empty() {
                    loaded.game_exe = p.game_exe.clone();
                }
                if loaded.victory_exe.trim().is_empty() {
                    loaded.victory_exe = p.victory_exe.clone();
                }
                (loaded.music_volume, loaded.muted, loaded.best_seen) =
                    (p.music_volume, p.muted, p.best_seen.clone());
                for (name, _) in loaded.extra() {
                    if !crate::settings::ENGINE_SWITCHES.contains(&name) {
                        return Err(format!(
                            "{}: {name} is not a switch the engine reads",
                            path.display()
                        ));
                    }
                }
                p = loaded;
                notes.push(format!("profile {}", path.display()));
            }
        }
    }
    if let Some(c) = cli.console {
        p.console = c;
        notes.push(format!("console {}", c.arg()));
    }
    if let Some(w) = cli.work {
        p.work = w;
        notes.push(format!("work {}", w.arg()));
    }
    if let Some(d) = &cli.cache_dir {
        p.cache_dir = resolve(base, d).display().to_string();
        notes.push(format!("cache folder {}", p.cache_dir));
    }
    if let Some(d) = &cli.data_dir {
        p.data_dir = resolve(base, d).display().to_string();
        notes.push(format!("data folder {}", p.data_dir));
    }
    if let Some(g) = &cli.game {
        let game = resolve(base, g);
        if !game.is_file() {
            return Err(format!("--game {}: no such file", game.display()));
        }
        p.game_exe = game.display().to_string();
        notes.push(format!("game {}", p.game_exe));
    }
    for (k, v) in &cli.sets {
        p.set_key(k, v)?;
        notes.push(format!("{k}={v}"));
    }
    for (k, v) in &cli.envs {
        p.set_env(k, v)?;
        notes.push(format!("{k}={v}"));
    }
    *profile = p;
    Ok(notes)
}

fn note_best(pick: &best::Pick) -> String {
    let m = &pick.entry.measured;
    format!(
        "best measured settings: {} (run {}, {} fps median){}",
        pick.entry.pc,
        m.run,
        m.fps_median,
        if pick.exact {
            ""
        } else {
            ", a guess for this PC"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    fn no_pc() -> SystemInfo {
        SystemInfo {
            adapters: Vec::new(),
            gpu: None,
            ram_mb: 16_000,
            cpu_name: String::new(),
            cpu_threads: 8,
            avx2: true,
            display_width: 0,
            display_height: 0,
            display_hz: 0,
        }
    }

    #[test]
    fn the_options_are_read() {
        let cli = parse(args(
            "--profile best --set work=gpu --env VICTORY_TEXTURE_ROOM_PERCENT=94 --console=series-s \
             --work balanced --cache-dir C:\\c --data-dir=C:\\d --game D:\\g\\t.exe --start --prebuild --no-music --no-save",
        ))
        .unwrap();
        assert_eq!(cli.profile.as_deref(), Some("best"));
        assert_eq!(cli.sets, [("work".to_string(), "gpu".to_string())]);
        assert_eq!(
            cli.envs,
            [("VICTORY_TEXTURE_ROOM_PERCENT".to_string(), "94".to_string())]
        );
        assert_eq!(cli.console, Some(Console::SeriesS));
        assert_eq!(cli.work, Some(Work::Balanced));
        assert_eq!(cli.cache_dir.as_deref(), Some("C:\\c"));
        assert_eq!(cli.data_dir.as_deref(), Some("C:\\d"));
        assert_eq!(cli.game.as_deref(), Some("D:\\g\\t.exe"));
        assert!(cli.start && cli.prebuild && cli.no_music && cli.no_save);
        assert!(cli.changes_profile());
        let empty = parse(Vec::new()).unwrap();
        assert_eq!(empty, Cli::default());
        assert!(!parse(args("--start --no-music")).unwrap().changes_profile());
    }

    #[test]
    fn bad_lines_are_refused() {
        for line in [
            "--console series-z",
            "--work fast",
            "--frobnicate",
            "game.exe",
            "--env",
            "--env VICTORY_X",
            "--set =1",
            "--start=yes",
            "--profile --start",
        ] {
            assert!(parse(args(line)).is_err(), "{line}");
        }
    }

    #[test]
    fn a_line_is_applied_all_or_nothing() {
        let base = Path::new(r"C:\here");
        let mut p = Profile::default();
        let cli = parse(args(
            "--env VICTORY_TEXTURE_ROOM_PERCENT=94 --set lossless.multiplier=3 --work gpu --cache-dir warm",
        ))
        .unwrap();
        let notes = apply(&cli, &mut p, base, || unreachable!(), no_pc).unwrap();
        assert_eq!(notes.len(), 4);
        assert_eq!(p.texture_room_percent, 94);
        assert_eq!(p.lossless.multiplier, 3);
        assert_eq!(p.work, Work::Gpu);
        assert_eq!(p.cache_dir, r"C:\here\warm");
        // An unknown switch refuses everything on the line.
        let before = p.clone();
        let cli = parse(args("--work cpu --env VICTORY_NO_SUCH=1")).unwrap();
        let err = apply(&cli, &mut p, base, || unreachable!(), no_pc).unwrap_err();
        assert!(err.contains("VICTORY_NO_SUCH"), "{err}");
        assert_eq!(p, before);
        let cli = parse(args("--game D:\\no\\such.exe")).unwrap();
        assert!(apply(&cli, &mut p, base, || unreachable!(), no_pc).is_err());
    }

    #[test]
    fn profile_best_puts_on_the_best_measured_settings() {
        let mut p = Profile {
            work: Work::Gpu,
            ..Profile::default()
        };
        let cli = parse(args("--profile best --env VICTORY_PIPELINE_WAIT_MS=50")).unwrap();
        let file = best::parse(include_str!("../../profiles/best.json")).unwrap();
        let notes = apply(&cli, &mut p, Path::new("."), || file, no_pc).unwrap();
        assert!(
            notes[0].contains("run 47") && notes[0].contains("a guess"),
            "{notes:?}"
        );
        assert_eq!(p.work, Work::Balanced);
        assert_eq!(p.pipeline_wait_ms, Some(50));
    }

    #[test]
    fn profile_from_a_file_keeps_the_game_and_sound() {
        let dir = std::env::temp_dir().join(format!("vertex-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.json"), r#"{"work":"cpu","textures":"drop2"}"#).unwrap();
        let mut p = Profile {
            game_exe: "g.exe".into(),
            music_volume: 9,
            ..Profile::default()
        };
        let cli = parse(args("--profile p.json")).unwrap();
        apply(&cli, &mut p, &dir, || unreachable!(), no_pc).unwrap();
        assert_eq!(
            (p.work, p.game_exe.as_str(), p.music_volume),
            (Work::Cpu, "g.exe", 9)
        );
        assert_eq!(p.version, crate::settings::PROFILE_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
