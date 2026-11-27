//! The player's keys (`native-keys.txt`) and which of them stands for each
//! pad button: of the title's actions bound to that button on the console
//! (`crate::actions`), the key bound to most of them in play (or in menus).

use std::collections::HashMap;

/// An action as a key line names it, with the direction written after an
/// axis action (`-1`, `+1`) if any.
type Action = (String, Option<i8>);

/// One line of `native-keys.txt`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyLine {
    pub key: String,
    pub play: Vec<Action>,
    pub menu: Vec<Action>,
}

/// The whole file, in its order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub lines: Vec<KeyLine>,
}

fn action(name: &str) -> Option<Action> {
    let n = name.trim();
    let (n, sign) = if let Some(n) = n.strip_suffix('-') {
        (n.trim_end(), Some(-1))
    } else if let Some(n) = n.strip_suffix('+') {
        (n.trim_end(), Some(1))
    } else {
        (n, None)
    };
    (!n.is_empty()).then(|| (n.to_ascii_uppercase(), sign))
}

/// Parses `native-keys.txt`: lines `KEY = PLAY_ACTION, ... | MENU_ACTION, ...`,
/// `#` comments. Lines whose key the engine does not know are left out.
pub fn parse_layout(text: &str) -> Layout {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .filter_map(|l| {
            let (key, actions) = l.split_once('=')?;
            let key = key.trim();
            if !known_key(key) {
                return None;
            }
            let (play, menu) = actions.split_once('|').unwrap_or((actions, ""));
            let list = |s: &str| s.split(',').filter_map(action).collect::<Vec<_>>();
            Some(KeyLine { key: key.to_string(), play: list(play), menu: list(menu) })
        })
        .collect();
    Layout { lines }
}

/// A key name the engine reads (`victory-loader` `keyboard_events.rs`,
/// `key_code`).
pub fn known_key(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    const NAMED: &[&str] = &[
        "space", "shift", "rightshift", "ctrl", "rightctrl", "alt", "rightalt", "tab", "esc", "enter", "backspace",
        "capslock", "insert", "delete", "home", "end", "pageup", "pagedown", "arrowup", "arrowdown", "arrowleft",
        "arrowright", "comma", "period", "minus", "equals", "leftbracket", "rightbracket", "semicolon", "quote", "slash",
        "backslash", "backquote", "leftclick", "rightclick", "middleclick", "mouse4", "mouse5", "wheelup", "wheeldown",
    ];
    if NAMED.contains(&n.as_str()) {
        return true;
    }
    if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() {
        return true;
    }
    if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u32>().ok()) {
        return (1..=12).contains(&f);
    }
    n.strip_prefix("numpad").and_then(|d| d.parse::<u32>().ok()).is_some_and(|d| d <= 9)
}

/// What a key cap says for `key`, longest first (a shorter one is used
/// when the cap is small).
pub fn labels(key: &str) -> Vec<String> {
    let k = key.trim().to_ascii_lowercase();
    let named: &[&str] = match k.as_str() {
        "space" => &["SPACE", "SPC"],
        "shift" => &["SHIFT", "SHF"],
        "rightshift" => &["RSHIFT", "SHIFT", "SHF"],
        "ctrl" => &["CTRL"],
        "rightctrl" => &["RCTRL", "CTRL"],
        "alt" => &["ALT"],
        "rightalt" => &["RALT", "ALT"],
        "tab" => &["TAB"],
        "esc" => &["ESC"],
        "enter" => &["ENTER", "ENT"],
        "backspace" => &["BKSP", "BS"],
        "capslock" => &["CAPS"],
        "insert" => &["INS"],
        "delete" => &["DEL"],
        "home" => &["HOME"],
        "end" => &["END"],
        "pageup" => &["PGUP", "PU"],
        "pagedown" => &["PGDN", "PD"],
        "arrowup" => &["↑"],
        "arrowdown" => &["↓"],
        "arrowleft" => &["←"],
        "arrowright" => &["→"],
        "comma" => &[","],
        "period" => &["."],
        "minus" => &["-"],
        "equals" => &["="],
        "leftbracket" => &["["],
        "rightbracket" => &["]"],
        "semicolon" => &[";"],
        "quote" => &["'"],
        "slash" => &["/"],
        "backslash" => &["\\"],
        "backquote" => &["`"],
        "leftclick" => &["LMB"],
        "rightclick" => &["RMB"],
        "middleclick" => &["MMB"],
        "mouse4" => &["MB4", "M4"],
        "mouse5" => &["MB5", "M5"],
        "wheelup" => &["WHL↑", "W↑"],
        "wheeldown" => &["WHL↓", "W↓"],
        _ => &[],
    };
    if !named.is_empty() {
        return named.iter().map(|s| s.to_string()).collect();
    }
    if let Some(d) = k.strip_prefix("numpad") {
        return vec![format!("NUM{d}"), format!("N{d}")];
    }
    vec![key.trim().to_ascii_uppercase()]
}

