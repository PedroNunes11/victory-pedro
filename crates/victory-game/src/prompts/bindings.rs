//! Dynamic prompt-glyph bindings derived from `native-keys.txt` (primary) or
//! `keys.txt` (fallback), hot-reloaded whenever the user applies changes in
//! Vertex Settings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use victory_core::pad::{KeySpec, MOUSE_MOVES, PadControl, prompt_key};

use super::actions::BINDINGS;

/// Resolved key/combo shown in prompts for every [`PadControl`], plus a
/// deterministic fingerprint so [`super::hook`] only redraws the GFx icon movie
/// when a binding actually changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptBindings {
    keys: [KeySpec; 24],
    pub fingerprint: u64,
}

impl PromptBindings {
    /// Default PC layout bindings (`pc == true`) or classic stand-in pad bindings.
    #[must_use]
    pub fn defaults(pc: bool) -> Self {
        let mut keys = [KeySpec::plain(0); 24];
        for (i, &control) in PadControl::ALL.iter().enumerate() {
            keys[i] = KeySpec::plain(prompt_key(control, pc));
        }
        Self::from_array(keys)
    }

    fn from_array(keys: [KeySpec; 24]) -> Self {
        // FNV-1a 64-bit over the 24 (modifiers, key) pairs.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for k in keys {
            h ^= u64::from(k.modifiers.bits());
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            h ^= u64::from(k.key);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            keys,
            fingerprint: h,
        }
    }

    /// The bound key/combo for `control`.
    #[must_use]
    pub fn key_of(&self, control: PadControl) -> KeySpec {
        let idx = control_index(control);
        self.keys[idx]
    }

    /// Returns a copy with `control` overridden to `spec`.
    #[must_use]
    pub fn with(mut self, control: PadControl, spec: KeySpec) -> Self {
        self.keys[control_index(control)] = spec;
        Self::from_array(self.keys)
    }

    /// Derives prompt bindings from a `native-keys.txt` file's contents.
    ///
    /// Supports both single keys (`E = INPUT_CONTEXT`) and modifier combinations
    /// (`Ctrl+E = INPUT_CONTEXT`), plus optional explicit prompt overrides via
    /// `# @prompt <BUTTON> = <KeySpec>` comments.
    #[must_use]
    pub fn from_native_keys(text: &str) -> Self {
        let mut keys = Self::defaults(true).keys;
        let mut lines: Vec<(KeySpec, Vec<String>, Vec<String>)> = Vec::new();
        let mut overrides: Vec<(PadControl, KeySpec)> = Vec::new();

        for raw in text.lines() {
            let trimmed = raw.trim();
            if let Some(rest) = trimmed
                .strip_prefix("# @prompt")
                .or_else(|| trimmed.strip_prefix("#@prompt"))
            {
                if let Some((lhs, rhs)) = rest.split_once('=')
                    && let Some(ctrl) = parse_pad_control(lhs.trim())
                    && let Some(spec) = KeySpec::parse(rhs.trim())
                {
                    overrides.push((ctrl, spec));
                }
                continue;
            }
            let body = raw.split('#').next().unwrap_or("").trim();
            let Some((lhs, rhs)) = body.split_once('=') else {
                continue;
            };
            let Some(spec) = KeySpec::parse(lhs) else {
                continue;
            };
            let (play_part, menu_part) = rhs.split_once('|').unwrap_or((rhs, ""));
            let split = |s: &str| -> Vec<String> {
                s.split(',')
                    .map(|a| a.trim().trim_end_matches(['+', '-']).trim())
                    .filter(|a| !a.is_empty())
                    .map(str::to_ascii_uppercase)
                    .collect()
            };
            let play = split(play_part);
            let menu = split(menu_part);
            if !play.is_empty() || !menu.is_empty() {
                lines.push((spec, play, menu));
            }
        }

        // Build lookup: action_name -> (pad_source_name, half)
        let mut action_pad: HashMap<&str, (&str, u8)> = HashMap::with_capacity(BINDINGS.len());
        for &(action, source, half) in BINDINGS {
            action_pad.entry(action).or_insert((source, half));
        }

        // Direct movement stick mappings (so rebinding WASD -> ESDF/Arrows updates LS cluster).
        let direct_axes: &[(PadControl, &str)] = &[
            (PadControl::LeftStickUp, "INPUT_MOVE_UP_ONLY"),
            (PadControl::LeftStickDown, "INPUT_MOVE_DOWN_ONLY"),
            (PadControl::LeftStickLeft, "INPUT_MOVE_LEFT_ONLY"),
            (PadControl::LeftStickRight, "INPUT_MOVE_RIGHT_ONLY"),
        ];
        for &(ctrl, target_action) in direct_axes {
            if let Some((spec, _, _)) = lines
                .iter()
                .find(|(_, play, _)| play.iter().any(|a| a == target_action))
            {
                keys[control_index(ctrl)] = *spec;
            }
        }

        // Right stick always remains Mouse in native KB/M mode.
        for ctrl in [
            PadControl::RightStickUp,
            PadControl::RightStickDown,
            PadControl::RightStickLeft,
            PadControl::RightStickRight,
        ] {
            keys[control_index(ctrl)] = KeySpec::plain(MOUSE_MOVES);
        }

        // Weighted action scoring per console pad button.
        let button_sources: &[(PadControl, &str)] = &[
            (PadControl::A, "A"),
            (PadControl::B, "B"),
            (PadControl::X, "X"),
            (PadControl::Y, "Y"),
            (PadControl::LeftShoulder, "LB"),
            (PadControl::RightShoulder, "RB"),
            (PadControl::LeftTrigger, "LT"),
            (PadControl::RightTrigger, "RT"),
            (PadControl::LeftStick, "L3"),
            (PadControl::RightStick, "R3"),
            (PadControl::Up, "DUp"),
            (PadControl::Down, "DDown"),
            (PadControl::Left, "DLeft"),
            (PadControl::Right, "DRight"),
            (PadControl::Menu, "Menu"),
            (PadControl::View, "View"),
        ];

        for &(ctrl, source_name) in button_sources {
            if let Some(best) = best_key_for_source(&lines, &action_pad, source_name) {
                keys[control_index(ctrl)] = best;
            }
        }

        // Explicit `# @prompt` overrides win last.
        for (ctrl, spec) in overrides {
            keys[control_index(ctrl)] = spec;
        }

        Self::from_array(keys)
    }

    /// Derives prompt bindings from legacy `keys.txt` (`[walk]` section).
    #[must_use]
    pub fn from_keys_txt(text: &str) -> Self {
        let mut keys = Self::defaults(false).keys;
        let mut section = String::new();
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name.trim().to_ascii_lowercase();
                continue;
            }
            if section != "walk" {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once('=') else {
                continue;
            };
            let Some(ctrl) = parse_pad_control(lhs.trim()) else {
                continue;
            };
            if let Some(first_key) = rhs
                .split(',')
                .filter_map(KeySpec::parse)
                .next()
            {
                keys[control_index(ctrl)] = first_key;
            }
        }
        Self::from_array(keys)
    }
}

