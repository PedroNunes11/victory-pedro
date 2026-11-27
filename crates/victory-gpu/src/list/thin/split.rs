//! Workers may read the scalars in their own chunk without observing or
//! changing a list, a descriptor heap, or a title object. Translate adjacent
//! scalar root setters to a batch of draw-state updates. All other opcodes
//! end the batch and use the original handler on the recording thread.

use super::{ABANDON_POLL, Plain};
use crate::draw::State;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

pub(super) const MAX_CHUNKS: usize = 4;

pub(super) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_THIN_SPLIT").is_some_and(|v| v == "1"))
}

enum Value {
    Table(u64),
    View(u64),
    Constant(u32, u32),
}

struct Binding {
    compute: bool,
    slot: u32,
    value: Value,
}

impl Binding {
    fn decode(op: u64, words: &[u64]) -> Option<Self> {
        let (compute, value) = match op {
            n if n == Plain::COMPUTE_TABLE as u64 => (true, Value::Table(words[1])),
            n if n == Plain::GRAPHICS_TABLE as u64 => (false, Value::Table(words[1])),
            n if n == Plain::COMPUTE_VIEW as u64 => (true, Value::View(words[1])),
            n if n == Plain::GRAPHICS_VIEW as u64 => (false, Value::View(words[1])),
            n if n == Plain::COMPUTE_CONSTANT as u64 => {
                (true, Value::Constant(words[1] as u32, words[2] as u32))
            }
            n if n == Plain::GRAPHICS_CONSTANT as u64 => {
                (false, Value::Constant(words[1] as u32, words[2] as u32))
            }
            _ => return None,
        };
        Some(Self {
            compute,
            slot: words[0] as u32,
            value,
        })
    }
}

pub(super) struct Run {
    pub(super) start: usize,
    pub(super) end: usize,
    bindings: Range<usize>,
}

impl Run {
    pub(super) fn len(&self) -> usize {
        self.bindings.len()
    }
}

#[derive(Default)]
pub(super) struct Plan {
    pub(super) runs: Vec<Run>,
    bindings: Vec<Binding>,
}

impl Plan {
    fn decode(words: &[u64]) -> Self {
        let mut plan = Self::default();
        let mut start = 0;
        let mut call = 0;
        while start < words.len() {
            let header = words[start];
            let end = start + 1 + (header >> 16) as usize;
            call += 1;
            if let Some(binding) = Binding::decode(header & 0xffff, &words[start + 1..end]) {
                let index = plan.bindings.len();
                plan.bindings.push(binding);
                if call % ABANDON_POLL != 0
                    && let Some(run) = plan.runs.last_mut().filter(|r| r.end == start)
                {
                    run.bindings.end += 1;
                    run.end = end;
                } else {
                    plan.runs.push(Run {
                        start,
                        end,
                        bindings: index..index + 1,
                    });
                }
            }
            start = end;
        }
        plan
    }

    pub(super) fn bindings(&self) -> usize {
        self.bindings.len()
    }

    pub(super) fn apply(&self, run: &Run, state: &mut State) {
        for b in &self.bindings[run.bindings.clone()] {
            match b.value {
                Value::Table(h) => state.set_table(b.compute, b.slot, h),
                Value::View(a) => state.set_view(b.compute, b.slot, a),
                Value::Constant(v, offset) => state.set_constants(b.compute, b.slot, &[v], offset),
            }
        }
    }
}

/// Each worker publishes into its chunk's slot. Only the owner touches the
/// host list, after all plans are ready, in the original order.
pub(super) fn prepare(chunks: impl Iterator<Item = Vec<u64>>) -> Vec<(Vec<u64>, Plan)> {
    let pieces: Vec<_> = chunks.map(|words| (words, OnceLock::new())).collect();
    let pieces = Arc::new(pieces);
    let work = Arc::clone(&pieces);
    crate::shadow::wide_each(pieces.len(), move |i| {
        let (words, plan) = &work[i];
        plan.get_or_init(|| Plan::decode(words));
    });
    Arc::into_inner(pieces)
        .expect("wide_each joined its workers")
        .into_iter()
        .map(|(words, plan)| (words, plan.into_inner().expect("each chunk was decoded")))
        .collect()
}
