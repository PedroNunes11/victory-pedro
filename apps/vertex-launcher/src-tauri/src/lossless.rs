//! Lossless Scaling (a Steam application of its own, app 993090): whether it
//! is installed, starting it with the game, and a profile for victory.exe in
//! its `Settings.xml`.
//!
//! The profile is written with care for the player's other settings: the
//! file is copied to `Settings.xml.vertex-backup` first, no other profile is
//! touched, nothing is written while Lossless Scaling runs (it writes the
//! file itself when it closes), and only field names the file already uses
//! are written.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::settings::Lossless;

pub const STEAM_APP: &str = "993090";
pub const EXE: &str = "LosslessScaling.exe";
const DEFAULT_FOLDER: &str = r"C:\Program Files (x86)\Steam\steamapps\common\Lossless Scaling";
/// The name a new profile gets.
const TITLE: &str = "Vertex";

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub installed: bool,
    pub exe: Option<String>,
    pub running: bool,
    pub settings: String,
    pub settings_exists: bool,
    /// The profile for victory.exe that is there now, as (field, value).
    pub profile: Option<Vec<(String, String)>>,
    pub profile_title: Option<String>,
}

/// `%LOCALAPPDATA%\Lossless Scaling\Settings.xml`.
pub fn settings_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Lossless Scaling")
        .join("Settings.xml")
}

/// The library folders Steam's `libraryfolders.vdf` lists.
pub fn library_folders(vdf: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in vdf.lines() {
        let parts: Vec<&str> = line.split('"').collect();
        // `"path"		"D:\\SteamLibrary"` splits into ["", path, ws, value, ""].
        if parts.len() >= 4 && parts[1].eq_ignore_ascii_case("path") {
            out.push(PathBuf::from(parts[3].replace("\\\\", "\\")));
        }
    }
    out
}

/// Where LosslessScaling.exe is, if it is installed.
pub fn find_exe() -> Option<PathBuf> {
    let mut folders = vec![PathBuf::from(DEFAULT_FOLDER)];
    if let Some(steam) = crate::system::steam_path() {
        let steam = PathBuf::from(steam.replace('/', "\\"));
        if let Ok(vdf) = std::fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf"))
        {
            for library in library_folders(&vdf) {
                folders.push(
                    library
                        .join("steamapps")
                        .join("common")
                        .join("Lossless Scaling"),
                );
            }
        }
        folders.push(
            steam
                .join("steamapps")
                .join("common")
                .join("Lossless Scaling"),
        );
    }
    folders
        .into_iter()
        .map(|f| f.join(EXE))
        .find(|p| p.is_file())
}

pub fn status(victory: Option<&Path>) -> Status {
    let exe = find_exe();
    let settings = settings_path();
    let text = read_text(&settings).ok();
    let (profile, profile_title) = match (&text, victory) {
        (Some((t, _)), Some(v)) => match find_profile(t, &v.display().to_string()) {
            Some((start, end)) => {
                let block = &t[start..end];
                (Some(fields(block)), field(block, "Title"))
            }
            None => (None, None),
        },
        _ => (None, None),
    };
    Status {
        installed: exe.is_some(),
        exe: exe.map(|p| p.display().to_string()),
        running: crate::system::is_running(EXE),
        settings: settings.display().to_string(),
        settings_exists: text.is_some(),
        profile,
        profile_title,
    }
}

/// Starts it through Steam, unless it runs already.
pub fn start() -> Result<(), String> {
    if crate::system::is_running(EXE) {
        return Ok(());
    }
    crate::system::open(&format!("steam://rungameid/{STEAM_APP}"))
}

/// The file's text without a byte-order mark, and whether it had one.
fn read_text(path: &Path) -> Result<(String, bool), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let text = String::from_utf8(bytes[if bom { 3 } else { 0 }..].to_vec())
        .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    Ok((text, bom))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The value of `<name>` directly in `block`, unescaped.
fn field(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(unescape(&block[start..end]))
}

