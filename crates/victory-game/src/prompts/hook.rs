//! Hands the redrawn Scaleform GFx icon movie to the title, with live
//! hot-reload when `native-keys.txt` / `keys.txt` changes and automatic
//! keyboard/controller prompt switching when `VICTORY_PROMPTS=auto`.

use std::cell::OnceCell;
use std::ffi::{CString, c_char, c_void};
use std::mem::offset_of;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use victory_core::pad::should_show_key_prompts;
use victory_formats::gfx;
use victory_loader::{LoadedImage, SlotError};

use super::bindings::{self, PromptBindings};
use super::{HookOffsets, Refusal, font::Fonts, is_icon_movie, key_caps_with};

/// Max length of a UI runtime string read into our stack buffer.
const TEXT_BUFFER_BYTES: usize = 512;
/// Largest movie inspected (the title's icon movie is ~28 KB).
const LARGEST_MOVIE_BYTES: usize = 256 << 10;
/// Maximum cached redrawn variants kept in memory across live rebinds.
const MOST_DRAWN: usize = 32;
/// Maximum store slots remembered at once.
const MOST_STORED: usize = 64;
/// Low-bit flags in Scaleform UI runtime string pointers (Series 1.0.2.1).
const STRING_FLAG_MASK: usize = 3;
/// Offset to UTF-8 text inside a Scaleform string block.
const STRING_TEXT_OFFSET_BYTES: usize = 12;

#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
struct UiString(usize);

#[repr(C)]
struct Location {
    kind: u32,
    name: UiString,
}

const _: () = assert!(offset_of!(Location, kind) == 0 && offset_of!(Location, name) == 8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// Kind 0: the streaming store has just read the movie into memory.
    HandedOver,
    /// Kind 3: a movie loads another movie by name.
    LoadedByAMovie,
}

impl TryFrom<u32> for Request {
    type Error = u32;

    fn try_from(kind: u32) -> Result<Self, u32> {
        match kind {
            0 => Ok(Self::HandedOver),
            3 => Ok(Self::LoadedByAMovie),
            other => Err(other),
        }
    }
}

type BuildAddress = unsafe extern "system" fn(*mut c_void, *mut UiString, *const Location);
type AssignString = unsafe extern "system" fn(*mut UiString, *const c_char);

struct Title {
    build_address: BuildAddress,
    assign_string: AssignString,
}

static TITLE: OnceLock<Title> = OnceLock::new();
static SERVED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
enum Warning {
    Unreadable,
    NothingDrawn,
    TooManyMovies,
}

static WARNED: AtomicU8 = AtomicU8::new(0);

fn warn_once(warning: Warning) {
    let bit = 1 << warning as u8;
    if WARNED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        tracing::warn!(
            ?warning,
            "prompts: the icon movie keeps its gamepad pictures"
        );
    }
}

/// A redrawn movie cached by `(original_index, bindings_fingerprint)`.
struct Drawn {
    original_index: usize,
    fingerprint: u64,
    redrawn: &'static [u8],
}

/// Where the store keeps an icon movie we recognised, referencing `originals[original_index]`.
struct Stored {
    tag: String,
    address: usize,
    len: usize,
    original_index: usize,
}

struct Kept {
    originals: Vec<Box<[u8]>>,
    drawn: Vec<Drawn>,
    refused: Vec<Box<[u8]>>,
    stored: Vec<Stored>,
}

static KEPT: Mutex<Kept> = Mutex::new(Kept {
    originals: Vec::new(),
    drawn: Vec::new(),
    refused: Vec::new(),
    stored: Vec::new(),
});

fn kept() -> MutexGuard<'static, Kept> {
    KEPT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why the hook was not placed.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    #[error("the pointer does not lead to code of the image")]
    NotATitlePointer,
    #[error("the hook is already placed")]
    AlreadyPlaced,
    #[error(transparent)]
    Slot(#[from] SlotError),
}

/// Replaces the title's movie address pointer with [`movie_address`].
///
/// # Safety
/// `image` is the mapped and bound image of the build `offsets` belong to,
/// and no title thread runs yet.
pub unsafe fn install(image: &LoadedImage, offsets: HookOffsets) -> Result<(), Refused> {
    let assign_string = image.code_address(offsets.string_assign.0)?;
    let slot = offsets.movie_address_builder.0;
    // SAFETY: no title thread runs, per the caller.
    let build_address = unsafe { image.read_pointer(slot) }?;
    let builder_offset = build_address
        .checked_sub(image.base())
        .and_then(|offset| u32::try_from(offset).ok())
        .ok_or(Refused::NotATitlePointer)?;
    if image.code_address(builder_offset).is_err() {
        return Err(Refused::NotATitlePointer);
    }
    // SAFETY: both addresses are verified code of the target build.
    let title = unsafe {
        Title {
            build_address: std::mem::transmute::<usize, BuildAddress>(build_address),
            assign_string: std::mem::transmute::<usize, AssignString>(assign_string),
        }
    };
    TITLE.set(title).map_err(|_| Refused::AlreadyPlaced)?;
    let hook: BuildAddress = movie_address;
    // SAFETY: no title thread runs, per the caller.
    unsafe { image.write_pointer(slot, hook as usize) }?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct InMemory<'a> {
    address: usize,
    len: usize,
    tag: &'a str,
}

