//! The mouse as part of the stand-in pad: its buttons press pad controls
//! ([`super::binds`]) and its motion pushes the right stick. A left click
//! inside the title's window takes the mouse: the cursor is hidden, kept in
//! the window's client area and held at its centre, and the view turns by
//! the mouse's own motion (raw input). Left Alt, Esc, a Windows key,
//! Alt+Tab, another window in front or a connected pad gives it back at
//! once; Esc still presses the pad's Menu (keys.txt).
//!
//! It is looked after on a thread of its own ([`Grab`], every
//! [`host::TICK`]), not when the title reads the pad: a slow frame does not
//! let the cursor out.

#[cfg(test)]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Virtual keys of the mouse's buttons.
pub(super) const LEFT: u8 = 0x01;
pub(super) const RIGHT: u8 = 0x02;
pub(super) const MIDDLE: u8 = 0x04;
pub(super) const BACK: u8 = 0x05;
pub(super) const FORWARD: u8 = 0x06;
const BUTTONS: [u8; 5] = [LEFT, RIGHT, MIDDLE, BACK, FORWARD];
/// Left Alt gives the mouse back.
pub(super) const RELEASE: u8 = 0xA4;
pub(super) const ESC: u8 = 0x1B;
const LEFT_WIN: u8 = 0x5B;
const RIGHT_WIN: u8 = 0x5C;
const RIGHT_ALT: u8 = 0xA5;

/// Keys that give the mouse back while held. Right Alt alone does not (it
/// can be bound to a control), with Tab it does.
const LET_GO: [u8; 4] = [RELEASE, ESC, LEFT_WIN, RIGHT_WIN];
/// In native KB/M mode, Esc pauses and navigates menus so it must NOT release
/// mouse capture (Left Alt, Win keys, RightAlt+Tab, or focus loss still do).
const LET_GO_NATIVE: [u8; 3] = [RELEASE, LEFT_WIN, RIGHT_WIN];

/// Whether the keys held give the mouse back.
#[cfg(test)]
pub(super) fn letting_go(mut down: impl FnMut(u8) -> bool) -> bool {
    letting_go_mode(&mut down, true)
}

/// Whether the keys held give the mouse back; `esc_releases` is false in
/// native KB/M mode so Esc can pause and navigate menus without dropping mouse.
pub(super) fn letting_go_mode(mut down: impl FnMut(u8) -> bool, esc_releases: bool) -> bool {
    let keys: &[u8] = if esc_releases { &LET_GO } else { &LET_GO_NATIVE };
    keys.iter().copied().any(&mut down) || (down(super::TAB) && down(RIGHT_ALT))
}

/// What the mouse does to the stand-in pad; nothing while it is not taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Mouse {
    /// Buttons held, as virtual keys.
    pub buttons: Vec<u8>,
    /// The right stick, up positive.
    pub stick: (f32, f32),
    /// Raw mouse motion since the previous poll (down positive), while taken.
    pub raw: (i32, i32),
    /// Scroll wheel notches since the previous poll (+up, -down), while taken.
    pub wheel: i32,
}

/// The title's window has the mouse.
pub(crate) static TAKEN: AtomicBool = AtomicBool::new(false);

/// The title's window has the mouse, so it shows no cursor.
pub(crate) fn taken() -> bool {
    TAKEN.load(Ordering::Relaxed)
}

/// Mouse counts per second that push the stick all the way, at sensitivity 1.
#[cfg(test)]
const FULL_SPEED: f32 = 2000.0;
/// Titles ignore a stick this close to its centre (XInput's value for the
/// right stick), so the slowest motion starts here.
#[cfg(test)]
const DEAD_ZONE: f32 = 8689.0 / 32767.0;

/// Frame-locked camera look tuning (no sliding-window bleed, crisp inverse curve).
const CAM_FULL_SPEED: f32 = 1400.0;
const CAM_DEAD_ZONE: f32 = 0.13;
const CAM_CURVE_EXP: f32 = 0.62;
/// Minimum/maximum frame delta used when normalizing per-frame raw counts.
const MIN_DT_US: u64 = 10_000;
const MAX_DT_US: u64 = 80_000;
const IDLE_RESET_US: u64 = 150_000;

/// Counter bumped whenever the title actually reads a new stand-in pad frame.
static PAD_FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PAD_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PAD_DT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(33_333);

/// Called from `current_reading` when the title reads the stand-in pad.
pub(super) fn note_pad_frame(now_us: u64) {
    let prev = PAD_LAST_US.load(Ordering::Relaxed);
    if prev == 0 {
        PAD_LAST_US.store(now_us, Ordering::Relaxed);
        PAD_FRAME.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let gap = now_us.saturating_sub(prev);
    if gap < Pace::SAME_FRAME {
        return;
    }
    PAD_LAST_US.store(now_us, Ordering::Relaxed);
    PAD_DT_US.store(gap.clamp(MIN_DT_US, MAX_DT_US), Ordering::Relaxed);
    PAD_GAP_US.store(gap, Ordering::Relaxed);
    PAD_FRAME.fetch_add(1, Ordering::Relaxed);
}

/// The last gap between two reads of the stand-in pad, not clamped (the map cursor's frames at
/// 3 fps are 330 ms).
static PAD_GAP_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(33_333);

/// The map screen's cursor driven by the mouse as a distance, not a speed: the title moves its map
/// cursor with the left stick (`INPUT_FRONTEND_MAP_MOVE_LR/UD`) at a speed for each deflection, so
/// mouse counts are owed distance (a fraction of the screen's width, whatever the output or render
/// size) and each frame the stick is set to cover what is owed during the title's next frame, what
/// the last frame covered paid off. Nothing is dropped between the title's reads, so the cursor
/// lands where the mouse went at 3 fps as at 30.
///
/// `VICTORY_MAP_CURSOR_SPEED` (screen widths per second at full stick, 0.6),
/// `VICTORY_MAP_DEAD_ZONE` (the title's map stick dead zone, 0.24) and
/// `VICTORY_MAP_CURSOR_COUNTS` (mouse counts per screen width at sensitivity 1, 1600) tune it.
pub(super) struct MapCursor {
    /// Owed distance, screen widths (x right, y up).
    owed: (f32, f32),
    /// The deflection beyond the dead zone sent for the frame now running (0..1).
    sent: (f32, f32),
    frame: u64,
    speed: f32,
    dead_zone: f32,
    counts: f32,
}

impl Default for MapCursor {
    fn default() -> Self {
        let env = |name: &str, default: f32, range: std::ops::RangeInclusive<f32>| {
            std::env::var(name).ok().and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| range.contains(v)).unwrap_or(default)
        };
        Self {
            owed: (0.0, 0.0),
            sent: (0.0, 0.0),
            frame: PAD_FRAME.load(Ordering::Relaxed),
            speed: env("VICTORY_MAP_CURSOR_SPEED", 0.6, 0.05..=10.0),
            dead_zone: env("VICTORY_MAP_DEAD_ZONE", 0.24, 0.0..=0.9),
            counts: env("VICTORY_MAP_CURSOR_COUNTS", 1600.0, 100.0..=20000.0),
        }
    }
}