/// A pad source and half as the action table writes them (`A`, 0; `LY`, 1).
pub type Target = (&'static str, u8);

/// The console bindings of each action, by name.
fn bindings() -> &'static HashMap<&'static str, Vec<Target>> {
    static MAP: std::sync::OnceLock<HashMap<&'static str, Vec<Target>>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: HashMap<&'static str, Vec<Target>> = HashMap::new();
        for &(action, source, half) in crate::actions::BINDINGS {
            map.entry(action).or_default().push((source, half));
        }
        map
    })
}

/// How much a play action counts towards the key a prompt means. The
/// title's generic prompt actions say it best: the `INPUT_CONTEXT*` ones
/// ("press ... to talk") and the `INPUT_SCRIPT_*` stand-ins for each button
/// that missions use. The many vehicle, parachute, minigame and shop
/// variants of one action count for less, so that six kinds of vehicle
/// attack on RB do not outvote taking cover.
fn weight(action: &str) -> f32 {
    const MINOR: &[&str] = &[
        "INPUT_VEH_",
        "INPUT_VEHICLE_",
        "INPUT_PARACHUTE_",
        "INPUT_MINIGAME_",
        "INPUT_SHOP_",
        "INPUT_FRONTEND_",
        "INPUT_CINEMATIC_",
    ];
    if action.starts_with("INPUT_CONTEXT") {
        6.0
    } else if action.starts_with("INPUT_SCRIPT_") {
        2.0
    } else if MINOR.iter().any(|p| action.starts_with(p)) {
        0.5
    } else {
        1.0
    }
}

/// How much of `actions` the console binds to `target` (by [`weight`] in
/// play; one each in menus). A whole-axis binding counts for the half the
/// key pushes: the direction written after the action, else the positive
/// one (as the engine sends it).
fn score(actions: &[Action], target: Target, menu: bool) -> f32 {
    let map = bindings();
    actions
        .iter()
        .filter(|(name, sign)| {
            map.get(name.as_str()).is_some_and(|b| {
                b.iter().any(|&(source, half)| {
                    let half = if half == 2 { u8::from(*sign == Some(-1)) } else { half };
                    source == target.0 && half == target.1
                })
            })
        })
        .map(|(name, _)| if menu { 1.0 } else { weight(name) })
        .sum()
}

impl Layout {
    /// The key that stands for `target` most (in play, or in menus with
    /// `menu`; see [`score`]); the earlier line wins a tie. In menus, a key with no
    /// menu actions for it falls back to play.
    pub fn key_for(&self, target: Target, menu: bool) -> Option<&str> {
        let best = |menu: bool| {
            let mut best: Option<(&str, f32)> = None;
            for line in &self.lines {
                let s = score(if menu { &line.menu } else { &line.play }, target, menu);
                if s > 0.0 && best.is_none_or(|(_, b)| s > b) {
                    best = Some((&line.key, s));
                }
            }
            best.map(|(k, _)| k)
        };
        if menu { best(true).or_else(|| best(false)) } else { best(false) }
    }

