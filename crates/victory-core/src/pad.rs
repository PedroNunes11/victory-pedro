//! Shared gamepad controls, PC key/modifier specifications, active input-device
//! state, and prompt-glyph mode.
//!
//! Both input translation (`victory-hle`, `victory-loader`) and vector button
//! prompts (`victory-game`) read and update this module so gameplay input and
//! displayed prompts always agree.

use std::sync::atomic::{AtomicU8, Ordering};

// --- Mouse virtual keys & synthetic codes ---
pub const VK_LBUTTON: u8 = 0x01;
pub const VK_RBUTTON: u8 = 0x02;
pub const VK_MBUTTON: u8 = 0x04;
pub const VK_XBUTTON1: u8 = 0x05;
pub const VK_XBUTTON2: u8 = 0x06;
/// Synthetic code (not a Windows VK): mouse scroll wheel up.
pub const MOUSE_WHEEL_UP: u8 = 0xFD;
/// Synthetic code (not a Windows VK): mouse scroll wheel down.
pub const MOUSE_WHEEL_DOWN: u8 = 0xFE;
/// Synthetic code (not a Windows VK): mouse movement (camera / right stick).
pub const MOUSE_MOVES: u8 = 0xFF;

// --- Core & navigation virtual keys ---
pub const VK_BACK: u8 = 0x08;
pub const VK_TAB: u8 = 0x09;
pub const VK_RETURN: u8 = 0x0D;
pub const VK_SHIFT: u8 = 0x10;
pub const VK_CONTROL: u8 = 0x11;
pub const VK_MENU: u8 = 0x12;
pub const VK_CAPITAL: u8 = 0x14;
pub const VK_ESCAPE: u8 = 0x1B;
pub const VK_SPACE: u8 = 0x20;
pub const VK_PRIOR: u8 = 0x21; // PageUp
pub const VK_NEXT: u8 = 0x22; // PageDown
pub const VK_END: u8 = 0x23;
pub const VK_HOME: u8 = 0x24;
pub const VK_LEFT: u8 = 0x25;
pub const VK_UP: u8 = 0x26;
pub const VK_RIGHT: u8 = 0x27;
pub const VK_DOWN: u8 = 0x28;
pub const VK_INSERT: u8 = 0x2D;
pub const VK_DELETE: u8 = 0x2E;

// --- Numpad virtual keys ---
pub const VK_NUMPAD0: u8 = 0x60;
pub const VK_NUMPAD1: u8 = 0x61;
pub const VK_NUMPAD2: u8 = 0x62;
pub const VK_NUMPAD3: u8 = 0x63;
pub const VK_NUMPAD4: u8 = 0x64;
pub const VK_NUMPAD5: u8 = 0x65;
pub const VK_NUMPAD6: u8 = 0x66;
pub const VK_NUMPAD7: u8 = 0x67;
pub const VK_NUMPAD8: u8 = 0x68;
pub const VK_NUMPAD9: u8 = 0x69;
pub const VK_MULTIPLY: u8 = 0x6A;
pub const VK_ADD: u8 = 0x6B;
pub const VK_SUBTRACT: u8 = 0x6D;
pub const VK_DECIMAL: u8 = 0x6E;
pub const VK_DIVIDE: u8 = 0x6F;

// --- Function keys F1–F12 ---
pub const VK_F1: u8 = 0x70;
pub const VK_F2: u8 = 0x71;
pub const VK_F3: u8 = 0x72;
pub const VK_F4: u8 = 0x73;
pub const VK_F5: u8 = 0x74;
pub const VK_F6: u8 = 0x75;
pub const VK_F7: u8 = 0x76;
pub const VK_F8: u8 = 0x77;
pub const VK_F9: u8 = 0x78;
pub const VK_F10: u8 = 0x79;
pub const VK_F11: u8 = 0x7A;
pub const VK_F12: u8 = 0x7B;

// --- Left/Right modifier keys ---
pub const VK_LSHIFT: u8 = 0xA0;
pub const VK_RSHIFT: u8 = 0xA1;
pub const VK_LCONTROL: u8 = 0xA2;
pub const VK_RCONTROL: u8 = 0xA3;
pub const VK_LMENU: u8 = 0xA4;
pub const VK_RMENU: u8 = 0xA5;