impl MapCursor {
    /// Owed distance kept at most (a screen width): a fling does not run on for seconds.
    const MOST_OWED: f32 = 1.0;

    pub fn clear(&mut self) {
        self.owed = (0.0, 0.0);
        self.sent = (0.0, 0.0);
        self.frame = PAD_FRAME.load(Ordering::Relaxed);
    }

    /// Adds raw mouse motion (down positive) and returns the stick for the title's next frame (up
    /// positive).
    pub fn step(&mut self, (dx, dy): (i32, i32), sensitivity: f32) -> (f32, f32) {
        let frame = PAD_FRAME.load(Ordering::Relaxed);
        let gap_s = (PAD_GAP_US.load(Ordering::Relaxed).clamp(5_000, 1_000_000) as f32) * 1e-6;
        self.step_at(frame, gap_s, (dx, dy), sensitivity)
    }

    fn step_at(&mut self, frame: u64, gap_s: f32, (dx, dy): (i32, i32), sensitivity: f32) -> (f32, f32) {
        if frame != self.frame {
            // The title ran a frame with what was sent: that much is paid.
            let frames = frame.wrapping_sub(self.frame).min(4) as f32;
            self.owed.0 -= self.sent.0 * self.speed * gap_s * frames;
            self.owed.1 -= self.sent.1 * self.speed * gap_s * frames;
            self.frame = frame;
            // Overshoot by rounding is not owed back.
            if self.sent.0 != 0.0 && self.owed.0.signum() != self.sent.0.signum() {
                self.owed.0 = 0.0;
            }
            if self.sent.1 != 0.0 && self.owed.1.signum() != self.sent.1.signum() {
                self.owed.1 = 0.0;
            }
        }
        let per_count = sensitivity.max(0.05) / self.counts;
        self.owed.0 = (self.owed.0 + dx as f32 * per_count).clamp(-Self::MOST_OWED, Self::MOST_OWED);
        self.owed.1 = (self.owed.1 - dy as f32 * per_count).clamp(-Self::MOST_OWED, Self::MOST_OWED);
        // Less than a thousandth of the screen owed: rest.
        if self.owed.0.hypot(self.owed.1) < 0.001 {
            self.sent = (0.0, 0.0);
            return (0.0, 0.0);
        }
        // What covers the owed distance in one frame like the last.
        let reach = self.speed * gap_s;
        let (mut x, mut y) = (self.owed.0 / reach, self.owed.1 / reach);
        let len = x.hypot(y);
        if len > 1.0 {
            x /= len;
            y /= len;
        }
        self.sent = (x, y);
        // The title ignores the dead zone and scales the rest: start past it.
        let len = x.hypot(y);
        let push = self.dead_zone + (1.0 - self.dead_zone) * len;
        (x / len * push, y / len * push)
    }
}

/// Frame-locked mouse look: accumulates raw deltas until the title reads the
/// pad once, then zeroes immediately so motion NEVER bleeds into the next frame.
pub(super) struct FrameLook {
    last_frame: u64,
    last_motion: u64,
    dx: i32,
    dy: i32,
    sensitivity: f32,
}

impl FrameLook {
    pub fn new(sensitivity: f32) -> Self {
        Self {
            last_frame: PAD_FRAME.load(Ordering::Relaxed),
            last_motion: 0,
            dx: 0,
            dy: 0,
            sensitivity,
        }
    }

    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity;
    }

    pub fn clear(&mut self) {
        self.dx = 0;
        self.dy = 0;
        self.last_motion = 0;
        self.last_frame = PAD_FRAME.load(Ordering::Relaxed);
    }

    /// Updates accumulated raw delta for the current title frame and returns
    /// the resulting right-stick deflection (up positive).
    pub fn step(&mut self, now_us: u64, frame: u64, dt_us: u64, moved: (i32, i32)) -> (f32, f32) {
        if frame != self.last_frame {
            self.last_frame = frame;
            self.dx = 0;
            self.dy = 0;
        } else if (self.dx != 0 || self.dy != 0)
            && now_us.saturating_sub(self.last_motion) > IDLE_RESET_US
        {
            self.dx = 0;
            self.dy = 0;
        }
        if moved != (0, 0) {
            self.dx = self.dx.saturating_add(moved.0);
            self.dy = self.dy.saturating_add(moved.1);
            self.last_motion = now_us;
        }
        let x = self.dx as f32;
        let y = -(self.dy as f32);
        let length = x.hypot(y);
        if length == 0.0 {
            return (0.0, 0.0);
        }
        let dt_s = (dt_us.clamp(MIN_DT_US, MAX_DT_US) as f32) * 1e-6;
        let speed = (length / (dt_s * CAM_FULL_SPEED)) * self.sensitivity;
        let curved = speed.min(1.0).powf(CAM_CURVE_EXP);
        let push = CAM_DEAD_ZONE + (1.0 - CAM_DEAD_ZONE) * curved;
        (x / length * push, y / length * push)
    }
}

