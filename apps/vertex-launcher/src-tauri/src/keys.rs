//! The keys file (`keys.txt` in the engine's data folder), read and written
//! in the format `crates/victory-hle/src/modules/gameinput/binds.rs` parses:
//! sections `[general]` (`SWITCH`), `[walk]` and `[drive]`, lines
//! `CONTROL = Key, Key`, `#` starting a comment. The tables below are the
//! engine's; the tests compare them with that file, and check that the
//! file the launcher writes for the defaults is the engine's `DEFAULT` to the
//! byte.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The pad's controls, as the file names them (`PARTS` in binds.rs).
pub const PARTS: [&str; 24] = [
    "A",
    "B",
    "X",
    "Y",
    "LB",
    "RB",
    "LT",
    "RT",
    "L3",
    "R3",
    "UP",
    "DOWN",
    "LEFT",
    "RIGHT",
    "MENU",
    "VIEW",
    "MOVE_UP",
    "MOVE_DOWN",
    "MOVE_LEFT",
    "MOVE_RIGHT",
    "LOOK_UP",
    "LOOK_DOWN",
    "LOOK_LEFT",
    "LOOK_RIGHT",
];

/// The names of keys that are not a letter or a digit (`KEYS` in binds.rs).
pub const KEYS: [(&str, u8); 43] = [
    ("LeftClick", 0x01),
    ("RightClick", 0x02),
    ("MiddleClick", 0x04),
    ("Mouse4", 0x05),
    ("Mouse5", 0x06),
    ("Backspace", 0x08),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("CapsLock", 0x14),
    ("Esc", 0x1B),
    ("Space", 0x20),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("ArrowLeft", 0x25),
    ("ArrowUp", 0x26),
    ("ArrowRight", 0x27),
    ("ArrowDown", 0x28),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("Shift", 0xA0),
    ("RightShift", 0xA1),
    ("Ctrl", 0xA2),
    ("RightCtrl", 0xA3),
    ("RightAlt", 0xA5),
    ("Semicolon", 0xBA),
    ("Comma", 0xBC),
    ("Minus", 0xBD),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Backquote", 0xC0),
];

/// The comment the engine's file starts with, word for word.
const HEADER: &str = "\
# VIctory keys. Change the keys on the right of '='; save; the running title
# takes them within a second. Several keys: separate them with commas.
# Keys: letters, digits, F1 to F11, Shift, RightShift, Ctrl, RightCtrl,
# RightAlt, Space, Enter, Esc, Tab, CapsLock, Backspace, Insert, Delete, Home,
# End, PageUp, PageDown, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Comma,
# Period, Slash, Semicolon, Minus, Backquote, LeftClick, RightClick,
# MiddleClick, Mouse4, Mouse5. The mouse's motion is always the right stick.
# Left Alt gives the mouse back and cannot be used. Delete this file to get
# the one VIctory comes with again. F1 in the game opens the Controls menu,
# which writes this file.
";

const MOUSE_FOOTER: &str = "\n[mouse]\n# The mouse turns the view (the right stick). SENSITIVITY: 0.1 to 10.\nSENSITIVITY = 1\nINVERT_Y = no\n";

const SWITCH_NOTE: &str = "# Switches between the two layouts below (the window's log says which).";
const DRIVE_NOTE: &str = "# W and S are the triggers (accelerate, brake), A and D steer.";

