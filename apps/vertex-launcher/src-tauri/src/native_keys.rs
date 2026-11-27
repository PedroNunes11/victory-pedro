//! Native GTA VI keyboard & mouse controls (`data\native-keys.txt`) and
//! live `[mouse]` settings (`data\keys.txt`), both hot-reloaded by the running
//! Vertex-AXX engine within 1 second (no game restart needed).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Every key and mouse name supported by `keyboard_events.rs` & `gameinput.rs`.
pub const NATIVE_KEY_NAMES: &[&str] = &[
    "LeftClick",
    "RightClick",
    "MiddleClick",
    "Mouse4",
    "Mouse5",
    "WheelUp",
    "WheelDown",
    "Space",
    "Shift",
    "RightShift",
    "Ctrl",
    "RightCtrl",
    "Alt",
    "RightAlt",
    "Tab",
    "Esc",
    "Enter",
    "Backspace",
    "CapsLock",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Comma",
    "Period",
    "Minus",
    "Equals",
    "LeftBracket",
    "RightBracket",
    "Semicolon",
    "Quote",
    "Slash",
    "Backslash",
    "Backquote",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "Numpad0",
    "Numpad1",
    "Numpad2",
    "Numpad3",
    "Numpad4",
    "Numpad5",
    "Numpad6",
    "Numpad7",
    "Numpad8",
    "Numpad9",
];

/// Built-in GTA V Enhanced PC layout tuned for GTA VI (used if
/// `data\native-keys.default.txt` is not present).
pub const DEFAULT_NATIVE_KEYS: &str = r#"# Vertex native keys — GTA V Enhanced PC layout tuned for GTA VI.
# Format: KEY = play actions (vehicle first, then on-foot) | menu actions (pause menu, settings, map).
# Changes apply live while playing (file re-read within 1 s).

