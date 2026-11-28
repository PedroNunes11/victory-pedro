//! In-game Controls menu: press **F1** while the game window is focused to open
//! a topmost translucent controls overlay directly over the game.
//!
//! - In **Native PC Controls** mode (default whenever `VICTORY_NATIVE_KEYS` or
//!   `<data>/native-keys.txt` exists): edits `native-keys.txt` directly by
//!   category (On Foot, Combat, Vehicle, Menus, Mouse), supporting single keys,
//!   mouse buttons, scroll wheel, and modifier combos (`Ctrl+Key`, `Shift+Key`,
//!   `Alt+Key`). Every change saves atomically and reloads live in
//!   `keyboard_events.rs` and Scaleform GFx prompt glyphs within 1 second.
//! - In **Stand-in Pad** fallback mode (when native keys are absent): edits
//!   `keys.txt` (On Foot, Driving, Mouse).
//! - On by default; set `VICTORY_CONTROLS_MENU=0` to disable.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, SetFocus,
};
use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::binds::{self, Binds, Part};

const VK_F1: u8 = 0x70;
const VK_ESC: u8 = 0x1B;

static NATIVE_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Registers the default `native-keys.txt` path (from `<data>/native-keys.txt`).
/// `VICTORY_NATIVE_KEYS` overrides this when set.
pub fn use_native_file(path: PathBuf) {
    *NATIVE_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn resolved_native_file() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VICTORY_NATIVE_KEYS").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    NATIVE_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Starts the thread that watches for F1 while the title's window is in front.
/// Enabled by default (`VICTORY_CONTROLS_MENU=0` disables).
pub fn start() {
    static STARTED: Once = Once::new();
    if std::env::var_os("VICTORY_CONTROLS_MENU").is_some_and(|v| v == "0") {
        return;
    }
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("controls-menu".into()).spawn(watch);
    });
}

fn watch() {
    let mut was = false;
    loop {
        std::thread::sleep(Duration::from_millis(30));
        // SAFETY: plain query.
        let down = unsafe { GetAsyncKeyState(i32::from(VK_F1)) } < 0;
        if down && !was {
            // SAFETY: plain query.
            let front = unsafe { GetForegroundWindow() };
            if crate::modules::lifecycle::is_title_window(front) {
                open(front);
            }
        }
        was = down;
    }
}

// ---- Native PC Controls (`native-keys.txt`) model ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NativeTab {
    OnFoot,
    Combat,
    Vehicle,
    Menus,
    Mouse,
}

#[derive(Clone, Copy)]
struct ActionSpec {
    id: &'static str,
    label: &'static str,
    tab: NativeTab,
    is_menu: bool,
}

