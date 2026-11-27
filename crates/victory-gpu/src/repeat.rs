//! Diagnostics: what of the title's draws and dispatches repeats from frame
//! to frame (`VICTORY_REPEAT_CENSUS=1`, live; off by default), for a cache
//! of translated lists. A draw is told by its shape (kind, pipeline,
//! topology, targets) and by its parts with the shape: descriptor tables,
//! root constants, root views, vertex and index buffers, arguments
//! (`crate::draw::State::census`). Each is looked for among those of the
//! last three frames: the title's rings of constants and dynamic tables
//! may turn over in a few frames.

use std::collections::HashSet;
use std::sync::Mutex;

/// Whether the census runs.
pub(crate) fn on() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("REPEAT_CENSUS", 0);
    ON.on()
}

/// One more word into a hash.
pub(crate) fn mix(h: u64, x: u64) -> u64 {
    (h.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95)
}

/// The frames to look back on.
const BACK: usize = 3;

/// What a draw is told by: its shape, its parts with the shape (in the
/// order of [`Draw`]), then the whole.
const KEYS: usize = 7;

/// A draw's hashes: shape, tables, constants, root views, buffers,
/// arguments.
pub(crate) type Draw = [u64; 6];

struct Frames {
    /// This frame's keys, then those of the frames before, the last first.
    now: [HashSet<u64>; KEYS],
    before: [[HashSet<u64>; KEYS]; BACK],
    /// Since the last line: draws, then the draws whose key was in one of
    /// the frames before, by key.
    counts: [u64; KEYS + 1],
}

impl Frames {
    fn new() -> Self {
        Self { now: Default::default(), before: Default::default(), counts: [0; KEYS + 1] }
    }

    /// A draw or dispatch with `draw`'s hashes.
    fn draw(&mut self, draw: Draw) {
        let whole = draw.iter().fold(0, |h, &k| mix(h, k));
        let keys: [u64; KEYS] = std::array::from_fn(|i| if i < draw.len() { draw[i] } else { whole });
        self.counts[0] += 1;
        for (i, key) in keys.into_iter().enumerate() {
            if self.before.iter().any(|b| b[i].contains(&key)) {
                self.counts[i + 1] += 1;
            }
            self.now[i].insert(key);
        }
    }

    /// A frame was presented: its keys become the last frame's.
    fn frame(&mut self) {
        let now = std::mem::take(&mut self.now);
        self.before.rotate_right(1);
        self.before[0] = now;
    }

    /// The counts since the last call: draws, then the repeated ones by key.
    fn taken(&mut self) -> [u64; KEYS + 1] {
        std::mem::replace(&mut self.counts, [0; KEYS + 1])
    }
}

static FRAMES: Mutex<Option<Frames>> = Mutex::new(None);

/// Since the last line: indirect calls, and of them those right after a
/// draw or dispatch of the same state in their list.
static INDIRECT: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// An indirect call is about to run; `same`: the list's last draw or
/// dispatch had its state.
pub(crate) fn indirect(same: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    INDIRECT[0].fetch_add(1, Relaxed);
    if same {
        INDIRECT[1].fetch_add(1, Relaxed);
    }
}

fn frames() -> std::sync::MutexGuard<'static, Option<Frames>> {
    FRAMES.lock().unwrap_or_else(|e| e.into_inner())
}

/// A draw or dispatch is about to run with `draw`'s hashes.
pub(crate) fn draw(draw: Draw) {
    frames().get_or_insert_with(Frames::new).draw(draw);
}

/// A frame was presented.
pub(crate) fn frame() {
    if let Some(f) = frames().as_mut() {
        f.frame();
    }
}

/// The census's line, per frame over `frames`, if it ran.
pub(crate) fn line(frames: f64) {
    let Some(c) = self::frames().as_mut().map(Frames::taken) else { return };
    if c[0] == 0 {
        return;
    }
    let share = |n: u64| 100.0 * n as f64 / c[0] as f64;
    let [calls, same] = [0, 1].map(|i| INDIRECT[i].swap(0, std::sync::atomic::Ordering::Relaxed));
    tracing::info!(
        "indirect calls per frame (VICTORY_REPEAT_CENSUS): {:.0}, of them {:.0} right after one of the same state in          their list",
        calls as f64 / frames,
        same as f64 / frames
    );
    tracing::info!(
        "draws and dispatches per frame (VICTORY_REPEAT_CENSUS): {:.0}; found in one of the last {BACK} frames: \
         the whole {:.1}%, the shape {:.1}%; with the shape, the same tables {:.1}%, constants {:.1}%, root views \
         {:.1}%, vertex and index buffers {:.1}%, arguments {:.1}%",
        c[0] as f64 / frames,
        share(c[7]),
        share(c[1]),
        share(c[2]),
        share(c[3]),
        share(c[4]),
        share(c[5]),
        share(c[6]),
    );
}

#[cfg(test)]
mod tests {
    use super::Frames;

    /// A draw counts as repeated, by part, while one like it was drawn in
    /// one of the last three frames, and no longer after that.
    #[test]
    fn a_draw_repeats_for_three_frames() {
        let mut f = Frames::new();
        let one = [11, 12, 13, 14, 15, 16];
        let mut other = one;
        other[2] = 99;
        f.draw(one);
        f.frame();
        // The constants differ: the shape and other parts were drawn the
        // frame before, the constants and the whole were not.
        f.draw(other);
        f.frame();
        f.frame();
        // `one` was drawn three frames ago.
        f.draw(one);
        for _ in 0..4 {
            f.frame();
        }
        // The last like it was drawn four frames ago.
        f.draw(other);
        // Draws; then shape, tables, constants, views, buffers, arguments, whole.
        assert_eq!(f.taken(), [4, 2, 2, 1, 2, 2, 2, 1]);
        assert_eq!(f.taken(), [0; 8], "counted again from zero");
    }
}
