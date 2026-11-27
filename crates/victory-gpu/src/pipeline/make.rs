//! Host pipelines are made by workers, not by the title thread that first
//! records a draw with them. Translating a shader and the host driver's
//! compile take from milliseconds to seconds each (structurisation of the
//! biggest compute programs ~0.4 s, some driver compiles 3.4 s), and
//! the title's first world frame wants over 1,200 new pipelines: made
//! on its recording threads, that frame took more than 30 s and the
//! title's GPU watchdog ended the run (decision 0007, "Pipelines made off
//! the title's threads").
//!
//! A first use queues the pipeline and waits for it within a budget per
//! frame, so the cheap ones (menus, one-off passes such as a colour-grading
//! bake) are there at once as before; past the budget a draw or dispatch
//! whose pipeline is not ready is dropped, as an untranslated one is.
//!
//! The default budget is half a second per title frame. A pass the title
//! runs only once and whose pipeline takes longer is lost: while a world
//! loads (pipelines are asked for by the hundred then) a frame may wait
//! longer, for compute pipelines most (`LOADING_FRAME_COMPUTE`), within a
//! total for the run (`VICTORY_PIPELINE_GATE_S`). Passes over the screen,
//! which fill lighting tables once, go ahead of the others and are waited
//! for longer (`Pending::screen_pass`).
//!
//! Some pipelines are never made (`left_out`): programs that only shade the
//! hits of ray-tracing structures nothing builds, which would stand in the
//! queue for minutes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::Shader;
use super::host::{GraphicsState, HostPipeline};

/// What a host pipeline is made from, and the host pipeline once made.
pub(crate) struct Pending {
    shaders: Vec<Arc<Shader>>,
    pub(crate) graphics: Option<Box<GraphicsState>>,
    queue_state: AtomicU8,
    made: OnceLock<Option<HostPipeline>>,
    /// What identifies it across runs (shader containers and state), and
    /// whether a draw or dispatch has requested it yet.
    pub(super) key: String,
    used: AtomicBool,
    /// Microseconds draws have waited for it while it was not made, over the
    /// run ([`wait_once`]).
    waited_us: AtomicU64,
}

/// Why a pipeline with these shaders is not made, if it is not: the
/// compute programs of `left_out.rs`.
pub(super) fn left_out(shaders: &[Arc<Shader>]) -> Option<&'static str> {
    use super::left_out::{ACTIVATE_RAYS, RAY_TRACING_RESOLVES};
    let on = |name: &str, value: &str| std::env::var_os(name).is_some_and(|v| v == value);
    // Made with the structure builder on, or when asked for.
    static RESOLVES: OnceLock<bool> = OnceLock::new();
    let resolves = *RESOLVES.get_or_init(|| on("VICTORY_RT_RESOLVE", "1") || super::builder_allowed());
    static ACTIVATE: OnceLock<bool> = OnceLock::new();
    let activate = *ACTIVATE.get_or_init(|| on("VICTORY_ACTIVATE_RAYS", "0"));
    static EXTRA: OnceLock<Vec<String>> = OnceLock::new();
    let extra = EXTRA.get_or_init(|| {
        std::env::var("VICTORY_LEFT_OUT")
            .unwrap_or_default()
            .split(',')
            .map(|hash| hash.trim().to_ascii_lowercase())
            .filter(|hash| !hash.is_empty())
            .collect()
    });
    shaders.iter().filter(|s| s.stage == "compute").find_map(|s| {
        let hash = s.hash.as_str();
        if !resolves && RAY_TRACING_RESOLVES.binary_search(&hash).is_ok() {
            Some("shades ray-tracing hits, left out")
        } else if activate && hash == ACTIVATE_RAYS {
            Some("too dear to compile, left out")
        } else if extra.iter().any(|e| e == hash) {
            Some("left out by VICTORY_LEFT_OUT")
        } else {
            None
        }
    })
}

/// Pipelines left out so far, for the log.
static LEFT_OUT: AtomicU64 = AtomicU64::new(0);

// SAFETY: D3D12 objects are free-threaded; the graphics state (whose input
// elements point into its own names) is only read after creation.
unsafe impl Send for Pending {}
// SAFETY: as above.
unsafe impl Sync for Pending {}

impl Pending {
    /// Whether it is a compute pipeline.
    fn computes(&self) -> bool {
        matches!(self.shaders.as_slice(), [only] if only.stage == "compute")
    }