/// Converts raw mouse motion inside pause/settings menus (where the Xbox build
/// draws no PC pointer) into crisp D-pad navigation steps.
#[derive(Default)]
pub(super) struct MenuStepper {
    acc_x: i32,
    acc_y: i32,
    last_motion: u64,
}

impl MenuStepper {
    const STEP_X: i32 = 56;
    const STEP_Y: i32 = 42;
    const IDLE_US: u64 = 250_000;

    pub fn clear(&mut self) {
        self.acc_x = 0;
        self.acc_y = 0;
        self.last_motion = 0;
    }

    /// Adds raw mouse delta (down positive) and returns D-pad button bits
    /// (UP/DOWN/LEFT/RIGHT) for any steps crossed.
    pub fn step(&mut self, now_us: u64, (dx, dy): (i32, i32)) -> u32 {
        if now_us.saturating_sub(self.last_motion) > Self::IDLE_US {
            self.acc_x = 0;
            self.acc_y = 0;
        }
        if dx != 0 || dy != 0 {
            self.acc_x = self.acc_x.saturating_add(dx);
            self.acc_y = self.acc_y.saturating_add(dy);
            self.last_motion = now_us;
        }
        let mut bits = 0u32;
        if self.acc_y <= -Self::STEP_Y {
            bits |= super::UP;
            self.acc_y += Self::STEP_Y;
            self.acc_x = 0;
        } else if self.acc_y >= Self::STEP_Y {
            bits |= super::DOWN;
            self.acc_y -= Self::STEP_Y;
            self.acc_x = 0;
        } else if self.acc_x <= -Self::STEP_X {
            bits |= super::LEFT;
            self.acc_x += Self::STEP_X;
            self.acc_y = 0;
        } else if self.acc_x >= Self::STEP_X {
            bits |= super::RIGHT;
            self.acc_x -= Self::STEP_X;
            self.acc_y = 0;
        }
        bits
    }
}

/// Virtual radial stick for Weapon Wheel / Radio Wheel selection: accumulates
/// raw mouse deltas into a 2D position clamped to the unit circle and holds
/// that angle until the wheel closes (unlike camera look, which zeroes on stop).
#[derive(Default)]
pub(super) struct WheelStick {
    x: f32,
    y: f32,
}

impl WheelStick {
    const RADIUS: f32 = 95.0;
    const DEAD_ZONE: f32 = 0.14;

    pub fn clear(&mut self) {
        self.x = 0.0;
        self.y = 0.0;
    }

    /// Adds raw mouse delta (down positive) and returns the held stick position (up positive).
    pub fn step(&mut self, (dx, dy): (i32, i32), sensitivity: f32) -> (f32, f32) {
        let scale = sensitivity.max(0.2) / Self::RADIUS;
        self.x += (dx as f32) * scale;
        self.y -= (dy as f32) * scale;
        let len = self.x.hypot(self.y);
        if len > 1.0 {
            self.x /= len;
            self.y /= len;
        }
        if len < Self::DEAD_ZONE {
            (0.0, 0.0)
        } else {
            (self.x, self.y)
        }
    }
}

/// Mouse motion as a stick. A mouse reports distance and a stick speed, and
/// the title looks only once per frame: the stick is the mean speed over the
/// time between two looks, so a frame turns by all the motion made since the
/// one before, however long it took.
#[cfg(test)]
pub(super) struct Look {
    /// When (microseconds), and how far.
    moves: VecDeque<(u64, i32, i32)>,
    sensitivity: f32,
}

#[cfg(test)]
#[allow(dead_code)]
impl Look {
    pub fn new(sensitivity: f32) -> Look {
        Look {
            moves: VecDeque::new(),
            sensitivity,
        }
    }

    /// A new speed (the Controls menu, or the keys file changed).
    pub fn set_sensitivity(&mut self, sensitivity: f32) {
        self.sensitivity = sensitivity;
    }

    pub fn clear(&mut self) {
        self.moves.clear();
    }

    /// Adds motion (down positive, as mice report it); the stick now.
    pub fn stick(&mut self, now: u64, window: Duration, (dx, dy): (i32, i32)) -> (f32, f32) {
        let window = (window.as_micros() as u64).max(1);
        if (dx, dy) != (0, 0) {
            self.moves.push_back((now, dx, dy));
        }
        while self.moves.front().is_some_and(|m| m.0 + window <= now) {
            self.moves.pop_front();
        }
        let (x, y) = self
            .moves
            .iter()
            .fold((0.0, 0.0), |(x, y), m| (x + m.1 as f32, y - m.2 as f32));
        let length = x.hypot(y);
        if length == 0.0 {
            return (0.0, 0.0);
        }
        let speed = length * 1e6 / window as f32 * self.sensitivity / FULL_SPEED;
        let push = DEAD_ZONE + (1.0 - DEAD_ZONE) * speed.min(1.0);
        (x / length * push, y / length * push)
    }
}

/// How often the title looks at the stand-in pad.
#[derive(Default)]
pub(super) struct Pace {
    last: Option<u64>,
    interval: u64,
}

impl Pace {
    /// Several looks this close together belong to one frame.
    const SAME_FRAME: u64 = 8_000;
    const SHORTEST: u64 = 16_000;
    const LONGEST: u64 = 500_000;