/// The engine's default layouts, in the order its file lists them.
const DEFAULT_SWITCH: &[&str] = &["G"];
const DEFAULT_WALK: &[(&str, &[&str])] = &[
    ("MOVE_UP", &["W"]),
    ("MOVE_DOWN", &["S"]),
    ("MOVE_LEFT", &["A"]),
    ("MOVE_RIGHT", &["D"]),
    ("LT", &["RightClick", "U"]),
    ("RT", &["LeftClick", "O"]),
    ("A", &["Shift", "Enter"]),
    ("B", &["R", "Backspace"]),
    ("X", &["Space", "X"]),
    ("Y", &["F", "Y"]),
    ("LB", &["Tab", "Mouse4"]),
    ("RB", &["Q", "Mouse5"]),
    ("L3", &["Ctrl", "Z"]),
    ("R3", &["C", "MiddleClick", "H"]),
    ("UP", &["ArrowUp", "1"]),
    ("DOWN", &["ArrowDown", "2"]),
    ("LEFT", &["ArrowLeft", "3"]),
    ("RIGHT", &["ArrowRight", "4", "E"]),
    ("MENU", &["Esc", "P"]),
    ("VIEW", &["V"]),
    ("LOOK_UP", &["I"]),
    ("LOOK_DOWN", &["K"]),
    ("LOOK_LEFT", &["J"]),
    ("LOOK_RIGHT", &["L"]),
];
const DEFAULT_DRIVE: &[(&str, &[&str])] = &[
    ("RT", &["W"]),
    ("LT", &["S"]),
    ("MOVE_LEFT", &["A"]),
    ("MOVE_RIGHT", &["D"]),
    ("MOVE_UP", &["ArrowUp"]),
    ("MOVE_DOWN", &["ArrowDown"]),
    ("LB", &["LeftClick", "RightClick"]),
    ("RB", &["Space"]),
    ("A", &["Shift", "Enter"]),
    ("B", &["R", "Backspace"]),
    ("X", &["X"]),
    ("Y", &["F", "Y"]),
    ("L3", &["Ctrl", "H"]),
    ("R3", &["C", "MiddleClick"]),
    ("UP", &["1"]),
    ("DOWN", &["2"]),
    ("LEFT", &["3", "ArrowLeft"]),
    ("RIGHT", &["4", "E", "ArrowRight"]),
    ("MENU", &["Esc", "P"]),
    ("VIEW", &["V"]),
    ("LOOK_UP", &["I"]),
    ("LOOK_DOWN", &["K"]),
    ("LOOK_LEFT", &["J"]),
    ("LOOK_RIGHT", &["L"]),
];

/// One control of a layout and its keys, by their names in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub action: String,
    pub keys: Vec<String>,
}

/// The whole file: the switch key and both layouts, every control listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyFile {
    pub switch: Vec<String>,
    pub walk: Vec<Binding>,
    pub drive: Vec<Binding>,
}

/// A line the engine would not understand: its number (from 1) and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub line: usize,
    pub why: String,
}

fn layout(table: &[(&str, &[&str])]) -> Vec<Binding> {
    table
        .iter()
        .map(|(a, k)| Binding {
            action: (*a).into(),
            keys: k.iter().map(|s| (*s).into()).collect(),
        })
        .collect()
}

impl KeyFile {
    pub fn defaults() -> KeyFile {
        KeyFile {
            switch: DEFAULT_SWITCH.iter().map(|s| (*s).into()).collect(),
            walk: layout(DEFAULT_WALK),
            drive: layout(DEFAULT_DRIVE),
        }
    }
}

/// The name of a key as the file writes it, whatever its case in the file;
/// `None` for a name the engine does not know.
pub fn key_name(name: &str) -> Option<String> {
    let name = name.trim();
    if let [c] = name.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some((c.to_ascii_uppercase() as char).to_string());
    }
    KEYS.iter()
        .find(|k| k.0.eq_ignore_ascii_case(name))
        .map(|k| k.0.to_string())
}

/// Every key name the file takes: letters, digits, then the named keys.
pub fn all_key_names() -> Vec<String> {
    let mut out: Vec<String> = ('A'..='Z')
        .chain('0'..='9')
        .map(|c| c.to_string())
        .collect();
    out.extend(KEYS.iter().map(|k| k.0.to_string()));
    out
}