fn action_weight(action: &str) -> u32 {
    if action.starts_with("INPUT_CONTEXT") {
        12
    } else if matches!(
        action,
        "INPUT_JUMP"
            | "INPUT_SPRINT"
            | "INPUT_DUCK"
            | "INPUT_ENTER"
            | "INPUT_RELOAD"
            | "INPUT_ATTACK"
            | "INPUT_AIM"
            | "INPUT_COVER"
            | "INPUT_OPEN_WHEEL_MENU"
            | "INPUT_FRONTEND_PAUSE"
            | "INPUT_MAP"
            | "INPUT_PHONE"
            | "INPUT_LOOK_BEHIND"
    ) {
        8
    } else if action.starts_with("INPUT_SCRIPT_") {
        4
    } else if action.starts_with("INPUT_VEH_")
        || action.starts_with("INPUT_PARACHUTE_")
        || action.starts_with("INPUT_MINIGAME_")
        || action.starts_with("INPUT_SHOP_")
        || action.starts_with("INPUT_FRONTEND_")
        || action.starts_with("INPUT_GAME_MENU_")
        || action.starts_with("INPUT_MAP_MENU_")
        || action.starts_with("INPUT_CINEMATIC_")
    {
        1
    } else {
        2
    }
}

fn best_key_for_source(
    lines: &[(KeySpec, Vec<String>, Vec<String>)],
    action_pad: &HashMap<&str, (&str, u8)>,
    source_name: &str,
) -> Option<KeySpec> {
    let mut best: Option<(u32, KeySpec)> = None;
    for (spec, play, menu) in lines {
        let mut score = 0u32;
        for a in play {
            if let Some(&(src, _)) = action_pad.get(a.as_str())
                && src == source_name
            {
                score = score.saturating_add(action_weight(a));
            }
        }
        // If no play action matched, allow menu actions at low weight so View/Menu/Cancel resolve.
        if score == 0 {
            for a in menu {
                if let Some(&(src, _)) = action_pad.get(a.as_str())
                    && src == source_name
                {
                    score = score.saturating_add(1);
                }
            }
        }
        if score > 0 && best.is_none_or(|(best_score, _)| score > best_score) {
            best = Some((score, *spec));
        }
    }
    best.map(|(_, spec)| spec)
}