W = INPUT_VEH_ACCELERATE, INPUT_VEH_CAR_ACCELERATE, INPUT_VEH_BOAT_ACCELERATE, INPUT_VEH_FLY_THROTTLE_UP, INPUT_VEH_PLANE_THROTTLE_UP, INPUT_VEH_PUSHBIKE_PEDAL_ALT, INPUT_MOVE_UP_ONLY, INPUT_PARACHUTE_PITCH_UP_ONLY, INPUT_SCRIPT_PAD_UP | INPUT_FRONTEND_MAP_MOVE_UD-, INPUT_GAME_MENU_STICK_UP, INPUT_GAME_MENU_UP, INPUT_FRONTEND_UP
S = INPUT_VEH_BRAKE, INPUT_VEH_CAR_BRAKE, INPUT_VEH_BOAT_BRAKE, INPUT_VEH_FLY_THROTTLE_DOWN, INPUT_VEH_PLANE_THROTTLE_DOWN, INPUT_VEH_PUSHBIKE_REAR_BRAKE, INPUT_MOVE_DOWN_ONLY, INPUT_PARACHUTE_PITCH_DOWN_ONLY, INPUT_SCRIPT_PAD_DOWN | INPUT_FRONTEND_MAP_MOVE_UD+, INPUT_GAME_MENU_STICK_DOWN, INPUT_GAME_MENU_DOWN, INPUT_FRONTEND_DOWN
A = INPUT_VEH_CAR_TURN_LEFT_ONLY, INPUT_VEH_BOAT_TURN_LEFT_ONLY, INPUT_VEH_MOVE_LEFT_ONLY, INPUT_VEH_FLY_ROLL_LEFT_ONLY, INPUT_VEH_PLANE_ROLL_LEFT_ONLY, INPUT_MOVE_LEFT_ONLY, INPUT_PARACHUTE_TURN_LEFT_ONLY, INPUT_SCRIPT_PAD_LEFT | INPUT_FRONTEND_MAP_MOVE_LR-, INPUT_GAME_MENU_STICK_LEFT, INPUT_GAME_MENU_LEFT, INPUT_FRONTEND_LEFT
D = INPUT_VEH_CAR_TURN_RIGHT_ONLY, INPUT_VEH_BOAT_TURN_RIGHT_ONLY, INPUT_VEH_MOVE_RIGHT_ONLY, INPUT_VEH_FLY_ROLL_RIGHT_ONLY, INPUT_VEH_PLANE_ROLL_RIGHT_ONLY, INPUT_MOVE_RIGHT_ONLY, INPUT_PARACHUTE_TURN_RIGHT_ONLY, INPUT_SCRIPT_PAD_RIGHT | INPUT_FRONTEND_MAP_MOVE_LR+, INPUT_GAME_MENU_STICK_RIGHT, INPUT_GAME_MENU_RIGHT, INPUT_FRONTEND_RIGHT
Space = INPUT_VEH_HANDBRAKE, INPUT_VEH_JUMP, INPUT_JUMP, INPUT_DIVE, INPUT_MELEE_BLOCK, INPUT_SKIP_CUTSCENE | INPUT_MAP_MENU_ACCEPT, INPUT_GAME_MENU_ACCEPT, INPUT_FRONTEND_ACCEPT
Shift = INPUT_VEH_CAR_BOOST, INPUT_VEH_PUSHBIKE_SPRINT_ALT, INPUT_SPRINT | INPUT_MAP_MENU_LS, INPUT_GAME_MENU_LS, INPUT_FRONTEND_LS
Ctrl = INPUT_VEH_DUCK, INPUT_DUCK, INPUT_SCRIPT_RLEFT | INPUT_MAP_MENU_RS, INPUT_GAME_MENU_RS, INPUT_FRONTEND_RS
LeftClick = INPUT_VEH_CAR_ATTACK, INPUT_VEH_BOAT_ATTACK, INPUT_VEH_PLANE_ATTACK, INPUT_VEH_ATTACK, INPUT_VEH_PASSENGER_ATTACK, INPUT_ATTACK, INPUT_MELEE_GRAPPLE, INPUT_SKIP_CUTSCENE | INPUT_MAP_MENU_ACCEPT, INPUT_GAME_MENU_ACCEPT, INPUT_FRONTEND_ACCEPT
RightClick = INPUT_VEH_CAR_AIM, INPUT_VEH_BOAT_AIM, INPUT_VEH_PLANE_AIM, INPUT_VEH_AIM, INPUT_VEH_PASSENGER_AIM, INPUT_AIM, INPUT_MELEE_STRAFE | INPUT_MAP_MENU_CANCEL, INPUT_GAME_MENU_CANCEL, INPUT_FRONTEND_CANCEL
MiddleClick = INPUT_PHONE, INPUT_MAP_POI | INPUT_MAP_POI, INPUT_GAME_MENU_EXTRA_OPTION
WheelUp = INPUT_RADIAL_MENU_SLOT_NAV_NEXT, INPUT_SNIPER_ZOOM_IN_SECONDARY, INPUT_VEH_NEXT_RADIO | INPUT_GAME_MENU_UP, INPUT_FRONTEND_UP
WheelDown = INPUT_RADIAL_MENU_SLOT_NAV_PREV, INPUT_SNIPER_ZOOM_OUT_SECONDARY, INPUT_VEH_NEXT_RADIO | INPUT_GAME_MENU_DOWN, INPUT_FRONTEND_DOWN
Mouse4 = INPUT_VEH_SELECT_NEXT_WEAPON, INPUT_OPEN_WHEEL_MENU | INPUT_MAP_MENU_CANCEL, INPUT_GAME_MENU_CANCEL, INPUT_FRONTEND_CANCEL
Mouse5 = INPUT_VEH_HEADLIGHT, INPUT_COVER | INPUT_MAP_MENU_ACCEPT, INPUT_GAME_MENU_ACCEPT, INPUT_FRONTEND_ACCEPT
Tab = INPUT_VEH_SELECT_NEXT_WEAPON, INPUT_OPEN_WHEEL_MENU | INPUT_MAP_MENU_EXTRA_OPTION, INPUT_GAME_MENU_EXTRA_OPTION, INPUT_FRONTEND_Y
F = INPUT_VEH_EXIT, INPUT_ENTER, INPUT_PARACHUTE_DEPLOY, INPUT_PARACHUTE_DETACH | INPUT_MAP_MENU_OPTION, INPUT_GAME_MENU_OPTION, INPUT_FRONTEND_X
E = INPUT_VEH_HORN, INPUT_VEH_FLY_VERTICAL_FLIGHT_MODE, INPUT_VEH_GRAPPLING_HOOK, INPUT_CONTEXT, INPUT_TALK, INPUT_WEAPON_SPECIAL_TWO, INPUT_PARACHUTE_BRAKE_RIGHT | INPUT_GAME_MENU_TAB_RIGHT, INPUT_FRONTEND_RB
Q = INPUT_VEH_NEXT_RADIO, INPUT_VEH_PUSHBIKE_FRONT_BRAKE, INPUT_COVER, INPUT_COVER_AIM, INPUT_CONTEXT_SECONDARY, INPUT_PARACHUTE_BRAKE_LEFT | INPUT_GAME_MENU_TAB_LEFT, INPUT_FRONTEND_LB
R = INPUT_VEH_RELOAD, INPUT_RELOAD, INPUT_MELEE_ATTACK_LIGHT, INPUT_MELEE_ATTACK | INPUT_MAP_MENU_OPTION, INPUT_GAME_MENU_OPTION, INPUT_FRONTEND_X
G = INPUT_DETONATE, INPUT_QUICK_USE_ITEM | INPUT_GAME_MENU_EXTRA_OPTION
H = INPUT_VEH_HEADLIGHT, INPUT_VEH_ROOF, INPUT_VEH_SHUFFLE, INPUT_TOGGLE_HOLSTER, INPUT_PICKUP | INPUT_GAME_MENU_EXTRA_OPTION
C = INPUT_LOOK_BEHIND, INPUT_FOCUS_CAM_HOLD | INPUT_FRONTEND_RS
V = INPUT_FRONTEND_SELECT, INPUT_MAP | INPUT_FRONTEND_SELECT
M = INPUT_MAP, INPUT_FRONTEND_SELECT | INPUT_MAP_MENU_CANCEL, INPUT_GAME_MENU_CANCEL, INPUT_FRONTEND_CANCEL
Z = INPUT_REVEAL_HUD, INPUT_SELECT_RADAR_MODE | INPUT_GAME_MENU_OPTION
X = INPUT_VEH_DUCK, INPUT_VEH_DROP_PROJECTILE, INPUT_SWITCH_SHOULDER, INPUT_PARACHUTE_SMOKE | INPUT_FRONTEND_X
B = INPUT_TOGGLE_WEAPON_SCOPE, INPUT_SWITCH_FIRING_MODE | INPUT_FRONTEND_Y
T = INPUT_SWITCH_CHARACTER, INPUT_SHOW_SWITCH_CHARACTER
Esc = INPUT_FRONTEND_PAUSE | INPUT_MAP_MENU_CANCEL, INPUT_GAME_MENU_CANCEL, INPUT_FRONTEND_CANCEL
P = INPUT_FRONTEND_PAUSE | INPUT_FRONTEND_PAUSE
Enter = INPUT_SKIP_CUTSCENE | INPUT_MAP_MENU_ACCEPT, INPUT_GAME_MENU_ACCEPT, INPUT_FRONTEND_ACCEPT
Backspace =  | INPUT_MAP_MENU_CANCEL, INPUT_GAME_MENU_CANCEL, INPUT_FRONTEND_CANCEL
ArrowUp = INPUT_PHONE | INPUT_GAME_MENU_UP, INPUT_FRONTEND_UP
ArrowDown = INPUT_REVEAL_HUD | INPUT_GAME_MENU_DOWN, INPUT_FRONTEND_DOWN
ArrowLeft = INPUT_PLAYER_MENU | INPUT_GAME_MENU_LEFT, INPUT_FRONTEND_LEFT
ArrowRight = INPUT_CONTEXT | INPUT_GAME_MENU_RIGHT, INPUT_FRONTEND_RIGHT
PageUp = INPUT_SCRIPTED_FLY_ZUP | INPUT_FRONTEND_RT
PageDown = INPUT_SCRIPTED_FLY_ZDOWN | INPUT_FRONTEND_LT
Delete =  | INPUT_FRONTEND_DELETE
CapsLock = INPUT_SPECIAL_ABILITY, INPUT_VEH_PUSHBIKE_SPRINT_ALT | INPUT_FRONTEND_SELECT
Comma = INPUT_VEH_NEXT_RADIO
Period = INPUT_VEH_NEXT_RADIO
LeftBracket = INPUT_SNIPER_ZOOM_OUT_SECONDARY, INPUT_SNIPER_ZOOM_OUT_ONLY
RightBracket = INPUT_SNIPER_ZOOM_IN_SECONDARY, INPUT_SNIPER_ZOOM_IN_ONLY
F9 = INPUT_DROP_WEAPON
F10 = INPUT_DROP_AMMO
Numpad4 = INPUT_VEH_FLY_ROLL_LEFT_ONLY, INPUT_VEH_PLANE_ROLL_LEFT_ONLY
Numpad5 = INPUT_VEH_FLY_PITCH_DOWN_ONLY, INPUT_VEH_PLANE_PITCH_DOWN_ONLY
Numpad6 = INPUT_VEH_FLY_ROLL_RIGHT_ONLY, INPUT_VEH_PLANE_ROLL_RIGHT_ONLY
Numpad7 = INPUT_VEH_FLY_SELECT_TARGET_LEFT
Numpad8 = INPUT_VEH_FLY_PITCH_UP_ONLY, INPUT_VEH_PLANE_PITCH_UP_ONLY
Numpad9 = INPUT_VEH_FLY_SELECT_TARGET_RIGHT
"#;