/// Reads a file's text as the engine does (`Binds::read`): a layout the file
/// leaves out or leaves empty, or a switch it does not name, is the
/// default. Controls are listed in the default file's order.
pub fn parse(text: &str) -> (KeyFile, Vec<Problem>) {
    let mut switch: Option<Vec<String>> = None;
    let mut walk: Vec<(String, String)> = Vec::new();
    let mut drive: Vec<(String, String)> = Vec::new();
    let mut problems = Vec::new();
    let mut section = String::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            if !matches!(section.as_str(), "general" | "walk" | "drive" | "mouse") {
                problems.push(Problem {
                    line: n + 1,
                    why: format!("no section [{name}]"),
                });
            }
            continue;
        }
        if section == "mouse" {
            continue;
        }
        let Some((what, keys)) = line.split_once('=') else {
            problems.push(Problem {
                line: n + 1,
                why: "no '='".into(),
            });
            continue;
        };
        let mut found = Vec::new();
        for name in keys.split(',').filter(|k| !k.trim().is_empty()) {
            match key_name(name) {
                Some(k) => found.push(k),
                None => problems.push(Problem {
                    line: n + 1,
                    why: format!("no key {:?}", name.trim()),
                }),
            }
        }
        let what = what.trim().to_ascii_uppercase();
        let into = match section.as_str() {
            "general" if what == "SWITCH" => {
                switch = Some(found);
                continue;
            }
            "walk" => &mut walk,
            "drive" => &mut drive,
            _ => {
                problems.push(Problem {
                    line: n + 1,
                    why: format!("{what} is not known here"),
                });
                continue;
            }
        };
        if PARTS.contains(&what.as_str()) {
            into.extend(found.into_iter().map(|k| (what.clone(), k)));
        } else {
            problems.push(Problem {
                line: n + 1,
                why: format!("no control {what}"),
            });
        }
    }
    let defaults = KeyFile::defaults();
    let gather = |pairs: Vec<(String, String)>, own: Vec<Binding>| {
        if pairs.is_empty() {
            return own;
        }
        own.into_iter()
            .map(|b| {
                let mut keys: Vec<String> = Vec::new();
                for (a, k) in &pairs {
                    if *a == b.action && !keys.contains(k) {
                        keys.push(k.clone());
                    }
                }
                Binding {
                    action: b.action,
                    keys,
                }
            })
            .collect()
    };
    let file = KeyFile {
        switch: switch.unwrap_or(defaults.switch),
        walk: gather(walk, defaults.walk),
        drive: gather(drive, defaults.drive),
    };
    (file, problems)
}

/// The file's text for `file`, in the engine's format: its header, then
/// the three sections, controls in the default file's order. Names the
/// engine does not know are left out, and so is a control the file lists
/// twice (the first holds).
pub fn write(file: &KeyFile) -> String {
    let keys = |list: &[String]| {
        let names: Vec<String> = list.iter().filter_map(|k| key_name(k)).collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(" {}", names.join(", "))
        }
    };
    let section = |out: &mut String,
                   title: &str,
                   note: Option<&str>,
                   order: &[(&str, &[&str])],
                   list: &[Binding]| {
        out.push_str(&format!("\n[{title}]\n"));
        if let Some(note) = note {
            out.push_str(note);
            out.push('\n');
        }
        for (action, _) in order {
            let bound = list.iter().find(|b| b.action.eq_ignore_ascii_case(action));
            let list = bound.map(|b| b.keys.as_slice()).unwrap_or(&[]);
            out.push_str(&format!("{action} ={}\n", keys(list)));
        }
    };
    let mut out = String::from(HEADER);
    out.push_str(&format!(
        "\n[general]\n{SWITCH_NOTE}\nSWITCH ={}\n",
        keys(&file.switch)
    ));
    section(&mut out, "walk", None, DEFAULT_WALK, &file.walk);
    section(
        &mut out,
        "drive",
        Some(DRIVE_NOTE),
        DEFAULT_DRIVE,
        &file.drive,
    );
    out.push_str(MOUSE_FOOTER);
    out
}

/// Where the engine reads the file: the data folder the launcher sets
/// (`VICTORY_DATA_DIR`, [`crate::settings::Profile::data_override`]), else
/// `[paths] data_dir` in a victory.toml in `workdir` (the run's working
/// folder), else `%LOCALAPPDATA%\victory\data` (`victory-core` `config.rs`).
pub fn file_path(data: Option<&std::path::Path>, workdir: Option<&std::path::Path>) -> PathBuf {
    data.map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| crate::settings::engine_dir(workdir, "data_dir", "data"))
        .join("keys.txt")
}