fn control_index(control: PadControl) -> usize {
    PadControl::ALL
        .iter()
        .position(|&c| c == control)
        .unwrap_or(0)
}

fn parse_pad_control(name: &str) -> Option<PadControl> {
    use PadControl as C;
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "MENU" | "START" => C::Menu,
        "VIEW" | "BACK" | "SELECT" => C::View,
        "A" => C::A,
        "B" => C::B,
        "X" => C::X,
        "Y" => C::Y,
        "UP" | "DUP" => C::Up,
        "DOWN" | "DDOWN" => C::Down,
        "LEFT" | "DLEFT" => C::Left,
        "RIGHT" | "DRIGHT" => C::Right,
        "LB" | "LEFTSHOULDER" => C::LeftShoulder,
        "RB" | "RIGHTSHOULDER" => C::RightShoulder,
        "L3" | "LS" | "LSB" | "LEFTSTICK" => C::LeftStick,
        "R3" | "RS" | "RSB" | "RIGHTSTICK" => C::RightStick,
        "LT" | "LEFTTRIGGER" => C::LeftTrigger,
        "RT" | "RIGHTTRIGGER" => C::RightTrigger,
        "MOVE_UP" | "LSUP" | "LEFTSTICKUP" => C::LeftStickUp,
        "MOVE_DOWN" | "LSDOWN" | "LEFTSTICKDOWN" => C::LeftStickDown,
        "MOVE_LEFT" | "LSLEFT" | "LEFTSTICKLEFT" => C::LeftStickLeft,
        "MOVE_RIGHT" | "LSRIGHT" | "LEFTSTICKRIGHT" => C::LeftStickRight,
        "LOOK_UP" | "RSUP" | "RIGHTSTICKUP" => C::RightStickUp,
        "LOOK_DOWN" | "RSDOWN" | "RIGHTSTICKDOWN" => C::RightStickDown,
        "LOOK_LEFT" | "RSLEFT" | "RIGHTSTICKLEFT" => C::RightStickLeft,
        "LOOK_RIGHT" | "RSRIGHT" | "RIGHTSTICKRIGHT" => C::RightStickRight,
        _ => return None,
    })
}

struct LiveBindings {
    data_dir: Option<PathBuf>,
    looked: Option<Instant>,
    source: Option<(PathBuf, Option<SystemTime>)>,
    current: PromptBindings,
}

static LIVE: Mutex<Option<LiveBindings>> = Mutex::new(None);

/// Configures the data directory used to locate `native-keys.txt` / `keys.txt`.
pub fn set_data_dir(dir: PathBuf) {
    let mut guard = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    *guard = Some(LiveBindings {
        data_dir: Some(dir),
        looked: None,
        source: None,
        current: PromptBindings::defaults(true),
    });
}

fn resolve_binding_file(data_dir: Option<&Path>) -> Option<(PathBuf, bool)> {
    if let Some(env_path) = std::env::var_os("VICTORY_NATIVE_KEYS").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Some((p, true));
        }
    }
    let dir = data_dir.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("VICTORY_DATA_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    if let Some(dir) = dir {
        let native = dir.join("native-keys.txt");
        if native.is_file() {
            return Some((native, true));
        }
        let legacy = dir.join("keys.txt");
        if legacy.is_file() {
            return Some((legacy, false));
        }
    }
    None
}