    /// Whether it draws a pass over the screen with a vertex and a pixel
    /// program ([`GraphicsState::screen_pass`]). The title fills lighting
    /// tables with such draws once, at a world's first frame; behind a
    /// world's worth of pipelines to make they waited longer than any frame
    /// does and were dropped, and a run that lost them had no diffuse light
    /// (a grey picture, white glare indoors). They go ahead of the others
    /// and a frame waits for them longer ([`SCREEN_FRAME`]).
    /// `VICTORY_SCREEN_PASSES=0` treats them as the others.
    /// (After Belobug/VIctory-Belobug-AMD 18e57db, which gives them workers
    /// of their own.)
    fn screen_pass(shaders: &[Arc<Shader>], graphics: Option<&GraphicsState>) -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("VICTORY_SCREEN_PASSES").is_none_or(|v| v != "0"))
            && graphics.is_some_and(GraphicsState::screen_pass)
            && shaders.iter().any(|s| s.stage == "vertex")
            && shaders.iter().all(|s| s.stage == "vertex" || s.stage == "pixel")
    }

    pub(crate) fn new(shaders: Vec<Arc<Shader>>, graphics: Option<Box<GraphicsState>>) -> Pending {
        let key = key(&shaders, graphics.as_deref());
        let screen = Self::screen_pass(&shaders, graphics.as_deref());
        let left_out = left_out(&shaders);
        if let Some(why) = left_out {
            let count = LEFT_OUT.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count.is_power_of_two() {
                tracing::info!(count, ?shaders, why, "host pipelines left out so far");
            }
        }
        Pending {
            shaders,
            graphics,
            queue_state: AtomicU8::new(NOT_QUEUED),
            made: OnceLock::new(),
            key,
            used: AtomicBool::new(false),
            waited_us: AtomicU64::new(0),
        }
    }

    fn make(&self) {
        let _ = self.begun.set(Instant::now());
        self.made
            .get_or_init(|| HostPipeline::new(&self.shaders, self.graphics.as_deref()));
        let (lock, done) = &DONE;
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        done.notify_all();
    }
}

/// Identifies the title pipeline by its shader containers and graphics state.
pub(crate) fn key(shaders: &[Arc<Shader>], graphics: Option<&GraphicsState>) -> String {
    let mut key = blake3::Hasher::new();
    for shader in shaders {
        key.update(shader.stage.as_bytes());
        key.update(shader.hash.as_bytes());
    }
    key.update(graphics.map(GraphicsState::describe).unwrap_or_default().as_bytes());
    key.finalize().to_hex()[..32].to_owned()
}

/// Why a pipeline has no host pipeline (diagnostics): its shaders that do
/// not translate, else "refused" or "being made"; `None` if it has one.
#[cfg(feature = "diagnostics")]
pub(crate) fn missing(p: &Pending) -> Option<String> {
    match p.made.get() {
        Some(Some(_)) => None,
        None => Some(p.left_out.map_or_else(|| "being made".into(), str::to_owned)),
        Some(None) => {
            let failed: Vec<String> = p
                .shaders
                .iter()
                .filter(|s| matches!(s.translated.get(), Some(None)))
                .map(|s| format!("{} {}", s.stage, s.hash))
                .collect();
            Some(if failed.is_empty() {
                format!("refused {:?}", p.shaders)
            } else {
                failed.join(" + ")
            })
        }
    }
}

/// Signalled whenever a worker has made a pipeline.
static DONE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Waiting allowed per title frame, over all threads.
fn wait_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_PIPELINE_WAIT_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(500);
        tracing::info!(millis, "host pipeline wait budget per frame");
        Duration::from_millis(millis)
    })
}

/// Whether the host's driver has been at `p` for longer than most take
/// (one of the title's takes 85 s): no frame waits for it any more.
fn slow(p: &Pending) -> bool {
    // A pass a title runs once (it unpacks a model, say) is lost if its
    // pipeline is not there; a draw comes again in the next frame.
    let most = if p.screen {
        SCREEN_FRAME.as_secs()
    } else if p.computes() {
        10
    } else {
        3
    };
    p.begun.get().is_some_and(|at| at.elapsed() > Duration::from_secs(most))
}

