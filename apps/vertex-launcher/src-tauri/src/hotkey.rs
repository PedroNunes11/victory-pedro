//! Global Windows hotkey (`Win + Alt + V` by default, customizable to any
//! `Win + [Alt|Shift|Ctrl] + <Key>`) that brings up the Vertex In-Game Menu
//! while the game is running and returns focus when pressed again.

use std::sync::{Arc, Mutex};
use std::thread;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_USER,
};

const HOTKEY_ID: i32 = 0x5654; // "VT"
const WM_UPDATE_HOTKEY: u32 = WM_USER + 42;

pub const DEFAULT_HOTKEY: &str = "Win+Alt+V";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedHotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// Parses a hotkey like `"Win+Alt+V"`, `"Win+F1"`, `"Win+Shift+V"`, or
/// `"Win+Backquote"`. Requires the Windows key (`Win`) plus at least one key.
pub fn parse_hotkey(spec: &str) -> Result<(ParsedHotkey, String), String> {
    let mut has_win = false;
    let mut has_alt = false;
    let mut has_shift = false;
    let mut has_ctrl = false;
    let mut key_name: Option<String> = None;
    let mut vk: Option<u32> = None;

    for raw in spec.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        match token.to_ascii_lowercase().as_str() {
            "win" | "super" | "meta" | "windows" => has_win = true,
            "alt" => has_alt = true,
            "shift" => has_shift = true,
            "ctrl" | "control" => has_ctrl = true,
            other => {
                if vk.is_some() {
                    return Err(format!("multiple keys in hotkey '{spec}'"));
                }
                let (code, canonical) = parse_vk(other)
                    .ok_or_else(|| format!("unsupported key '{token}' in hotkey"))?;
                vk = Some(code);
                key_name = Some(canonical);
            }
        }
    }

    if !has_win {
        return Err("In-game menu hotkey must include Win (e.g. Win+Alt+V or Win+F1)".into());
    }
    let (vk, key_name) = match (vk, key_name) {
        (Some(v), Some(k)) => (v, k),
        _ => return Err("hotkey needs a key after Win (e.g. Win+Alt+V)".into()),
    };

    let mut mods = MOD_WIN.0 | MOD_NOREPEAT.0;
    let mut parts = vec!["Win".to_string()];
    if has_ctrl {
        mods |= MOD_CONTROL.0;
        parts.push("Ctrl".into());
    }
    if has_alt {
        mods |= MOD_ALT.0;
        parts.push("Alt".into());
    }
    if has_shift {
        mods |= MOD_SHIFT.0;
        parts.push("Shift".into());
    }
    parts.push(key_name);
    Ok((ParsedHotkey { modifiers: mods, vk }, parts.join("+")))
}

fn parse_vk(token: &str) -> Option<(u32, String)> {
    if token.len() == 1 {
        let b = token.as_bytes()[0].to_ascii_uppercase();
        if b.is_ascii_alphabetic() || b.is_ascii_digit() {
            return Some((u32::from(b), (b as char).to_string()));
        }
    }
    if let Some(rest) = token.strip_prefix('f')
        && let Ok(n) = rest.parse::<u32>()
        && (1..=12).contains(&n)
    {
        return Some((0x70 + (n - 1), format!("F{n}")));
    }
    match token {
        "`" | "backquote" | "tilde" | "grave" => Some((0xC0, "Backquote".into())),
        "tab" => Some((0x09, "Tab".into())),
        "space" => Some((0x20, "Space".into())),
        "home" => Some((0x24, "Home".into())),
        "end" => Some((0x23, "End".into())),
        "insert" => Some((0x2D, "Insert".into())),
        "pageup" => Some((0x21, "PageUp".into())),
        "pagedown" => Some((0x22, "PageDown".into())),
        "minus" | "-" => Some((0xBD, "Minus".into())),
        "equals" | "=" => Some((0xBB, "Equals".into())),
        _ => None,
    }
}

pub struct HotkeyManager {
    thread_id: u32,
    desired: Arc<Mutex<(ParsedHotkey, String)>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl HotkeyManager {
    pub fn start<F>(initial_spec: &str, on_trigger: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let initial = parse_hotkey(initial_spec)
            .unwrap_or_else(|_| parse_hotkey(DEFAULT_HOTKEY).expect("valid default hotkey"));
        let desired = Arc::new(Mutex::new(initial));
        let last_error = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        let d2 = desired.clone();
        let e2 = last_error.clone();

        thread::Builder::new()
            .name("vertex-hotkey".into())
            .spawn(move || {
                // Ensure the thread's message queue exists before publishing its id.
                // SAFETY: standard Win32 message queue creation on this thread.
                let tid = unsafe {
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    GetCurrentThreadId()
                };
                let _ = tx.send(tid);

                let register = |hk: ParsedHotkey, label: &str| {
                    // SAFETY: thread-local hotkey registration with null HWND.
                    unsafe {
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                        match RegisterHotKey(None, HOTKEY_ID, HOT_KEY_MODIFIERS(hk.modifiers), hk.vk) {
                            Ok(()) => {
                                *e2.lock().unwrap() = None;
                            }
                            Err(err) => {
                                *e2.lock().unwrap() = Some(format!(
                                    "Could not register {label} ({}). Try another Win+Key combo.",
                                    err.message()
                                ));
                            }
                        }
                    }
                };

                {
                    let (hk, label) = d2.lock().unwrap().clone();
                    register(hk, &label);
                }

                let mut msg = MSG::default();
                // SAFETY: standard blocking message loop for WM_HOTKEY and update messages.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                    if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                        on_trigger();
                    } else if msg.message == WM_UPDATE_HOTKEY {
                        let (hk, label) = d2.lock().unwrap().clone();
                        register(hk, &label);
                    }
                }
                // SAFETY: clean up on exit.
                unsafe {
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
                }
            })
            .expect("spawning hotkey thread");

        let thread_id = rx.recv().unwrap_or(0);
        Self {
            thread_id,
            desired,
            last_error,
        }
    }

    pub fn update(&self, spec: &str) -> Result<String, String> {
        let (parsed, canonical) = parse_hotkey(spec)?;
        *self.desired.lock().unwrap() = (parsed, canonical.clone());
        if self.thread_id != 0 {
            // SAFETY: posting custom message to our live hotkey thread.
            let _ = unsafe {
                PostThreadMessageW(self.thread_id, WM_UPDATE_HOTKEY, WPARAM(0), LPARAM(0))
            };
        }
        Ok(canonical)
    }

    pub fn status(&self) -> (String, Option<String>) {
        let label = self.desired.lock().unwrap().1.clone();
        let err = self.last_error.lock().unwrap().clone();
        (label, err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_win_hotkeys() {
        let (hk, name) = parse_hotkey("win + alt + v").unwrap();
        assert_eq!(name, "Win+Alt+V");
        assert_eq!(hk.vk, u32::from(b'V'));
        assert!(hk.modifiers & MOD_WIN.0 != 0);
        assert!(hk.modifiers & MOD_ALT.0 != 0);

        let (hk2, name2) = parse_hotkey("Win+F1").unwrap();
        assert_eq!(name2, "Win+F1");
        assert_eq!(hk2.vk, 0x70);

        let (hk3, name3) = parse_hotkey("Win+Backquote").unwrap();
        assert_eq!(name3, "Win+Backquote");
        assert_eq!(hk3.vk, 0xC0);
    }

    #[test]
    fn rejects_hotkey_without_win() {
        assert!(parse_hotkey("Alt+V").is_err());
        assert!(parse_hotkey("Win+").is_err());
    }
}