    pub fn look(&mut self, now: u64) {
        let Some(last) = self.last else {
            self.last = Some(now);
            return;
        };
        let gap = now.saturating_sub(last);
        if gap < Self::SAME_FRAME {
            return;
        }
        self.last = Some(now);
        let gap = gap.min(Self::LONGEST);
        self.interval = if self.interval == 0 {
            gap
        } else {
            (self.interval * 3 + gap) / 4
        };
    }

    pub fn interval(&self) -> Duration {
        Duration::from_micros(self.interval.clamp(Self::SHORTEST, Self::LONGEST))
    }
}

/// Who has the mouse.
#[derive(Default)]
pub(super) struct Capture {
    taken: bool,
    left_was_down: bool,
    /// The click that took the mouse presses nothing.
    swallow: bool,
}

impl Capture {
    /// One poll. `title`: the title's window is in front and no pad is
    /// connected; `inside`: the cursor is in its client area. The buttons
    /// that count, `None` while the mouse is not taken.
    pub fn step(
        &mut self,
        title: bool,
        inside: bool,
        release: bool,
        mut held: Vec<u8>,
    ) -> Option<Vec<u8>> {
        let left = held.contains(&LEFT);
        let clicked = left && !self.left_was_down;
        self.left_was_down = left;
        self.swallow &= left;
        if !title || release {
            self.taken = false;
        } else if !self.taken && clicked && inside {
            self.taken = true;
            self.swallow = true;
        }
        if !self.taken {
            return None;
        }
        if self.swallow {
            held.retain(|&vk| vk != LEFT);
        }
        Some(held)
    }
}

/// A rectangle on the screen, right and bottom outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Area {
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        (self.left..self.right).contains(&x) && (self.top..self.bottom).contains(&y)
    }

    fn centre(&self) -> (i32, i32) {
        (
            self.left + (self.right - self.left) / 2,
            self.top + (self.bottom - self.top) / 2,
        )
    }
}

/// What taking the mouse needs of the system: Win32 in a run, a fake in
/// tests.
pub(super) trait Desktop {
    /// The client area, on the screen, of the title's window while it is in
    /// front; `None` while another window is, or it is minimised.
    fn title_area(&mut self) -> Option<Area>;
    /// Where the cursor is.
    fn cursor(&mut self) -> Option<(i32, i32)>;
    /// Whether a key or a mouse button is held.
    fn down(&mut self, vk: u8) -> bool;
    /// Where the cursor is kept now; `None` if not known.
    fn clip(&mut self) -> Option<Area>;
    /// Keeps the cursor in `area`, or lets it go anywhere (`None`).
    fn set_clip(&mut self, area: Option<Area>);
    /// Puts the cursor there.
    fn put_cursor(&mut self, at: (i32, i32));
    /// Hides the cursor over the title's window, or shows it again.
    fn hide_cursor(&mut self, hidden: bool);
}

/// The mouse taken and given back, on the screen: [`Capture`] plus what the
/// system must do about it.
#[derive(Default)]
pub(super) struct Grab {
    capture: Capture,
    /// The cursor is hidden and kept in the window.
    held: bool,
}

impl Grab {
    /// One look at the desktop; `wanted`: no pad is connected. The buttons
    /// that count, `None` while the mouse is not taken.
    #[cfg(test)]
    pub fn tick(&mut self, desktop: &mut impl Desktop, wanted: bool) -> Option<Vec<u8>> {
        self.tick_mode(desktop, wanted, true)
    }

    /// Like [`tick`], with `esc_releases` controlling whether Esc frees the cursor.
    pub fn tick_mode(
        &mut self,
        desktop: &mut impl Desktop,
        wanted: bool,
        esc_releases: bool,
    ) -> Option<Vec<u8>> {
        let area = if wanted { desktop.title_area() } else { None };
        let at = desktop.cursor();
        let inside = area.zip(at).is_some_and(|(area, at)| area.contains(at));
        let release = letting_go_mode(|vk| desktop.down(vk), esc_releases);
        let held = BUTTONS.into_iter().filter(|&vk| desktop.down(vk)).collect();
        let buttons = self.capture.step(area.is_some(), inside, release, held);
        match (&buttons, area) {
            (Some(_), Some(area)) => {
                if !self.held {
                    self.held = true;
                    desktop.hide_cursor(true);
                }
                // The system drops the clip on its own (focus changes), and
                // the window moves and changes size: checked every tick.
                if desktop.clip() != Some(area) {
                    desktop.set_clip(Some(area));
                }
                // At the centre the cursor is never near an edge, even if
                // something let the clip go between two ticks. The view
                // turns by raw motion, which this does not change.
                let centre = area.centre();
                if at != Some(centre) {
                    desktop.put_cursor(centre);
                }
            }
            _ if self.held => {
                self.held = false;
                desktop.set_clip(None);
                desktop.hide_cursor(false);
            }
            _ => {}
        }
        buttons
    }

    pub fn held(&self) -> bool {
        self.held
    }
}

#[cfg(windows)]
pub(super) use host::Host;

#[cfg(windows)]
mod host {
    use std::sync::atomic::AtomicI32;
    use std::sync::{Mutex, Once, PoisonError};
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
        RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging as wm;
    use windows_sys::core::w;

    use super::*;
    use crate::modules::lifecycle;

    /// How often the mouse thread looks at the desktop.
    pub(super) const TICK: Duration = Duration::from_millis(4);

    /// Motion and scroll wheel notches since the last poll.
    static DX: AtomicI32 = AtomicI32::new(0);
    static DY: AtomicI32 = AtomicI32::new(0);
    static WHEEL: AtomicI32 = AtomicI32::new(0);
    /// No pad is connected (the pad poll says so).
    static WANTED: AtomicBool = AtomicBool::new(false);
    /// The buttons that count, `None` while the mouse is not taken.
    static HELD: Mutex<Option<Vec<u8>>> = Mutex::new(None);