/// What a frame may wait while a world loads: pipelines are asked for by
/// the hundred then, and a pass the title runs once is lost if its
/// pipeline is not there. All such waits of a run together are bounded;
/// the title's GPU watchdog bounds one frame's (30 s).
const LOADING_FRAME: Duration = Duration::from_secs(4);
/// The same for compute pipelines, waited for apart: the passes that unpack
/// a world's models are dispatched once, and in a run whose pipelines were
/// all still to be loaded 130 of them were dropped, and with them walls,
/// floor, ceiling and faces (they never came back).
const LOADING_FRAME_COMPUTE: Duration = Duration::from_secs(12);
/// What a frame may wait for passes over the screen ([`Pending::screen_pass`]),
/// apart from the others and at any time (below the title's 30 s watchdog).
const SCREEN_FRAME: Duration = Duration::from_secs(20);

/// What is left of the bound on all the waits past the frame's budget of a
/// run, after `spent` of them. `VICTORY_PIPELINE_GATE_S` sets it: 120 s
/// without it, and 0 for none of them.
fn loading_left(spent: Duration) -> Duration {
    static TOTAL: OnceLock<Duration> = OnceLock::new();
    let total = *TOTAL.get_or_init(|| {
        let seconds = std::env::var("VICTORY_PIPELINE_GATE_S")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(120);
        Duration::from_secs(seconds)
    });
    total.saturating_sub(spent)
}

/// Pipelines queued and not made yet.
static WAITING: AtomicU64 = AtomicU64::new(0);

/// Pipelines queued and not made yet (for the frame line).
pub(crate) fn waiting() -> u64 {
    WAITING.load(Ordering::Relaxed)
}

/// The host pipeline if it is made (or is made within the frame's budget);
/// queues it on first use.
pub(crate) fn ready(p: &Arc<Pending>) -> Option<&HostPipeline> {
    if p.left_out.is_some() {
        return None;
    }
    // A still-pending pipeline was requested by the title too. Prewarm it
    // on the next launch, even if this run never got to use it.
    if !p.used.load(Ordering::Relaxed) && !p.used.swap(true, Ordering::Relaxed) {
        super::library::note_used(&p.key);
    }
    if p.made.get().is_none() {
        start(p, REQUESTED);
        wait(p, crate::frame::current());
    }
    p.made.get()?.as_ref()
}

/// Makes `p` on this thread if it is not made yet (waiting for a worker
/// that is making it); whether it has a host pipeline.
pub(crate) fn now(p: &Arc<Pending>) -> bool {
    if p.left_out.is_some() {
        return false;
    }
    p.make();
    p.made.get().is_some_and(|made| made.is_some())
}

/// At the title's creation of a pipeline: starts it at once if an earlier
/// run drew with it (during loading, so the world's first frames find it).
pub(crate) fn early(p: &Arc<Pending>) {
    if p.left_out.is_none() && !p.shaders.is_empty() && super::library::wanted_early(&p.key) {
        super::library::EARLY.fetch_add(1, Ordering::Relaxed);
        start(p, EARLY);
    }
}

/// Whether the frame's budget is one for all threads together: a thread
/// that waits takes what is left of it and gives back what it did not
/// use. Counted only when a wait had ended, the budget was one per thread:
/// each of the title's five recording threads began its first wait of a
/// frame with nothing counted yet and waited the whole half second, 2.5 s
/// of waiting in every frame in which pipelines were being made (runs
/// 34-49: "2504 ms waiting for pipelines" in each such frame).
/// On by default; `VICTORY_PIPELINE_WAIT_SHARED=0` counts as before.
fn shared_budget() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WAIT_SHARED").is_none_or(|v| v != "0"))
}

/// Whether one pipeline gets one frame's budget of waiting over the whole
/// run instead of one in every frame (`VICTORY_PIPELINE_WAIT_ONCE=1`, live,
/// off by default). A pipeline the driver takes longer than the budget to
/// make otherwise costs the whole budget in every frame a draw asks for it:
/// the picture stands still while what it draws is in view and moves once
/// the camera turns away. Past its own budget its draws are dropped until
/// it is made, as with any pipeline past the frame's budget.
///
/// Idea from rorodev6/victory-nvidia-megafork commit `77e13a9` (the defect
/// of fxceless issue 1181), GPL-3.0, written anew here; see THIRD_PARTY.md.
fn wait_once() -> bool {
    static ON: crate::live::Switch = crate::live::Switch::new("PIPELINE_WAIT_ONCE", 0);
    ON.on()
}

/// Pipelines that used up their own budget ([`wait_once`]), since the start.
pub(crate) static WAITED_OUT: AtomicU64 = AtomicU64::new(0);