/// One action row for the Controls UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAction {
    pub id: String,
    pub label: String,
    pub category: String,
    pub is_menu: bool,
    pub keys: Vec<String>,
}

/// Full payload read/written by the Controls UI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeControlsData {
    pub path: String,
    pub exists: bool,
    pub actions: Vec<NativeAction>,
    pub mouse_sensitivity: f32,
    pub mouse_invert_y: bool,
}

#[derive(Clone, Debug, Default)]
struct KeyEntry {
    play: Vec<String>,
    menu: Vec<String>,
}

pub fn native_keys_path(data_dir: &Path) -> PathBuf {
    data_dir.join("native-keys.txt")
}

pub fn native_defaults_path(data_dir: &Path) -> PathBuf {
    data_dir.join("native-keys.default.txt")
}

pub fn actions_catalog_path(data_dir: &Path) -> PathBuf {
    data_dir.join("controls-actions.txt")
}

/// Action priority within a single key line (mirrors `make_native_keys.py` and
/// `Controls.ps1`): vehicle first, aircraft/bike, on-foot, parachute, phone,
/// script; menu actions go after `|`.
fn rank(action: &str) -> u8 {
    if action.starts_with("INPUT_VEH_FLY_")
        || action.starts_with("INPUT_VEH_SUB_")
        || action.starts_with("INPUT_VEH_PUSHBIKE_")
        || action.starts_with("INPUT_VEH_PLANE_")
        || action.starts_with("INPUT_VEH_HELI_")
    {
        2
    } else if action.starts_with("INPUT_VEH_") {
        1
    } else if action.starts_with("INPUT_PARACHUTE_") || action == "INPUT_SKIP_CUTSCENE" {
        4
    } else if action.starts_with("INPUT_CELLPHONE_") {
        5
    } else if action.starts_with("INPUT_SCRIPT_") || action.starts_with("INPUT_SCRIPTED_") {
        6
    } else {
        3
    }
}