// --- Punctuation / OEM keys ---
pub const VK_OEM_1: u8 = 0xBA; // ;
pub const VK_OEM_PLUS: u8 = 0xBB; // =
pub const VK_OEM_COMMA: u8 = 0xBC; // ,
pub const VK_OEM_MINUS: u8 = 0xBD; // -
pub const VK_OEM_PERIOD: u8 = 0xBE; // .
pub const VK_OEM_2: u8 = 0xBF; // /
pub const VK_OEM_3: u8 = 0xC0; // `
pub const VK_OEM_4: u8 = 0xDB; // [
pub const VK_OEM_5: u8 = 0xDC; // \
pub const VK_OEM_6: u8 = 0xDD; // ]
pub const VK_OEM_7: u8 = 0xDE; // '

/// Modifier flags for key combinations (`Ctrl+E`, `Shift+R`, `Alt+F1`, etc.).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1 << 0);
    pub const SHIFT: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns the modifier bit if `vk` is a Shift, Ctrl, or Alt key.
    #[must_use]
    pub const fn from_vk(vk: u8) -> Self {
        match vk {
            VK_CONTROL | VK_LCONTROL | VK_RCONTROL => Self::CTRL,
            VK_SHIFT | VK_LSHIFT | VK_RSHIFT => Self::SHIFT,
            VK_MENU | VK_LMENU | VK_RMENU => Self::ALT,
            _ => Self::NONE,
        }
    }
}

/// A PC key or mouse control, optionally combined with Ctrl/Shift/Alt modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeySpec {
    pub modifiers: Modifiers,
    pub key: u8,
}

impl KeySpec {
    #[must_use]
    pub const fn plain(key: u8) -> Self {
        Self {
            modifiers: Modifiers::NONE,
            key,
        }
    }

    #[must_use]
    pub const fn combo(modifiers: Modifiers, key: u8) -> Self {
        Self { modifiers, key }
    }

    /// Parses a key name or modifier combo from `native-keys.txt` or `keys.txt`
    /// (e.g. `"E"`, `"Space"`, `"LeftClick"`, `"Numpad5"`, `"Ctrl+E"`, `"Shift+F1"`).
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parts: Vec<&str> = trimmed.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }
        if parts.len() == 1 {
            return parse_single_vk(parts[0]).map(Self::plain);
        }
        let mut mods = Modifiers::NONE;
        for &m in &parts[..parts.len() - 1] {
            let bit = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "lctrl" | "rctrl" | "leftctrl" | "rightctrl" => Modifiers::CTRL,
                "shift" | "lshift" | "rshift" | "leftshift" | "rightshift" => Modifiers::SHIFT,
                "alt" | "lalt" | "ralt" | "leftalt" | "rightalt" => Modifiers::ALT,
                _ => return None,
            };
            mods = mods.with(bit);
        }
        let key = parse_single_vk(parts[parts.len() - 1])?;
        // Avoid redundant `Ctrl+Ctrl`
        let key_mod = Modifiers::from_vk(key);
        if !key_mod.is_empty() && mods.contains(key_mod) {
            return None;
        }
        Some(Self { modifiers: mods, key })
    }

    /// Formats back to canonical config name (e.g. `"Ctrl+E"`, `"Space"`, `"LeftClick"`).
    #[must_use]
    pub fn format(self) -> Option<String> {
        let base = format_single_vk(self.key)?;
        if self.modifiers.is_empty() {
            return Some(base);
        }
        let mut parts = Vec::with_capacity(4);
        if self.modifiers.contains(Modifiers::CTRL) {
            parts.push("Ctrl");
        }
        if self.modifiers.contains(Modifiers::SHIFT) {
            parts.push("Shift");
        }
        if self.modifiers.contains(Modifiers::ALT) {
            parts.push("Alt");
        }
        let mut out = parts.join("+");
        out.push('+');
        out.push_str(&base);
        Some(out)
    }
}