const NATIVE_ACTIONS: &[ActionSpec] = &[
    // On Foot
    ActionSpec { id: "INPUT_MOVE_UP_ONLY", label: "Move Forward", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_MOVE_DOWN_ONLY", label: "Move Back", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_MOVE_LEFT_ONLY", label: "Move Left", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_MOVE_RIGHT_ONLY", label: "Move Right", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_JUMP", label: "Jump / Climb", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_SPRINT", label: "Sprint", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_DUCK", label: "Crouch / Stealth", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "VERTEX_WALK_TOGGLE", label: "Walk (Toggle)", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "VERTEX_WALK_HOLD", label: "Walk (Hold)", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_ENTER", label: "Enter Vehicle", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_CONTEXT", label: "Interact / Talk", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_PHONE", label: "Phone", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_MAP", label: "Map", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_REVEAL_HUD", label: "Reveal HUD / Radar", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_PLAYER_MENU", label: "Interaction Menu", tab: NativeTab::OnFoot, is_menu: false },
    ActionSpec { id: "INPUT_SWITCH_CHARACTER", label: "Switch Character", tab: NativeTab::OnFoot, is_menu: false },
    // Combat
    ActionSpec { id: "INPUT_ATTACK", label: "Fire / Attack", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_AIM", label: "Aim", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_RELOAD", label: "Reload / Melee", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_COVER", label: "Take Cover", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_OPEN_WHEEL_MENU", label: "Weapon Wheel", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_PREV_WEAPON", label: "Previous Weapon", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_NEXT_WEAPON", label: "Next Weapon", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_DETONATE", label: "Throw / Use Item", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_TOGGLE_HOLSTER", label: "Holster Weapon", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_SWITCH_SHOULDER", label: "Switch Shoulder", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_TOGGLE_WEAPON_SCOPE", label: "Weapon Scope / Mode", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_SPECIAL_ABILITY", label: "Special Ability", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_DROP_WEAPON", label: "Drop Weapon", tab: NativeTab::Combat, is_menu: false },
    ActionSpec { id: "INPUT_DROP_AMMO", label: "Drop Ammo", tab: NativeTab::Combat, is_menu: false },
    // Vehicle
    ActionSpec { id: "INPUT_VEH_ACCELERATE", label: "Accelerate", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_BRAKE", label: "Brake / Reverse", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_CAR_TURN_LEFT_ONLY", label: "Steer Left", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_CAR_TURN_RIGHT_ONLY", label: "Steer Right", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_HANDBRAKE", label: "Handbrake", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_CAR_BOOST", label: "Vehicle Boost", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_EXIT", label: "Exit Vehicle", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_HORN", label: "Horn", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_NEXT_RADIO", label: "Radio Wheel / Station", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_HEADLIGHT", label: "Headlights / Roof", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_DUCK", label: "Duck in Vehicle", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_LOOK_BEHIND", label: "Look Behind", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_ATTACK", label: "Drive-By Fire", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_AIM", label: "Drive-By Aim", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_SELECT_NEXT_WEAPON", label: "Cycle Vehicle Weapon", tab: NativeTab::Vehicle, is_menu: false },
    ActionSpec { id: "INPUT_VEH_RELOAD", label: "Vehicle Reload", tab: NativeTab::Vehicle, is_menu: false },
    // Menus & UI
    ActionSpec { id: "INPUT_FRONTEND_PAUSE", label: "Pause Menu", tab: NativeTab::Menus, is_menu: false },
    ActionSpec { id: "INPUT_FRONTEND_ACCEPT", label: "Menu Accept", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_FRONTEND_CANCEL", label: "Menu Back / Cancel", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_UP", label: "Menu Up", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_DOWN", label: "Menu Down", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_LEFT", label: "Menu Left", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_RIGHT", label: "Menu Right", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_TAB_LEFT", label: "Previous Tab", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_GAME_MENU_TAB_RIGHT", label: "Next Tab", tab: NativeTab::Menus, is_menu: true },
    ActionSpec { id: "INPUT_FRONTEND_SELECT", label: "Change Camera / View", tab: NativeTab::Menus, is_menu: false },
    ActionSpec { id: "INPUT_OPEN_PHOTO_MODE", label: "Photo Mode", tab: NativeTab::Menus, is_menu: false },
    ActionSpec { id: "INPUT_SKIP_CUTSCENE", label: "Skip Cutscene", tab: NativeTab::Menus, is_menu: false },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeRow {
    key: String,
    play: Vec<String>,
    menu: Vec<String>,
}

/// Lossless in-memory representation of `native-keys.txt`.
#[derive(Clone, Debug, Default)]
struct NativeBinds {
    header: Vec<String>,
    rows: Vec<NativeRow>,
}

impl NativeBinds {
    fn parse(text: &str) -> Self {
        let mut header = Vec::new();
        let mut rows: Vec<NativeRow> = Vec::new();
        let mut saw_entry = false;
        for raw in text.lines() {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                if !saw_entry {
                    header.push(raw.to_string());
                }
                continue;
            }
            let Some((k, rest)) = trimmed.split_once('=') else { continue };
            let key = k.trim().to_string();
            if key.is_empty() {
                continue;
            }
            saw_entry = true;
            let (play_part, menu_part) = rest.split_once('|').unwrap_or((rest, ""));
            let split_list = |s: &str| -> Vec<String> {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            };
            let play = split_list(play_part);
            let menu = split_list(menu_part);
            if let Some(existing) = rows.iter_mut().find(|r| r.key.eq_ignore_ascii_case(&key)) {
                for a in play {
                    if !existing.play.iter().any(|x| x == &a) {
                        existing.play.push(a);
                    }
                }
                for a in menu {
                    if !existing.menu.iter().any(|x| x == &a) {
                        existing.menu.push(a);
                    }
                }
            } else {
                rows.push(NativeRow { key, play, menu });
            }
        }
        Self { header, rows }
    }

    fn format(&self) -> String {
        let mut out = String::new();
        if self.header.is_empty() {
            out.push_str("# Vertex native keys — edited live from the in-game F1 Controls menu.\n\n");
        } else {
            for line in &self.header {
                out.push_str(line);
                out.push('\n');
            }
            if !self.header.last().is_some_and(|l| l.trim().is_empty()) {
                out.push('\n');
            }
        }
        for row in &self.rows {
            if row.play.is_empty() && row.menu.is_empty() {
                continue;
            }
            if row.menu.is_empty() {
                out.push_str(&format!("{} = {}\n", row.key, row.play.join(", ")));
            } else {
                out.push_str(&format!("{} = {} | {}\n", row.key, row.play.join(", "), row.menu.join(", ")));
            }
        }
        out
    }

    fn keys_for(&self, action: &str, is_menu: bool) -> Vec<String> {
        let mut out = Vec::new();
        for row in &self.rows {
            let list = if is_menu { &row.menu } else { &row.play };
            if list.iter().any(|a| a.trim_end_matches(['+', '-']) == action) {
                out.push(row.key.clone());
            }
        }
        out
    }

    fn add_key(&mut self, action: &str, is_menu: bool, key: &str) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.key.eq_ignore_ascii_case(key)) {
            let list = if is_menu { &mut row.menu } else { &mut row.play };
            if !list.iter().any(|a| a.trim_end_matches(['+', '-']) == action) {
                list.push(action.to_string());
            }
        } else {
            let mut row = NativeRow {
                key: key.to_string(),
                play: Vec::new(),
                menu: Vec::new(),
            };
            if is_menu {
                row.menu.push(action.to_string());
            } else {
                row.play.push(action.to_string());
            }
            self.rows.push(row);
        }
    }

    fn remove_key(&mut self, action: &str, is_menu: bool, key: &str) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.key.eq_ignore_ascii_case(key)) {
            let list = if is_menu { &mut row.menu } else { &mut row.play };
            list.retain(|a| a.trim_end_matches(['+', '-']) != action);
        }
        self.rows.retain(|r| !r.play.is_empty() || !r.menu.is_empty());
    }

    fn replace_key(&mut self, action: &str, is_menu: bool, old_key: &str, new_key: &str) {
        if old_key.eq_ignore_ascii_case(new_key) {
            return;
        }
        self.remove_key(action, is_menu, old_key);
        self.add_key(action, is_menu, new_key);
    }

    fn load_from(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        Some(Self::parse(&text))
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, self.format())?;
        std::fs::rename(&tmp, path)
    }
}

/// Converts a Windows VK (or synthetic wheel/mouse code) into its canonical
/// `native-keys.txt` key name.
fn native_key_name(vk: u8) -> Option<String> {
    if let Some(n) = binds::key_name(vk) {
        return Some(n);
    }
    let extra = match vk {
        0x60 => "Numpad0",
        0x61 => "Numpad1",
        0x62 => "Numpad2",
        0x63 => "Numpad3",
        0x64 => "Numpad4",
        0x65 => "Numpad5",
        0x66 => "Numpad6",
        0x67 => "Numpad7",
        0x68 => "Numpad8",
        0x69 => "Numpad9",
        0x6A => "NumpadMul",
        0x6B => "NumpadAdd",
        0x6D => "NumpadSub",
        0x6E => "NumpadDecimal",
        0x6F => "NumpadDiv",
        0x7B => "F12",
        0xBB => "Equals",
        0xDB => "LeftBracket",
        0xDC => "Backslash",
        0xDD => "RightBracket",
        0xDE => "Quote",
        0x81 => "WheelUp",
        0x82 => "WheelDown",
        _ => return None,
    };
    Some(extra.to_string())
}

fn is_modifier_vk(vk: u8) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5)
}

/// Formats a captured key with optional held Ctrl/Shift/Alt modifiers (for
/// `native-keys.txt` combo bindings like `Ctrl+E` or `Shift+F`).
fn format_combo(base: &str, vk: u8) -> String {
    if is_modifier_vk(vk) {
        return base.to_string();
    }
    // SAFETY: plain modifier state queries.
    let ctrl = unsafe { GetAsyncKeyState(0x11) } < 0;
    let shift = unsafe { GetAsyncKeyState(0x10) } < 0;
    let alt = unsafe { GetAsyncKeyState(0xA5) } < 0; // RightAlt only (LeftAlt releases mouse)
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if shift {
        parts.push("Shift");
    }
    if alt {
        parts.push("Alt");
    }
    parts.push(base);
    parts.join("+")
}

// ---- Legacy Stand-in Pad (`keys.txt`) tabs ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum LegacyTab {
    Walk,
    Drive,
    Mouse,
}