pub fn is_menu_action(action: &str) -> bool {
    (action.starts_with("INPUT_FRONTEND_")
        || action.starts_with("INPUT_CURSOR_")
        || action.starts_with("INPUT_GAME_MENU_")
        || action.starts_with("INPUT_MAP_MENU_"))
        && action != "INPUT_FRONTEND_PAUSE"
        && action != "INPUT_FRONTEND_SELECT"
}

pub fn category_of(action: &str) -> &'static str {
    if action.starts_with("INPUT_VEH_FLY_")
        || action.starts_with("INPUT_VEH_PLANE_")
        || action.starts_with("INPUT_VEH_HELI_")
    {
        "Aircraft"
    } else if action.starts_with("INPUT_VEH_SUB_") || action.starts_with("INPUT_VEH_BOAT_") {
        "Boat / Sub"
    } else if action.starts_with("INPUT_VEH_PUSHBIKE_") {
        "Bicycle"
    } else if action.starts_with("INPUT_VEH_") {
        "Vehicle"
    } else if action.starts_with("INPUT_PARACHUTE_") {
        "Parachute"
    } else if action.starts_with("INPUT_CELLPHONE_") || action.starts_with("INPUT_PHONE") {
        "Phone"
    } else if action.starts_with("INPUT_FRONTEND_")
        || action.starts_with("INPUT_CURSOR_")
        || action.starts_with("INPUT_GAME_MENU_")
        || action.starts_with("INPUT_MAP_MENU_")
        || action.starts_with("INPUT_SHOP_MENU_")
        || action == "INPUT_MAP"
        || action == "INPUT_MAP_POI"
    {
        "Menus & Map"
    } else if action.starts_with("INPUT_MELEE_")
        || action.contains("ATTACK")
        || action.contains("AIM")
        || action.contains("RELOAD")
        || action.contains("WEAPON")
        || action.contains("SNIPER")
        || action.contains("HOLSTER")
        || action.contains("COVER")
        || action == "INPUT_DETONATE"
        || action == "INPUT_OPEN_WHEEL_MENU"
        || action == "INPUT_RADIAL_MENU_SLOT_NAV_NEXT"
        || action == "INPUT_RADIAL_MENU_SLOT_NAV_PREV"
        || action == "INPUT_SWITCH_SHOULDER"
        || action == "INPUT_SWITCH_FIRING_MODE"
        || action == "INPUT_DROP_WEAPON"
        || action == "INPUT_DROP_AMMO"
    {
        "Combat & Weapons"
    } else if action.starts_with("INPUT_SCRIPT_")
        || action.starts_with("INPUT_SCRIPTED_")
        || action.starts_with("INPUT_MINIGAME_")
        || action.starts_with("INPUT_REPLAY_")
        || action.starts_with("INPUT_CREATOR_")
    {
        "Other"
    } else {
        "On Foot"
    }
}