    pub(in super::super) struct Host {
        look: FrameLook,
        taken: bool,
        /// VICTORY_MOUSE_SENSITIVITY is set: it wins over the keys file's.
        env_sensitivity: bool,
    }

    impl Default for Host {
        fn default() -> Host {
            let sensitivity = std::env::var("VICTORY_MOUSE_SENSITIVITY")
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .filter(|s: &f32| s.is_finite() && *s > 0.0)
                .unwrap_or(1.0);
            Host {
                look: FrameLook::new(sensitivity),
                taken: false,
                env_sensitivity: std::env::var_os("VICTORY_MOUSE_SENSITIVITY").is_some(),
            }
        }
    }

    impl Host {
        /// `wanted`: no pad is connected.
        pub fn poll(&mut self, now: u64, wanted: bool, _window: Duration) -> Mouse {
            WANTED.store(wanted, Ordering::Relaxed);
            start();
            let held = HELD.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if held.is_some() != self.taken {
                self.taken = held.is_some();
                self.look.clear();
            }
            // Motion and wheel while the mouse is not taken turn nothing.
            let mut moved = (DX.swap(0, Ordering::Relaxed), DY.swap(0, Ordering::Relaxed));
            let wheel = WHEEL.swap(0, Ordering::Relaxed);
            // The keys file's [mouse] section (the Controls menu writes it).
            let options = super::super::binds::mouse_options();
            if !self.env_sensitivity {
                self.look.set_sensitivity(options.sensitivity);
            }
            if options.invert_y {
                moved.1 = -moved.1;
            }
            let Some(buttons) = held else {
                return Mouse::default();
            };
            let frame = PAD_FRAME.load(Ordering::Relaxed);
            let dt_us = PAD_DT_US.load(Ordering::Relaxed);
            Mouse {
                buttons,
                stick: self.look.step(now, frame, dt_us, moved),
                raw: moved,
                wheel,
            }
        }
    }

    /// The desktop through Win32.
    struct Win32;

    impl Desktop for Win32 {
        fn title_area(&mut self) -> Option<Area> {
            // SAFETY: plain queries.
            let front = unsafe { wm::GetForegroundWindow() };
            if !lifecycle::is_title_window(front) || unsafe { wm::IsIconic(front) } != 0 {
                return None;
            }
            client_area(front)
        }

        fn cursor(&mut self) -> Option<(i32, i32)> {
            let mut at = POINT::default();
            // SAFETY: our own point.
            (unsafe { wm::GetCursorPos(&mut at) } != 0).then_some((at.x, at.y))
        }

        fn down(&mut self, vk: u8) -> bool {
            // SAFETY: a plain query.
            unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
        }

        fn clip(&mut self) -> Option<Area> {
            let mut clip = RECT::default();
            // SAFETY: our own rectangle.
            (unsafe { wm::GetClipCursor(&mut clip) } != 0).then_some(Area {
                left: clip.left,
                top: clip.top,
                right: clip.right,
                bottom: clip.bottom,
            })
        }

        fn set_clip(&mut self, area: Option<Area>) {
            let rect = area.map(|a| RECT {
                left: a.left,
                top: a.top,
                right: a.right,
                bottom: a.bottom,
            });
            // SAFETY: our own rectangle, or none: the cursor goes anywhere.
            unsafe { wm::ClipCursor(rect.as_ref().map_or(std::ptr::null(), |r| r)) };
        }

        fn put_cursor(&mut self, (x, y): (i32, i32)) {
            // SAFETY: a plain call.
            unsafe { wm::SetCursorPos(x, y) };
        }

        fn hide_cursor(&mut self, hidden: bool) {
            // The title's window hides it while this is set (lifecycle's
            // WM_SETCURSOR).
            TAKEN.store(hidden, Ordering::Relaxed);
            if !hidden && let Some(at) = self.cursor() {
                // A move where it is: the window sets its cursor again now,
                // not at the next motion.
                self.put_cursor(at);
            }
        }
    }

    /// The window's client area on the screen; `None` when it has none
    /// (minimised).
    fn client_area(hwnd: HWND) -> Option<Area> {
        let mut area = RECT::default();
        let mut corner = POINT::default();
        // SAFETY: our own rectangle and point; a window that is gone fails.
        let known = unsafe {
            wm::GetClientRect(hwnd, &mut area) != 0 && ClientToScreen(hwnd, &mut corner) != 0
        };
        (known && area.right > 0 && area.bottom > 0).then(|| Area {
            left: corner.x,
            top: corner.y,
            right: corner.x + area.right,
            bottom: corner.y + area.bottom,
        })
    }

