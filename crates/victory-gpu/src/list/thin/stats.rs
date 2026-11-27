//! VICTORY_THIN_STATS=1. Group submissions between title Present calls. A
//! frame holds only completion tickets, so delayed translation neither stalls
//! Present nor extends the life of any list or title object. Report completed
//! frames in order every five seconds, including frames with no lists.
//!
//! The repeat rate estimates identical input among the last 4096 submitted
//! lists. Include list kind, scalar arguments, named object addresses and
//! copied descriptors. Addresses can be reused and live descriptors can
//! change. This fingerprint is a diagnostic, never a replay key.
//! Recordings transferring an upload have distinct identities even if their
//! allocator reuses a box address. Translation time sums the recording
//! worker's active intervals, including parallel preparation and its join.

use super::{BARRIER, COPY_TEXTURE, CopyLocation, Recording, UPLOAD};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

const HISTORY: usize = 4096;
type Key = [u8; 16];
pub(super) type Ticket = Arc<OnceLock<Sample>>;

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_THIN_STATS").is_some_and(|v| v == "1"))
}

pub(super) fn ticket() -> Option<Ticket> {
    sender().map(|_| Ticket::default())
}

#[derive(Default, Clone, Copy)]
pub(super) struct Timing {
    elapsed: Duration,
    pieces: usize,
    bindings: usize,
}

impl Timing {
    #[cfg(test)]
    pub(super) fn split_counts(&self) -> (usize, usize) {
        (self.pieces, self.bindings)
    }

    pub(super) fn add(&mut self, elapsed: Duration, bindings: Option<usize>) {
        self.elapsed += elapsed;
        if let Some(n) = bindings {
            self.pieces += 1;
            self.bindings += n;
        }
    }
}

pub(super) struct Sample {
    words: usize,
    key: Key,
    timing: Timing,
}

// Hash the active members of the two FFI unions. Their inactive bytes and
// alignment padding may be unspecified by the caller.
fn hash_chunk(hash: &mut blake3::Hasher, words: &[u64]) -> bool {
    fn bytes(words: &[u64], len: usize) -> &[u8] {
        assert!(len <= size_of_val(words));
        // SAFETY: callers select only initialized fields in this slice.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), len) }
    }
    let mut from = 0;
    let mut i = 0;
    let mut upload = false;
    while i < words.len() {
        let op = words[i] & 0xffff;
        upload |= op == UPLOAD;
        let end = i + 1 + (words[i] >> 16) as usize;
        let w = &words[i + 1..end];
        if op == BARRIER || op == COPY_TEXTURE {
            hash.update(bytes(&words[from..=i], (i + 1 - from) * 8));
            if op == BARRIER {
                hash.update(&w[0].to_le_bytes());
                for b in w[1..].chunks_exact(size_of::<super::Barrier>() / 8) {
                    let used = match b[0] as u32 {
                        0 => 28, // header, resource, subresource, before, after
                        1 => 24, // header and two resources
                        2 => 16, // header and resource
                        _ => 8,
                    };
                    hash.update(bytes(b, used));
                }
            } else {
                hash.update(bytes(w, 16)); // x, y, z, boxed
                let step = size_of::<CopyLocation>() / 8;
                for loc in w[2..2 + 2 * step].chunks_exact(step) {
                    hash.update(bytes(loc, 12)); // resource and kind
                    let kind = u32::from_le_bytes(bytes(&loc[1..], 4).try_into().unwrap());
                    let used = if kind == 1 { 28 } else { 4 };
                    hash.update(bytes(&loc[2..], used));
                }
                let bounds = &w[2 + 2 * step..];
                hash.update(bytes(bounds, size_of_val(bounds)));
            }
            from = end;
        }
        i = end;
    }
    hash.update(bytes(&words[from..], (words.len() - from) * 8));
    upload
}