pub fn pretty_label(action: &str) -> String {
    match action {
        "INPUT_MOVE_UP_ONLY" => return "Move Forward".into(),
        "INPUT_MOVE_DOWN_ONLY" => return "Move Backward".into(),
        "INPUT_MOVE_LEFT_ONLY" => return "Move Left".into(),
        "INPUT_MOVE_RIGHT_ONLY" => return "Move Right".into(),
        "INPUT_SPRINT" => return "Sprint".into(),
        "INPUT_JUMP" => return "Jump".into(),
        "INPUT_DUCK" => return "Crouch / Sneak".into(),
        "INPUT_ENTER" => return "Enter Vehicle / Interact".into(),
        "INPUT_CONTEXT" => return "Context Action / Interact".into(),
        "INPUT_ATTACK" => return "Shoot / Primary Attack".into(),
        "INPUT_AIM" => return "Aim Down Sights".into(),
        "INPUT_RELOAD" => return "Reload Weapon".into(),
        "INPUT_COVER" => return "Take Cover".into(),
        "INPUT_OPEN_WHEEL_MENU" => return "Weapon Wheel".into(),
        "INPUT_TOGGLE_HOLSTER" => return "Holster / Unholster".into(),
        "INPUT_SWITCH_SHOULDER" => return "Switch Shoulder".into(),
        "INPUT_LOOK_BEHIND" => return "Look Behind".into(),
        "INPUT_VEH_ACCELERATE" => return "Accelerate".into(),
        "INPUT_VEH_BRAKE" => return "Brake / Reverse".into(),
        "INPUT_VEH_CAR_TURN_LEFT_ONLY" | "INPUT_VEH_MOVE_LEFT_ONLY" => return "Steer Left".into(),
        "INPUT_VEH_CAR_TURN_RIGHT_ONLY" | "INPUT_VEH_MOVE_RIGHT_ONLY" => {
            return "Steer Right".into();
        }
        "INPUT_VEH_HANDBRAKE" => return "Handbrake".into(),
        "INPUT_VEH_HORN" => return "Horn".into(),
        "INPUT_VEH_HEADLIGHT" => return "Headlights".into(),
        "INPUT_VEH_EXIT" => return "Exit Vehicle".into(),
        "INPUT_VEH_NEXT_RADIO" => return "Next Radio Station".into(),
        "INPUT_VEH_SELECT_NEXT_WEAPON" => return "Cycle Vehicle Weapon".into(),
        "INPUT_VEH_ATTACK" => return "Drive-By Fire".into(),
        "INPUT_VEH_AIM" => return "Drive-By Aim".into(),
        "INPUT_FRONTEND_PAUSE" => return "Pause Menu".into(),
        "INPUT_MAP" => return "Open Map".into(),
        "INPUT_FRONTEND_ACCEPT" => return "Menu Confirm".into(),
        "INPUT_FRONTEND_CANCEL" => return "Menu Back".into(),
        "INPUT_PHONE" => return "Open Phone".into(),
        "INPUT_SWITCH_CHARACTER" => return "Switch Character".into(),
        "INPUT_REVEAL_HUD" => return "Expand Radar / Reveal HUD".into(),
        _ => {}
    }
    let trimmed = action
        .strip_prefix("INPUT_")
        .unwrap_or(action);
    let trimmed = trimmed
        .strip_prefix("VEH_FLY_")
        .or_else(|| trimmed.strip_prefix("VEH_PLANE_"))
        .or_else(|| trimmed.strip_prefix("VEH_SUB_"))
        .or_else(|| trimmed.strip_prefix("VEH_PUSHBIKE_"))
        .or_else(|| trimmed.strip_prefix("VEH_CAR_"))
        .or_else(|| trimmed.strip_prefix("VEH_BOAT_"))
        .or_else(|| trimmed.strip_prefix("VEH_"))
        .or_else(|| trimmed.strip_prefix("CELLPHONE_"))
        .or_else(|| trimmed.strip_prefix("FRONTEND_"))
        .or_else(|| trimmed.strip_prefix("PARACHUTE_"))
        .or_else(|| trimmed.strip_prefix("GAME_MENU_"))
        .or_else(|| trimmed.strip_prefix("MAP_MENU_"))
        .unwrap_or(trimmed);
    trimmed
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_layout(text: &str) -> Vec<(String, KeyEntry)> {
    let mut out: Vec<(String, KeyEntry)> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let (play_part, menu_part) = match rest.split_once('|') {
            Some((p, m)) => (p, m),
            None => (rest, ""),
        };
        let split_list = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        };
        let entry = KeyEntry {
            play: split_list(play_part),
            menu: split_list(menu_part),
        };
        if let Some(&idx) = index.get(&key) {
            out[idx].1 = entry;
        } else {
            index.insert(key.clone(), out.len());
            out.push((key, entry));
        }
    }
    out
}