impl<'a> InMemory<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let (fields, tag) = text.strip_prefix("memory:$")?.split_once(':')?;
        let mut fields = fields.split(',');
        let address = usize::from_str_radix(fields.next()?, 16).ok()?;
        let len: usize = fields.next()?.parse().ok()?;
        (address != 0 && (gfx::HEADER_BYTES..=LARGEST_MOVIE_BYTES).contains(&len)).then_some(Self {
            address,
            len,
            tag,
        })
    }
}

fn address_text(movie: &[u8], name: &str) -> Option<CString> {
    CString::new(format!(
        "memory:${:016X},{},0:victory:{name}",
        movie.as_ptr().addr(),
        movie.len()
    ))
    .ok()
}

struct UiText {
    bytes: [u8; TEXT_BUFFER_BYTES],
    len: usize,
}

impl UiText {
    fn as_str(&self) -> &str {
        self.bytes
            .get(..self.len)
            .and_then(|text| std::str::from_utf8(text).ok())
            .unwrap_or_default()
    }

    /// # Safety
    /// `string` is a live Scaleform string owned by the caller for this call.
    unsafe fn read(string: UiString) -> Option<Self> {
        let block = string.0 & !STRING_FLAG_MASK;
        if block == 0 {
            return None;
        }
        let from = block.checked_add(STRING_TEXT_OFFSET_BYTES)? as *const u8;
        let mut text = Self {
            bytes: [0; TEXT_BUFFER_BYTES],
            len: 0,
        };
        for at in 0..TEXT_BUFFER_BYTES {
            // SAFETY: reads until terminating NUL within `TEXT_BUFFER_BYTES`.
            let byte = unsafe { from.add(at).read() };
            if byte == 0 {
                text.len = at;
                let valid = std::str::from_utf8(text.bytes.get(..at)?).is_ok();
                return (valid && at > 0).then_some(text);
            }
            *text.bytes.get_mut(at)? = byte;
        }
        None
    }
}

/// # Safety
/// `memory` names bytes the title holds readable for this call.
unsafe fn copy_of(memory: &InMemory) -> Option<Box<[u8]>> {
    let from = memory.address as *const u8;
    // SAFETY: `InMemory` guarantees `len >= HEADER_BYTES`.
    let header = unsafe { from.cast::<[u8; gfx::HEADER_BYTES]>().read_unaligned() };
    if !gfx::names_movie(&header, memory.len) {
        return None;
    }
    let mut copy = Vec::with_capacity(memory.len);
    // SAFETY: caller guarantees `memory.len` readable bytes at `from`.
    unsafe {
        from.copy_to_nonoverlapping(copy.as_mut_ptr(), memory.len);
        copy.set_len(memory.len);
    }
    Some(copy.into_boxed_slice())
}

/// Returns a redrawn copy of `originals[original_index]` for `bindings`, redrawing
/// outside the lock if this `(original_index, fingerprint)` pair is not yet cached.
fn redrawn_for(original_index: usize, bindings: &PromptBindings) -> Option<&'static [u8]> {
    let original_bytes: Box<[u8]> = {
        let kept = kept();
        if let Some(hit) = kept
            .drawn
            .iter()
            .find(|d| d.original_index == original_index && d.fingerprint == bindings.fingerprint)
        {
            return Some(hit.redrawn);
        }
        if kept.drawn.len() >= MOST_DRAWN {
            // Reuse the latest drawn copy for this original rather than failing.
            if let Some(last) = kept
                .drawn
                .iter()
                .rev()
                .find(|d| d.original_index == original_index)
            {
                return Some(last.redrawn);
            }
            drop(kept);
            warn_once(Warning::TooManyMovies);
            return None;
        }
        kept.originals.get(original_index)?.clone()
    };

    let fonts = OnceCell::new();
    let glyph = |character| fonts.get_or_init(Fonts::open).glyph(character);
    let warning = match key_caps_with(&original_bytes, bindings, &glyph) {
        Ok(drawn) => {
            let drawn = drawn.into_boxed_slice();
            let mut kept = kept();
            if let Some(hit) = kept
                .drawn
                .iter()
                .find(|d| d.original_index == original_index && d.fingerprint == bindings.fingerprint)
            {
                return Some(hit.redrawn);
            }
            let redrawn: &'static [u8] = Box::leak(drawn);
            kept.drawn.push(Drawn {
                original_index,
                fingerprint: bindings.fingerprint,
                redrawn,
            });
            drop(kept);
            tracing::info!(
                bytes = redrawn.len(),
                fingerprint = format_args!("{:#018x}", bindings.fingerprint),
                "prompts: key caps drawn into the icon movie"
            );
            return Some(redrawn);
        }
        Err(Refusal::NotIconMovie) => return None,
        Err(Refusal::Unreadable) => Warning::Unreadable,
        Err(Refusal::NothingDrawn) => Warning::NothingDrawn,
    };
    let mut kept = kept();
    if kept.refused.len() < MOST_DRAWN && !kept.refused.contains(&original_bytes) {
        kept.refused.push(original_bytes);
    }
    drop(kept);
    warn_once(warning);
    None
}