/// Returns the current live prompt bindings, re-reading `native-keys.txt` or
/// `keys.txt` at most once per second if its modification timestamp changed.
#[must_use]
pub fn current() -> PromptBindings {
    let mut guard = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    let state = guard.get_or_insert_with(|| LiveBindings {
        data_dir: None,
        looked: None,
        source: None,
        current: PromptBindings::defaults(true),
    });
    let now = Instant::now();
    if state
        .looked
        .is_some_and(|t| now.duration_since(t) < Duration::from_secs(1))
    {
        return state.current.clone();
    }
    state.looked = Some(now);
    let Some((path, is_native)) = resolve_binding_file(state.data_dir.as_deref()) else {
        return state.current.clone();
    };
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if state
        .source
        .as_ref()
        .is_some_and(|(p, t)| *p == path && *t == mtime)
    {
        return state.current.clone();
    }
    if let Ok(text) = std::fs::read_to_string(&path) {
        let parsed = if is_native {
            PromptBindings::from_native_keys(&text)
        } else {
            PromptBindings::from_keys_txt(&text)
        };
        state.source = Some((path, mtime));
        state.current = parsed;
    }
    state.current.clone()
}

#[cfg(test)]
mod tests {
    use victory_core::pad::{Modifiers, VK_ESCAPE, VK_LBUTTON, VK_LSHIFT, VK_RBUTTON, VK_SPACE, VK_TAB};

    use super::*;

    #[test]
    fn native_keys_derive_expected_controls_and_respect_rebinds_and_combos() {
        let text = "\
W = INPUT_MOVE_UP_ONLY
S = INPUT_MOVE_DOWN_ONLY
A = INPUT_MOVE_LEFT_ONLY
D = INPUT_MOVE_RIGHT_ONLY
Space = INPUT_JUMP | INPUT_FRONTEND_ACCEPT
Shift = INPUT_SPRINT
LeftClick = INPUT_ATTACK
RightClick = INPUT_AIM
Tab = INPUT_OPEN_WHEEL_MENU
Q = INPUT_COVER
R = INPUT_RELOAD
Ctrl+E = INPUT_CONTEXT
F = INPUT_ENTER
Esc = INPUT_FRONTEND_PAUSE | INPUT_FRONTEND_CANCEL
";
        let b = PromptBindings::from_native_keys(text);
        assert_eq!(b.key_of(PadControl::LeftStickUp), KeySpec::plain(b'W'));
        assert_eq!(b.key_of(PadControl::A), KeySpec::plain(VK_SPACE));
        assert_eq!(b.key_of(PadControl::LeftStick), KeySpec::plain(VK_LSHIFT));
        assert_eq!(b.key_of(PadControl::RightTrigger), KeySpec::plain(VK_LBUTTON));
        assert_eq!(b.key_of(PadControl::LeftTrigger), KeySpec::plain(VK_RBUTTON));
        assert_eq!(b.key_of(PadControl::LeftShoulder), KeySpec::plain(VK_TAB));
        assert_eq!(b.key_of(PadControl::RightShoulder), KeySpec::plain(b'Q'));
        assert_eq!(b.key_of(PadControl::X), KeySpec::plain(b'R'));
        assert_eq!(b.key_of(PadControl::Y), KeySpec::plain(b'F'));
        assert_eq!(b.key_of(PadControl::Menu), KeySpec::plain(VK_ESCAPE));
        // Modifier combination propagates to the D-pad right prompt!
        assert_eq!(
            b.key_of(PadControl::Right),
            KeySpec::combo(Modifiers::CTRL, b'E')
        );
        // Rebinding Jump changes A and changes the fingerprint.
        let b2 = PromptBindings::from_native_keys(&text.replace("Space = INPUT_JUMP", "G = INPUT_JUMP"));
        assert_eq!(b2.key_of(PadControl::A), KeySpec::plain(b'G'));
        assert_ne!(b.fingerprint, b2.fingerprint);
    }

    #[test]
    fn explicit_prompt_override_takes_precedence() {
        let text = "# @prompt LB = Alt+F4\nTab = INPUT_OPEN_WHEEL_MENU\n";
        let b = PromptBindings::from_native_keys(text);
        assert_eq!(
            b.key_of(PadControl::LeftShoulder),
            KeySpec::combo(Modifiers::ALT, victory_core::pad::VK_F4)
        );
    }
}
