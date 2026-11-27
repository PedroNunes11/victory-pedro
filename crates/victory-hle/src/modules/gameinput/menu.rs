//! The Controls menu: F1 in the game opens it over the title's window. Every control of the stand-in
//! pad, on foot and driving, with its keys and mouse buttons; click a key to change it (then press
//! the new key or mouse button), click + to add one, right-click a key to remove it; the mouse's
//! sensitivity and invert Y; the key that switches between the two layouts. Each change is written
//! to `keys.txt` at once ([`super::binds::save`]) and holds in the game from the next poll.
//!
//! The title itself has no keyboard and mouse controls (it asks GameInput for gamepads and the
//! keyboard device only, and has no such settings), so its own menus cannot hold these: this is a
//! window of ours, topmost over the title's client area, drawn with GDI into a layered window
//! (per-pixel alpha). While it is in front the title's window is not, so the title gets no keys
//! and the mouse is given back (`mouse`, `focused`). F1 or Esc closes it.
//!
//! Off with `VICTORY_CONTROLS_MENU=0`.

use std::cell::RefCell;
use std::sync::Once;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MAPVK_VSC_TO_VK_EX, MapVirtualKeyW, SetFocus};
use windows_sys::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::binds::{self, Binds, Part};

const VK_F1: u8 = 0x70;
const VK_ESC: u8 = 0x1B;

/// Starts the thread that watches for F1 while the title's window is in front.
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
        // SAFETY: plain queries.
        let down = unsafe { GetAsyncKeyState(i32::from(VK_F1)) } < 0;
        if down && !was {
            // SAFETY: a plain query.
            let front = unsafe { GetForegroundWindow() };
            if crate::modules::lifecycle::is_title_window(front) {
                open(front);
            }
        }
        was = down;
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tab {
    Walk,
    Drive,
    Mouse,
}

/// What a click or key press is changing.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot {
    /// A key of `part` in the layout `tab`: `Some(vk)` replaces that key, `None` adds one.
    Bind(Tab, Part, Option<u8>),
    /// The key that switches layouts.
    Switch,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Tab(Tab),
    Key(Tab, Part, u8),
    Add(Tab, Part),
    Switch,
    SlowerMouse,
    FasterMouse,
    InvertY,
    Reset,
    Done,
}

struct Menu {
    window: HWND,
    title: HWND,
    width: i32,
    height: i32,
    tab: Tab,
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
    let name = wide("Controls");
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
    let driving = binds::driving();
    MENU.with(|m| {
        *m.borrow_mut() = Some(Menu {
            window,
            title,
            width,
            height,
            tab: if driving { Tab::Drive } else { Tab::Walk },
            binds: binds::current(),
            capture: None,
            hits: Vec::new(),
            note: String::new(),
            closed: false,
        });
    });
    tracing::info!("controls menu opened");
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
        let Some(slot) = m.capture else { return false };
        if vk == VK_F1 {
            m.note = "F1 opens this menu and cannot be used for a control.".into();
            return true;
        }
        let Some(name) = binds::key_name(vk) else {
            m.note = format!("That key (code {vk:#04x}) cannot be used; pick another.");
            return true;
        };
        match slot {
            Slot::Switch => m.binds.switch = vec![vk],
            Slot::Bind(tab, part, old) => {
                let layout = layout_mut(&mut m.binds, tab);
                match old {
                    Some(old) => {
                        for b in layout.iter_mut().filter(|b| b.0 == old && b.1 == part) {
                            b.0 = vk;
                        }
                    }
                    None => layout.push((vk, part)),
                }
                // One entry per key and control.
                let mut seen = Vec::new();
                layout.retain(|b| {
                    let fresh = !seen.contains(b);
                    seen.push(*b);
                    fresh
                });
            }
        }
        m.capture = None;
        m.note = format!("{name} set.");
        save(m);
        true
    })
    .unwrap_or(false);
    if done {
        redraw();
    }
    done
}

fn save(m: &mut Menu) {
    if let Err(e) = binds::save(&m.binds) {
        m.note = format!("Could not write keys.txt: {e}");
    }
}

fn layout_mut(b: &mut Binds, tab: Tab) -> &mut binds::Layout {
    if tab == Tab::Drive { &mut b.drive } else { &mut b.walk }
}

fn hit(m: &Menu, (x, y): (i32, i32)) -> Option<Hit> {
    m.hits.iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|h| h.1)
}