/// Parses a single key/mouse token (case-insensitive) into its Windows virtual key
/// or synthetic mouse code (`MOUSE_WHEEL_UP`, `MOUSE_WHEEL_DOWN`, `MOUSE_MOVES`).
#[must_use]
pub fn parse_single_vk(name: &str) -> Option<u8> {
    let n = name.trim();
    if let [c] = n.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some(c.to_ascii_uppercase());
    }
    let low = n.to_ascii_lowercase();
    let named = match low.as_str() {
        "leftclick" | "lmb" | "mouse1" => VK_LBUTTON,
        "rightclick" | "rmb" | "mouse2" => VK_RBUTTON,
        "middleclick" | "mmb" | "mouse3" => VK_MBUTTON,
        "mouse4" | "mb4" | "xbutton1" => VK_XBUTTON1,
        "mouse5" | "mb5" | "xbutton2" => VK_XBUTTON2,
        "wheelup" | "mwheelup" => MOUSE_WHEEL_UP,
        "wheeldown" | "mwheeldown" => MOUSE_WHEEL_DOWN,
        "mouse" | "mousemove" | "mousemoves" => MOUSE_MOVES,
        "backspace" | "back" => VK_BACK,
        "tab" => VK_TAB,
        "enter" | "return" => VK_RETURN,
        "capslock" | "caps" => VK_CAPITAL,
        "esc" | "escape" => VK_ESCAPE,
        "space" | "spacebar" => VK_SPACE,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "end" => VK_END,
        "home" => VK_HOME,
        "arrowleft" | "left" => VK_LEFT,
        "arrowup" | "up" => VK_UP,
        "arrowright" | "right" => VK_RIGHT,
        "arrowdown" | "down" => VK_DOWN,
        "insert" | "ins" => VK_INSERT,
        "delete" | "del" => VK_DELETE,
        "shift" | "lshift" | "leftshift" => VK_LSHIFT,
        "rightshift" | "rshift" => VK_RSHIFT,
        "ctrl" | "control" | "lctrl" | "leftctrl" => VK_LCONTROL,
        "rightctrl" | "rctrl" => VK_RCONTROL,
        "alt" | "lalt" | "leftalt" => VK_LMENU,
        "rightalt" | "ralt" => VK_RMENU,
        "semicolon" | ";" => VK_OEM_1,
        "equals" | "equal" | "=" => VK_OEM_PLUS,
        "comma" | "," => VK_OEM_COMMA,
        "minus" | "-" => VK_OEM_MINUS,
        "period" | "." => VK_OEM_PERIOD,
        "slash" | "/" => VK_OEM_2,
        "backquote" | "grave" | "tilde" | "`" => VK_OEM_3,
        "leftbracket" | "[" => VK_OEM_4,
        "backslash" | "\\" => VK_OEM_5,
        "rightbracket" | "]" => VK_OEM_6,
        "quote" | "apostrophe" | "'" => VK_OEM_7,
        "numpadmultiply" | "numpadmul" | "num*" => VK_MULTIPLY,
        "numpadadd" | "numpadplus" | "num+" => VK_ADD,
        "numpadsubtract" | "numpadsub" | "num-" => VK_SUBTRACT,
        "numpaddecimal" | "numpadperiod" | "num." => VK_DECIMAL,
        "numpaddivide" | "numpaddiv" | "num/" => VK_DIVIDE,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let up = n.to_ascii_uppercase();
    if let Some(f) = up
        .strip_prefix('F')
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|f| (1..=12).contains(f))
    {
        return Some(0x6F + f);
    }
    if let Some(d) = up
        .strip_prefix("NUMPAD")
        .and_then(|d| d.parse::<u8>().ok())
        .filter(|d| *d <= 9)
    {
        return Some(VK_NUMPAD0 + d);
    }
    None
}

/// Formats a Windows VK or synthetic mouse code into its canonical `native-keys.txt` name.
#[must_use]
pub fn format_single_vk(vk: u8) -> Option<String> {
    if vk.is_ascii_uppercase() || vk.is_ascii_digit() {
        return Some((vk as char).to_string());
    }
    if (VK_F1..=VK_F12).contains(&vk) {
        return Some(format!("F{}", vk - VK_F1 + 1));
    }
    if (VK_NUMPAD0..=VK_NUMPAD9).contains(&vk) {
        return Some(format!("Numpad{}", vk - VK_NUMPAD0));
    }
    let s = match vk {
        VK_LBUTTON => "LeftClick",
        VK_RBUTTON => "RightClick",
        VK_MBUTTON => "MiddleClick",
        VK_XBUTTON1 => "Mouse4",
        VK_XBUTTON2 => "Mouse5",
        MOUSE_WHEEL_UP => "WheelUp",
        MOUSE_WHEEL_DOWN => "WheelDown",
        MOUSE_MOVES => "Mouse",
        VK_BACK => "Backspace",
        VK_TAB => "Tab",
        VK_RETURN => "Enter",
        VK_CAPITAL => "CapsLock",
        VK_ESCAPE => "Esc",
        VK_SPACE => "Space",
        VK_PRIOR => "PageUp",
        VK_NEXT => "PageDown",
        VK_END => "End",
        VK_HOME => "Home",
        VK_LEFT => "ArrowLeft",
        VK_UP => "ArrowUp",
        VK_RIGHT => "ArrowRight",
        VK_DOWN => "ArrowDown",
        VK_INSERT => "Insert",
        VK_DELETE => "Delete",
        VK_SHIFT | VK_LSHIFT => "Shift",
        VK_RSHIFT => "RightShift",
        VK_CONTROL | VK_LCONTROL => "Ctrl",
        VK_RCONTROL => "RightCtrl",
        VK_MENU | VK_LMENU => "Alt",
        VK_RMENU => "RightAlt",
        VK_OEM_1 => "Semicolon",
        VK_OEM_PLUS => "Equals",
        VK_OEM_COMMA => "Comma",
        VK_OEM_MINUS => "Minus",
        VK_OEM_PERIOD => "Period",
        VK_OEM_2 => "Slash",
        VK_OEM_3 => "Backquote",
        VK_OEM_4 => "LeftBracket",
        VK_OEM_5 => "Backslash",
        VK_OEM_6 => "RightBracket",
        VK_OEM_7 => "Quote",
        VK_MULTIPLY => "NumpadMul",
        VK_ADD => "NumpadAdd",
        VK_SUBTRACT => "NumpadSub",
        VK_DECIMAL => "NumpadDecimal",
        VK_DIVIDE => "NumpadDiv",
        _ => return None,
    };
    Some(s.to_owned())
}