    /// Starts the mouse thread: raw motion, and the cursor looked after.
    fn start() {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let thread = std::thread::Builder::new()
                .name("victory-mouse".into())
                .spawn(|| {
                    if open().is_null() {
                        tracing::warn!("no raw mouse input: the mouse does not turn the view");
                    }
                    run();
                });
            if let Err(e) = thread {
                tracing::warn!(%e, "no mouse thread: the mouse is not taken");
            }
        });
    }

    /// A message-only window of this thread that gets raw mouse input, with
    /// or without focus; null if there is none to be had.
    fn open() -> HWND {
        // SAFETY: a window of a system class, and our own registration.
        unsafe {
            let null = std::ptr::null_mut();
            let hwnd = wm::CreateWindowExW(
                0,
                w!("STATIC"),
                w!("victory mouse"),
                0,
                0,
                0,
                0,
                0,
                wm::HWND_MESSAGE,
                null,
                null,
                std::ptr::null(),
            );
            let mouse = RAWINPUTDEVICE {
                usUsagePage: 1,
                usUsage: 2,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };
            if hwnd.is_null()
                || RegisterRawInputDevices(&mouse, 1, size_of::<RAWINPUTDEVICE>() as u32) == 0
            {
                return null;
            }
            hwnd
        }
    }

    /// Reads this thread's raw mouse input and looks after the cursor every
    /// [`TICK`], until the process ends.
    fn run() {
        let mut grab = Grab::default();
        let mut last: Option<Instant> = None;
        loop {
            let wait = last.map_or(Duration::ZERO, |at| TICK.saturating_sub(at.elapsed()));
            // SAFETY: waits for this thread's messages or the time; the
            // standard message loop.
            unsafe {
                wm::MsgWaitForMultipleObjects(
                    0,
                    std::ptr::null(),
                    0,
                    wait.as_millis() as u32,
                    wm::QS_ALLINPUT,
                );
                let mut message = wm::MSG::default();
                while wm::PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, wm::PM_REMOVE) != 0
                {
                    if message.message == wm::WM_QUIT {
                        return;
                    }
                    if message.message == wm::WM_INPUT {
                        moved(message.lParam);
                    }
                    wm::DispatchMessageW(&message);
                }
            }
            if last.is_some_and(|at| at.elapsed() < TICK) {
                continue;
            }
            last = Some(Instant::now());
            let was = grab.held();
            static ESC_RELEASES: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
                !std::env::var_os("VICTORY_NATIVE_INPUT").is_some_and(|v| v == "1")
            });
            let buttons = grab.tick_mode(&mut Win32, WANTED.load(Ordering::Relaxed), *ESC_RELEASES);
            if grab.held() != was {
                tracing::info!(taken = !was, "mouse");
                DX.store(0, Ordering::Relaxed);
                DY.store(0, Ordering::Relaxed);
                WHEEL.store(0, Ordering::Relaxed);
            }
            *HELD.lock().unwrap_or_else(PoisonError::into_inner) = buttons;
        }
    }

    fn moved(handle: isize) {
        const RI_MOUSE_WHEEL: u16 = 0x0400;
        let mut input = RAWINPUT::default();
        let mut size = size_of::<RAWINPUT>() as u32;
        // SAFETY: room for one report and its size; the union is read as
        // what the header says it is.
        unsafe {
            let got = GetRawInputData(
                handle as _,
                RID_INPUT,
                (&raw mut input).cast(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            );
            if got == u32::MAX || input.header.dwType != RIM_TYPEMOUSE {
                return;
            }
            let mouse = input.data.mouse;
            // shortcut: pointers that report places (tablets, remote
            // desktops) press buttons but do not turn the view.
            if mouse.usFlags & MOUSE_MOVE_ABSOLUTE == 0 {
                DX.fetch_add(mouse.lLastX, Ordering::Relaxed);
                DY.fetch_add(mouse.lLastY, Ordering::Relaxed);
            }
            let flags = mouse.Anonymous.Anonymous.usButtonFlags;
            if flags & RI_MOUSE_WHEEL != 0 {
                let delta = mouse.Anonymous.Anonymous.usButtonData as i16;
                let notches = if delta.abs() >= 120 {
                    i32::from(delta / 120)
                } else {
                    i32::from(delta.signum())
                };
                if notches != 0 {
                    WHEEL.fetch_add(notches, Ordering::Relaxed);
                }
            }
        }
    }

    #[test]
    fn raw_mouse_input_opens() {
        let hwnd = open();
        assert!(!hwnd.is_null());
        // SAFETY: this thread's window.
        unsafe { wm::DestroyWindow(hwnd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(100);

    #[test]
    fn motion_pushes_the_stick_past_the_dead_zone_and_rests_when_it_stops() {
        let mut look = Look::new(1.0);
        assert_eq!(look.stick(0, FRAME, (0, 0)), (0.0, 0.0));
        // Right, as slow as it gets: just past the dead zone.
        let (x, y) = look.stick(1_000, FRAME, (1, 0));
        assert!(x > DEAD_ZONE && x < DEAD_ZONE + 0.01 && y == 0.0);
        // Mouse down is stick down.
        let (_, y) = look.stick(2_000, FRAME, (0, 50));
        assert!(y < -DEAD_ZONE);
        // Nothing in the window any more.
        assert_eq!(look.stick(200_000, FRAME, (0, 0)), (0.0, 0.0));
        // However fast, the stick stays on its circle.
        let (x, y) = look.stick(300_000, FRAME, (-9_000, -9_000));
        assert!((x.hypot(y) - 1.0).abs() < 1e-5 && x < 0.0 && y > 0.0);
    }

    #[test]
    fn all_motion_between_two_looks_counts() {
        // 100 counts in 100 ms are half of full speed, in one go or in ten.
        let half = DEAD_ZONE + (1.0 - DEAD_ZONE) * 0.5;
        let mut look = Look::new(1.0);
        assert!((look.stick(0, FRAME, (100, 0)).0 - half).abs() < 1e-5);
        let mut look = Look::new(1.0);
        let mut last = 0.0;
        for n in 0..10 {
            last = look.stick(n * 10_000, FRAME, (10, 0)).0;
        }
        assert!((last - half).abs() < 1e-5);
        // Sensitivity scales the speed.
        let mut fast = Look::new(2.0);
        assert!((fast.stick(0, FRAME, (100, 0)).0 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn the_window_follows_the_title() {
        let mut pace = Pace::default();
        assert_eq!(pace.interval(), Duration::from_millis(16));
        // Two looks a frame, 200 ms a frame.
        for n in 0..4 {
            pace.look(n * 200_000);
            pace.look(n * 200_000 + 1_000);
        }
        assert_eq!(pace.interval(), Duration::from_millis(200));
        // A long stop (a loading screen) does not stretch it for good.
        pace.look(60_000_000);
        assert!(pace.interval() <= Duration::from_millis(500));
    }

    #[test]
    fn a_click_in_the_window_takes_the_mouse_and_alt_gives_it_back() {
        let mut c = Capture::default();
        // Not taken: buttons press nothing, a click outside takes nothing.
        assert_eq!(c.step(true, true, false, vec![RIGHT]), None);
        assert_eq!(c.step(true, false, false, vec![LEFT]), None);
        // Held into the window is no click in it.
        assert_eq!(c.step(true, true, false, vec![LEFT]), None);
        assert_eq!(c.step(true, true, false, vec![]), None);
        // The click that takes the mouse presses nothing itself.
        assert_eq!(c.step(true, true, false, vec![LEFT]), Some(vec![]));
        assert_eq!(
            c.step(true, true, false, vec![LEFT, RIGHT]),
            Some(vec![RIGHT])
        );
        assert_eq!(c.step(true, true, false, vec![]), Some(vec![]));
        assert_eq!(c.step(true, true, false, vec![LEFT]), Some(vec![LEFT]));
        assert_eq!(c.step(true, true, false, vec![]), Some(vec![]));
        // Alt, then a click takes it again.
        assert_eq!(c.step(true, true, true, vec![]), None);
        assert_eq!(c.step(true, true, false, vec![]), None);
        assert_eq!(c.step(true, true, false, vec![LEFT]), Some(vec![]));
        // Another window in front, or a pad connected.
        assert_eq!(c.step(false, true, false, vec![]), None);
        assert_eq!(c.step(true, true, false, vec![]), None);
    }

    /// A desktop with one window, a cursor and keys.
    #[derive(Default)]
    struct Fake {
        /// The title's window, while it is in front.
        area: Option<Area>,
        at: (i32, i32),
        keys: Vec<u8>,
        clip: Option<Area>,
        hidden: bool,
        clips: usize,
    }

    impl Desktop for Fake {
        fn title_area(&mut self) -> Option<Area> {
            self.area
        }
        fn cursor(&mut self) -> Option<(i32, i32)> {
            Some(self.at)
        }
        fn down(&mut self, vk: u8) -> bool {
            self.keys.contains(&vk)
        }
        fn clip(&mut self) -> Option<Area> {
            self.clip
        }
        fn set_clip(&mut self, area: Option<Area>) {
            self.clip = area;
            self.clips += 1;
        }
        fn put_cursor(&mut self, at: (i32, i32)) {
            self.at = at;
        }
        fn hide_cursor(&mut self, hidden: bool) {
            self.hidden = hidden;
        }
    }

    const WINDOW: Area = Area {
        left: 100,
        top: 50,
        right: 900,
        bottom: 650,
    };
    const CENTRE: (i32, i32) = (500, 350);

    /// A focused title window, the cursor over it.
    fn focused() -> Fake {
        Fake {
            area: Some(WINDOW),
            at: (200, 100),
            ..Fake::default()
        }
    }

    /// A left click: down for one tick, then up. What the down tick gave.
    fn click(grab: &mut Grab, desktop: &mut Fake) -> Option<Vec<u8>> {
        desktop.keys.push(LEFT);
        let got = grab.tick(desktop, true);
        desktop.keys.retain(|&vk| vk != LEFT);
        grab.tick(desktop, true);
        got
    }

    fn free(desktop: &Fake) -> bool {
        desktop.clip.is_none() && !desktop.hidden
    }

    #[test]
    fn a_click_in_the_focused_window_hides_the_cursor_and_keeps_it_inside() {
        let (mut grab, mut desktop) = (Grab::default(), focused());
        assert_eq!(grab.tick(&mut desktop, true), None);
        assert!(free(&desktop) && desktop.clips == 0);
        // A click outside the client area takes nothing.
        desktop.at = (50, 20);
        assert_eq!(click(&mut grab, &mut desktop), None);
        assert!(free(&desktop));
        // One inside does, and presses nothing in the title.
        desktop.at = (200, 100);
        assert_eq!(click(&mut grab, &mut desktop), Some(vec![]));
        assert!(desktop.hidden && grab.held());
        assert_eq!((desktop.clip, desktop.at), (Some(WINDOW), CENTRE));
        // Aiming: the buttons count, and the cursor goes back to the centre
        // however far it was pushed.
        desktop.keys = vec![RIGHT];
        desktop.at = (899, 649);
        assert_eq!(grab.tick(&mut desktop, true), Some(vec![RIGHT]));
        assert_eq!(desktop.at, CENTRE);
        // Nothing changed: the clip is not set again.
        let clips = desktop.clips;
        grab.tick(&mut desktop, true);
        assert_eq!(desktop.clips, clips);
    }

    #[test]
    fn each_key_that_lets_go_frees_the_cursor_at_once() {
        let keys: [&[u8]; 5] = [
            &[RELEASE],
            &[ESC],
            &[LEFT_WIN],
            &[RIGHT_WIN],
            &[RIGHT_ALT, super::super::TAB],
        ];
        for keys in keys {
            let (mut grab, mut desktop) = (Grab::default(), focused());
            assert_eq!(click(&mut grab, &mut desktop), Some(vec![]));
            desktop.keys = keys.to_vec();
            assert_eq!(grab.tick(&mut desktop, true), None, "{keys:?}");
            assert!(free(&desktop) && !grab.held(), "{keys:?}");
            // It stays free until the next click in the window.
            desktop.keys.clear();
            assert_eq!(grab.tick(&mut desktop, true), None);
            assert!(free(&desktop));
            assert_eq!(click(&mut grab, &mut desktop), Some(vec![]));
            assert!(desktop.hidden && desktop.clip == Some(WINDOW));
        }
        // Right Alt alone is a key like any other.
        let (mut grab, mut desktop) = (Grab::default(), focused());
        click(&mut grab, &mut desktop);
        desktop.keys = vec![RIGHT_ALT];
        assert_eq!(grab.tick(&mut desktop, true), Some(vec![]));
        assert!(desktop.hidden);
    }

    #[test]
    fn focus_lost_frees_the_cursor_and_focus_back_waits_for_a_click() {
        let (mut grab, mut desktop) = (Grab::default(), focused());
        click(&mut grab, &mut desktop);
        desktop.area = None;
        assert_eq!(grab.tick(&mut desktop, true), None);
        assert!(free(&desktop));
        // Away, the clip is left alone (another program may set its own).
        let clips = desktop.clips;
        grab.tick(&mut desktop, true);
        assert_eq!(desktop.clips, clips);
        desktop.area = Some(WINDOW);
        assert_eq!(grab.tick(&mut desktop, true), None);
        assert!(free(&desktop));
        assert_eq!(click(&mut grab, &mut desktop), Some(vec![]));
        assert_eq!(desktop.clip, Some(WINDOW));
    }

    #[test]
    fn the_clip_follows_the_window_and_comes_back_when_the_system_drops_it() {
        let (mut grab, mut desktop) = (Grab::default(), focused());
        click(&mut grab, &mut desktop);
        // Moved and made larger.
        let moved = Area {
            left: -1000,
            top: 0,
            right: 600,
            bottom: 900,
        };
        desktop.area = Some(moved);
        grab.tick(&mut desktop, true);
        assert_eq!((desktop.clip, desktop.at), (Some(moved), (-200, 450)));
        // The system let the cursor go.
        desktop.clip = None;
        grab.tick(&mut desktop, true);
        assert_eq!(desktop.clip, Some(moved));
        assert!(desktop.hidden);
    }

    #[test]
    fn a_connected_pad_frees_the_cursor() {
        let (mut grab, mut desktop) = (Grab::default(), focused());
        click(&mut grab, &mut desktop);
        assert_eq!(grab.tick(&mut desktop, false), None);
        assert!(free(&desktop));
        // While it is there, a click takes nothing.
        desktop.keys = vec![LEFT];
        assert_eq!(grab.tick(&mut desktop, false), None);
        assert!(free(&desktop));
    }

    #[test]
    fn native_mode_keeps_mouse_on_esc_and_releases_on_alt() {
        let (mut grab, mut desktop) = (Grab::default(), focused());
        assert_eq!(click(&mut grab, &mut desktop), Some(vec![]));
        desktop.keys = vec![ESC];
        assert_eq!(grab.tick_mode(&mut desktop, true, false), Some(vec![]));
        assert!(desktop.hidden && grab.held());
        desktop.keys = vec![RELEASE];
        assert_eq!(grab.tick_mode(&mut desktop, true, false), None);
        assert!(free(&desktop) && !grab.held());
    }

    #[test]
    fn frame_look_stops_instantly_on_new_frame_without_bleed() {
        let mut look = FrameLook::new(1.0);
        let (x, y) = look.step(10_000, 1, 33_333, (20, -10));
        assert!(x > CAM_DEAD_ZONE && y > 0.0);
        // Same frame: another poll with zero new delta still preserves the frame's motion.
        assert_eq!(look.step(18_000, 1, 33_333, (0, 0)), (x, y));
        // Title consumed frame 1 -> frame 2 starts with zero bleed.
        assert_eq!(look.step(26_000, 2, 33_333, (0, 0)), (0.0, 0.0));
    }

    #[test]
    fn menu_stepper_emits_crisp_dpad_steps() {
        let mut stepper = MenuStepper::default();
        assert_eq!(stepper.step(10_000, (0, 20)), 0);
        assert_eq!(stepper.step(20_000, (0, 25)), super::super::DOWN);
        assert_eq!(stepper.step(30_000, (0, 0)), 0);
        assert_eq!(stepper.step(40_000, (0, -50)), super::super::UP);
    }

    #[test]
    fn the_map_cursor_covers_the_mouse_distance_at_any_frame_rate() {
        // A model of the title: each frame its cursor moves speed * gap * deflection past the dead zone.
        for gap in [0.033f32, 0.25] {
            let mut m = MapCursor { speed: 0.6, dead_zone: 0.24, counts: 1600.0, ..MapCursor::default() };
            m.frame = 0;
            let mut cursor = 0.0f32;
            let mut stick = m.step_at(0, gap, (800, 0), 1.0); // half a screen width to the right
            for f in 1..200u64 {
                let past = ((stick.0.abs() - 0.24).max(0.0) / 0.76) * stick.0.signum();
                cursor += past * 0.6 * gap;
                stick = m.step_at(f, gap, (0, 0), 1.0);
            }
            assert!((cursor - 0.5).abs() < 0.01, "gap {gap}: the cursor went {cursor} of 0.5");
            assert_eq!(stick, (0.0, 0.0), "rests once there");
        }
        // Up is up.
        let mut m = MapCursor::default();
        m.frame = 0;
        assert!(m.step_at(0, 0.033, (0, -50), 1.0).1 > 0.0);
    }

    #[test]
    fn wheel_stick_holds_position_and_clamps_to_unit_circle() {
        let mut wheel = WheelStick::default();
        // Tiny jitter stays inside deadzone.
        assert_eq!(wheel.step((5, 0), 1.0), (0.0, 0.0));
        // Flick up-right selects a wedge and holds it even when mouse stops.
        let (x, y) = wheel.step((120, -120), 1.0);
        assert!(x > 0.65 && y > 0.65);
        assert!((x.hypot(y) - 1.0).abs() < 1e-4);
        assert_eq!(wheel.step((0, 0), 1.0), (x, y));
        // Closing the wheel resets to center.
        wheel.clear();
        assert_eq!(wheel.step((0, 0), 1.0), (0.0, 0.0));
    }
}
