# Vertex

**Vertex** is an open-source Windows compatibility layer and desktop launcher for running Xbox Series X|S titles natively on PC from files you supply. It ships **one unified engine**—combining [VIctory](https://github.com/Marvisak/VIctory), [AnyXboxX](https://github.com/LooperSalty/AnyXboxX) / [Vice-Tay](https://github.com/TaynanGT/Vice-Tay), VIctory-NVIDIA, and Vertex's own GPU, input, and UI work—paired with the **Vertex Launcher** desktop app.

> **Legal & Asset Policy:** **No game files, executables, keys, shaders, fonts, or proprietary NVIDIA/Microsoft binaries are included in this repository.** You must supply your own legally obtained copy of the title. Vertex is not affiliated with or endorsed by any game developer, publisher, or hardware vendor. See [docs/asset-policy.md](docs/asset-policy.md).

---

## Highlights

- **Single Unified Engine (`victory.exe`):** One high-performance engine build—no fragmented engine variants—combining multi-threaded command-list translation (`VICTORY_THIN`), async submission (`VICTORY_SUBMIT_FRAMES=2`), 64-shard write-watch journaling (`VICTORY_PORT_JOURNAL=1`), D3D12 barrier coalescing & UAV deduplication (`VICTORY_JOIN_BARRIERS=1`), parallel wide copies (`VICTORY_WIDE_COPY=1`), honest title memory reporting (`VICTORY_TITLE_USED_MB=title`), and 4K render-target heap expansion (`resolution.rs`).
- **Vertex Launcher (`apps/vertex-launcher`):** Streamlined Tauri desktop UI with Hardware Auto Tune, one-click Graphics Modes, shader pre-translation & pipeline warming, live log viewer, and a global `Win+Alt+V` **In-Game Menu** overlay.
- **Working Graphics Modes:** Four distinct presets—**Performance**, **Balanced**, **High Quality**, and **4K Native**—that genuinely change output resolution, internal render scale (both with optional NVIDIA DLSS Super Resolution and with the title's built-in upscaler on any GPU), texture LOD sharpening, VRAM streaming budgets, and D3D12 tuning.
- **Native PC Keyboard & Mouse + Scaleform Vector Glyphs:** Context-aware GTA VI PC controls (`native-keys.txt`, hot-reloaded within 1 second), full key/mouse/modifier-combo rebinding (`Ctrl+Key`, `Shift+Key`, `Alt+Key`), interactive conflict resolution, and procedural Scaleform GFx vector keyboard key caps (`VICTORY_PROMPTS=auto|keys|gamepad`) that automatically switch between keyboard caps and Xbox controller icons.

---

## Graphics Modes

Every mode in Vertex Launcher changes real engine parameters—and works safely on **both NVIDIA GPUs (with optional DLSS plugin) and AMD/Intel GPUs (native upscaler fallback)**:

| Mode | Output (`VICTORY_OUTPUT`) | Internal Render (with DLSS SR) | Internal Render (Native Upscaler) | Texture Sharpness (`VICTORY_LOD_BIAS`) | VRAM & Streaming Tuning | Target Hardware |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Performance** | `1920×1080` | `1114×626` (`0.58×` DLSS Balanced) | `1280×720` (`0.67×` `VICTORY_RENDER_SCALE`) | `-0.79` | `90%` VRAM room · `6` pool spares · `12`-frame mip check · `12 ms` stage · `1024/768 MB` buffers · `VICTORY_D3D12_DRED=0` | 8–10 GB GPUs / maximum FPS (25–30 fps class) |
| **Balanced** *(Recommended)* | `2560×1440` | `1707×960` (`0.67×` DLSS Quality) | `1707×960` (`0.67×` `VICTORY_RENDER_SCALE`) | `-0.58` | `95%` VRAM room · `8` pool spares · `6`-frame mip check · `16 ms` stage · `1536/1024 MB` buffers · `VICTORY_D3D12_DRED=light` | 12 GB GPUs (best FPS & sharpness balance) |
| **High Quality** | `2560×1440` | `2560×1440` (`1.0×` DLAA) | `2560×1440` (`1.0×` native) | `-0.50` | `96%` VRAM room · `12` pool spares · `4`-frame mip check · `20 ms` stage · `2048/1280 MB` buffers | 12–16+ GB GPUs wanting native 1440p sharpness |
| **4K Native** | `3840×2160` | `3840×2160` (`1.0×` DLAA) + auto `2.25×` RT heap growth | `3840×2160` (`1.0×` native) + auto `2.25×` RT heap growth | `-0.25` | `96%` VRAM room · `16` pool spares · `4`-frame mip check · `24 ms` stage · `2048/1280 MB` buffers | 16–24 GB GPUs (heavy on 12 GB VRAM) |

> On Series X without `victory_dlss.dll`, Vertex automatically stays on the title's built-in upscaler (`VICTORY_RENDER_SCALE`) rather than forcing `VICTORY_UPSCALER=7`, preventing non-1080p FidelityFX context failures on non-DLSS setups.

---

## Quick Start

### System Requirements

| Component | Requirement |
| :--- | :--- |
| **OS** | Windows 10 or Windows 11 (64-bit) |
| **CPU** | x86-64 with AVX2 (12+ threads recommended) |
| **GPU** | Direct3D 12 with Shader Model 6.6, Resource Binding Tier 3, and Mesh Shaders (12 GB+ VRAM recommended for 1440p) |
| **RAM** | 32 GB recommended (the title holds 20–26 GB in the world) |
| **Storage** | SSD with several GB free for translated shaders & host pipelines |
| **Build Tools** | Rust (see `rust-toolchain.toml`), Visual Studio Build Tools (C++ workload + Windows SDK), WebView2 Runtime (pre-installed on Windows 11) |

### Build & Run from Source

```powershell
# 1. Clone Vertex
git clone https://github.com/proneonn/Vertex.git
cd Vertex

# 2. Build the unified engine (with performance panel & diagnostics)
cargo build --locked --profile diag -p victory --features diagnostics
# Output: target\diag\victory.exe

# 3. Build Vertex Launcher
cd apps\vertex-launcher\src-tauri
cargo build --release
# Output: apps\vertex-launcher\src-tauri\target\release\vertex-launcher.exe
```

Place `vertex-launcher.exe` and `victory.exe` together (either in the same folder or with `victory.exe` inside `bin\`), or run directly from the build tree—Vertex Launcher auto-discovers `target\diag\victory.exe` and `target\release\victory.exe`.

1. Launch **Vertex Launcher** and select your copy of the title's executable (`Choose .exe`).
2. Pick a **Graphics Mode** (**Performance**, **Balanced**, **High Quality**, or **4K Native**), or click **Detect Hardware & Auto Tune** under **Shaders & Tune**.
3. Open **Shaders & Tune** and run **Pre-Translate All Shaders on Disk** and **Warm Used Pipelines** once to avoid first-launch compilation stutters.
4. Click **Play**. Press **`Win+Alt+V`** anytime in-game to open the Vertex Menu (Controls & mouse sensitivity update live within 1 second; Graphics changes prompt to Save & Restart).

For full build instructions (including optional NVIDIA DLSS plugin setup and release builds), see [docs/building.md](docs/building.md).

---

## Native PC Controls & In-Game Keyboard Glyphs

- **Live Rebinding:** Open **Controls** in Vertex Launcher (or press **`Win+Alt+V`** in-game) to browse all categorized actions, search by action or key, capture single keys, mouse buttons, scroll wheel, or modifier combos (`Ctrl+E`, `Shift+Space`, `Alt+F`), resolve duplicate conflicts (**Swap** / **Reassign** / **Keep Both**), and stage edits with **Apply / Cancel**.
- **Scaleform GFx Vector Glyphs:** Vertex intercepts the title's Scaleform GFx icon movie at runtime and procedurally draws crisp resolution-independent key caps and mouse icons using host Windows GDI outlines (`VICTORY_PROMPTS=auto|keys|gamepad`).
  - `auto` *(default)*: automatically switches between keyboard key caps and Xbox controller icons based on your last input.
  - `keys`: always show keyboard & mouse glyphs.
  - `gamepad`: keep original Xbox icons.
- **Gamepad Coexistence:** XInput and DualSense controllers work alongside keyboard and mouse without triggering controller-disconnect interruptions.

See [docs/integration/keyboard-glyph-integration.md](docs/integration/keyboard-glyph-integration.md) for the full glyph matrix and architecture.

---

## Documentation

| Document | Contents |
| :--- | :--- |
| [docs/launcher.md](docs/launcher.md) | Complete Vertex Launcher user guide: Graphics Modes, Controls, In-Game Menu, Shader/Pipeline Warming, CLI flags, and troubleshooting |
| [docs/building.md](docs/building.md) | Building the unified engine, Vertex Launcher, optional plugins, running tests, Clippy, and `cargo xtask policy` |
| [docs/architecture.md](docs/architecture.md) | Engine architecture: PE loader, HLE services, D3D12 translation, RDNA 2 → DXIL shader recompiler |
| [docs/dlss.md](docs/dlss.md) | Optional NVIDIA DLSS Super Resolution plugin (built locally against your own NVIDIA DLSS SDK) |
| [docs/integration/keyboard-glyph-integration.md](docs/integration/keyboard-glyph-integration.md) | Scaleform GFx vector keyboard glyphs, modifier combos, and launcher Controls UI report |
| [docs/integration/keyboard-input-comparison.md](docs/integration/keyboard-input-comparison.md) | Architectural comparison of keyboard input and glyph systems |
| [docs/VERTEX-BEST.md](docs/VERTEX-BEST.md) | Technical summary of integrated engine patches, switches, and known limitations |
| [docs/asset-policy.md](docs/asset-policy.md) | Repository asset policy (Rule #1) |
| [docs/credits.md](docs/credits.md) | Full contributor and upstream attribution list |

---

## Repository Structure

```text
Vertex/
├── apps/
│   ├── victory/               # Unified engine CLI & runtime (victory.exe)
│   └── vertex-launcher/       # Tauri desktop launcher (vertex-launcher.exe)
├── crates/
│   ├── victory-core/          # Config, paths, pad/input & active-device core
│   ├── victory-pe/            # PE32+ parser
│   ├── victory-loader/        # Native loader, resolution/heap patching, RAGE keyboard events
│   ├── victory-hle/           # Xbox GDK/kernel/audio/GameInput/FFX HLE
│   ├── victory-gpu/           # D3D12 backend, barriers, texture streaming, DLSS bridge, overlay
│   ├── victory-shader/        # RDNA 2 → DXIL shader recompiler
│   ├── victory-formats/       # Console texture tiling, BCPack, Scaleform GFx/SWF writer
│   ├── victory-pipeline/      # Parallel host pipeline caching & warming
│   ├── victory-bc/            # BC1–BC7 texture codec utilities
│   └── victory-game/          # Title-specific hooks & Scaleform GFx prompt glyph renderer
├── plugins/
│   └── glyphs/                # Optional MIT procedural key-cap plugin DLL
├── docs/                      # Architecture, launcher, build, DLSS, and integration docs
└── xtask/                     # Repository policy (`cargo xtask policy`) & switch validation
```

---

## Credits & License

Vertex builds directly on the open-source work of the VIctory ecosystem. Full git history and commit authorship are preserved; see [docs/credits.md](docs/credits.md) for everyone with commits in this tree.

- **[VIctory](https://github.com/Marvisak/VIctory)** (Marvisak & contributors) — loader, runtime services, D3D12 backend, and RDNA 2 → DXIL shader recompiler.
- **[AnyXboxX](https://github.com/LooperSalty/AnyXboxX)** & **[Vice-Tay](https://github.com/TaynanGT/Vice-Tay)** — multi-threaded command lists, async submission, VRAM residency & texture streaming, performance panel, and resolution patching.
- **[avatarmanz/VIctory-7900XTX](https://github.com/avatarmanz/VIctory-7900XTX)** — thin-list recording, async copy queues, write-guard optimizations, and frame record/replay.
- **VIctory-NVIDIA (trustno1-coder & contributors)** — `VICTORY_WC_NO_BACK`, `VICTORY_SOFT_VIEWS`, `VICTORY_RIPE_BLOCKS`, texture staging limits, and NVIDIA shader/descriptor fixes.
- **[DexNom/VIctory](https://github.com/DexNom/VIctory)**, **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**, **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)**, **[myokex/victory-amd](https://github.com/myokex/victory-amd)**, **PastiiJay**, **Trexies**, and community PR authors.

Licensed under **GPL-3.0-or-later**. See [LICENSE](LICENSE) (with the single exception noted in [docs/asset-policy.md](docs/asset-policy.md)).