/// Declares the gamepad's controls and the list of all of them from one
/// list, so that the list cannot miss a control.
macro_rules! pad_controls {
    ($($control:ident),* $(,)?) => {
        /// A control of a gamepad.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum PadControl {
            $($control),*
        }

        impl PadControl {
            /// Every control, in the order declared.
            pub const ALL: &[Self] = &[$(Self::$control),*];
        }
    };
}

pad_controls!(
    Menu,
    View,
    A,
    B,
    X,
    Y,
    Up,
    Down,
    Left,
    Right,
    LeftShoulder,
    RightShoulder,
    LeftStick,
    RightStick,
    LeftTrigger,
    RightTrigger,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
);

/// The virtual key that drives `control` on the classic stand-in gamepad.
#[must_use]
pub const fn fallback_key(control: PadControl) -> u8 {
    match control {
        PadControl::Menu => VK_ESCAPE,
        PadControl::View => VK_TAB,
        PadControl::A => VK_RETURN,
        PadControl::B => VK_BACK,
        PadControl::X => b'X',
        PadControl::Y => b'Y',
        PadControl::Up => VK_UP,
        PadControl::Down => VK_DOWN,
        PadControl::Left => VK_LEFT,
        PadControl::Right => VK_RIGHT,
        PadControl::LeftShoulder => b'Q',
        PadControl::RightShoulder => b'E',
        PadControl::LeftStick => b'F',
        PadControl::RightStick => b'H',
        PadControl::LeftTrigger => b'Z',
        PadControl::RightTrigger => b'C',
        PadControl::LeftStickUp => b'W',
        PadControl::LeftStickDown => b'S',
        PadControl::LeftStickLeft => b'A',
        PadControl::LeftStickRight => b'D',
        PadControl::RightStickUp => b'I',
        PadControl::RightStickDown => b'K',
        PadControl::RightStickLeft => b'J',
        PadControl::RightStickRight => b'L',
    }
}

/// Default PC key shown for `control` when no custom binding overrides it.
#[must_use]
pub const fn prompt_key(control: PadControl, pc: bool) -> u8 {
    if !pc {
        return fallback_key(control);
    }
    match control {
        PadControl::Menu => VK_ESCAPE,
        PadControl::View => b'M',
        PadControl::A => VK_SPACE,
        PadControl::B => VK_BACK,
        PadControl::X => b'R',
        PadControl::Y => b'F',
        PadControl::Up => VK_UP,
        PadControl::Down => VK_DOWN,
        PadControl::Left => VK_LEFT,
        PadControl::Right => VK_RIGHT,
        PadControl::LeftShoulder => VK_TAB,
        PadControl::RightShoulder => b'Q',
        PadControl::LeftStick => VK_LSHIFT,
        PadControl::RightStick => b'C',
        PadControl::LeftTrigger => VK_RBUTTON,
        PadControl::RightTrigger => VK_LBUTTON,
        PadControl::LeftStickUp => b'W',
        PadControl::LeftStickDown => b'S',
        PadControl::LeftStickLeft => b'A',
        PadControl::LeftStickRight => b'D',
        PadControl::RightStickUp
        | PadControl::RightStickDown
        | PadControl::RightStickLeft
        | PadControl::RightStickRight => MOUSE_MOVES,
    }
}

/// Which input device the player most recently used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ActiveDevice {
    KeyboardMouse = 0,
    Gamepad = 1,
}

static ACTIVE_DEVICE: AtomicU8 = AtomicU8::new(ActiveDevice::KeyboardMouse as u8);

/// Records keyboard or mouse activity; switches `auto` prompt mode to keyboard key caps.
pub fn note_keyboard_mouse() {
    ACTIVE_DEVICE.store(ActiveDevice::KeyboardMouse as u8, Ordering::Relaxed);
}