#[derive(Clone, PartialEq, Debug)]
enum Slot {
    LegacyBind(LegacyTab, Part, Option<u8>),
    LegacySwitch,
    /// `(action_idx into NATIVE_ACTIONS, Some(old_key) to replace or None to add)`.
    NativeBind(usize, Option<String>),
}

#[derive(Clone, PartialEq, Debug)]
enum Hit {
    LegacyTab(LegacyTab),
    LegacyKey(LegacyTab, Part, u8),
    LegacyAdd(LegacyTab, Part),
    LegacySwitch,
    NativeTab(NativeTab),
    NativeKey(usize, String),
    NativeAdd(usize),
    SlowerMouse,
    FasterMouse,
    InvertY,
    Reset,
    Done,
}

enum Mode {
    Native {
        path: PathBuf,
        tab: NativeTab,
        native: NativeBinds,
    },
    Legacy {
        tab: LegacyTab,
    },
}

struct Menu {
    window: HWND,
    title: HWND,
    width: i32,
    height: i32,
    mode: Mode,
    binds: Binds,
    capture: Option<Slot>,
    hits: Vec<(RECT, Hit)>,
    note: String,
    closed: bool,
}

thread_local! {
    static MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens the menu over `title`'s client area and runs it until it is closed.
fn open(title: HWND) {
    let mut area = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let mut corner = POINT { x: 0, y: 0 };
    // SAFETY: our own rectangle and point.
    if unsafe { GetClientRect(title, &mut area) } == 0 || unsafe { ClientToScreen(title, &mut corner) } == 0 {
        return;
    }
    let (width, height) = (area.right, area.bottom);
    if width < 320 || height < 240 {
        return;
    }
    let class = wide("VertexControlsMenu");
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            // SAFETY: our own module.
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hIcon: std::ptr::null_mut(),
            // SAFETY: a system cursor.
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        // SAFETY: a class description of ours, its strings alive for the call.
        unsafe { RegisterClassW(&wc) };
    });
    let name = wide("Vertex Controls");
    // SAFETY: the class registered above.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            name.as_ptr(),
            WS_POPUP,
            corner.x,
            corner.y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if window.is_null() {
        tracing::warn!("controls menu: its window could not be made");
        return;
    }

    let mode = if let Some(path) = resolved_native_file()
        && let Some(native) = NativeBinds::load_from(&path)
    {
        Mode::Native {
            path,
            tab: NativeTab::OnFoot,
            native,
        }
    } else {
        Mode::Legacy {
            tab: if binds::driving() { LegacyTab::Drive } else { LegacyTab::Walk },
        }
    };

    MENU.with(|m| {
        *m.borrow_mut() = Some(Menu {
            window,
            title,
            width,
            height,
            mode,
            binds: binds::current(),
            capture: None,
            hits: Vec::new(),
            note: String::new(),
            closed: false,
        });
    });
    tracing::info!("controls menu opened (F1)");
    redraw();
    // SAFETY: our window.
    unsafe {
        ShowWindow(window, SW_SHOW);
        SetForegroundWindow(window);
        SetFocus(window);
    }
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        if MENU.with(|m| m.borrow().as_ref().is_none_or(|m| m.closed)) {
            break;
        }
        // SAFETY: our own message structure; messages of this thread's windows.
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            break;
        }
        // SAFETY: the message just got.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let title = MENU.with(|m| m.borrow_mut().take().map(|m| m.title));
    // SAFETY: our window; the title's, which may be gone.
    unsafe {
        DestroyWindow(window);
        if let Some(t) = title {
            SetForegroundWindow(t);
        }
    }
    tracing::info!("controls menu closed");
}

extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let at = || ((lparam & 0xffff) as i16 as i32, ((lparam >> 16) & 0xffff) as i16 as i32);
    let handled = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => key(virtual_key(wparam as u32, lparam)),
        WM_LBUTTONDOWN => click(0x01, at()),
        WM_RBUTTONDOWN => right_click(at()),
        WM_MBUTTONDOWN => press(0x04),
        WM_XBUTTONDOWN => press(if (wparam >> 16) & 0xffff == 1 { 0x05 } else { 0x06 }),
        WM_MOUSEWHEEL => {
            let delta = ((wparam >> 16) & 0xffff) as i16;
            wheel(delta)
        }
        _ => false,
    };
    if handled {
        return 0;
    }
    // SAFETY: the message as it came.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// The virtual key of a key message, with left and right told apart as the keys file names them.
fn virtual_key(vk: u32, lparam: LPARAM) -> Option<u8> {
    let scan = ((lparam >> 16) & 0xff) as u32;
    let extended = lparam & (1 << 24) != 0;
    match vk {
        // SAFETY: a plain conversion.
        0x10 => Some(unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) } as u8),
        0x11 => Some(if extended { 0xA3 } else { 0xA2 }),
        0x12 => extended.then_some(0xA5),
        v => u8::try_from(v).ok(),
    }
}

fn with_menu<R>(f: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    MENU.with(|m| m.borrow_mut().as_mut().map(f))
}

fn key(vk: Option<u8>) -> bool {
    let capturing = with_menu(|m| m.capture.is_some()).unwrap_or(false);
    match vk {
        Some(VK_ESC) if capturing => {
            with_menu(|m| {
                m.capture = None;
                m.note.clear();
            });
            redraw();
        }
        Some(VK_ESC | VK_F1) => {
            with_menu(|m| m.closed = true);
            // Wake the message loop.
            with_menu(|m| unsafe { PostMessageW(m.window, WM_NULL, 0, 0) });
        }
        Some(vk) if capturing => return press(vk),
        None if capturing => {
            with_menu(|m| m.note = "Left Alt gives the mouse back in the game and cannot be used.".into());
            redraw();
        }
        _ => {}
    }
    true
}

