# Building & Testing Vertex

This guide covers building the unified Vertex engine, the Vertex Launcher desktop app, optional plugins, and running the full test and policy checks required by CI.

---

## Prerequisites

- **Windows 10 / 11 x64**
- **Rust toolchain:** pinned in [`rust-toolchain.toml`](../rust-toolchain.toml) (or current `stable-x86_64-pc-windows-msvc` with `rustfmt` and `clippy`)
- **Visual Studio 2022+ Build Tools** with the **Desktop development with C++** workload and **Windows 10/11 SDK**
- **Microsoft Edge WebView2 Runtime** (pre-installed on Windows 11; required at runtime for Vertex Launcher)
- **Git**

---

## 1. Build the Unified Engine

Vertex builds a single engine binary (`victory.exe`):

### Diagnostics Build *(Recommended — includes the `F3` Performance Panel)*

```powershell
cargo build --locked --profile diag -p victory --features diagnostics
# Output: target\diag\victory.exe
```

### Plain Release Build *(No performance panel or diagnostic switches)*

```powershell
cargo build --locked --release -p victory
# Output: target\release\victory.exe
```

> **Tip:** Both profiles use release-level optimization (`[profile.diag]` inherits `[profile.release]`). The `diag` build keeps the `F3` performance overlay and diagnostic environment switches available without hurting steady-state frame rate.

---

## 2. Build Vertex Launcher

The launcher is a standalone Tauri v2 crate under `apps/vertex-launcher/src-tauri` (excluded from the root Cargo workspace so engine builds don't require Tauri dependencies):

```powershell
cd apps\vertex-launcher\src-tauri
cargo build --release
# Output: apps\vertex-launcher\src-tauri\target\release\vertex-launcher.exe
```

### Optional Custom Launcher Art & Audio

Per [docs/asset-policy.md](asset-policy.md), binary media assets are kept outside Git. Without them, `build.rs` generates a procedural vector icon and the launcher renders cleanly with its built-in CSS theme and silent audio.

To bundle local art and audio, point `VERTEX_LAUNCHER_ASSETS` to a folder containing `README.md`, `art/`, `music/`, and `sfx/` (or place `launcher-assets/` next to the repository root) before running `cargo build --release`.

### Portable Install Layout (Optional)

You can run directly from the build tree, or assemble a clean portable folder:

```text
Vertex/
├── Vertex-Launcher.exe
├── bin/
│   ├── vertex.exe              # or victory.exe
│   └── victory_dlss.dll        # optional, built from your own NVIDIA DLSS SDK
├── cache/                      # auto-detected if present
└── data/                       # auto-detected if present (saves, native-keys.txt)
```

---

## 3. Optional Plugins

- **NVIDIA DLSS Super Resolution (`victory_dlss.dll`):** Requires your own NVIDIA DLSS SDK (never committed or redistributed). See [docs/dlss.md](dlss.md).
- **Procedural Raster Key-Cap Plugin (`plugins/glyphs`):** Optional MIT-licensed DLL (`plugins\glyphs\build.ps1`). Note that GTA VI's built-in on-screen button prompts are handled directly in the engine via Scaleform GFx vector rewriting (`VICTORY_PROMPTS=auto|keys|gamepad`) with no external plugin required.

---

## 4. Running Tests, Clippy, and Asset Policy Checks

GitHub Actions (`.github/workflows/ci.yml`) runs three required checks on every push and pull request:

```powershell
# 1. Install pre-commit asset-policy hook (once per clone)
cargo xtask install-hooks

# 2. Run asset-policy & documented-switch checks + all workspace tests
cargo test --workspace --locked

# 3. Run diagnostics-enabled workspace tests
cargo test --workspace --all-features --locked -- --test-threads=1

# 4. Run Clippy (default and diagnostics builds)
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# 5. Run Vertex Launcher unit tests & UI JS syntax checks
cargo test --manifest-path apps\vertex-launcher\src-tauri\Cargo.toml
node --check apps\vertex-launcher\ui\app.js
node --check apps\vertex-launcher\ui\strings.js
```

### Asset Policy (`cargo xtask policy`)

`xtask` enforces [docs/asset-policy.md](asset-policy.md) statically:
- Allowlists only small text source/doc/launcher UI files (rejecting executables, archives, textures, audio, and embedded hex/base64 blobs).
- Verifies that every `VICTORY_*` environment variable named in any Markdown file under the repository is actually read by Rust source code (`switches::documented_switches_are_read`).