fn click(vk: u8, at: (i32, i32)) -> bool {
    if with_menu(|m| m.capture.is_some()).unwrap_or(false) {
        return press(vk);
    }
    let Some(h) = with_menu(|m| hit(m, at)).flatten() else { return true };
    with_menu(|m| {
        m.note.clear();
        match h {
            Hit::Tab(t) => m.tab = t,
            Hit::Key(t, p, vk) => {
                m.capture = Some(Slot::Bind(t, p, Some(vk)));
                m.note = "Press the new key or mouse button. Esc: keep it.".into();
            }
            Hit::Add(t, p) => {
                m.capture = Some(Slot::Bind(t, p, None));
                m.note = "Press a key or mouse button to add. Esc: cancel.".into();
            }
            Hit::Switch => {
                m.capture = Some(Slot::Switch);
                m.note = "Press the key that switches between on foot and driving. Esc: keep it.".into();
            }
            Hit::SlowerMouse | Hit::FasterMouse => {
                let step = if h == Hit::FasterMouse { 1.1f32 } else { 1.0 / 1.1 };
                m.binds.mouse.sensitivity = ((m.binds.mouse.sensitivity * step).clamp(0.1, 10.0) * 100.0).round() / 100.0;
                save(m);
            }
            Hit::InvertY => {
                m.binds.mouse.invert_y = !m.binds.mouse.invert_y;
                save(m);
            }
            Hit::Reset => {
                m.binds = binds::defaults();
                m.note = "Keys and mouse set back to VIctory's own.".into();
                save(m);
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
    with_menu(|m| {
        if let Some(Hit::Key(t, p, vk)) = hit(m, at) {
            layout_mut(&mut m.binds, t).retain(|b| !(b.0 == vk && b.1 == p));
            m.note = format!("{} removed.", binds::key_name(vk).unwrap_or_default());
            save(m);
        }
    });
    redraw();
    true
}

fn wheel(delta: i16) -> bool {
    let on_mouse_tab = with_menu(|m| m.tab == Tab::Mouse && m.capture.is_none()).unwrap_or(false);
    if on_mouse_tab {
        with_menu(|m| {
            let step = if delta > 0 { 1.05f32 } else { 1.0 / 1.05 };
            m.binds.mouse.sensitivity = ((m.binds.mouse.sensitivity * step).clamp(0.1, 10.0) * 100.0).round() / 100.0;
            save(m);
        });
        redraw();
    }
    true
}

/// What the menu calls a pad control.
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

const BG: u32 = rgb(20, 18, 28);
const ROW: u32 = rgb(28, 25, 40);
const CHIP: u32 = rgb(46, 41, 64);
const CHIP_EDGE: u32 = rgb(86, 76, 120);
const ACCENT: u32 = rgb(139, 92, 246);
const TEXT: u32 = rgb(236, 233, 246);
const MUTED: u32 = rgb(158, 152, 182);

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
            DrawTextW(self.dc, w.as_ptr(), w.len() as i32, &mut r, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
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

/// Draws the menu and hands it to the layered window.
fn redraw() {
    with_menu(draw);
}

fn draw(m: &mut Menu) {
    let (w, h) = (m.width, m.height);
    let scale = (h as f32 / 1080.0).clamp(0.7, 2.0);
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
        let p = Painter { dc, fonts: [font(17.0, 400), font(17.0, 600), font(30.0, 700)], scale };

        // The panel.
        let pw = p.px(1080.0).min(w - p.px(40.0));
        let ph = p.px(820.0).min(h - p.px(40.0));
        let (px0, py0) = ((w - pw) / 2, (h - ph) / 2);
        let panel = rect(px0, py0, px0 + pw, py0 + ph);
        let radius = p.px(22.0);
        p.fill(panel, BG, radius);
        let pad = p.px(28.0);
        m.hits.clear();

        // Header.
        let mut y = py0 + pad;
        p.text("Controls", rect(px0 + pad, y, px0 + pw - pad, y + p.px(40.0)), TEXT, 2, DT_LEFT);
        let input = if pad_connected() { "Controller connected (both work)" } else { "Keyboard & mouse" };
        p.text(input, rect(px0 + pad, y, px0 + pw - pad, y + p.px(20.0)), ACCENT, 1, DT_RIGHT);
        p.text("F1 or Esc closes", rect(px0 + pad, y + p.px(20.0), px0 + pw - pad, y + p.px(40.0)), MUTED, 0, DT_RIGHT);
        y += p.px(52.0);

        // Tabs.
        let mut x = px0 + pad;
        for (tab, name) in [(Tab::Walk, "On foot"), (Tab::Drive, "Driving"), (Tab::Mouse, "Mouse")] {
            let tw = p.width(name, 1) + p.px(36.0);
            let r = rect(x, y, x + tw, y + p.px(38.0));
            if m.tab == tab {
                p.fill(r, ACCENT, p.px(19.0));
                p.text(name, r, TEXT, 1, DT_CENTER);
            } else {
                p.outline(r, BG, CHIP_EDGE, p.px(19.0));
                p.text(name, r, MUTED, 1, DT_CENTER);
            }
            m.hits.push((r, Hit::Tab(tab)));
            x += tw + p.px(10.0);
        }
        // The layout switch key, at the tabs' right.
        let switch = m.binds.switch.iter().filter_map(|&k| binds::key_name(k)).collect::<Vec<_>>().join(", ");
        let switch_text = if m.capture == Some(Slot::Switch) { "press a key...".to_string() } else { switch };
        let sw = p.width(&switch_text, 1) + p.px(28.0);
        let sr = rect(px0 + pw - pad - sw, y, px0 + pw - pad, y + p.px(38.0));
        if m.capture == Some(Slot::Switch) {
            p.fill(sr, ACCENT, p.px(10.0));
        } else {
            p.outline(sr, CHIP, CHIP_EDGE, p.px(10.0));
        }
        p.text(&switch_text, sr, TEXT, 1, DT_CENTER);
        m.hits.push((sr, Hit::Switch));
        p.text("Switch on foot / driving:", rect(x, y, sr.left - p.px(10.0), y + p.px(38.0)), MUTED, 0, DT_RIGHT);
        y += p.px(56.0);

        let body_bottom = py0 + ph - pad - p.px(52.0);
        match m.tab {
            Tab::Walk | Tab::Drive => {
                let tab = m.tab;
                let layout = if tab == Tab::Drive { m.binds.drive.clone() } else { m.binds.walk.clone() };
                let parts: Vec<Part> = binds::parts().collect();
                let rows = parts.len().div_ceil(2) as i32;
                let row_h = ((body_bottom - y) / rows).min(p.px(44.0));
                let col_w = (pw - 2 * pad - p.px(16.0)) / 2;
                for (i, &part) in parts.iter().enumerate() {
                    let (col, row) = ((i as i32) / rows, (i as i32) % rows);
                    let cx = px0 + pad + col * (col_w + p.px(16.0));
                    let cy = y + row * row_h;
                    let r = rect(cx, cy + p.px(2.0), cx + col_w, cy + row_h - p.px(2.0));
                    p.fill(r, ROW, p.px(10.0));
                    let label_w = (col_w as f32 * 0.42) as i32;
                    p.text(label(part), rect(cx + p.px(12.0), r.top, cx + label_w, r.bottom), TEXT, 0, DT_LEFT | DT_END_ELLIPSIS);
                    let mut kx = cx + label_w;
                    let chip_h = (r.bottom - r.top) - p.px(10.0);
                    let ky = r.top + p.px(5.0);
                    for &(vk, _) in layout.iter().filter(|b| b.1 == part) {
                        let capturing = m.capture == Some(Slot::Bind(tab, part, Some(vk)));
                        let name = if capturing { "press...".to_string() } else { binds::key_name(vk).unwrap_or_default() };
                        let cw = p.width(&name, 1) + p.px(22.0);
                        if kx + cw > r.right - p.px(44.0) {
                            break;
                        }
                        let c = rect(kx, ky, kx + cw, ky + chip_h);
                        if capturing {
                            p.fill(c, ACCENT, p.px(8.0));
                        } else {
                            p.outline(c, CHIP, CHIP_EDGE, p.px(8.0));
                        }
                        p.text(&name, c, TEXT, 1, DT_CENTER);
                        m.hits.push((c, Hit::Key(tab, part, vk)));
                        kx += cw + p.px(6.0);
                    }
                    let adding = m.capture == Some(Slot::Bind(tab, part, None));
                    let add = rect(r.right - p.px(40.0), ky, r.right - p.px(8.0), ky + chip_h);
                    if adding {
                        p.fill(add, ACCENT, p.px(8.0));
                    } else {
                        p.outline(add, ROW, CHIP_EDGE, p.px(8.0));
                    }
                    p.text(if adding { "..." } else { "+" }, add, TEXT, 1, DT_CENTER);
                    m.hits.push((add, Hit::Add(tab, part)));
                }
            }
            Tab::Mouse => {
                let line = p.px(54.0);
                let lx = px0 + pad;
                let row = |yy: i32| rect(lx, yy, px0 + pw - pad, yy + line - p.px(6.0));
                let r = row(y);
                p.fill(r, ROW, p.px(10.0));
                p.text("Look sensitivity (the mouse turns the view)", rect(r.left + p.px(14.0), r.top, r.right, r.bottom), TEXT, 0, DT_LEFT);
                let value = format!("{:.2}", m.binds.mouse.sensitivity);
                let bw = p.px(44.0);
                let plus = rect(r.right - p.px(8.0) - bw, r.top + p.px(6.0), r.right - p.px(8.0), r.bottom - p.px(6.0));
                let val = rect(plus.left - p.px(96.0), plus.top, plus.left - p.px(6.0), plus.bottom);
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
                p.text("Invert Y (moving the mouse up looks down)", rect(r2.left + p.px(14.0), r2.top, r2.right, r2.bottom), TEXT, 0, DT_LEFT);
                let toggle = rect(r2.right - p.px(8.0) - p.px(110.0), r2.top + p.px(6.0), r2.right - p.px(8.0), r2.bottom - p.px(6.0));
                if m.binds.mouse.invert_y {
                    p.fill(toggle, ACCENT, p.px(16.0));
                    p.text("On", toggle, TEXT, 1, DT_CENTER);
                } else {
                    p.outline(toggle, CHIP, CHIP_EDGE, p.px(16.0));
                    p.text("Off", toggle, MUTED, 1, DT_CENTER);
                }
                m.hits.push((toggle, Hit::InvertY));
                let tips = [
                    "The mouse wheel changes the sensitivity while this page is open.",
                    "Left click is the right trigger (shoot), right click the left trigger (aim), as the On foot page binds them.",
                    "Click in the game's window to take the mouse; Left Alt or Esc gives it back.",
                    "Every change is written to keys.txt in the data folder and holds at once.",
                ];
                for (i, t) in tips.iter().enumerate() {
                    let ty = y + 2 * line + p.px(10.0) + i as i32 * p.px(30.0);
                    p.text(t, rect(lx + p.px(4.0), ty, px0 + pw - pad, ty + p.px(28.0)), MUTED, 0, DT_LEFT | DT_END_ELLIPSIS);
                }
            }
        }

        // Footer: note, Reset, Done.
        let fy = py0 + ph - pad - p.px(42.0);
        let done = rect(px0 + pw - pad - p.px(120.0), fy, px0 + pw - pad, fy + p.px(42.0));
        p.fill(done, ACCENT, p.px(12.0));
        p.text("Done", done, TEXT, 1, DT_CENTER);
        m.hits.push((done, Hit::Done));
        let reset = rect(done.left - p.px(12.0) - p.px(200.0), fy, done.left - p.px(12.0), fy + p.px(42.0));
        p.outline(reset, BG, CHIP_EDGE, p.px(12.0));
        p.text("Reset to defaults", reset, MUTED, 1, DT_CENTER);
        m.hits.push((reset, Hit::Reset));
        let note = if m.note.is_empty() {
            "Click a key to change it, + to add one, right-click a key to remove it."
        } else {
            m.note.as_str()
        };
        p.text(note, rect(px0 + pad, fy, reset.left - p.px(12.0), fy + p.px(42.0)), if m.note.is_empty() { MUTED } else { TEXT }, 0, DT_LEFT | DT_END_ELLIPSIS);

        // Alpha: GDI leaves it 0. Inside the rounded panel opaque; outside a dimmed veil.
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (w * h) as usize);
        let rr = radius / 2;
        for yy in 0..h {
            for xx in 0..w {
                let i = (yy * w + xx) as usize;
                let inside = xx >= panel.left && xx < panel.right && yy >= panel.top && yy < panel.bottom && {
                    let cx = if xx < panel.left + rr { panel.left + rr } else if xx >= panel.right - rr { panel.right - rr - 1 } else { xx };
                    let cy = if yy < panel.top + rr { panel.top + rr } else if yy >= panel.bottom - rr { panel.bottom - rr - 1 } else { yy };
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
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
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
                // Premultiplied over a mid grey.
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

    /// Draws two pages into files to look at (no window, no title): ignored, run by hand with
    /// `VICTORY_CONTROLS_MENU_DUMP_DIR` set.
    #[test]
    #[ignore]
    fn the_menu_draws() {
        let Some(dir) = std::env::var_os("VICTORY_CONTROLS_MENU_DUMP_DIR") else { return };
        for (tab, name) in [(Tab::Walk, "walk.bmp"), (Tab::Mouse, "mouse.bmp")] {
            let mut m = Menu {
                window: std::ptr::null_mut(),
                title: std::ptr::null_mut(),
                width: 2560,
                height: 1440,
                tab,
                binds: binds::defaults(),
                capture: Some(Slot::Bind(Tab::Walk, binds::parts().nth(6).unwrap(), Some(0x02))),
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
