use super::*;

fn packet(words: &mut Vec<u64>, op: u64, args: &[u64]) {
    words.push(op | (args.len() as u64) << 16);
    words.extend_from_slice(args);
}

#[test]
fn parallel_plans_preserve_root_bytes_and_stop_at_non_bindings() {
    let mut expected = crate::draw::State::default();
    let mut chunks = Vec::new();
    for i in 1..=4 {
        let mut w = Vec::new();
        let slot = i as u32;
        packet(&mut w, Plain::COMPUTE_TABLE as u64, &[i, 99 + i]);
        expected.set_table(true, slot, 99 + i);
        packet(&mut w, Plain::GRAPHICS_VIEW as u64, &[i, u64::MAX - i]);
        expected.set_view(false, slot, u64::MAX - i);
        packet(&mut w, Plain::SYNC as u64, &[0; 4]);
        for offset in 0..65 {
            packet(
                &mut w,
                Plain::COMPUTE_CONSTANT as u64,
                &[i, offset + i, offset],
            );
            expected.set_constants(true, slot, &[(offset + i) as u32], offset as u32);
        }
        chunks.push(w);
    }
    let mut actual = crate::draw::State::default();
    for (_, plan) in split::prepare(chunks.into_iter()) {
        // Two bindings, SYNC, then 60 constants before call 64 polls again.
        let counts: Vec<_> = plan.runs.iter().map(split::Run::len).collect();
        assert_eq!(counts, [2, 60, 5]);
        for run in &plan.runs {
            plan.apply(run, &mut actual);
        }
    }
    assert_eq!(actual.root_bytes(), expected.root_bytes());
}

/// Run the normal recording worker on five queued, synthetic chunks. Every
/// chunk changes the preceding one's state and transfers an owned upload.
/// Reordering chunks or replaying an upload twice is observable.
fn translate_recording(split: bool) -> (Vec<u8>, Vec<u8>, stats::Timing) {
    let mut recording = Recording::new(0);
    recording.sample = Some(Default::default());
    for i in 0..5 {
        let mut w = Vec::new();
        if i == 0 {
            packet(&mut w, RESET, &[0]);
        }
        for slot in 0..128 {
            packet(
                &mut w,
                Plain::COMPUTE_VIEW as u64,
                &[slot, (i << 48) + slot],
            );
            packet(
                &mut w,
                Plain::GRAPHICS_TABLE as u64,
                &[slot, (slot << 32) + i],
            );
            packet(
                &mut w,
                Plain::GRAPHICS_CONSTANT as u64,
                &[slot, slot + i, i],
            );
        }
        // A variable-sized baseline packet interrupts a run of scalar bindings.
        packet(
            &mut w,
            COMPUTE_CONSTANTS,
            &[i | (2 << 32), 2, 7 | (9 << 32)],
        );
        let mut upload = Upload::new(1).unwrap();
        // SAFETY: one byte was allocated and the recording will own it.
        unsafe { upload.start().write(i as u8) };
        packet(&mut w, UPLOAD, &[Box::into_raw(Box::new(upload)) as u64]);
        if i == 4 {
            packet(&mut w, CLOSE, &[]);
        }
        held(&recording.inner).chunks.push_back(w);
    }
    recording.run_with_split(split);
    assert!(recording.done());
    // SAFETY: the recording owns the back list until the result is copied.
    let list = unsafe { &*recording.back().cast::<List>() };
    let root_bytes = held(&list.state).root_bytes();
    let uploads = held(&list.uploads)
        .iter_mut()
        .map(|u| {
            // SAFETY: every upload here holds one byte.
            unsafe { u.start().read() }
        })
        .collect();
    let timing = held(&recording.inner).timing;
    (root_bytes, uploads, timing)
}

#[test]
fn split_and_serial_recordings_produce_identical_bytes() {
    let (serial, expected, before) = translate_recording(false);
    let (split, uploads, after) = translate_recording(true);
    assert_eq!(expected, [0, 1, 2, 3, 4]);
    assert_eq!((split, uploads), (serial, expected));
    assert_eq!(before.split_counts(), (0, 0));
    // The fifth chunk is alone and follows the serial path.
    assert_eq!(after.split_counts(), (4, 4 * 128 * 3));
}

#[test]
fn abandoned_chunks_still_transfer_uploads_and_close() {
    let run = |split| {
        let mut list = super::super::boxed(0, std::ptr::null(), false);
        let this = (&raw mut *list).cast();
        super::super::close(this);
        let mut w = vec![RESET | 1 << 16, 0];
        for i in 0..130 {
            packet(&mut w, Plain::COMPUTE_CONSTANT as u64, &[0, i, i]);
        }
        let upload = Upload::new(1).unwrap();
        packet(&mut w, UPLOAD, &[Box::into_raw(Box::new(upload)) as u64]);
        packet(&mut w, CLOSE, &[]);
        let polls = std::cell::Cell::new(0);
        let abandoned = || {
            polls.set(polls.get() + 1);
            polls.get() >= 2
        };
        let result = if split {
            let (words, plan) = split::prepare(std::iter::once(w)).pop().unwrap();
            translate::<true>(this, &words, &plan, abandoned)
        } else {
            translate::<false>(this, &w, &split::Plan::default(), abandoned)
        };
        assert_eq!(result, (133, true));
        assert_eq!(polls.get(), 3);
        assert_eq!(held(&list.uploads).len(), 1);
        held(&list.state).root_bytes()
    };
    assert_eq!(run(false), run(true));
}

/// Run explicitly to measure worker wall time on a binding-heavy recording.
#[test]
#[ignore = "synthetic THIN translation timing"]
fn time_queued_root_bindings() {
    use std::time::Instant;
    crate::host::host().expect("the timing needs a D3D12 host");
    let mut chunks = VecDeque::new();
    let mut calls = 0;
    for i in 0..64 {
        let mut words = Vec::new();
        if i == 0 {
            packet(&mut words, RESET, &[0]);
        }
        while words.len() < CHUNK {
            let slot = calls % 16;
            packet(&mut words, Plain::GRAPHICS_VIEW as u64, &[slot, calls]);
            packet(
                &mut words,
                Plain::COMPUTE_CONSTANT as u64,
                &[slot, calls, i % 8],
            );
            calls += 2;
        }
        if i == 63 {
            packet(&mut words, CLOSE, &[]);
        }
        chunks.push_back(words);
    }
    let measure = |split| {
        let recording = Recording::new(0);
        held(&recording.inner).chunks = chunks.clone();
        let start = Instant::now();
        recording.run_with_split(split);
        let elapsed = start.elapsed();
        assert!(recording.done());
        elapsed
    };
    let mut times = [Vec::new(), Vec::new()];
    for i in 0..32 {
        for split in [i % 2 == 0, i % 2 != 0] {
            let elapsed = measure(split);
            if i > 0 {
                times[usize::from(split)].push(elapsed);
            }
        }
    }
    for t in &mut times {
        t.sort_unstable();
    }
    eprintln!(
        "{calls} scalar root setters, 64 chunks, median of 31: serial {:?}, split {:?}",
        times[0][15], times[1][15]
    );
}