/// Records real gamepad (XInput/DualSense) activity; switches `auto` prompt mode to controller icons.
pub fn note_gamepad() {
    ACTIVE_DEVICE.store(ActiveDevice::Gamepad as u8, Ordering::Relaxed);
}

/// The most recently active input device.
#[must_use]
pub fn active_device() -> ActiveDevice {
    match ACTIVE_DEVICE.load(Ordering::Relaxed) {
        1 => ActiveDevice::Gamepad,
        _ => ActiveDevice::KeyboardMouse,
    }
}

/// How button prompts in the title's Scaleform icon movie are rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PromptMode {
    /// Follow the active input device: keyboard caps for KB/M, original Xbox icons for gamepad.
    Auto = 0,
    /// Always show keyboard key caps.
    Keys = 1,
    /// Keep original Xbox gamepad icons.
    Gamepad = 2,
}

static PROMPT_MODE: AtomicU8 = AtomicU8::new(PromptMode::Auto as u8);

impl PromptMode {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "keys" | "keyboard" | "pc" | "1" | "on" => Some(Self::Keys),
            "gamepad" | "controller" | "xbox" | "0" | "off" => Some(Self::Gamepad),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Keys => "keys",
            Self::Gamepad => "gamepad",
        }
    }
}

/// Sets the active prompt mode (typically from `--prompts` or `VICTORY_PROMPTS`).
pub fn set_prompt_mode(mode: PromptMode) {
    PROMPT_MODE.store(mode as u8, Ordering::Relaxed);
}

/// Current prompt mode.
#[must_use]
pub fn prompt_mode() -> PromptMode {
    match PROMPT_MODE.load(Ordering::Relaxed) {
        1 => PromptMode::Keys,
        2 => PromptMode::Gamepad,
        _ => PromptMode::Auto,
    }
}

/// Whether keyboard key caps should currently be served for icon movie loads.
#[must_use]
pub fn should_show_key_prompts() -> bool {
    match prompt_mode() {
        PromptMode::Keys => true,
        PromptMode::Gamepad => false,
        PromptMode::Auto => active_device() == ActiveDevice::KeyboardMouse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_has_a_distinct_fallback_key() {
        let mut keys: Vec<u8> = PadControl::ALL.iter().map(|&c| fallback_key(c)).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 24);
    }

    #[test]
    fn keyspec_parses_single_keys_and_modifier_combos() {
        assert_eq!(KeySpec::parse("E"), Some(KeySpec::plain(b'E')));
        assert_eq!(KeySpec::parse("7"), Some(KeySpec::plain(b'7')));
        assert_eq!(KeySpec::parse("F12"), Some(KeySpec::plain(VK_F12)));
        assert_eq!(KeySpec::parse("Numpad5"), Some(KeySpec::plain(VK_NUMPAD5)));
        assert_eq!(KeySpec::parse("NumpadAdd"), Some(KeySpec::plain(VK_ADD)));
        assert_eq!(KeySpec::parse("LeftBracket"), Some(KeySpec::plain(VK_OEM_4)));
        assert_eq!(
            KeySpec::parse("Ctrl+E"),
            Some(KeySpec::combo(Modifiers::CTRL, b'E'))
        );
        assert_eq!(
            KeySpec::parse("Ctrl+Shift+F5"),
            Some(KeySpec::combo(Modifiers::CTRL.with(Modifiers::SHIFT), VK_F5))
        );
        assert_eq!(
            KeySpec::parse("Alt+LeftClick"),
            Some(KeySpec::combo(Modifiers::ALT, VK_LBUTTON))
        );
        assert_eq!(KeySpec::parse("Ctrl+Ctrl"), None);
        assert_eq!(KeySpec::parse("Ctrl+E").unwrap().format().as_deref(), Some("Ctrl+E"));
        assert_eq!(
            KeySpec::parse("Shift+Ctrl+Space").unwrap().format().as_deref(),
            Some("Ctrl+Shift+Space")
        );
    }

    #[test]
    fn active_device_controls_auto_prompt_mode() {
        set_prompt_mode(PromptMode::Auto);
        note_keyboard_mouse();
        assert!(should_show_key_prompts());
        note_gamepad();
        assert!(!should_show_key_prompts());
        set_prompt_mode(PromptMode::Keys);
        assert!(should_show_key_prompts());
        set_prompt_mode(PromptMode::Gamepad);
        assert!(!should_show_key_prompts());
        // Restore default for other tests
        set_prompt_mode(PromptMode::Auto);
        note_keyboard_mouse();
    }
}