/// A key or mouse button for the slot being changed.
fn press(vk: u8) -> bool {
    let done = with_menu(|m| {
        let Some(slot) = m.capture.clone() else { return false };
        if vk == VK_F1 {
            m.note = "F1 opens this menu and cannot be used for a control.".into();
            return true;
        }
        match slot {
            Slot::NativeBind(idx, old_key) => {
                let Some(base) = native_key_name(vk) else {
                    m.note = format!("That key (code {vk:#04x}) cannot be used; pick another.");
                    return true;
                };
                let key_name = format_combo(&base, vk);
                let spec = NATIVE_ACTIONS[idx];
                if let Mode::Native { native, .. } = &mut m.mode {
                    match old_key {
                        Some(old) => native.replace_key(spec.id, spec.is_menu, &old, &key_name),
                        None => native.add_key(spec.id, spec.is_menu, &key_name),
                    }
                }
                m.capture = None;
                m.note = format!("{key_name} bound to {} (updates live within 1 s).", spec.label);
                save_native(m);
                true
            }
            Slot::LegacySwitch => {
                let Some(name) = binds::key_name(vk) else {
                    m.note = format!("That key (code {vk:#04x}) cannot be used; pick another.");
                    return true;
                };
                m.binds.switch = vec![vk];
                m.capture = None;
                m.note = format!("{name} set.");
                save_legacy(m);
                true
            }
            Slot::LegacyBind(tab, part, old) => {
                let Some(name) = binds::key_name(vk) else {
                    m.note = format!("That key (code {vk:#04x}) cannot be used; pick another.");
                    return true;
                };
                let layout = layout_mut(&mut m.binds, tab);
                match old {
                    Some(old) => {
                        for b in layout.iter_mut().filter(|b| b.0 == old && b.1 == part) {
                            b.0 = vk;
                        }
                    }
                    None => layout.push((vk, part)),
                }
                let mut seen = Vec::new();
                layout.retain(|b| {
                    let fresh = !seen.contains(b);
                    seen.push(*b);
                    fresh
                });
                m.capture = None;
                m.note = format!("{name} set.");
                save_legacy(m);
                true
            }
        }
    })
    .unwrap_or(false);
    if done {
        redraw();
    }
    done
}

fn save_legacy(m: &mut Menu) {
    if let Err(e) = binds::save(&m.binds) {
        m.note = format!("Could not write keys.txt: {e}");
    }
}

fn save_native(m: &mut Menu) {
    if let Mode::Native { path, native, .. } = &m.mode
        && let Err(e) = native.save_to(path)
    {
        m.note = format!("Could not write {}: {e}", path.display());
    }
}

fn layout_mut(b: &mut Binds, tab: LegacyTab) -> &mut binds::Layout {
    if tab == LegacyTab::Drive { &mut b.drive } else { &mut b.walk }
}

fn hit(m: &Menu, (x, y): (i32, i32)) -> Option<Hit> {
    m.hits
        .iter()
        .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
        .map(|h| h.1.clone())
}

fn click(vk: u8, at: (i32, i32)) -> bool {
    if with_menu(|m| m.capture.is_some()).unwrap_or(false) {
        return press(vk);
    }
    let Some(h) = with_menu(|m| hit(m, at)).flatten() else { return true };
    with_menu(|m| {
        m.note.clear();
        match h {
            Hit::LegacyTab(t) => {
                if let Mode::Legacy { tab } = &mut m.mode {
                    *tab = t;
                }
            }
            Hit::NativeTab(t) => {
                if let Mode::Native { tab, .. } = &mut m.mode {
                    *tab = t;
                }
            }
            Hit::LegacyKey(t, p, vk) => {
                m.capture = Some(Slot::LegacyBind(t, p, Some(vk)));
                m.note = "Press the new key or mouse button. Esc: cancel.".into();
            }
            Hit::LegacyAdd(t, p) => {
                m.capture = Some(Slot::LegacyBind(t, p, None));
                m.note = "Press a key or mouse button to add. Esc: cancel.".into();
            }
            Hit::LegacySwitch => {
                m.capture = Some(Slot::LegacySwitch);
                m.note = "Press the key that switches between on foot and driving. Esc: cancel.".into();
            }
            Hit::NativeKey(idx, old_key) => {
                m.capture = Some(Slot::NativeBind(idx, Some(old_key)));
                m.note = format!(
                    "Rebinding {}: press key, combo (Ctrl/Shift + key), mouse button, or wheel. Esc: cancel.",
                    NATIVE_ACTIONS[idx].label
                );
            }
            Hit::NativeAdd(idx) => {
                m.capture = Some(Slot::NativeBind(idx, None));
                m.note = format!(
                    "Add key for {}: press key, combo, mouse button, or wheel. Esc: cancel.",
                    NATIVE_ACTIONS[idx].label
                );
            }
            Hit::SlowerMouse | Hit::FasterMouse => {
                let step = if h == Hit::FasterMouse { 1.1f32 } else { 1.0 / 1.1 };
                m.binds.mouse.sensitivity =
                    ((m.binds.mouse.sensitivity * step).clamp(0.1, 10.0) * 100.0).round() / 100.0;
                save_legacy(m);
            }
            Hit::InvertY => {
                m.binds.mouse.invert_y = !m.binds.mouse.invert_y;
                save_legacy(m);
            }
            Hit::Reset => {
                m.binds = binds::defaults();
                save_legacy(m);
                let mut reset_note = "Controls and mouse reset to defaults.".to_string();
                if let Mode::Native { path, native, .. } = &mut m.mode {
                    let default_file = path.with_file_name("native-keys.default.txt");
                    if let Some(def) = NativeBinds::load_from(&default_file) {
                        *native = def;
                        if let Err(e) = native.save_to(path) {
                            reset_note = format!("Could not write {}: {e}", path.display());
                        } else {
                            reset_note = "Restored native-keys.default.txt and default mouse settings.".into();
                        }
                    } else {
                        reset_note = "Mouse reset (use Vertex Launcher -> Controls -> Restore All Defaults to reset native-keys.txt).".into();
                    }
                }
                m.note = reset_note;
            }
            Hit::Done => {
                m.closed = true;
                // SAFETY: our window.
                unsafe { PostMessageW(m.window, WM_NULL, 0, 0) };
            }
        }
    });
    redraw();
    true
}

fn right_click(at: (i32, i32)) -> bool {
    if with_menu(|m| m.capture.is_some()).unwrap_or(false) {
        return press(0x02);
    }
    with_menu(|m| match hit(m, at) {
        Some(Hit::LegacyKey(t, p, vk)) => {
            layout_mut(&mut m.binds, t).retain(|b| !(b.0 == vk && b.1 == p));
            m.note = format!("{} removed.", binds::key_name(vk).unwrap_or_default());
            save_legacy(m);
        }
        Some(Hit::NativeKey(idx, key_name)) => {
            let spec = NATIVE_ACTIONS[idx];
            if let Mode::Native { native, .. } = &mut m.mode {
                native.remove_key(spec.id, spec.is_menu, &key_name);
            }
            m.note = format!("{key_name} removed from {}.", spec.label);
            save_native(m);
        }
        _ => {}
    });
    redraw();
    true
}

