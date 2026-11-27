//! D3D12.X on host D3D12.
//!
//! D3D12.X records commands from code inlined into the title, which writes
//! packets into the command list while its put pointer is below a limit and
//! calls the list's method otherwise. Our lists keep the limits at zero, so
//! every command arrives here as a call, to be run on host D3D12. The
//! title's GPU addresses are CPU addresses (console memory is unified).
//!
//! [`device`] is the device object titles create, [`dxgi`] its adapter side,
//! [`resource`] heaps and resources, [`fence`] fences, [`queue`] command
//! queues, [`list`] command allocators and lists, [`descriptor`]
//! descriptor heaps, [`root`] root signatures, [`query`] query heaps,
//! [`signature`] command signatures, [`pipeline`] pipeline states,
//! [`frame`] frame pacing, [`object`] what all console graphics objects
//! share, [`console`] which console model the title sees, `external` host
//! libraries recording into title lists. Planned: `present` (swap chain).

#[cfg(windows)]
mod ab;
mod ab_core;
#[cfg(all(windows, feature = "diagnostics"))]
mod bake_probe;
#[cfg(all(windows, feature = "diagnostics"))]
mod capture;
#[cfg(all(windows, feature = "diagnostics"))]
mod renderdoc;
#[cfg(windows)]
mod cardmeter;
mod live;
pub mod console;
#[cfg(all(windows, feature = "diagnostics"))]
mod copystat;
#[cfg(all(windows, feature = "diagnostics"))]
mod repeat;
pub mod descriptor;
pub mod device;
#[cfg(windows)]
mod ambient;
#[cfg(windows)]
mod drvwatch;
#[cfg(windows)]
mod spread;
#[cfg(windows)]
mod draw;
pub mod dxgi;
#[cfg(windows)]
pub mod external;
pub mod fence;
pub mod frame;
#[cfg(windows)]
pub mod framelog;
#[cfg(all(windows, feature = "diagnostics"))]
mod framegen;
#[cfg(windows)]
mod heap_worker;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod indirect;
#[cfg(windows)]
mod inplace;
#[cfg(windows)]
mod unified;
#[cfg(windows)]
mod paged;
#[cfg(windows)]
mod placed;
pub mod list;
pub mod object;
pub mod pipeline;
#[cfg(windows)]
mod plaincopy;
#[cfg(windows)]
mod primitive;
#[cfg(all(windows, feature = "diagnostics"))]
mod profile;
pub mod query;
pub mod queue;
#[cfg(windows)]
mod queue2;
pub mod resource;
pub mod root;
#[cfg(windows)]
mod census;
#[cfg(windows)]
mod sampler;
#[cfg(windows)]
pub mod shader_cache;
#[cfg(windows)]
mod marks;
#[cfg(windows)]
mod memory;
#[cfg(windows)]
mod shadow;
#[cfg(windows)]
mod tiles;
pub mod signature;
#[cfg(windows)]
mod texture;
#[cfg(windows)]
mod texhash;
#[cfg(windows)]
mod watch;
#[cfg(windows)]
pub mod softview;
#[cfg(all(windows, feature = "diagnostics"))]
mod threadtimes;
mod tick;

#[cfg(windows)]
pub use host::on_failure;
#[cfg(all(windows, feature = "diagnostics"))]
pub use bake_probe::{
    Writer as BakeProbeWriter, dstorage_tag as bake_probe_dstorage_tag, note as bake_probe_note,
    note_signal as bake_probe_dstorage_signal, probe_on as bake_probe_on,
};
#[cfg(windows)]
pub use census::title_memory_reserved;
#[cfg(windows)]
pub use inplace::{title_memory_freeing, title_memory_section, title_memory_view};
#[cfg(windows)]
pub use softview::title_memory_section_gone;
#[cfg(windows)]
pub use unified::{in_place_on, title_memory_change};
#[cfg(windows)]
pub use watch::{
    Writing, title_memory_reading, title_memory_reading_now, title_memory_reading_range,
    title_memory_remapped, title_memory_remapping, title_memory_remapping_range, title_memory_written,
    title_pages_changed, title_pages_told, writing_async as kernel_will_write_async, writing_held as kernel_will_write,
};
