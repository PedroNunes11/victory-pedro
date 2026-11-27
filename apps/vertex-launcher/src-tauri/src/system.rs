//! What this PC has, read from Windows: graphics cards (DXGI, as `victory
//! doctor` lists them), memory, processor, display. And the small Windows
//! calls the launcher needs: processes, the title's window, opening a link.

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FEATURE_D3D12_OPTIONS, D3D12_FEATURE_DATA_D3D12_OPTIONS, D3D12CreateDevice, ID3D12Device,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, IDXGIAdapter1, IDXGIDevice, IDXGIFactory6,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, ClipCursor, EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE,
    GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, SW_RESTORE, SW_SHOWNORMAL, SWP_ASYNCWINDOWPOS,
    SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow, WINDOW_EX_STYLE,
    WINDOW_STYLE,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR};

/// One graphics adapter.
#[derive(Clone, Debug, Serialize)]
pub struct Adapter {
    pub name: String,
    pub vendor_id: u32,
    /// Dedicated video memory, MiB.
    pub vram_mb: u64,
    /// Memory of the PC it may share, MiB.
    pub shared_mb: u64,
    /// The driver's version as Windows numbers it (a.b.c.d).
    pub driver: String,
    /// NVIDIA's own number for it (e.g. 581.57), on NVIDIA cards.
    pub driver_nvidia: Option<String>,
    /// Tiled resources tier (2 or more: textures given level by level work).
    /// `None`: no Direct3D 12 device at feature level 12.0.
    pub tiled_tier: Option<i32>,
    pub software: bool,
}

/// What Auto Tune and the memory pages read.
#[derive(Clone, Debug, Serialize)]
pub struct SystemInfo {
    pub adapters: Vec<Adapter>,
    /// The first hardware adapter in the system's fastest-first order (the
    /// one a run takes unless that one cannot draw).
    pub gpu: Option<Adapter>,
    pub ram_mb: u64,
    pub cpu_name: String,
    pub cpu_threads: u32,
    pub avx2: bool,
    pub display_width: u32,
    pub display_height: u32,
    pub display_hz: u32,
}

pub fn read() -> SystemInfo {
    let adapters = adapters();
    let gpu = adapters
        .iter()
        .find(|a| !a.software && a.tiled_tier.is_some())
        .cloned();
    let (display_width, display_height, display_hz) = display();
    SystemInfo {
        gpu,
        adapters,
        ram_mb: ram_mb(),
        cpu_name: registry_string(
            HKEY_LOCAL_MACHINE,
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        )
        .map(|s| s.trim().to_string())
        .unwrap_or_default(),
        cpu_threads: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        avx2: std::is_x86_feature_detected!("avx2"),
        display_width,
        display_height,
        display_hz,
    }
}

fn adapters() -> Vec<Adapter> {
    let mut out = Vec::new();
    // SAFETY: plain DXGI enumeration and queries.
    unsafe {
        let Ok(factory) = CreateDXGIFactory2::<IDXGIFactory6>(DXGI_CREATE_FACTORY_FLAGS(0)) else {
            return out;
        };
        let mut i = 0;
        while let Ok(adapter) = factory
            .EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
        {
            i += 1;
            let Ok(d) = adapter.GetDesc1() else { continue };
            let end = d
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(d.Description.len());
            let version = adapter.CheckInterfaceSupport(&IDXGIDevice::IID).ok();
            let driver = version.map(|v| {
                let v = v as u64;
                format!(
                    "{}.{}.{}.{}",
                    v >> 48,
                    (v >> 32) & 0xffff,
                    (v >> 16) & 0xffff,
                    v & 0xffff
                )
            });
            let nvidia = (d.VendorId == 0x10de)
                .then(|| version.map(|v| nvidia_version(v as u64)))
                .flatten();
            out.push(Adapter {
                name: String::from_utf16_lossy(&d.Description[..end]),
                vendor_id: d.VendorId,
                vram_mb: (d.DedicatedVideoMemory >> 20) as u64,
                shared_mb: (d.SharedSystemMemory >> 20) as u64,
                driver: driver.unwrap_or_default(),
                driver_nvidia: nvidia,
                tiled_tier: tiled_tier(&adapter),
                software: d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
            });
        }
    }
    out
}

/// NVIDIA's number from Windows' (32.0.15.8157 is 581.57).
pub fn nvidia_version(v: u64) -> String {
    let (c, d) = ((v >> 16) & 0xffff, v & 0xffff);
    let n = (c % 10) * 10_000 + d;
    format!("{}.{:02}", n / 100, n % 100)
}

fn tiled_tier(adapter: &IDXGIAdapter1) -> Option<i32> {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: device creation on a live adapter; a feature query into its
    // own structure.
    unsafe {
        D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_12_0, &mut device).ok()?;
        let device = device?;
        let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS,
                (&raw mut options).cast(),
                size_of_val(&options) as u32,
            )
            .ok()?;
        Some(options.TiledResourcesTier.0)
    }
}