pub(super) fn complete(ticket: &Ticket, kind: u32, chunks: &[Vec<u64>], timing: Timing) {
    let mut hash = blake3::Hasher::new();
    hash.update(&kind.to_le_bytes());
    let mut upload = false;
    for chunk in chunks {
        upload |= hash_chunk(&mut hash, chunk);
    }
    if upload {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        hash.update(&NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    }
    let key = hash.finalize().as_bytes()[..16].try_into().unwrap();
    let words = chunks.iter().map(Vec::len).sum();
    let _ = ticket.set(Sample { words, key, timing });
}

#[derive(Default)]
struct Frames {
    current: Vec<Ticket>,
    pending: VecDeque<(u64, Vec<Ticket>)>,
    recent: VecDeque<Key>,
    counts: HashMap<Key, usize>,
}

impl Frames {
    fn repeated(&mut self, key: Key) -> bool {
        let repeated = self.counts.contains_key(&key);
        self.recent.push_back(key);
        *self.counts.entry(key).or_default() += 1;
        if self.recent.len() > HISTORY {
            let old = self.recent.pop_front().unwrap();
            let count = self.counts.get_mut(&old).unwrap();
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&old);
            }
        }
        repeated
    }

    fn take_ready(&mut self) -> Vec<Report> {
        let mut reports = Vec::new();
        while self
            .pending
            .front()
            .is_some_and(|(_, lists)| lists.iter().all(|t| t.get().is_some()))
        {
            let (frame, tickets) = self.pending.pop_front().unwrap();
            let mut row = Report {
                frame,
                ..Report::default()
            };
            for t in tickets {
                let s = t.get().unwrap();
                row.lists.push((s.words, s.timing.elapsed.as_micros()));
                row.repeated += usize::from(self.repeated(s.key));
                row.split_chunks += s.timing.pieces;
                row.root_bindings += s.timing.bindings;
            }
            reports.push(row);
        }
        reports
    }
}

#[derive(Default)]
struct Report {
    frame: u64,
    lists: Vec<(usize, u128)>,
    repeated: usize,
    split_chunks: usize,
    root_bindings: usize,
}

impl Report {
    fn sizes(&self) -> (usize, f64) {
        let mut sizes: Vec<_> = self.lists.iter().map(|l| l.0).collect();
        sizes.sort_unstable();
        let n = sizes.len();
        match sizes.last() {
            Some(&max) => (max, (sizes[(n - 1) / 2] as f64 + sizes[n / 2] as f64) / 2.0),
            None => (0, 0.0),
        }
    }

    fn log(self) {
        let (max_words, median_words) = self.sizes();
        let n = self.lists.len();
        tracing::info!(frame = self.frame, lists = n, max_words, median_words,
            repeated = self.repeated, repeat_pct = 100.0 * self.repeated as f64 / n.max(1) as f64,
            split_chunks = self.split_chunks, root_bindings = self.root_bindings,
            words_and_translation_us = ?self.lists, "THIN submitted frame (VICTORY_THIN_STATS)");
    }
}

// Only send tickets and frame boundaries while the queue holds its lock.
// Hash history, sorting and log I/O belong to this optional observer.
enum Event {
    Lists(Vec<Ticket>),
    Present(u64),
}

fn observe(events: mpsc::Receiver<Event>) {
    let interval = Duration::from_secs(5);
    let mut next = Instant::now() + interval;
    let mut frames = Frames::default();
    loop {
        match events.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(Event::Lists(tickets)) => frames.current.extend(tickets),
            Ok(Event::Present(frame)) => {
                frames
                    .pending
                    .push_back((frame, std::mem::take(&mut frames.current)));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next {
            for report in frames.take_ready() {
                report.log();
            }
            next = Instant::now() + interval;
        }
    }
}

fn sender() -> Option<&'static mpsc::Sender<Event>> {
    static SENDER: OnceLock<Option<mpsc::Sender<Event>>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            if !on() {
                return None;
            }
            let (tx, rx) = mpsc::channel();
            match std::thread::Builder::new()
                .name("victory-thin-stats".into())
                .spawn(move || observe(rx))
            {
                Ok(_) => Some(tx),
                Err(e) => {
                    tracing::warn!(error = %e, "could not start THIN statistics");
                    None
                }
            }
        })
        .as_ref()
}

pub(crate) fn submitted(lists: &[Arc<Recording>]) {
    if let Some(tx) = sender() {
        let tickets = lists.iter().filter_map(|r| r.sample.clone()).collect();
        let _ = tx.send(Event::Lists(tickets));
    }
}