/// Recognises an icon movie handed over by the streaming store, keeps its
/// original bytes, and pre-draws the initial keyboard copy.
///
/// # Safety
/// `memory` names bytes the title holds readable for this call.
unsafe fn keep(memory: &InMemory) {
    // SAFETY: forwarded from caller.
    let Some(copy) = (unsafe { copy_of(memory) }) else {
        let mut kept = kept();
        kept.stored.retain(|s| s.tag != memory.tag);
        return;
    };
    if !is_icon_movie(&copy) {
        let mut kept = kept();
        kept.stored.retain(|s| s.tag != memory.tag);
        return;
    }
    let original_index = {
        let mut kept = kept();
        if kept.refused.contains(&copy) {
            kept.stored.retain(|s| s.tag != memory.tag);
            return;
        }
        let idx = if let Some(pos) = kept.originals.iter().position(|o| *o == copy) {
            pos
        } else {
            if kept.originals.len() >= MOST_DRAWN {
                drop(kept);
                warn_once(Warning::TooManyMovies);
                return;
            }
            let idx = kept.originals.len();
            kept.originals.push(copy);
            idx
        };
        kept.stored.retain(|s| s.tag != memory.tag);
        if kept.stored.len() >= MOST_STORED {
            kept.stored.remove(0);
        }
        kept.stored.push(Stored {
            tag: memory.tag.to_owned(),
            address: memory.address,
            len: memory.len,
            original_index: idx,
        });
        idx
    };
    // Pre-draw now on the loader thread so the first `serve` is instant.
    let current = bindings::current();
    let _ = redrawn_for(original_index, &current);
}

fn stored_index(memory: &InMemory) -> Option<usize> {
    kept()
        .stored
        .iter()
        .find(|s| s.tag == memory.tag && s.address == memory.address && s.len == memory.len)
        .map(|s| s.original_index)
}

/// Answers a movie that loads the icon movie by name with our live redrawn
/// keyboard copy, or leaves the original Xbox icons untouched when gamepad
/// prompts are active (`VICTORY_PROMPTS=auto` + gamepad, or `gamepad`).
fn serve(title: &Title, out: *mut UiString, name: UiString, memory: &InMemory) {
    if !should_show_key_prompts() {
        return;
    }
    let Some(original_index) = stored_index(memory) else {
        return;
    };
    let current = bindings::current();
    let Some(movie) = redrawn_for(original_index, &current) else {
        return;
    };
    // SAFETY: caller owns `name` for this call.
    let Some(text) =
        unsafe { UiText::read(name) }.and_then(|name| address_text(movie, name.as_str()))
    else {
        return;
    };
    // SAFETY: title's own string assignment on its output `UiString`.
    unsafe { (title.assign_string)(out, text.as_ptr()) };
    if !SERVED.swap(true, Ordering::Relaxed) {
        tracing::info!("prompts: the title loads the icon movie with keyboard key caps");
    }
}

extern "system" fn movie_address(this: *mut c_void, out: *mut UiString, location: *const Location) {
    let Some(title) = TITLE.get() else {
        return;
    };
    // SAFETY: title's own function with its own arguments.
    unsafe { (title.build_address)(this, out, location) };
    if out.is_null() || location.is_null() {
        return;
    }
    // SAFETY: caller owns `location` and `out` for this call.
    let (Location { kind, name }, answer) =
        unsafe { (location.read_unaligned(), out.read_unaligned()) };
    let Ok(request) = Request::try_from(kind) else {
        return;
    };
    // SAFETY: answer string just written by `build_address`.
    let Some(answer) = (unsafe { UiText::read(answer) }) else {
        return;
    };
    let Some(memory) = InMemory::parse(answer.as_str()) else {
        return;
    };
    match request {
        // SAFETY: store has just placed movie bytes at `memory`.
        Request::HandedOver => unsafe { keep(&memory) },
        Request::LoadedByAMovie => serve(title, out, name, &memory),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_address_parses_valid_and_rejects_oversized() {
        let parsed = InMemory::parse("memory:$00007FF712340000,28672,0:common:/icons").unwrap();
        assert_eq!(parsed.address, 0x0000_7FF7_1234_0000);
        assert_eq!(parsed.len, 28672);
        assert_eq!(parsed.tag, "common:/icons");
        assert_eq!(InMemory::parse("memory:$0,1024,0:x"), None);
        assert_eq!(InMemory::parse("memory:$1000,9999999,0:x"), None);
    }
}