fn ram_mb() -> u64 {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure's length is set.
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => status.ullTotalPhys >> 20,
        Err(_) => 0,
    }
}

/// The primary display's mode: width, height, refresh rate.
fn display() -> (u32, u32, u32) {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: the structure's size is set; null names the primary display.
    if unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut mode) }.as_bool() {
        (mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency)
    } else {
        (0, 0, 0)
    }
}

/// A string value from the registry.
pub fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..len]);
    Some(text.trim_end_matches('\0').to_string())
}

/// Steam's folder, as Steam writes it for the user.
pub fn steam_path() -> Option<String> {
    registry_string(HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath")
}

/// Running processes: (id, parent id, executable name).
pub fn processes() -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    // SAFETY: a process snapshot walked with a sized entry, then closed.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            out.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..end]),
            ));
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Whether a process with this executable name runs.
pub fn is_running(exe: &str) -> bool {
    processes()
        .iter()
        .any(|(_, _, name)| name.eq_ignore_ascii_case(exe))
}

/// `root` and every process started under it.
pub fn process_tree(root: u32) -> Vec<u32> {
    let all = processes();
    let mut tree = vec![root];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for (pid, ppid, _) in &all {
            if *ppid == parent && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
        i += 1;
    }
    tree
}

/// The first visible top-level window, without an owner, of one of `pids`:
/// its handle (as a number) and its text.
pub fn window_of(pids: &[u32]) -> Option<(isize, String)> {
    struct Search<'a> {
        pids: &'a [u32],
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Search` below, alive for the walk.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: plain window queries.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if search.pids.contains(&pid)
                && IsWindowVisible(hwnd).as_bool()
                && GetWindow(hwnd, GW_OWNER).is_err()
            {
                search.found = Some(hwnd);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut search = Search { pids, found: None };
    // SAFETY: the callback reads `search` only during the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&raw mut search as isize)) };
    let hwnd = search.found?;
    let mut text = vec![0u16; 512];
    // SAFETY: a window handle and a buffer.
    let n = unsafe { GetWindowTextW(hwnd, &mut text) }.max(0) as usize;
    Some((
        hwnd.0 as isize,
        String::from_utf16_lossy(&text[..n.min(text.len())]),
    ))
}

/// Gives the window a client area of `width` x `height`, centred on its
/// display's work area (smaller if it does not fit). Asked without waiting
/// for the window's thread, as dragging its edges would be.
pub fn resize_window(handle: isize, width: u32, height: u32) -> Result<(), String> {
    let hwnd = HWND(handle as *mut _);
    // SAFETY: window queries and an asynchronous move of a live window.
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        AdjustWindowRectEx(&mut rect, style, false, ex).map_err(|e| e.message())?;
        let (mut w, mut h) = (rect.right - rect.left, rect.bottom - rect.top);
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut x, mut y) = (0, 0);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let area = info.rcWork;
            let (aw, ah) = (area.right - area.left, area.bottom - area.top);
            if w > aw || h > ah {
                let scale = f64::min(aw as f64 / w as f64, ah as f64 / h as f64);
                w = (w as f64 * scale) as i32;
                h = (h as f64 * scale) as i32;
            }
            x = area.left + (aw - w) / 2;
            y = area.top + (ah - h) / 2;
        }
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        )
        .map_err(|e| e.message())
    }
}

/// Releases any cursor confinement so the mouse can freely interact with the
/// in-game Vertex menu even if the game window held a clip rect.
pub fn release_cursor_clip() {
    // SAFETY: passing None releases the cursor clip rect cleanly.
    let _ = unsafe { ClipCursor(None) };
}

/// Brings a live window (either the Vertex menu or the game window) to the
/// foreground cleanly, restoring it if minimized.
pub fn focus_window(handle: isize) {
    if handle == 0 {
        return;
    }
    let hwnd = HWND(handle as *mut _);
    // SAFETY: guarded by IsWindow; standard Win32 foreground activation.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        if fg != hwnd {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Opens a file, folder or link as Explorer would.
pub fn open(target: &str) -> Result<(), String> {
    let target = HSTRING::from(target);
    // SAFETY: plain shell call with valid strings.
    let result = unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &target,
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "Windows could not open it (code {})",
            result.0 as isize
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nvidia_numbers_come_from_windows_numbers() {
        let v = |a: u64, b: u64, c: u64, d: u64| (a << 48) | (b << 32) | (c << 16) | d;
        assert_eq!(super::nvidia_version(v(32, 0, 15, 8157)), "581.57");
        assert_eq!(super::nvidia_version(v(31, 0, 15, 5222)), "552.22");
        assert_eq!(super::nvidia_version(v(32, 0, 15, 6094)), "560.94");
    }

    /// Makes a Direct3D 12 device on each card: not run by default (`cargo
    /// test -- --ignored` runs it).
    #[test]
    #[ignore]
    fn this_pc_can_be_read() {
        let info = super::read();
        assert!(info.cpu_threads >= 1);
        assert!(info.ram_mb > 0);
    }
}