/// Waits for `p` while frame `frame` has budget left.
fn wait(p: &Pending, frame: u64) {
    let budget = wait_budget();
    if budget.is_zero() || slow(p) {
        return;
    }
    let once = wait_once();
    // What is left of this pipeline's own budget ([`wait_once`]).
    let own = if once {
        let own = budget.saturating_sub(Duration::from_micros(p.waited_us.load(Ordering::Relaxed)));
        if own.is_zero() {
            return;
        }
        own
    } else {
        Duration::MAX
    };
    // The frame the budget belongs to, and what was waited of it (or is
    // taken by a thread waiting now); for each of the three kinds.
    static SPENTS: [Mutex<(u64, Duration)>; 3] = [const { Mutex::new((u64::MAX, Duration::ZERO)) }; 3];
    let frame_spent = &SPENTS[kind];
    let shared = shared_budget();
    let left = {
        let mut spent = frame_spent.lock().unwrap_or_else(|e| e.into_inner());
        if spent.0 != frame {
            *spent = (frame, Duration::ZERO);
        }
        let left = budget.saturating_sub(spent.1).min(own);
        if shared {
            spent.1 += left;
        }
        left
    };
    if left.is_zero() {
        return;
    }
    let started = Instant::now();
    let (lock, done) = &DONE;
    let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
    while p.made.get().is_none() && !slow(p) {
        let rest = left.saturating_sub(started.elapsed());
        if rest.is_zero() {
            break;
        }
        // Woken now and then to see whether it has become a slow one.
        let rest = rest.min(Duration::from_millis(250));
        held = done.wait_timeout(held, rest).unwrap_or_else(|e| e.into_inner()).0;
    }
    drop(held);
    let waited = started.elapsed().min(left);
    if once && p.made.get().is_none() {
        let micros = waited.as_micros() as u64;
        let before = p.waited_us.fetch_add(micros, Ordering::Relaxed);
        let limit = budget.as_micros() as u64;
        if before < limit && before + micros >= limit && WAITED_OUT.fetch_add(1, Ordering::Relaxed) < 20 {
            tracing::info!(
                shaders = ?p.shaders,
                ms = budget.as_millis() as u64,
                "a pipeline not made within its own wait: its draws are dropped until it is (VICTORY_PIPELINE_WAIT_ONCE)"
            );
        }
    }
    {
        let mut spent = frame_spent.lock().unwrap_or_else(|e| e.into_inner());
        // A frame that began meanwhile has its own budget.
        if spent.0 == frame {
            if shared {
                spent.1 = spent.1.saturating_sub(left - waited);
            } else {
                spent.1 += waited;
            }
        }
    }
    crate::framelog::add(crate::framelog::T::PipelineWaitUs, started.elapsed().as_micros() as u64);
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        crate::copystat::C::PipelineWaitUs,
        started.elapsed().as_micros() as u64,
    );
}

const NOT_QUEUED: u8 = 0;
const EARLY: u8 = 1;
const REQUESTED: u8 = 2;
const MAKING: u8 = 3;

struct Queue {
    early: VecDeque<Arc<Pending>>,
    early_depth: VecDeque<Arc<Pending>>,
    requested: VecDeque<Arc<Pending>>,
}

impl Queue {
    const fn new() -> Self {
        Self {
            early: VecDeque::new(),
            early_depth: VecDeque::new(),
            requested: VecDeque::new(),
        }
    }

    /// Queues an unrequested pipeline or promotes one queued at creation.
    /// Returns whether it is new to the queue, or `None` if nothing changed.
    fn push(&mut self, p: &Arc<Pending>, priority: u8) -> Option<bool> {
        let prev = p.queue_state.load(Ordering::Relaxed);
        if prev >= priority {
            return None;
        }
        p.queue_state.store(priority, Ordering::Relaxed);
        if priority == REQUESTED && p.screen {
            self.requested.push_front(p.clone());
        } else if priority == REQUESTED {
            self.requested.push_back(p.clone());
        } else if p.graphics.as_ref().is_some_and(|state| state.writes_depth_only()) {
            // Shadow maps and depth prepasses supply several later draws. Rebuild
            // their previously used PSOs first while no draw is requesting one.
            self.early_depth.push_back(p.clone());
        } else {
            self.early.push_back(p.clone());
        }
        Some(prev == NOT_QUEUED)
    }