/// Every simple `<Field>value</Field>` of a profile block, in order.
fn fields(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let Some(close) = rest.find('>') else { break };
        let name = &rest[..close];
        if name.starts_with('/') || name.contains(' ') || name.ends_with('/') || name == "Profile" {
            continue;
        }
        let end_tag = format!("</{name}>");
        if let Some(end) = rest.find(&end_tag) {
            let value = &rest[close + 1..end];
            if !value.contains('<') {
                out.push((name.to_string(), unescape(value)));
                rest = &rest[end + end_tag.len()..];
            }
        }
    }
    out
}

/// The byte ranges of every `<Profile>...</Profile>` inside `<GameProfiles>`.
fn profiles(text: &str) -> Vec<(usize, usize)> {
    let Some(list) = text.find("<GameProfiles>") else {
        return Vec::new();
    };
    let list_end = text[list..]
        .find("</GameProfiles>")
        .map_or(text.len(), |e| e + list);
    let mut out = Vec::new();
    let mut at = list;
    while let Some(i) = text[at..list_end].find("<Profile>") {
        let start = at + i;
        let Some(e) = text[start..list_end].find("</Profile>") else {
            break;
        };
        let end = start + e + "</Profile>".len();
        out.push((start, end));
        at = end;
    }
    out
}

/// The profile for `victory`: the one whose Path is it, else one named
/// "Vertex".
fn find_profile(text: &str, victory: &str) -> Option<(usize, usize)> {
    let all = profiles(text);
    all.iter()
        .find(|&&(s, e)| {
            field(&text[s..e], "Path").is_some_and(|p| p.eq_ignore_ascii_case(victory))
        })
        .or_else(|| {
            all.iter()
                .find(|&&(s, e)| field(&text[s..e], "Title").as_deref() == Some(TITLE))
        })
        .copied()
}

/// What the profile says, field by field.
fn wanted(victory: &str, choice: &Lossless) -> Vec<(&'static str, String)> {
    let c = choice.clamped();
    vec![
        ("Path", victory.to_string()),
        ("FrameGeneration", "LSFG3".into()),
        ("LSFG3Mode1", "FIXED".into()),
        ("LSFG3Multiplier", c.multiplier.to_string()),
        ("LSFGFlowScale", c.flow_scale.to_string()),
        ("LSFGSize", "PERFORMANCE".into()),
        ("ScalingType", "Off".into()),
        ("AutoScale", c.auto_scale.to_string()),
        ("AutoScaleDelay", c.auto_scale_delay.to_string()),
        ("MaxFrameLatency", c.max_frame_latency.to_string()),
        ("DrawFps", c.draw_fps.to_string()),
    ]
}

/// What a write did.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Written {
    pub created: bool,
    /// Fields the file does not use anywhere, so not written.
    pub skipped: Vec<String>,
    pub backup: String,
}

/// The file's new text with the profile for `victory` written or updated.
/// Pure, for the tests. `Err` where the file has no profile list to add to.
pub fn edit(
    text: &str,
    victory: &str,
    choice: &Lossless,
) -> Result<(String, bool, Vec<String>), String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let known = |name: &str| text.contains(&format!("<{name}>"));
    let mut skipped = Vec::new();
    let wanted = wanted(victory, choice);
    for (name, _) in &wanted {
        if !known(name) {
            skipped.push((*name).to_string());
        }
    }
    if skipped.iter().any(|s| s == "Path") {
        return Err(
            "this Lossless Scaling's settings name no Path field: no profile was written".into(),
        );
    }
    let first = *profiles(text)
        .first()
        .ok_or("the settings file has no profiles to start from")?;
    let (mut block, created, range) = match find_profile(text, victory) {
        Some((s, e)) => (text[s..e].to_string(), false, Some((s, e))),
        None => {
            // A new profile from the first one (Lossless Scaling's Default),
            // named Vertex; a Path it has is replaced below.
            let block = set(&text[first.0..first.1], "Title", TITLE, newline);
            (block, true, None)
        }
    };
    for (name, value) in &wanted {
        if known(name) {
            block = set(&block, name, &escape(value), newline);
        }
    }
    let out = match range {
        Some((s, e)) => format!("{}{}{}", &text[..s], block, &text[e..]),
        None => {
            // On a line of its own before `</GameProfiles>`, indented as the
            // first profile is.
            let end = text
                .find("</GameProfiles>")
                .ok_or("the settings file has no profile list")?;
            let line_start = text[..end].rfind('\n').map_or(0, |i| i + 1);
            let indent_start = text[..first.0].rfind('\n').map_or(0, |i| i + 1);
            let indent = &text[indent_start..first.0];
            format!(
                "{}{indent}{block}{newline}{}",
                &text[..line_start],
                &text[line_start..]
            )
        }
    };
    Ok((out, created, skipped))
}