fn format_layout(entries: &[(String, KeyEntry)]) -> String {
    let mut out = String::from(
        "# Vertex native keys (edited in Vertex Launcher).\r\n\
         # Format: KEY = play actions | menu actions. Changes apply live in game within 1 second.\r\n\r\n",
    );
    for (key, entry) in entries {
        if entry.play.is_empty() && entry.menu.is_empty() {
            continue;
        }
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(&entry.play.join(", "));
        if !entry.menu.is_empty() {
            out.push_str(" | ");
            out.push_str(&entry.menu.join(", "));
        }
        out.push_str("\r\n");
    }
    out
}

fn read_catalog(data_dir: &Path) -> Vec<String> {
    let mut set = BTreeSet::new();
    if let Ok(text) = std::fs::read_to_string(actions_catalog_path(data_dir)) {
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let _hash = parts.next();
            if let Some(name) = parts.next()
                && name.starts_with("INPUT_")
                && !name.starts_with("INPUT_REPLAY_")
                && !name.starts_with("INPUT_CREATOR_")
                && !name.starts_with("INPUT_MINIGAME_")
            {
                set.insert(name.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// Reads `[mouse]` `SENSITIVITY` and `INVERT_Y` from `data\keys.txt` (if present).
pub fn read_mouse_from_keys_txt(data_dir: &Path) -> (f32, bool) {
    let mut sens = 1.0f32;
    let mut invert = false;
    let Ok(text) = std::fs::read_to_string(data_dir.join("keys.txt")) else {
        return (sens, invert);
    };
    let mut in_mouse = false;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_mouse = sec.trim().eq_ignore_ascii_case("mouse");
            continue;
        }
        if !in_mouse {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim().to_ascii_uppercase().as_str() {
                "SENSITIVITY" => {
                    if let Ok(parsed) = v.trim().parse::<f32>()
                        && parsed.is_finite()
                        && parsed > 0.0
                    {
                        sens = parsed.clamp(0.1, 10.0);
                    }
                }
                "INVERT_Y" => {
                    invert = matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "yes" | "true" | "on"
                    );
                }
                _ => {}
            }
        }
    }
    (sens, invert)
}

/// Updates ONLY the `[mouse]` section of `data\keys.txt` atomically so the
/// running game reloads sensitivity & invert Y within 1 second without restart.
pub fn write_mouse_to_keys_txt(data_dir: &Path, sensitivity: f32, invert_y: bool) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let path = data_dir.join("keys.txt");
    let sens = sensitivity.clamp(0.1, 10.0);
    let inv_str = if invert_y { "yes" } else { "no" };
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| crate::keys::write(&crate::keys::KeyFile::defaults()));
    let mut out = Vec::new();
    let mut in_mouse = false;
    let mut wrote_section = false;
    let mut wrote_sens = false;
    let mut wrote_inv = false;
    for line in existing.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if let Some(sec) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if in_mouse {
                if !wrote_sens {
                    out.push(format!("SENSITIVITY = {sens:.2}"));
                    wrote_sens = true;
                }
                if !wrote_inv {
                    out.push(format!("INVERT_Y = {inv_str}"));
                    wrote_inv = true;
                }
            }
            in_mouse = sec.trim().eq_ignore_ascii_case("mouse");
            if in_mouse {
                wrote_section = true;
            }
            out.push(line.to_string());
            continue;
        }
        if in_mouse && let Some((k, _)) = trimmed.split_once('=') {
            match k.trim().to_ascii_uppercase().as_str() {
                "SENSITIVITY" => {
                    out.push(format!("SENSITIVITY = {sens:.2}"));
                    wrote_sens = true;
                    continue;
                }
                "INVERT_Y" => {
                    out.push(format!("INVERT_Y = {inv_str}"));
                    wrote_inv = true;
                    continue;
                }
                _ => {}
            }
        }
        out.push(line.to_string());
    }
    if in_mouse {
        if !wrote_sens {
            out.push(format!("SENSITIVITY = {sens:.2}"));
        }
        if !wrote_inv {
            out.push(format!("INVERT_Y = {inv_str}"));
        }
    } else if !wrote_section {
        out.push(String::new());
        out.push("[mouse]".into());
        out.push(format!("SENSITIVITY = {sens:.2}"));
        out.push(format!("INVERT_Y = {inv_str}"));
    }
    let content = out.join("\r\n") + "\r\n";
    let temp = path.with_extension("txt.vertex-mouse-tmp");
    std::fs::write(&temp, content).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn load(data_dir: &Path) -> NativeControlsData {
    let path = native_keys_path(data_dir);
    let raw = std::fs::read_to_string(&path).ok();
    let exists = raw.is_some();
    let text = raw
        .or_else(|| std::fs::read_to_string(native_defaults_path(data_dir)).ok())
        .unwrap_or_else(|| DEFAULT_NATIVE_KEYS.to_string());
    let layout = parse_layout(&text);

    // Collect every action ID: all bound actions + catalog.
    let mut all_ids = BTreeSet::new();
    let mut keys_by_action: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, entry) in &layout {
        for a in entry.play.iter().chain(entry.menu.iter()) {
            all_ids.insert(a.clone());
            let list = keys_by_action.entry(a.clone()).or_default();
            if !list.contains(key) {
                list.push(key.clone());
            }
        }
    }
    for a in read_catalog(data_dir) {
        all_ids.insert(a);
    }

    let mut actions: Vec<NativeAction> = all_ids
        .into_iter()
        .map(|id| {
            let keys = keys_by_action.remove(&id).unwrap_or_default();
            let label = pretty_label(&id);
            let category = category_of(&id).to_string();
            let is_menu = is_menu_action(&id);
            NativeAction {
                id,
                label,
                category,
                is_menu,
                keys,
            }
        })
        .collect();
    // Bound actions first within each category, then alphabetical by label.
    actions.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| b.keys.is_empty().cmp(&a.keys.is_empty()).reverse())
            .then_with(|| a.label.cmp(&b.label))
    });

    let (mouse_sensitivity, mouse_invert_y) = read_mouse_from_keys_txt(data_dir);
    NativeControlsData {
        path: path.display().to_string(),
        exists,
        actions,
        mouse_sensitivity,
        mouse_invert_y,
    }
}