    /// What a cap shows for the glyph `glyph` (see [`glyph_targets`]), the
    /// labels longest first; `None` if no key stands for it.
    pub fn glyph_labels(&self, glyph: &str, menu: bool) -> Option<Vec<String>> {
        let g = canonical_glyph(glyph)?;
        let one = |t: Target| self.key_for(t, menu).map(labels);
        let joined = |parts: &[Target], sep: &str| -> Option<Vec<String>> {
            let each: Vec<Vec<String>> = parts.iter().map(|&t| one(t)).collect::<Option<_>>()?;
            // Short labels side by side ("WASD", "A/D"); else the first.
            if each.iter().all(|l| l.last().is_some_and(|s| s.chars().count() == 1)) {
                Some(vec![each.iter().map(|l| l.last().unwrap().clone()).collect::<Vec<_>>().join(sep)])
            } else {
                Some(each[0].clone())
            }
        };
        match g {
            "LS" => joined(&[("LY", 1), ("LX", 1), ("LY", 0), ("LX", 0)], "").or_else(|| one(("L3", 0))),
            "LX" => joined(&[("LX", 1), ("LX", 0)], "/"),
            "LY" => joined(&[("LY", 1), ("LY", 0)], "/"),
            "DPAD" => joined(&[("DUp", 0), ("DLeft", 0), ("DDown", 0), ("DRight", 0)], ""),
            // The camera stick is the mouse.
            "RS" | "RX" | "RY" => Some(vec!["MOUSE".into(), "MSE".into()]),
            "LSUP" => one(("LY", 1)),
            "LSDOWN" => one(("LY", 0)),
            "LSLEFT" => one(("LX", 1)),
            "LSRIGHT" => one(("LX", 0)),
            "RSUP" => one(("RY", 1)),
            "RSDOWN" => one(("RY", 0)),
            "RSLEFT" => one(("RX", 1)),
            "RSRIGHT" => one(("RX", 0)),
            button => one((table_name(button)?, 0)),
        }
    }
}

/// The glyph names `glyphs.txt` takes, upper case, with their aliases.
pub fn canonical_glyph(name: &str) -> Option<&'static str> {
    let n = name.trim().to_ascii_uppercase();
    Some(match n.as_str() {
        "A" => "A",
        "B" => "B",
        "X" => "X",
        "Y" => "Y",
        "LB" => "LB",
        "RB" => "RB",
        "LT" => "LT",
        "RT" => "RT",
        "VIEW" | "BACK" | "SELECT" => "VIEW",
        "MENU" | "START" => "MENU",
        "L3" | "LSB" | "LSCLICK" => "L3",
        "R3" | "RSB" | "RSCLICK" => "R3",
        "DUP" => "DUP",
        "DDOWN" => "DDOWN",
        "DLEFT" => "DLEFT",
        "DRIGHT" => "DRIGHT",
        "DPAD" => "DPAD",
        "LS" => "LS",
        "RS" => "RS",
        "LX" => "LX",
        "LY" => "LY",
        "RX" => "RX",
        "RY" => "RY",
        "LSUP" => "LSUP",
        "LSDOWN" => "LSDOWN",
        "LSLEFT" => "LSLEFT",
        "LSRIGHT" => "LSRIGHT",
        "RSUP" => "RSUP",
        "RSDOWN" => "RSDOWN",
        "RSLEFT" => "RSLEFT",
        "RSRIGHT" => "RSRIGHT",
        _ => return None,
    })
}

