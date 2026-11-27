use super::*;
use crate::descriptor::{
    self,
    copied::{Options, Record, Schedule},
};

#[test]
fn only_placed_records_outside_recent_tables_skip_the_copy_grace() {
    let on = Options {
        at_once: true,
        orphans: false,
    };
    assert!(matches!(on.schedule(descriptor::PLACED, None), Schedule::Now));
    assert!(matches!(
        Options::default().schedule(descriptor::PLACED, None),
        Schedule::Wait
    ));
    assert!(matches!(on.schedule(descriptor::CONSTANT, None), Schedule::Wait));
    for age in [COPIED_FRAMES + 1, TABLED_FRAMES] {
        assert!(matches!(on.schedule(descriptor::PLACED, Some(age)), Schedule::Wait));
    }
    assert!(matches!(
        on.schedule(descriptor::PLACED, Some(COPIED_FRAMES)),
        Schedule::Table
    ));
    assert!(matches!(
        on.schedule(descriptor::PLACED, Some(TABLED_FRAMES + 1)),
        Schedule::Now
    ));
}

#[test]
fn copied_records_keep_their_first_discovery_time_and_expire_once() {
    let waiting = copied::Deferred::default();
    let record = Record { index: 2, key: 42 };
    waiting.push(10, 7, vec![record]);
    waiting.push(11, 40, vec![record]);
    assert!(waiting.ready(13, 4, |_, _| true).is_empty());
    let due = waiting.ready(15, 4, |_, _| true);
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].1, 7);
    assert!(waiting.ready(16, 4, |_, _| true).is_empty());

    let mut queue = TextureQueue::default();
    queue.written(
        2,
        15,
        0,
        Origin::Copied {
            frame: 15,
            found_ms: due[0].1,
            due_now: true,
        },
    );
    assert_eq!(queue.since(2), Some((7, false, 0)));
}

/// Four independent runs on the same synthetic texture data. Feed real
/// write-watched descriptor bytes through heap sync, then detile what the
/// texture pass gets from each due slot. The later draw also consumes the
/// record that the default sync policy leaves to its table indefinitely.
#[test]
fn copied_options_produce_identical_texels_at_different_frames() {
    use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree};

    const START: u64 = 100;
    const COUNT: usize = 4;
    const PAGE: usize = 65536;
    let base = crate::upload::reserve((COUNT + 1) * PAGE).expect("write-watched title memory");
    let resource = [3u32, 0, 0, 0, 8, 0, 8, 0x10001, 28, 1, 0, 0x100, 0, 0];
    // SAFETY: these words describe an 8x8 RGBA8 texture in ResourceDesc layout.
    let layout = crate::resource::texture_layout(&unsafe { std::mem::transmute(resource) }).unwrap();
    let layout = &layout.planes[0].layout;
    assert!(layout.size <= PAGE as u64);
    let pictures: [[u8; 256]; COUNT] =
        std::array::from_fn(|slot| std::array::from_fn(|byte| (slot * 67 + byte * 11) as u8));
    let mut records = [[0u32; 8]; COUNT];
    for (i, record) in records.iter_mut().enumerate() {
        let address = base + (i + 1) * PAGE;
        // SAFETY: initialized, disjoint pages of our own reservation.
        let tiled = unsafe { std::slice::from_raw_parts_mut(address as *mut u8, PAGE) };
        layout.tile(tiled, 0, 0, &pictures[i], 32, 256).unwrap();
        let desc = [
            28,
            4,
            0x1688,
            0,
            0,
            1,
            0,
            0,
            0,
            0,
            address as u32,
            (address >> 32) as u32,
            0,
            0,
            0,
            0,
        ];
        descriptor::create_placed_srv(std::ptr::null(), &resource, &desc, record.as_mut_ptr() as u64);
        assert_eq!(record[2] >> descriptor::KIND_SHIFT, descriptor::PLACED);
    }
    for (at_once, orphans) in [(false, false), (true, false), (false, true), (true, true)] {
        let heap = descriptor::Heap {
            object: crate::object::Object::new(&descriptor::FUNCTIONS),
            desc: descriptor::HeapDesc {
                kind: 0,
                count: COUNT as u32,
                flags: descriptor::SHADER_VISIBLE,
                nodes: 1,
            },
            start: base as u64,
            memory: None,
            host: Default::default(),
            pending: Default::default(),
            written: Some(crate::watch::track(base, PAGE, true).unwrap()),
        };
        let Some(host) = heap.host() else {
            eprintln!("no D3D12 device for the heap-sync integration test");
            break;
        };
        // Slots 1 and 2 belonged to a table one frame before the copy.
        for index in [1, 2] {
            host.tabled_in(index, true, START - 1);
        }
        // Slot 3 has left the short table window but is still in the long one.
        host.tabled_in(3, true, START - COPIED_FRAMES - 1);
        // SAFETY: four 32-byte records fit in the first page.
        unsafe { std::ptr::copy_nonoverlapping(records.as_ptr(), base as *mut [u32; 8], COUNT) };
        let mut shown = [None; COUNT];
        let late_draw = START + copied::copied_grace().max(COPIED_FRAMES) + tabled_grace() + 1;
        for frame in START..=late_draw {
            descriptor::sync_records(
                std::iter::once((heap.start, heap.start + (COUNT * 32) as u64, &heap)),
                frame,
                Options { at_once, orphans },
            );
            let mut consume = |index: u32| {
                let handle = heap.handle(index).unwrap();
                let (View::Placed(false, words, desc), _) = host.prepare_texture_slot(index, handle).unwrap() else {
                    panic!("expected a placed texture");
                };
                assert_eq!(words, resource);
                let address = (u64::from(desc[10]) | u64::from(desc[11]) << 32) as usize;
                assert_eq!(address, base + (index as usize + 1) * PAGE);
                // SAFETY: validated above; layout.size fits in this texture's page.
                let source = unsafe { std::slice::from_raw_parts(address as *const u8, layout.size as usize) };
                let mut texels = [0; 8 * 8 * 4];
                layout.detile(source, 0, 0, &mut texels, 32, 256).unwrap();
                assert_eq!(texels, pictures[index as usize]);
                shown[index as usize].get_or_insert(frame);
            };
            for (index, write) in host.texture_slots_due(frame, COUNT).0 {
                assert_ne!(index, 2, "the draw already claimed this slot");
                consume(index);
                host.texture_slot_done(index, write, None);
            }
            if frame == START + 1 {
                host.tabled_in(2, true, frame);
                consume(2);
            }
            if frame == late_draw {
                consume(1);
            }
        }
        assert_eq!(
            shown,
            [
                Some(if at_once { START } else { START + copied::copied_grace() }),
                Some(if orphans { START + COPIED_FRAMES } else { late_draw }),
                Some(START + 1),
                Some(START + copied::copied_grace() + if at_once { 0 } else { tabled_grace() }),
            ]
        );
        // A queued record must never expedite the new occupant of its slot.
        let old = Record {
            index: 1,
            key: record_key(heap.handle(1).unwrap()),
        };
        // SAFETY: replacing one live 32-byte descriptor in our first page.
        unsafe {
            (heap.handle(1).unwrap() as *mut [u32; 8]).write(records[0]);
        }
        assert!(
            host.mirror_copied(old.index, heap.handle(1).unwrap(), Some(old.key), late_draw, 7, true)
                .is_none()
        );
    }
    // SAFETY: all local heaps have been dropped.
    assert_ne!(unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) }, 0);
}