    fn pop(&mut self) -> Option<Arc<Pending>> {
        let p = self.requested.pop_front().or_else(|| {
            // An early entry can have been promoted and claimed already.
            std::iter::from_fn(|| self.early_depth.pop_front().or_else(|| self.early.pop_front()))
                .find(|p| p.queue_state.load(Ordering::Relaxed) == EARLY)
        })?;
        p.queue_state.store(MAKING, Ordering::Relaxed);
        Some(p)
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue::new());
static AVAILABLE: Condvar = Condvar::new();

/// Start or promote work for a draw without waiting behind the entire early
/// list. Queue changes and worker claims happen under the same lock.
fn start(p: &Arc<Pending>, priority: u8) {
    if p.queue_state.load(Ordering::Relaxed) >= priority {
        return;
    }
    if !workers() {
        if p.queue_state.swap(MAKING, Ordering::Relaxed) == NOT_QUEUED {
            p.make();
        }
        return;
    }
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(new) = queue.push(p, priority) {
        if new {
            PEAK.fetch_max(WAITING.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        AVAILABLE.notify_one();
    }
}

fn next() -> Arc<Pending> {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(p) = queue.pop() {
            return p;
        }
        queue = AVAILABLE.wait(queue).unwrap_or_else(|e| e.into_inner());
    }
}

/// Number of background pipeline compile workers for a machine with `cores`
/// logical processors.
fn pipeline_worker_count(cores: usize) -> usize {
    (cores / 2).clamp(2, 12)
}

/// Starts the workers (half the logical processors, 2 to 12) on first use.
fn workers() -> bool {
    static RUNNING: OnceLock<bool> = OnceLock::new();
    *RUNNING.get_or_init(|| {
        let mut running = false;
        let workers =
            pipeline_worker_count(std::thread::available_parallelism().map_or(4, |n| n.get()));
        for i in 0..workers {
            let spawned = std::thread::Builder::new()
                .name(format!("victory-pipelines-{i}"))
                .spawn(|| {
                    // Below the title's own threads (loading, recording).
                    // SAFETY: a plain call on our own thread.
                    unsafe {
                        use windows::Win32::System::Threading::*;
                        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                    loop {
                        next().make();
                        if WAITING.fetch_sub(1, Ordering::Relaxed) == 1 {
                            drained();
                        } else {
                            super::library::save(false);
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "no pipeline worker");
            } else {
                running = true;
            }
        }
        running
    })
}

/// The most pipelines waiting at once since the workers last ran out.
static PEAK: AtomicU64 = AtomicU64::new(0);

/// The workers ran out of work: logged after a batch of 10 or more, and
/// the cache saved (at most every 20 s).
fn drained() {
    use super::library::{EARLY, LOADED, MADE};
    if PEAK.swap(0, Ordering::Relaxed) >= 10 {
        tracing::info!(
            made = MADE.load(Ordering::Relaxed),
            loaded = LOADED.load(Ordering::Relaxed),
            started_early = EARLY.load(Ordering::Relaxed),
            "host pipelines all made"
        );
    }
    super::library::save(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A first use queues the pipeline and, within the budget, gets the
    /// worker's answer (here: no host pipeline for no shaders).
    #[test]
    fn first_use_is_made_by_a_worker() {
        let p = Arc::new(Pending::new(Vec::new(), None));
        assert!(ready(&p).is_none());
        assert_ne!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        let started = Instant::now();
        while p.made.get().is_none() && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(matches!(p.made.get(), Some(None)));
    }

    #[test]
    fn pending_pipeline_is_recorded_for_the_next_launch() {
        let mut p = Pending::new(Vec::new(), None);
        p.key = "pending-pipeline-test".to_owned();
        // Simulate a worker that has taken the job but cannot finish it yet.
        p.queue_state.store(MAKING, Ordering::Relaxed);
        let p = Arc::new(p);

        assert!(!super::super::library::wanted_early(&p.key));
        assert!(ready(&p).is_none());
        assert!(p.made.get().is_none());
        assert!(super::super::library::wanted_early(&p.key));
    }

    #[test]
    fn draws_promote_early_pipelines_without_claiming_them_twice() {
        let earlier = Arc::new(Pending::new(Vec::new(), None));
        let promoted = Arc::new(Pending::new(Vec::new(), None));
        let requested = Arc::new(Pending::new(Vec::new(), None));
        let mut queue = Queue::new();

        assert_eq!(queue.push(&earlier, EARLY), Some(true));
        assert_eq!(queue.push(&promoted, EARLY), Some(true));
        assert_eq!(queue.push(&requested, REQUESTED), Some(true));
        assert_eq!(queue.push(&promoted, REQUESTED), Some(false));
        assert_eq!(queue.push(&promoted, REQUESTED), None);

        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &requested));
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &promoted));
        assert_eq!(queue.push(&promoted, REQUESTED), None);
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &earlier));
        assert!(queue.pop().is_none());
    }