/// A button glyph as the action table names its source.
fn table_name(glyph: &str) -> Option<&'static str> {
    Some(match glyph {
        "A" => "A",
        "B" => "B",
        "X" => "X",
        "Y" => "Y",
        "LB" => "LB",
        "RB" => "RB",
        "LT" => "LT",
        "RT" => "RT",
        "VIEW" => "View",
        "MENU" => "Menu",
        "L3" => "L3",
        "R3" => "R3",
        "DUP" => "DUp",
        "DDOWN" => "DDown",
        "DLEFT" => "DLeft",
        "DRIGHT" => "DRight",
        _ => return None,
    })
}

/// Every glyph name, for the summary the plugin logs.
pub const GLYPHS: &[&str] = &[
    "A", "B", "X", "Y", "LB", "RB", "LT", "RT", "View", "Menu", "L3", "R3", "DUp", "DDown", "DLeft", "DRight", "LS",
    "RS",
];

#[cfg(test)]
mod tests {
    use super::*;

    const USER_LAYOUT: &str = include_str!("../tests/native-keys.txt");

    #[test]
    fn layout_lines_parse() {
        let l = parse_layout(
            "\u{feff}# comment\nW = INPUT_MOVE_UP_ONLY, INPUT_X | INPUT_FRONTEND_MAP_MOVE_UD- # tail\n\
             Bogus = INPUT_JUMP\nHome =  | INPUT_FRONTEND_SOCIAL_CLUB\n",
        );
        assert_eq!(l.lines.len(), 2, "unknown keys are left out");
        assert_eq!(l.lines[0].key, "W");
        assert_eq!(l.lines[0].play, vec![("INPUT_MOVE_UP_ONLY".into(), None), ("INPUT_X".into(), None)]);
        assert_eq!(l.lines[0].menu, vec![("INPUT_FRONTEND_MAP_MOVE_UD".into(), Some(-1))]);
        assert!(l.lines[1].play.is_empty());
        assert_eq!(l.lines[1].menu.len(), 1);
    }

    #[test]
    fn key_names_follow_the_engine() {
        for k in ["Space", "LeftClick", "WheelDown", "F12", "Numpad5", "q", "7", "RightAlt"] {
            assert!(known_key(k), "{k}");
        }
        for k in ["F13", "Numpad10", "Banana", ""] {
            assert!(!known_key(k), "{k}");
        }
    }

    #[test]
    fn labels_are_short_and_upper_case() {
        assert_eq!(labels("Space"), ["SPACE", "SPC"]);
        assert_eq!(labels("e"), ["E"]);
        assert_eq!(labels("LeftClick"), ["LMB"]);
        assert_eq!(labels("RightClick"), ["RMB"]);
        assert_eq!(labels("F10"), ["F10"]);
        assert_eq!(labels("Numpad4"), ["NUM4", "N4"]);
        assert_eq!(labels("ArrowUp"), ["↑"]);
        assert_eq!(labels("WheelUp")[0], "WHL↑");
    }

    #[test]
    fn whole_axis_bindings_count_for_the_half_the_key_pushes() {
        // INPUT_MOVE_UD is LY/02: plain, it pushes the positive half (down).
        let l = parse_layout("S = INPUT_MOVE_UD\nW = INPUT_MOVE_UD-\n");
        assert_eq!(l.key_for(("LY", 0), false), Some("S"));
        assert_eq!(l.key_for(("LY", 1), false), Some("W"));
    }