fn wheel(delta: i16) -> bool {
    if with_menu(|m| matches!(m.capture, Some(Slot::NativeBind(..)))).unwrap_or(false) {
        return press(if delta > 0 { 0x81 } else { 0x82 });
    }
    let on_mouse_tab = with_menu(|m| match &m.mode {
        Mode::Native { tab, .. } => *tab == NativeTab::Mouse && m.capture.is_none(),
        Mode::Legacy { tab } => *tab == LegacyTab::Mouse && m.capture.is_none(),
    })
    .unwrap_or(false);
    if on_mouse_tab {
        with_menu(|m| {
            let step = if delta > 0 { 1.05f32 } else { 1.0 / 1.05 };
            m.binds.mouse.sensitivity =
                ((m.binds.mouse.sensitivity * step).clamp(0.1, 10.0) * 100.0).round() / 100.0;
            save_legacy(m);
        });
        redraw();
    }
    true
}

/// What the legacy menu calls a pad control.
fn label(part: Part) -> &'static str {
    match binds::part_name(part) {
        "A" => "A button",
        "B" => "B button",
        "X" => "X button",
        "Y" => "Y button",
        "LB" => "Left bumper (LB)",
        "RB" => "Right bumper (RB)",
        "LT" => "Left trigger (LT)",
        "RT" => "Right trigger (RT)",
        "L3" => "Left stick click (L3)",
        "R3" => "Right stick click (R3)",
        "UP" => "D-pad up",
        "DOWN" => "D-pad down",
        "LEFT" => "D-pad left",
        "RIGHT" => "D-pad right",
        "MENU" => "Menu (pause)",
        "VIEW" => "View (map)",
        "MOVE_UP" => "Move forward",
        "MOVE_DOWN" => "Move back",
        "MOVE_LEFT" => "Move left",
        "MOVE_RIGHT" => "Move right",
        "LOOK_UP" => "Look up (keys)",
        "LOOK_DOWN" => "Look down (keys)",
        "LOOK_LEFT" => "Look left (keys)",
        "LOOK_RIGHT" => "Look right (keys)",
        other => other,
    }
}

fn pad_connected() -> bool {
    (0..4).any(|slot| {
        let mut state: XINPUT_STATE = unsafe { std::mem::zeroed() };
        // SAFETY: room for the state.
        unsafe { XInputGetState(slot, &mut state) == 0 }
    })
}

// ---- drawing ----

const BG: u32 = rgb(18, 13, 30);
const ROW: u32 = rgb(28, 22, 44);
const CHIP: u32 = rgb(46, 38, 70);
const CHIP_EDGE: u32 = rgb(94, 80, 136);
const ACCENT: u32 = rgb(139, 92, 246);
const TEXT: u32 = rgb(238, 235, 248);
const MUTED: u32 = rgb(162, 155, 186);

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

struct Painter {
    dc: HDC,
    fonts: [HFONT; 3],
    scale: f32,
}

impl Painter {
    fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn fill(&self, r: RECT, colour: u32, radius: i32) {
        // SAFETY: GDI objects made and freed here, on our DC.
        unsafe {
            let brush = CreateSolidBrush(colour);
            let pen = CreatePen(PS_SOLID, 1, colour);
            let (ob, op) = (SelectObject(self.dc, brush as _), SelectObject(self.dc, pen as _));
            RoundRect(self.dc, r.left, r.top, r.right, r.bottom, radius, radius);
            SelectObject(self.dc, ob);
            SelectObject(self.dc, op);
            DeleteObject(brush as _);
            DeleteObject(pen as _);
        }
    }

    fn outline(&self, r: RECT, fill: u32, edge: u32, radius: i32) {
        // SAFETY: as in `fill`.
        unsafe {
            let brush = CreateSolidBrush(fill);
            let pen = CreatePen(PS_SOLID, self.px(1.5).max(1), edge);
            let (ob, op) = (SelectObject(self.dc, brush as _), SelectObject(self.dc, pen as _));
            RoundRect(self.dc, r.left, r.top, r.right, r.bottom, radius, radius);
            SelectObject(self.dc, ob);
            SelectObject(self.dc, op);
            DeleteObject(brush as _);
            DeleteObject(pen as _);
        }
    }

    /// `font`: 0 body, 1 bold, 2 title.
    fn text(&self, s: &str, mut r: RECT, colour: u32, font: usize, flags: u32) {
        let w: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: our DC and font; the text and rectangle are ours.
        unsafe {
            SelectObject(self.dc, self.fonts[font] as _);
            SetTextColor(self.dc, colour);
            DrawTextW(
                self.dc,
                w.as_ptr(),
                w.len() as i32,
                &mut r,
                flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        }
    }

    fn width(&self, s: &str, font: usize) -> i32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        // SAFETY: as in `text`.
        unsafe {
            SelectObject(self.dc, self.fonts[font] as _);
            GetTextExtentPoint32W(self.dc, w.as_ptr(), w.len() as i32, &mut size);
        }
        size.cx
    }
}

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT { left: l, top: t, right: r, bottom: b }
}

fn redraw() {
    with_menu(draw);
}

fn draw_mouse_tab(p: &Painter, m: &mut Menu, px0: i32, pw: i32, pad: i32, y: i32, native_file: Option<&Path>) {
    let line = p.px(52.0);
    let lx = px0 + pad;
    let row = |yy: i32| rect(lx, yy, px0 + pw - pad, yy + line - p.px(6.0));
    let r = row(y);
    p.fill(r, ROW, p.px(10.0));
    p.text(
        "Look sensitivity (mouse camera speed)",
        rect(r.left + p.px(14.0), r.top, r.right, r.bottom),
        TEXT,
        0,
        DT_LEFT,
    );
    let value = format!("{:.2}", m.binds.mouse.sensitivity);
    let bw = p.px(42.0);
    let plus = rect(r.right - p.px(8.0) - bw, r.top + p.px(6.0), r.right - p.px(8.0), r.bottom - p.px(6.0));
    let val = rect(plus.left - p.px(88.0), plus.top, plus.left - p.px(6.0), plus.bottom);
    let minus = rect(val.left - p.px(6.0) - bw, plus.top, val.left - p.px(6.0), plus.bottom);
    p.outline(minus, CHIP, CHIP_EDGE, p.px(8.0));
    p.text("-", minus, TEXT, 1, DT_CENTER);
    p.text(&value, val, ACCENT, 1, DT_CENTER);
    p.outline(plus, CHIP, CHIP_EDGE, p.px(8.0));
    p.text("+", plus, TEXT, 1, DT_CENTER);
    m.hits.push((minus, Hit::SlowerMouse));
    m.hits.push((plus, Hit::FasterMouse));

    let r2 = row(y + line);
    p.fill(r2, ROW, p.px(10.0));
    p.text(
        "Invert Y-Axis (moving mouse up looks down)",
        rect(r2.left + p.px(14.0), r2.top, r2.right, r2.bottom),
        TEXT,
        0,
        DT_LEFT,
    );
    let toggle = rect(
        r2.right - p.px(8.0) - p.px(104.0),
        r2.top + p.px(6.0),
        r2.right - p.px(8.0),
        r2.bottom - p.px(6.0),
    );
    if m.binds.mouse.invert_y {
        p.fill(toggle, ACCENT, p.px(15.0));
        p.text("On", toggle, TEXT, 1, DT_CENTER);
    } else {
        p.outline(toggle, CHIP, CHIP_EDGE, p.px(15.0));
        p.text("Off", toggle, MUTED, 1, DT_CENTER);
    }
    m.hits.push((toggle, Hit::InvertY));

    let live_line = match native_file {
        Some(path) => format!("Live file: {} (input + on-screen prompt glyphs reload within 1 s).", path.display()),
        None => "Every change is written to keys.txt in the data folder and applies at once.".to_string(),
    };
    let tips = [
        "Scroll the mouse wheel on this tab to fine-tune look sensitivity.",
        "Keyboard, mouse, and Xbox/PS5 controllers all work seamlessly together.",
        "Left Alt or Esc frees the mouse cursor; click the game window to recapture it.",
        live_line.as_str(),
    ];
    for (i, t) in tips.iter().enumerate() {
        let ty = y + 2 * line + p.px(12.0) + i as i32 * p.px(28.0);
        p.text(
            t,
            rect(lx + p.px(4.0), ty, px0 + pw - pad, ty + p.px(26.0)),
            MUTED,
            0,
            DT_LEFT | DT_END_ELLIPSIS,
        );
    }
}