    /// A shader of `stage` with the hash `hash`.
    fn shader(stage: &'static str, hash: &str) -> Arc<Shader> {
        let mut shader = Shader::new(stage, b"container", blake3::hash(b"container"));
        shader.hash = hash.to_owned();
        Arc::new(shader)
    }

    /// The programs of `left_out.rs` get no host pipeline, are never queued
    /// and never made, however a draw or the early list asks; others are.
    #[test]
    fn left_out_programs_are_never_queued_or_made() {
        use super::super::left_out::RAY_TRACING_RESOLVES;
        let resolve = |at: usize| Arc::new(Pending::new(vec![shader("compute", RAY_TRACING_RESOLVES[at])], None));
        let (first, last) = (resolve(0), resolve(RAY_TRACING_RESOLVES.len() - 1));
        // Unless a run of the tests asks for them to be made.
        if std::env::var_os("VICTORY_RT_RESOLVE").is_some()
            || std::env::var_os("VICTORY_BVH_BUILDER").is_some()
            || std::env::var_os("VICTORY_RAYS").is_some()
        {
            return;
        }
        for p in [&first, &last] {
            assert_eq!(p.left_out, Some("shades ray-tracing hits, left out"));
            assert!(ready(p).is_none());
            assert!(!now(p));
            early(p);
            assert_eq!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
            assert!(p.made.get().is_none());
            #[cfg(feature = "diagnostics")]
            assert_eq!(missing(p).as_deref(), Some("shades ray-tracing hits, left out"));
        }
        // Another compute program, and the same hash as a pixel shader, are
        // not left out.
        let other = Pending::new(vec![shader("compute", "0000000000000000")], None);
        assert_eq!(other.left_out, None);
        let pixel = Pending::new(vec![shader("pixel", RAY_TRACING_RESOLVES[0])], None);
        assert_eq!(pixel.left_out, None);
        // The program left out only on request is made.
        let activate = Pending::new(vec![shader("compute", super::super::left_out::ACTIVATE_RAYS)], None);
        assert_eq!(activate.left_out.is_some(), std::env::var_os("VICTORY_ACTIVATE_RAYS").is_some_and(|v| v == "0"));
    }

    /// A frame does not wait for a pipeline the driver has been at for
    /// longer than most take: ten seconds for a compute program (a pass
    /// that unpacks a model is not drawn again, so it is waited for the
    /// longest), three for a graphics pipeline.
    #[test]
    fn a_pipeline_the_driver_has_long_been_at_is_not_waited_for() {
        let at = |seconds: u64| Instant::now().checked_sub(Duration::from_secs(seconds)).expect("a clock past that");
        for (stage, most) in [("compute", 10), ("vertex", 3)] {
            let p = Pending::new(vec![shader(stage, "1111111111111111")], None);
            assert_eq!(p.computes(), stage == "compute");
            assert!(!slow(&p), "not begun");
            p.begun.set(at(most - 1)).unwrap();
            assert!(!slow(&p), "{stage}: {} s in", most - 1);
            let q = Pending::new(vec![shader(stage, "2222222222222222")], None);
            q.begun.set(at(most + 1)).unwrap();
            assert!(slow(&q), "{stage}: {} s in", most + 1);
        }
    }

    /// What the waits past a frame's budget add up to is bounded.
    #[test]
    fn the_waits_of_a_loading_world_are_bounded() {
        let total = loading_left(Duration::ZERO);
        assert_eq!(loading_left(total), Duration::ZERO);
        assert_eq!(loading_left(total + Duration::from_secs(5)), Duration::ZERO);
        if total > Duration::from_secs(1) {
            assert!(loading_left(Duration::from_secs(1)) < total);
        }
        assert!(LOADING_FRAME_COMPUTE > LOADING_FRAME);
    }

    #[test]
    fn pipeline_worker_count_scales_with_cores() {
        assert_eq!(pipeline_worker_count(2), 2);
        assert_eq!(pipeline_worker_count(4), 2);
        assert_eq!(pipeline_worker_count(8), 4);
        assert_eq!(pipeline_worker_count(16), 8);
        assert_eq!(pipeline_worker_count(28), 12);
        assert_eq!(pipeline_worker_count(64), 12);
    }
}