/// `block` with `<name>` set to `value` (already escaped): replaced where it
/// is, else added after the Title line (Path) or before `</Profile>`.
fn set(block: &str, name: &str, value: &str, newline: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    if let Some(i) = block.find(&open) {
        let start = i + open.len();
        if let Some(e) = block[start..].find(&close) {
            return format!("{}{}{}", &block[..start], value, &block[start + e..]);
        }
    }
    // The indent of the fields: that of the line of the first field.
    let indent = block
        .find(newline)
        .map(|i| &block[i + newline.len()..])
        .map(|rest| &rest[..rest.len() - rest.trim_start_matches([' ', '\t']).len()])
        .unwrap_or("      ");
    let line = format!("{indent}{open}{value}{close}{newline}");
    if name == "Path"
        && let Some(t) = block.find("</Title>")
    {
        let after = t + "</Title>".len();
        let after = block[after..]
            .find(newline)
            .map_or(block.len(), |n| after + n + newline.len());
        return format!("{}{}{}", &block[..after], line, &block[after..]);
    }
    match block.rfind("</Profile>") {
        Some(end) => {
            let line_start = block[..end]
                .rfind(newline)
                .map_or(end, |i| i + newline.len());
            format!("{}{}{}", &block[..line_start], line, &block[line_start..])
        }
        None => block.to_string(),
    }
}