fn draw(m: &mut Menu) {
    let (w, h) = (m.width, m.height);
    // Keep sizing balanced at 1080p, 1440p, and 4K (never blown up to 2x).
    let scale = (h as f32 / 1080.0).clamp(0.75, 1.25);
    // SAFETY: a DIB section and DC made and freed here; the window is ours.
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let dc = CreateCompatibleDC(screen);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            ReleaseDC(std::ptr::null_mut(), screen);
            return;
        }
        let old = SelectObject(dc, bitmap as _);
        SetBkMode(dc, TRANSPARENT as _);
        let face = wide("Segoe UI");
        let font = |size: f32, weight: i32| {
            CreateFontW(-((size * scale) as i32), 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 4, 0, face.as_ptr())
        };
        let p = Painter { dc, fonts: [font(15.5, 400), font(15.5, 600), font(26.0, 700)], scale };

        let pw = p.px(1020.0).min(w - p.px(40.0));
        let ph = p.px(740.0).min(h - p.px(40.0));
        let (px0, py0) = ((w - pw) / 2, (h - ph) / 2);
        let panel = rect(px0, py0, px0 + pw, py0 + ph);
        let radius = p.px(20.0);
        p.fill(panel, BG, radius);
        let pad = p.px(24.0);
        m.hits.clear();

        // Header.
        let mut y = py0 + pad;
        let title_str = match &m.mode {
            Mode::Native { .. } => "Controls (PC Native)",
            Mode::Legacy { .. } => "Controls",
        };
        p.text(title_str, rect(px0 + pad, y, px0 + pw - pad, y + p.px(36.0)), TEXT, 2, DT_LEFT);
        let input = if pad_connected() { "Controller + KB/M active" } else { "Keyboard & Mouse" };
        p.text(input, rect(px0 + pad, y, px0 + pw - pad, y + p.px(18.0)), ACCENT, 1, DT_RIGHT);
        p.text("F1 or Esc closes", rect(px0 + pad, y + p.px(18.0), px0 + pw - pad, y + p.px(36.0)), MUTED, 0, DT_RIGHT);
        y += p.px(46.0);

        let body_bottom = py0 + ph - pad - p.px(48.0);
        let is_native = matches!(m.mode, Mode::Native { .. });

        if is_native {
            let (cur_tab, native_clone, native_path) = match &m.mode {
                Mode::Native { tab, native, path } => (*tab, native.clone(), path.clone()),
                Mode::Legacy { .. } => unreachable!(),
            };
            let mut x = px0 + pad;
            for (tab, name) in [
                (NativeTab::OnFoot, "On Foot"),
                (NativeTab::Combat, "Combat"),
                (NativeTab::Vehicle, "Vehicle"),
                (NativeTab::Menus, "Menus"),
                (NativeTab::Mouse, "Mouse"),
            ] {
                let tw = p.width(name, 1) + p.px(30.0);
                let r = rect(x, y, x + tw, y + p.px(34.0));
                if cur_tab == tab {
                    p.fill(r, ACCENT, p.px(17.0));
                    p.text(name, r, TEXT, 1, DT_CENTER);
                } else {
                    p.outline(r, BG, CHIP_EDGE, p.px(17.0));
                    p.text(name, r, MUTED, 1, DT_CENTER);
                }
                m.hits.push((r, Hit::NativeTab(tab)));
                x += tw + p.px(8.0);
            }
            p.text(
                "Changes apply live in-game",
                rect(x, y, px0 + pw - pad, y + p.px(34.0)),
                MUTED,
                0,
                DT_RIGHT,
            );
            y += p.px(48.0);

            if cur_tab == NativeTab::Mouse {
                draw_mouse_tab(&p, m, px0, pw, pad, y, Some(&native_path));
            } else {
                let items: Vec<(usize, &ActionSpec)> = NATIVE_ACTIONS
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a.tab == cur_tab)
                    .collect();
                let rows = items.len().div_ceil(2).max(1) as i32;
                let row_h = ((body_bottom - y) / rows).min(p.px(44.0));
                let col_w = (pw - 2 * pad - p.px(14.0)) / 2;
                for (i, &(idx, spec)) in items.iter().enumerate() {
                    let (col, row) = ((i as i32) / rows, (i as i32) % rows);
                    let cx = px0 + pad + col * (col_w + p.px(14.0));
                    let cy = y + row * row_h;
                    let r = rect(cx, cy + p.px(2.0), cx + col_w, cy + row_h - p.px(2.0));
                    p.fill(r, ROW, p.px(9.0));
                    let label_w = (col_w as f32 * 0.44) as i32;
                    p.text(
                        spec.label,
                        rect(cx + p.px(11.0), r.top, cx + label_w, r.bottom),
                        TEXT,
                        0,
                        DT_LEFT | DT_END_ELLIPSIS,
                    );
                    let mut kx = cx + label_w;
                    let chip_h = (r.bottom - r.top) - p.px(8.0);
                    let ky = r.top + p.px(4.0);
                    let bound = native_clone.keys_for(spec.id, spec.is_menu);
                    for key_name in bound {
                        let capturing = m.capture == Some(Slot::NativeBind(idx, Some(key_name.clone())));
                        let text = if capturing { "press..." } else { key_name.as_str() };
                        let cw = p.width(text, 1) + p.px(20.0);
                        if kx + cw > r.right - p.px(40.0) {
                            break;
                        }
                        let c = rect(kx, ky, kx + cw, ky + chip_h);
                        if capturing {
                            p.fill(c, ACCENT, p.px(7.0));
                        } else {
                            p.outline(c, CHIP, CHIP_EDGE, p.px(7.0));
                        }
                        p.text(text, c, TEXT, 1, DT_CENTER);
                        m.hits.push((c, Hit::NativeKey(idx, key_name)));
                        kx += cw + p.px(6.0);
                    }
                    let adding = m.capture == Some(Slot::NativeBind(idx, None));
                    let add = rect(r.right - p.px(36.0), ky, r.right - p.px(7.0), ky + chip_h);
                    if adding {
                        p.fill(add, ACCENT, p.px(7.0));
                    } else {
                        p.outline(add, ROW, CHIP_EDGE, p.px(7.0));
                    }
                    p.text(if adding { "..." } else { "+" }, add, TEXT, 1, DT_CENTER);
                    m.hits.push((add, Hit::NativeAdd(idx)));
                }
            }
        } else {
            let cur_tab = match m.mode {
                Mode::Legacy { tab } => tab,
                Mode::Native { .. } => unreachable!(),
            };
            let mut x = px0 + pad;
            for (tab, name) in [(LegacyTab::Walk, "On foot"), (LegacyTab::Drive, "Driving"), (LegacyTab::Mouse, "Mouse")] {
                let tw = p.width(name, 1) + p.px(32.0);
                let r = rect(x, y, x + tw, y + p.px(34.0));
                if cur_tab == tab {
                    p.fill(r, ACCENT, p.px(17.0));
                    p.text(name, r, TEXT, 1, DT_CENTER);
                } else {
                    p.outline(r, BG, CHIP_EDGE, p.px(17.0));
                    p.text(name, r, MUTED, 1, DT_CENTER);
                }
                m.hits.push((r, Hit::LegacyTab(tab)));
                x += tw + p.px(10.0);
            }
            let switch = m.binds.switch.iter().filter_map(|&k| binds::key_name(k)).collect::<Vec<_>>().join(", ");
            let switch_text = if m.capture == Some(Slot::LegacySwitch) { "press a key...".to_string() } else { switch };
            let sw = p.width(&switch_text, 1) + p.px(24.0);
            let sr = rect(px0 + pw - pad - sw, y, px0 + pw - pad, y + p.px(34.0));
            if m.capture == Some(Slot::LegacySwitch) {
                p.fill(sr, ACCENT, p.px(9.0));
            } else {
                p.outline(sr, CHIP, CHIP_EDGE, p.px(9.0));
            }
            p.text(&switch_text, sr, TEXT, 1, DT_CENTER);
            m.hits.push((sr, Hit::LegacySwitch));
            p.text("Switch on foot / driving:", rect(x, y, sr.left - p.px(10.0), y + p.px(34.0)), MUTED, 0, DT_RIGHT);
            y += p.px(48.0);

            match cur_tab {
                LegacyTab::Walk | LegacyTab::Drive => {
                    let layout = if cur_tab == LegacyTab::Drive { m.binds.drive.clone() } else { m.binds.walk.clone() };
                    let parts: Vec<Part> = binds::parts().collect();
                    let rows = parts.len().div_ceil(2) as i32;
                    let row_h = ((body_bottom - y) / rows).min(p.px(42.0));
                    let col_w = (pw - 2 * pad - p.px(14.0)) / 2;
                    for (i, &part) in parts.iter().enumerate() {
                        let (col, row) = ((i as i32) / rows, (i as i32) % rows);
                        let cx = px0 + pad + col * (col_w + p.px(14.0));
                        let cy = y + row * row_h;
                        let r = rect(cx, cy + p.px(2.0), cx + col_w, cy + row_h - p.px(2.0));
                        p.fill(r, ROW, p.px(9.0));
                        let label_w = (col_w as f32 * 0.42) as i32;
                        p.text(
                            label(part),
                            rect(cx + p.px(11.0), r.top, cx + label_w, r.bottom),
                            TEXT,
                            0,
                            DT_LEFT | DT_END_ELLIPSIS,
                        );
                        let mut kx = cx + label_w;
                        let chip_h = (r.bottom - r.top) - p.px(8.0);
                        let ky = r.top + p.px(4.0);
                        for &(vk, _) in layout.iter().filter(|b| b.1 == part) {
                            let capturing = m.capture == Some(Slot::LegacyBind(cur_tab, part, Some(vk)));
                            let name = if capturing { "press...".to_string() } else { binds::key_name(vk).unwrap_or_default() };
                            let cw = p.width(&name, 1) + p.px(20.0);
                            if kx + cw > r.right - p.px(40.0) {
                                break;
                            }
                            let c = rect(kx, ky, kx + cw, ky + chip_h);
                            if capturing {
                                p.fill(c, ACCENT, p.px(7.0));
                            } else {
                                p.outline(c, CHIP, CHIP_EDGE, p.px(7.0));
                            }
                            p.text(&name, c, TEXT, 1, DT_CENTER);
                            m.hits.push((c, Hit::LegacyKey(cur_tab, part, vk)));
                            kx += cw + p.px(6.0);
                        }
                        let adding = m.capture == Some(Slot::LegacyBind(cur_tab, part, None));
                        let add = rect(r.right - p.px(36.0), ky, r.right - p.px(7.0), ky + chip_h);
                        if adding {
                            p.fill(add, ACCENT, p.px(7.0));
                        } else {
                            p.outline(add, ROW, CHIP_EDGE, p.px(7.0));
                        }
                        p.text(if adding { "..." } else { "+" }, add, TEXT, 1, DT_CENTER);
                        m.hits.push((add, Hit::LegacyAdd(cur_tab, part)));
                    }
                }
                LegacyTab::Mouse => draw_mouse_tab(&p, m, px0, pw, pad, y, None),
            }
        }

        // Footer: note, Reset, Done.
        let fy = py0 + ph - pad - p.px(38.0);
        let done = rect(px0 + pw - pad - p.px(110.0), fy, px0 + pw - pad, fy + p.px(38.0));
        p.fill(done, ACCENT, p.px(11.0));
        p.text("Done", done, TEXT, 1, DT_CENTER);
        m.hits.push((done, Hit::Done));
        let reset = rect(done.left - p.px(10.0) - p.px(178.0), fy, done.left - p.px(10.0), fy + p.px(38.0));
        p.outline(reset, BG, CHIP_EDGE, p.px(11.0));
        p.text("Reset to defaults", reset, MUTED, 1, DT_CENTER);
        m.hits.push((reset, Hit::Reset));
        let note = if m.note.is_empty() {
            "Click a key to rebind (combos supported), + to add, right-click to unbind."
        } else {
            m.note.as_str()
        };
        p.text(
            note,
            rect(px0 + pad, fy, reset.left - p.px(12.0), fy + p.px(38.0)),
            if m.note.is_empty() { MUTED } else { TEXT },
            0,
            DT_LEFT | DT_END_ELLIPSIS,
        );

        // Alpha: GDI leaves it 0. Inside the rounded panel opaque; outside a dimmed veil.
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (w * h) as usize);
        let rr = radius / 2;
        for yy in 0..h {
            for xx in 0..w {
                let i = (yy * w + xx) as usize;
                let inside = xx >= panel.left && xx < panel.right && yy >= panel.top && yy < panel.bottom && {
                    let cx = if xx < panel.left + rr {
                        panel.left + rr
                    } else if xx >= panel.right - rr {
                        panel.right - rr - 1
                    } else {
                        xx
                    };
                    let cy = if yy < panel.top + rr {
                        panel.top + rr
                    } else if yy >= panel.bottom - rr {
                        panel.bottom - rr - 1
                    } else {
                        yy
                    };
                    let (dx, dy) = (xx - cx, yy - cy);
                    dx * dx + dy * dy <= rr * rr
                };
                pixels[i] = if inside { pixels[i] | 0xff00_0000 } else { 0xb000_0000 };
            }
        }

        if let Some(path) = std::env::var_os("VICTORY_CONTROLS_MENU_DUMP") {
            dump(std::path::Path::new(&path), pixels, w, h);
        }

        let size = SIZE { cx: w, cy: h };
        let from = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(m.window, screen, std::ptr::null(), &size, dc, &from, 0, &blend, ULW_ALPHA);

        SelectObject(dc, old);
        DeleteObject(bitmap as _);
        for f in p.fonts {
            DeleteObject(f as _);
        }
        DeleteDC(dc);
        ReleaseDC(std::ptr::null_mut(), screen);
    }
}