/// Writes `actions` back to `data\native-keys.txt` atomically, preserving
/// proper per-key priority ordering (`play | menu`).
pub fn save(data_dir: &Path, actions: &[NativeAction]) -> Result<String, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let mut by_key: BTreeMap<String, KeyEntry> = BTreeMap::new();
    for action in actions {
        let menu = is_menu_action(&action.id);
        for key in &action.keys {
            let k = key.trim();
            if k.is_empty() {
                continue;
            }
            let entry = by_key.entry(k.to_string()).or_default();
            let target = if menu { &mut entry.menu } else { &mut entry.play };
            if !target.contains(&action.id) {
                target.push(action.id.clone());
            }
        }
    }
    for entry in by_key.values_mut() {
        entry.play.sort_by_key(|a| rank(a));
    }
    let ordered: Vec<(String, KeyEntry)> = by_key.into_iter().collect();
    let content = format_layout(&ordered);
    let path = native_keys_path(data_dir);
    let temp = path.with_extension("txt.vertex-native-tmp");
    std::fs::write(&temp, content).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// Resets `data\native-keys.txt` from `data\native-keys.default.txt` (or the
/// built-in GTA VI layout) and returns the freshly loaded controls.
pub fn reset(data_dir: &Path) -> Result<NativeControlsData, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("{}: {e}", data_dir.display()))?;
    let text = std::fs::read_to_string(native_defaults_path(data_dir))
        .unwrap_or_else(|_| DEFAULT_NATIVE_KEYS.to_string());
    let path = native_keys_path(data_dir);
    let temp = path.with_extension("txt.vertex-reset-tmp");
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(load(data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_native_keys_preserves_bindings_and_order() {
        let dir = std::env::temp_dir().join(format!("vertex-nk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = reset(&dir).unwrap();
        assert!(data.exists);
        assert!(data.actions.iter().any(|a| a.id == "INPUT_VEH_ACCELERATE" && a.keys.contains(&"W".to_string())));
        assert!(data.actions.iter().any(|a| a.id == "INPUT_ATTACK" && a.keys.contains(&"LeftClick".to_string())));
        save(&dir, &data.actions).unwrap();
        let reloaded = load(&dir);
        let w_veh = reloaded.actions.iter().find(|a| a.id == "INPUT_VEH_ACCELERATE").unwrap();
        assert_eq!(w_veh.keys, vec!["W".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_mouse_section_round_trips() {
        let dir = std::env::temp_dir().join(format!("vertex-mouse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_mouse_to_keys_txt(&dir, 1.75, true).unwrap();
        let (s, inv) = read_mouse_from_keys_txt(&dir);
        assert!((s - 1.75).abs() < 0.01);
        assert!(inv);
        write_mouse_to_keys_txt(&dir, 0.85, false).unwrap();
        let (s2, inv2) = read_mouse_from_keys_txt(&dir);
        assert!((s2 - 0.85).abs() < 0.01);
        assert!(!inv2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
