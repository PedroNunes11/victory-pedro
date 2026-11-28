//! Host-memory policy for reusable CPU-visible GPU resources.

use std::sync::OnceLock;

use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

const GIB: u64 = 1 << 30;

fn memory_status() -> Option<MEMORYSTATUSEX> {
    // SAFETY: the API initializes a correctly sized structure.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status)
}

fn auto_small_host(total_phys: Option<u64>) -> bool {
    total_phys.is_some_and(|bytes| bytes <= 24 * GIB)
}

/// Whether reusable host allocations should favor headroom over cache size.
///
/// `VICTORY_SMALL_HOST=1` forces the reduced policy and `=0` disables it.
/// Without an override it is selected on machines with 24 GiB of RAM or
/// less. A 32 GiB machine can opt in explicitly while keeping the existing
/// behavior as the default.
pub(crate) fn small_host_ram() -> bool {
    static SMALL: OnceLock<bool> = OnceLock::new();
    *SMALL.get_or_init(|| {
        let small = match std::env::var("VICTORY_SMALL_HOST")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("1") => true,
            Some("0") => false,
            _ => auto_small_host(memory_status().map(|s| s.ullTotalPhys)),
        };
        tracing::info!(small, "small host memory policy (VICTORY_SMALL_HOST)");
        small
    })
}

#[cfg(test)]
mod tests {
    use super::auto_small_host;

    #[test]
    fn automatic_policy_only_selects_hosts_up_to_24_gib() {
        assert!(auto_small_host(Some(24 << 30)));
        assert!(!auto_small_host(Some(32 << 30)));
        assert!(!auto_small_host(None));
    }
}