/// Diagnostics (`VICTORY_CONTROLS_MENU_DUMP=<file>`): what was drawn, over a grey stand-in for the
/// game as the window composes it, as a 24-bit BMP.
fn dump(path: &std::path::Path, pixels: &[u32], w: i32, h: i32) {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size as usize);
    out.extend(b"BM");
    out.extend((size as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend(w.to_le_bytes());
    out.extend(h.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend([0u8; 24]);
    for y in (0..h).rev() {
        for x in 0..w {
            let px = pixels[(y * w + x) as usize];
            let a = px >> 24;
            for shift in [0, 8, 16] {
                let c = (px >> shift) & 0xff;
                out.push((c + 110 * (255 - a) / 255).min(255) as u8);
            }
        }
        out.resize(out.len() + (row - w * 3) as usize, 0);
    }
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_binds_parse_rebind_add_remove_and_preserve_unlisted_actions() {
        let sample = "\
# Header comment
W = INPUT_VEH_ACCELERATE, INPUT_MOVE_UP_ONLY | INPUT_GAME_MENU_UP
Space = INPUT_JUMP, INPUT_VEH_HANDBRAKE | INPUT_FRONTEND_ACCEPT
E = INPUT_CONTEXT, INPUT_UNLISTED_ACTION
";
        let mut nb = NativeBinds::parse(sample);
        assert_eq!(nb.keys_for("INPUT_JUMP", false), vec!["Space"]);
        assert_eq!(nb.keys_for("INPUT_FRONTEND_ACCEPT", true), vec!["Space"]);

        // Rebind Jump from Space to Ctrl+Space: Space still keeps INPUT_VEH_HANDBRAKE + menu accept.
        nb.replace_key("INPUT_JUMP", false, "Space", "Ctrl+Space");
        assert_eq!(nb.keys_for("INPUT_JUMP", false), vec!["Ctrl+Space"]);
        assert_eq!(nb.keys_for("INPUT_VEH_HANDBRAKE", false), vec!["Space"]);
        assert_eq!(nb.keys_for("INPUT_FRONTEND_ACCEPT", true), vec!["Space"]);

        // Add secondary key to Interact and remove E from Interact while preserving INPUT_UNLISTED_ACTION.
        nb.add_key("INPUT_CONTEXT", false, "F");
        assert_eq!(nb.keys_for("INPUT_CONTEXT", false), vec!["E", "F"]);
        nb.remove_key("INPUT_CONTEXT", false, "E");
        assert_eq!(nb.keys_for("INPUT_CONTEXT", false), vec!["F"]);
        assert_eq!(nb.keys_for("INPUT_UNLISTED_ACTION", false), vec!["E"]);

        // Round-trip serialization preserves header and all edits.
        let text = nb.format();
        assert!(text.starts_with("# Header comment\n"));
        let reparsed = NativeBinds::parse(&text);
        assert_eq!(reparsed.keys_for("INPUT_JUMP", false), vec!["Ctrl+Space"]);
        assert_eq!(reparsed.keys_for("INPUT_CONTEXT", false), vec!["F"]);
        assert_eq!(reparsed.keys_for("INPUT_UNLISTED_ACTION", false), vec!["E"]);
    }

    #[test]
    #[ignore]
    fn the_menu_draws() {
        let Some(dir) = std::env::var_os("VICTORY_CONTROLS_MENU_DUMP_DIR") else { return };
        for (tab, name) in [(LegacyTab::Walk, "walk.bmp"), (LegacyTab::Mouse, "mouse.bmp")] {
            let mut m = Menu {
                window: std::ptr::null_mut(),
                title: std::ptr::null_mut(),
                width: 2560,
                height: 1440,
                mode: Mode::Legacy { tab },
                binds: binds::defaults(),
                capture: Some(Slot::LegacyBind(LegacyTab::Walk, binds::parts().nth(6).unwrap(), Some(0x02))),
                hits: Vec::new(),
                note: String::new(),
                closed: false,
            };
            // SAFETY: a test of its own process.
            unsafe { std::env::set_var("VICTORY_CONTROLS_MENU_DUMP", std::path::Path::new(&dir).join(name)) };
            draw(&mut m);
            assert!(m.hits.len() >= 9, "{} places to click", m.hits.len());
        }
    }
}