/// `data_dir` of a victory.toml's `[paths]`.
#[cfg(test)]
fn toml_data_dir(file: &std::path::Path) -> Option<PathBuf> {
    crate::settings::toml_path(file, "data_dir")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: &str =
        include_str!("../../../../crates/victory-hle/src/modules/gameinput/binds.rs");

    /// The engine's `DEFAULT` text, taken from its source.
    fn engine_default() -> &'static str {
        let start = ENGINE
            .find("pub(super) const DEFAULT: &str = \"\\\n")
            .expect("DEFAULT in binds.rs");
        let body = &ENGINE[start..];
        let body = &body[body.find('\n').unwrap() + 1..];
        &body[..body.find("\";\n").expect("end of DEFAULT")]
    }

    /// The text between `from` and the next `];` of the engine's source.
    fn engine_table(from: &str) -> &'static str {
        let start = ENGINE.find(from).expect(from);
        let end = ENGINE[start..].find("];").unwrap() + start;
        &ENGINE[start..end]
    }

    #[test]
    fn the_default_file_written_is_the_engines_to_the_byte() {
        let default = engine_default();
        assert!(default.starts_with("# VIctory keys."));
        assert_eq!(write(&KeyFile::defaults()), default);
    }

    #[test]
    fn the_engines_default_reads_back_to_the_same_layouts() {
        let (file, problems) = parse(engine_default());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(file, KeyFile::defaults());
        // As the engine's own test: aim and fire on foot, W and S at the wheel.
        let keys = |l: &[Binding], a: &str| l.iter().find(|b| b.action == a).unwrap().keys.clone();
        assert_eq!(keys(&file.walk, "LT")[0], "RightClick");
        assert_eq!(keys(&file.walk, "RT")[0], "LeftClick");
        assert_eq!(keys(&file.drive, "RT"), ["W"]);
        assert_eq!(keys(&file.drive, "LT"), ["S"]);
    }

    #[test]
    fn a_changed_file_survives_writing_and_reading() {
        let mut file = KeyFile::defaults();
        file.switch = vec!["F5".into()];
        file.walk[0].keys = vec!["ArrowUp".into(), "Mouse4".into()];
        file.drive[6].keys.clear();
        let text = write(&file);
        assert!(text.contains("\nSWITCH = F5\n"));
        assert!(text.contains("\nMOVE_UP = ArrowUp, Mouse4\n"));
        assert!(text.contains("\nLB =\n"));
        let (back, problems) = parse(&text);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(back, file);
    }

    #[test]
    fn the_parser_follows_the_engines_rules() {
        // The engine's own test file, line for line.
        let (file, problems) = parse(
            "[general]\nswitch = f5 # comment\n[walk]\nA = Space, nokey\nnothing\nQQ = 1\n[fly]\nA = B\n",
        );
        assert_eq!(file.switch, ["F5"]);
        let a = file.walk.iter().find(|b| b.action == "A").unwrap();
        assert_eq!(a.keys, ["Space"]);
        assert!(
            file.walk
                .iter()
                .filter(|b| b.action != "A")
                .all(|b| b.keys.is_empty())
        );
        assert_eq!(file.drive, KeyFile::defaults().drive);
        let lines: Vec<usize> = problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, [4, 5, 6, 7, 8]);
        assert_eq!(parse("").0, KeyFile::defaults());
        let (file, _) = parse("[general]\nSWITCH =\n[drive]\nRB = Space, Ctrl\n");
        assert!(file.switch.is_empty());
        assert_eq!(
            file.drive.iter().find(|b| b.action == "RB").unwrap().keys,
            ["Space", "Ctrl"]
        );
    }

    #[test]
    fn the_tables_are_the_engines() {
        let parts = engine_table("const PARTS:");
        let named: Vec<&str> = parts
            .split("(\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        assert_eq!(named, PARTS);
        let keys = engine_table("const KEYS:");
        let named: Vec<(String, u8)> = keys
            .split("(\"")
            .skip(1)
            .map(|s| {
                let name = s.split('"').next().unwrap().to_string();
                let hex = s.split("0x").nth(1).unwrap();
                let hex: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                (name, u8::from_str_radix(&hex, 16).unwrap())
            })
            .collect();
        let ours: Vec<(String, u8)> = KEYS.iter().map(|(n, v)| (n.to_string(), *v)).collect();
        assert_eq!(named, ours);
    }

    #[test]
    fn key_names_take_any_case() {
        assert_eq!(key_name("q").as_deref(), Some("Q"));
        assert_eq!(key_name(" leftclick ").as_deref(), Some("LeftClick"));
        assert_eq!(key_name("7").as_deref(), Some("7"));
        assert_eq!(key_name("F12"), None);
        assert_eq!(key_name("Alt"), None);
        assert_eq!(all_key_names().len(), 26 + 10 + KEYS.len());
    }

    #[test]
    fn the_data_folder_of_a_victory_toml_is_read() {
        let dir = std::env::temp_dir().join(format!("vertex-keys-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("victory.toml"),
            "[paths]\ngame_dir = 'x'\ndata_dir = 'E:\\data'\n",
        )
        .unwrap();
        assert_eq!(
            toml_data_dir(&dir.join("victory.toml")),
            Some(PathBuf::from(r"E:\data"))
        );
        std::fs::write(
            dir.join("victory.toml"),
            "[other]\ndata_dir = \"E:\\\\no\"\n",
        )
        .unwrap();
        assert_eq!(toml_data_dir(&dir.join("victory.toml")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
