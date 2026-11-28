//! Global Windows hotkey (`Win + Alt + V` by default, customizable to any
//! `Win + [Alt|Shift|Ctrl] + <Key>`) that brings up the Vertex In-Game Menu
//! while the game is running and returns focus when pressed again.
//!
//! Implemented with a Win32 Low-Level Keyboard Hook (`WH_KEYBOARD_LL`) rather
//! than `RegisterHotKey` because Windows 11 Shell reserves many `Win+Alt+*` /
//! `Win+Shift+*` combinations (failing `RegisterHotKey` with error 1409), and
//! because a low-level hook cleanly consumes the keystroke before Windows or
//! the game's window messages see it — while also suppressing the Start Menu
//! pop-up when releasing `Win`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, keybd_event,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

const WM_TRIGGER_HOTKEY: u32 = WM_USER + 43;

pub const DEFAULT_HOTKEY: &str = "Win+Alt+V";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedHotkey {
    pub modifiers: u32,
    pub vk: u32,
}

impl ParsedHotkey {
    fn want_alt(self) -> bool {
        self.modifiers & MOD_ALT.0 != 0
    }
    fn want_ctrl(self) -> bool {
        self.modifiers & MOD_CONTROL.0 != 0
    }
    fn want_shift(self) -> bool {
        self.modifiers & MOD_SHIFT.0 != 0
    }
    fn matches(self, vk: u32, win: bool, alt: bool, ctrl: bool, shift: bool) -> bool {
        win && vk == self.vk
            && alt == self.want_alt()
            && ctrl == self.want_ctrl()
            && shift == self.want_shift()
    }
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

/// Shared state read from the process-wide `WH_KEYBOARD_LL` callback.
struct HookState {
    thread_id: AtomicU32,
    key_down: AtomicBool,
    desired: Mutex<(ParsedHotkey, String)>,
    last_trigger: Mutex<Option<Instant>>,
}

static HOOK_STATE: OnceLock<Arc<HookState>> = OnceLock::new();

fn is_down(vk: i32) -> bool {
    // SAFETY: plain Win32 key state query.
    unsafe { GetAsyncKeyState(vk) < 0 }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(state) = HOOK_STATE.get()
    {
        // SAFETY: `lparam` points to a valid `KBDLLHOOKSTRUCT` when `code == HC_ACTION`.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let injected = (info.flags.0 & LLKHF_INJECTED.0) != 0;
        if !injected {
            let msg = wparam.0 as u32;
            let vk = info.vkCode;
            let target = state.desired.lock().unwrap_or_else(|e| e.into_inner()).0;

            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                let win = is_down(i32::from(VK_LWIN.0)) || is_down(i32::from(VK_RWIN.0));
                let alt = is_down(i32::from(VK_MENU.0));
                let ctrl = is_down(i32::from(VK_CONTROL.0));
                let shift = is_down(i32::from(VK_SHIFT.0));

                if target.matches(vk, win, alt, ctrl, shift) {
                    if !state.key_down.swap(true, Ordering::SeqCst) {
                        *state.last_trigger.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(Instant::now());
                        let tid = state.thread_id.load(Ordering::Acquire);
                        if tid != 0 {
                            // SAFETY: lightweight async wake to our dedicated hotkey thread.
                            let _ = unsafe {
                                PostThreadMessageW(tid, WM_TRIGGER_HOTKEY, WPARAM(0), LPARAM(0))
                            };
                        }
                    }
                    // Swallow keydown so neither Windows nor the game sees the trigger key.
                    return LRESULT(1);
                }
            } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                if vk == target.vk && state.key_down.swap(false, Ordering::SeqCst) {
                    return LRESULT(1);
                }
                if (vk == u32::from(VK_LWIN.0) || vk == u32::from(VK_RWIN.0))
                    && state
                        .last_trigger
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                {
                    // Prevent Windows Start Menu from opening when releasing Win after our hotkey.
                    *state.last_trigger.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    // SAFETY: harmless unassigned virtual-key mask pulse to cancel Start Menu.
                    unsafe {
                        keybd_event(0xFF, 0, Default::default(), 0);
                        keybd_event(0xFF, 0, KEYEVENTF_KEYUP, 0);
                    }
                }
            }
        }
    }
    // SAFETY: standard hook chain forwarding.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

pub struct HotkeyManager {
    state: Arc<HookState>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl HotkeyManager {
    pub fn start<F>(initial_spec: &str, on_trigger: F) -> Self
    where
        F: Fn() + Send + 'static,
    {
        let initial = parse_hotkey(initial_spec)
            .unwrap_or_else(|_| parse_hotkey(DEFAULT_HOTKEY).expect("valid default hotkey"));
        let state = HOOK_STATE
            .get_or_init(|| {
                Arc::new(HookState {
                    thread_id: AtomicU32::new(0),
                    key_down: AtomicBool::new(false),
                    desired: Mutex::new(initial.clone()),
                    last_trigger: Mutex::new(None),
                })
            })
            .clone();
        *state.desired.lock().unwrap() = initial;

        let last_error = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let s2 = state.clone();
        let e2 = last_error.clone();

        thread::Builder::new()
            .name("vertex-hotkey".into())
            .spawn(move || {
                // Ensure the thread's message queue exists before publishing its id.
                // SAFETY: standard Win32 message queue creation and LL keyboard hook install.
                let hook = unsafe {
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    s2.thread_id.store(GetCurrentThreadId(), Ordering::Release);
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), None, 0)
                };
                match hook {
                    Ok(h) => {
                        *e2.lock().unwrap() = None;
                        let _ = tx.send(());
                        let mut msg = MSG::default();
                        // SAFETY: blocking message loop required to service WH_KEYBOARD_LL.
                        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                            if msg.message == WM_TRIGGER_HOTKEY {
                                on_trigger();
                            }
                        }
                        // SAFETY: clean hook removal on thread exit.
                        unsafe {
                            let _ = UnhookWindowsHookEx(h);
                        }
                    }
                    Err(err) => {
                        *e2.lock().unwrap() = Some(format!(
                            "Could not install global hotkey hook ({}).",
                            err.message()
                        ));
                        let _ = tx.send(());
                    }
                }
            })
            .expect("spawning hotkey thread");

        let _ = rx.recv();
        Self { state, last_error }
    }

    pub fn update(&self, spec: &str) -> Result<String, String> {
        let (parsed, canonical) = parse_hotkey(spec)?;
        *self.state.desired.lock().unwrap() = (parsed, canonical.clone());
        self.state.key_down.store(false, Ordering::SeqCst);
        Ok(canonical)
    }

    pub fn status(&self) -> (String, Option<String>) {
        let label = self.state.desired.lock().unwrap().1.clone();
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
        assert!(hk.matches(u32::from(b'V'), true, true, false, false));
        assert!(!hk.matches(u32::from(b'V'), false, true, false, false));
        assert!(!hk.matches(u32::from(b'V'), true, false, false, false));

        let (hk2, name2) = parse_hotkey("Win+F1").unwrap();
        assert_eq!(name2, "Win+F1");
        assert_eq!(hk2.vk, 0x70);
        assert!(hk2.matches(0x70, true, false, false, false));

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