/// Writes the profile for `victory` into the player's Settings.xml.
pub fn write_profile(victory: &Path, choice: &Lossless) -> Result<Written, String> {
    if crate::system::is_running(EXE) {
        return Err("running".into());
    }
    let path = settings_path();
    if !path.is_file() {
        return Err("no_settings".into());
    }
    let (text, bom) = read_text(&path)?;
    let (new, created, skipped) = edit(&text, &victory.display().to_string(), choice)?;
    let backup = path.with_file_name("Settings.xml.vertex-backup");
    std::fs::copy(&path, &backup).map_err(|e| format!("backup {}: {e}", backup.display()))?;
    let mut bytes = Vec::with_capacity(new.len() + 3);
    if bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(new.as_bytes());
    let temp = path.with_file_name("Settings.xml.vertex-new");
    std::fs::write(&temp, &bytes).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Written {
        created,
        skipped,
        backup: backup.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up file of the same shape as Lossless Scaling's.
    const SAMPLE: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<Settings>\r\n  <Hotkey>S</Hotkey>\r\n  <GameProfiles>\r\n    <Profile>\r\n      <Title>Default</Title>\r\n      <AutoScale>false</AutoScale>\r\n      <AutoScaleDelay>0</AutoScaleDelay>\r\n      <ScalingType>Off</ScalingType>\r\n      <LSFG3Mode1>FIXED</LSFG3Mode1>\r\n      <LSFG3Multiplier>2</LSFG3Multiplier>\r\n      <LSFGFlowScale>100</LSFGFlowScale>\r\n      <LSFGSize>BALANCED</LSFGSize>\r\n      <FrameGeneration>Off</FrameGeneration>\r\n      <MaxFrameLatency>3</MaxFrameLatency>\r\n      <DrawFps>false</DrawFps>\r\n    </Profile>\r\n    <Profile>\r\n      <Title>Other game</Title>\r\n      <Path>C:\\Games\\other.exe</Path>\r\n      <LSFG3Multiplier>3</LSFG3Multiplier>\r\n    </Profile>\r\n  </GameProfiles>\r\n</Settings>";

    const VICTORY: &str = r"D:\Vertex & co\victory.exe";

    fn choice() -> Lossless {
        Lossless {
            multiplier: 3,
            flow_scale: 60,
            ..Lossless::default()
        }
    }

    #[test]
    fn a_new_profile_is_added_and_the_others_stay() {
        let (out, created, skipped) = edit(SAMPLE, VICTORY, &choice()).unwrap();
        assert!(created);
        assert!(skipped.is_empty(), "{skipped:?}");
        let all = profiles(&out);
        assert_eq!(all.len(), 3);
        // The first two, byte for byte.
        let old = profiles(SAMPLE);
        for i in 0..2 {
            assert_eq!(&out[all[i].0..all[i].1], &SAMPLE[old[i].0..old[i].1]);
        }
        let new = &out[all[2].0..all[2].1];
        assert_eq!(field(new, "Title").as_deref(), Some("Vertex"));
        assert_eq!(field(new, "Path").as_deref(), Some(VICTORY));
        assert!(new.contains("<Path>D:\\Vertex &amp; co\\victory.exe</Path>"));
        assert_eq!(field(new, "FrameGeneration").as_deref(), Some("LSFG3"));
        assert_eq!(field(new, "LSFG3Multiplier").as_deref(), Some("3"));
        assert_eq!(field(new, "LSFGFlowScale").as_deref(), Some("60"));
        assert_eq!(field(new, "LSFGSize").as_deref(), Some("PERFORMANCE"));
        assert_eq!(field(new, "AutoScale").as_deref(), Some("true"));
        assert_eq!(field(new, "DrawFps").as_deref(), Some("true"));
        // Before and after the list: unchanged.
        let head = SAMPLE.find("<GameProfiles>").unwrap();
        assert_eq!(&out[..head], &SAMPLE[..head]);
        assert!(out.ends_with("  </GameProfiles>\r\n</Settings>"));
        assert!(!out.contains("\r\r") && !out.replace("\r\n", "").contains('\r'));
        // Written again: found by its Path and updated in place.
        let again = Lossless {
            multiplier: 4,
            ..choice()
        };
        let (out2, created, _) = edit(&out, VICTORY, &again).unwrap();
        assert!(!created);
        assert_eq!(profiles(&out2).len(), 3);
        let new = &out2[profiles(&out2)[2].0..profiles(&out2)[2].1];
        assert_eq!(field(new, "LSFG3Multiplier").as_deref(), Some("4"));
        assert_eq!(out2.len(), out.len());
    }

    #[test]
    fn fields_the_file_does_not_use_are_not_written() {
        let text = SAMPLE.replace("      <DrawFps>false</DrawFps>\r\n", "");
        let (out, _, skipped) = edit(&text, VICTORY, &choice()).unwrap();
        assert_eq!(skipped, ["DrawFps"]);
        assert!(!out.contains("<DrawFps>"));
        let none = SAMPLE.replace("      <Path>C:\\Games\\other.exe</Path>\r\n", "");
        assert!(edit(&none, VICTORY, &choice()).is_err());
    }

    #[test]
    fn values_are_kept_in_range() {
        let wild = Lossless {
            multiplier: 9,
            flow_scale: 5,
            max_frame_latency: 0,
            ..Lossless::default()
        };
        let (out, _, _) = edit(SAMPLE, VICTORY, &wild).unwrap();
        let new = &out[profiles(&out)[2].0..profiles(&out)[2].1];
        assert_eq!(field(new, "LSFG3Multiplier").as_deref(), Some("4"));
        assert_eq!(field(new, "LSFGFlowScale").as_deref(), Some("25"));
        assert_eq!(field(new, "MaxFrameLatency").as_deref(), Some("1"));
    }

    #[test]
    fn steam_library_folders_are_read() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"apps\"\n\t\t{\n\t\t\t\"993090\"\t\t\"123\"\n\t\t}\n\t}\n}\n";
        assert_eq!(
            library_folders(vdf),
            [
                PathBuf::from(r"C:\Program Files (x86)\Steam"),
                PathBuf::from(r"D:\SteamLibrary")
            ]
        );
    }
}