    #[test]
    fn generic_prompt_actions_outweigh_vehicle_variants() {
        let l = parse_layout(
            "LeftClick = INPUT_VEH_CAR_ATTACK, INPUT_VEH_BOAT_ATTACK, INPUT_VEH_ATTACK, INPUT_VEH_PLANE_ATTACK
             Q = INPUT_COVER, INPUT_COVER_AIM, INPUT_SPECIAL_ABILITY_ACTION
Mouse5 = INPUT_SCRIPT_RB
",
        );
        assert_eq!(l.key_for(("RB", 0), false), Some("Q"));
        let c = parse_layout("ArrowRight = INPUT_SCRIPT_PAD_RIGHT, INPUT_MINIGAME_MOVE_PAD_RIGHT
E = INPUT_CONTEXT
");
        assert_eq!(c.key_for(("DRight", 0), false), Some("E"));
    }

    #[test]
    fn the_key_bound_to_most_actions_wins_and_ties_go_to_the_first() {
        let l = parse_layout("G = INPUT_CATCH\nSpace = INPUT_JUMP, INPUT_CATCH_HOLD\nEnter = INPUT_JUMP | INPUT_FRONTEND_ACCEPT\n");
        assert_eq!(l.key_for(("A", 0), false), Some("Space"));
        assert_eq!(l.key_for(("A", 0), true), Some("Enter"));
        let tie = parse_layout("G = INPUT_CATCH\nH = INPUT_JUMP\n");
        assert_eq!(tie.key_for(("A", 0), false), Some("G"));
        // Nothing in menus: play's key.
        assert_eq!(tie.key_for(("A", 0), true), Some("G"));
        assert_eq!(tie.key_for(("Menu", 0), false), None);
    }

    #[test]
    fn the_users_layout_gives_the_expected_keys() {
        let l = parse_layout(USER_LAYOUT);
        assert!(l.lines.len() > 60);
        let play = |g: &str| l.glyph_labels(g, false).map(|v| v[0].clone());
        assert_eq!(play("A").as_deref(), Some("SPACE"), "A is jump: Space");
        assert_eq!(play("RT").as_deref(), Some("LMB"), "RT is attack: left click");
        assert_eq!(play("LT").as_deref(), Some("RMB"), "LT is aim: right click");
        assert_eq!(play("LS").as_deref(), Some("WASD"));
        assert_eq!(play("LY").as_deref(), Some("W/S"));
        assert_eq!(play("L3").as_deref(), Some("SHIFT"), "L3 is sprint");
        assert_eq!(play("Y").as_deref(), Some("F"), "Y is enter vehicle");
        assert_eq!(play("RS").as_deref(), Some("MOUSE"));
        assert_eq!(play("B").as_deref(), Some("X"), "B's context actions are on X");
        assert_eq!(play("RB").as_deref(), Some("Q"), "RB is cover, not the vehicle attacks on left click");
        assert_eq!(play("DRight").as_deref(), Some("E"), "INPUT_CONTEXT");
        assert_eq!(play("DUp").as_deref(), Some("↑"), "phone");
        assert_eq!(play("Menu").as_deref(), Some("ESC"));
        let menu = |g: &str| l.glyph_labels(g, true).map(|v| v[0].clone());
        assert_eq!(menu("A").as_deref(), Some("SPACE"), "accept");
        assert!(menu("B").is_some(), "cancel has a key");
        // Every button has some key in this complete layout.
        for g in GLYPHS {
            assert!(play(g).is_some(), "{g} has no key");
        }
    }

    /// `cargo test -- --ignored --nocapture show`: what each button shows
    /// with the layout in tests/native-keys.txt.
    #[test]
    #[ignore = "prints a table"]
    fn show_the_users_layout() {
        let l = parse_layout(USER_LAYOUT);
        for g in GLYPHS.iter().chain(&["DPad", "LX", "LY", "LSUp", "LSDown", "LSLeft", "LSRight"]) {
            let show = |menu| l.glyph_labels(g, menu).map_or("-".to_string(), |v| v[0].clone());
            println!("{g:8} play {:8} menu {}", show(false), show(true));
        }
    }

    #[test]
    fn glyph_names_and_aliases() {
        assert_eq!(canonical_glyph("start"), Some("MENU"));
        assert_eq!(canonical_glyph("Back"), Some("VIEW"));
        assert_eq!(canonical_glyph("lsb"), Some("L3"));
        assert_eq!(canonical_glyph("dpad"), Some("DPAD"));
        assert_eq!(canonical_glyph("Z"), None);
    }
}