pub(crate) fn present(frame: u64) {
    if super::wanted()
        && let Some(tx) = sender()
    {
        let _ = tx.send(Event::Present(frame));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::list::thin::{CLOSE, Plain, RESET};

    fn ticket_for(kind: u32, chunks: &[Vec<u64>]) -> Ticket {
        let t = Ticket::default();
        complete(&t, kind, chunks, Timing::default());
        t
    }

    #[test]
    fn stream_identity_includes_named_objects_and_ignores_chunk_boundaries() {
        let chunks = [
            vec![RESET | 1 << 16, 0],
            vec![Plain::COPY_RESOURCE as u64 | 2 << 16, 123, 456, CLOSE],
        ];
        let key = |kind, chunks: &[Vec<u64>]| ticket_for(kind, chunks).get().unwrap().key;
        let first = key(0, &chunks);
        assert_eq!(key(0, &[chunks.concat()]), first);
        assert_ne!(key(1, &chunks), first);
        let mut renamed = chunks.clone();
        renamed[1][2] += 1;
        assert_ne!(key(0, &renamed), first);
        let with_upload = [vec![UPLOAD | 1 << 16, 123, CLOSE]];
        assert_ne!(key(0, &with_upload), key(0, &with_upload));
    }

    #[test]
    fn frame_statistics_wait_for_tickets_and_count_prior_frames() {
        let mut frames = Frames::default();
        let late = Ticket::default();
        let first = ticket_for(0, &[vec![CLOSE]]);
        frames
            .pending
            .push_back((1, vec![late.clone(), first.clone()]));
        frames.pending.push_back((2, vec![first]));
        frames.pending.push_back((3, vec![]));
        assert!(frames.take_ready().is_empty());
        let mut timing = Timing::default();
        timing.add(Duration::from_micros(42), Some(3));
        complete(&late, 0, &[vec![RESET | 1 << 16, 0, CLOSE]], timing);
        let rows = frames.take_ready();
        assert_eq!(rows[0].lists, [(3, 42), (1, 0)]);
        assert_eq!(rows[0].sizes(), (3, 2.0));
        assert_eq!(
            (
                rows[0].repeated,
                rows[0].split_chunks,
                rows[0].root_bindings
            ),
            (0, 1, 3)
        );
        assert_eq!((rows[1].frame, rows[1].repeated), (2, 1));
        assert_eq!(rows[2].sizes(), (0, 0.0));
        assert!(frames.pending.is_empty());
    }

    #[test]
    fn fingerprints_cover_copied_views_and_active_ffi_fields() {
        use crate::list::thin::{RECORD, TARGETS};
        let key = |words: &[u64]| ticket_for(0, &[words.to_vec()]).get().unwrap().key;
        let mut words = vec![BARRIER | (5 << 16), 1, 0, 42, 3 << 32, 8];
        let first = key(&words);
        words[5] |= 123 << 32; // transition padding
        assert_eq!(key(&words), first);
        words[3] += 1;
        assert_ne!(key(&words), first);

        let step = size_of::<CopyLocation>() / 8;
        let mut copy = vec![0; 3 + 2 * step];
        copy[0] = COPY_TEXTURE | ((copy.len() - 1) as u64) << 16;
        let first = key(&copy);
        copy[4] = 17 << 32; // padding after kind
        copy[6] = 888; // inactive footprint in a subresource location
        assert_eq!(key(&copy), first);
        copy[5] = 9; // subresource index
        assert_ne!(key(&copy), first);

        let mut view = vec![0; 4 + 2 * RECORD];
        view[0] = TARGETS | ((view.len() - 1) as u64) << 16;
        view[1] = 1;
        view[2] = 1;
        let first = key(&view);
        view[5] = 42; // named resource in the recorded RTV
        assert_ne!(key(&view), first);
        let first = key(&view);
        view[6] = 1; // changed descriptor on the same resource
        assert_ne!(key(&view), first);
    }

    #[test]
    fn history_expires_after_the_last_occurrence_leaves_the_window() {
        let mut frames = Frames::default();
        let key = [1; 16];
        assert!(!frames.repeated(key));
        assert!(frames.repeated(key));
        for _ in 0..HISTORY - 1 {
            frames.repeated([2; 16]);
        }
        assert!(frames.counts.contains_key(&key));
        frames.repeated([2; 16]);
        assert!(!frames.repeated(key));
    }
}
