# Vertex Launcher Guide

**Vertex Launcher** (`apps/vertex-launcher`) is the desktop frontend and in-game menu for the unified Vertex engine. It manages graphics presets, resolution and upscaling, native PC keyboard/mouse bindings, Scaleform vector button prompts, shader/pipeline warming, and optional Lossless Scaling profiles.

---

## 1. Engine Discovery (Single Engine)

Vertex uses **one unified engine binary** (`victory.exe`, also recognized as `vertex.exe` or `vertex-axx.exe` for existing portable layouts). On startup, the launcher automatically searches in order:

1. Custom path saved in **Settings → Advanced → Engine Binary** (if set).
2. Same directory as `vertex-launcher.exe`.
3. `bin\` subfolder beside `vertex-launcher.exe`.
4. Cargo build directories in ancestor folders (`target\diag\`, `target\release\`).

Portable installs that keep `cache\` and `data\` folders beside `vertex-launcher.exe` are detected automatically; otherwise Vertex stores caches and saves under `%LOCALAPPDATA%\victory\cache` and `%LOCALAPPDATA%\victory\data`. Your launcher profile is stored in `%LOCALAPPDATA%\Vertex\launcher.json`.

---

## 2. Graphics Modes

Selecting a mode on the Home screen or under **Settings → Graphics** updates resolution, internal render scale, texture LOD bias, VRAM streaming budgets, and GPU barrier / DRED tuning together.

### Exact Engine Switches per Mode

| Setting / Switch | Performance | Balanced *(Default)* | High Quality | 4K Native |
| :--- | :--- | :--- | :--- | :--- |
| `VICTORY_OUTPUT` | `1920x1080` | `2560x1440` | `2560x1440` | `3840x2160` |
| **With DLSS plugin** (`VICTORY_UPSCALER=7`, `VICTORY_FFX_SCALE`) | `0.58` (`1114×626` → 1080p) | `0.6667` (`1707×960` → 1440p) | `1.0` (`2560×1440` DLAA) | `1.0` (`3840×2160` DLAA) |
| **Without DLSS plugin** (`VICTORY_RENDER_SCALE`) | `0.6667` (`1280×720` → 1080p) | `0.6667` (`1707×960` → 1440p) | `1.0` (`2560×1440` native) | `1.0` (`3840×2160` native) |
| `VICTORY_LOD_BIAS` | `-0.79` | `-0.58` | `-0.50` | `-0.25` |
| `VICTORY_TEXTURE_ROOM_PERCENT` | `90` | `95` | `96` | `96` |
| `VICTORY_TEXTURE_POOL_SPARES` | `6` | `8` | `12` | `16` |
| `VICTORY_TEXTURE_PARTIAL_FRAMES` | `12` | `6` | `4` | `4` |
| `VICTORY_TEXTURE_STAGE_MS` | `12` | `16` | `20` | `24` |
| `VICTORY_CARD_BUFFERS_MB` | `1024` | `1536` | `2048` | `2048` |
| `VICTORY_CARD_WINDOWS_MB` | `768` | `1024` | `1280` | `1280` |
| `VICTORY_PIPELINE_WAIT_MS` | `8` | `16` | *(engine default)* | *(engine default)* |
| `VICTORY_D3D12_DRED` | `0` *(off, +1–2 ms saved)* | `light` | `light` | `light` |
| `VICTORY_JOIN_BARRIERS` | `1` | `1` | `1` | `1` |
| `VICTORY_SUBMIT_FRAMES` | `2` | `2` | `2` | `2` |
| `VICTORY_TITLE_USED_MB` | `title` | `title` | `title` | `title` |
| `VICTORY_WIDE_COPY` / `VICTORY_PORT_JOURNAL` | `1` / `1` | `1` / `1` | `1` / `1` | `1` / `1` |

### 4K Native Render-Target Heap Expansion

The console build budgets its internal render-target heaps and placed pool for a `2560×1440` render size. When **4K Native** runs (`3840×2160` at `1.0×` scale, whether via DLAA or `VICTORY_RENDER_SCALE=1.0`), `victory-loader::resolution` automatically detects the `2.25×` pixel ratio and expands the title's GPU render-target heaps (up to the 32-bit 4094 MiB cap) and placed RT pool (relocating its static page array) so 4K Native boots and renders cleanly. You can override the multiplier manually with `VICTORY_RT_HEAP_SCALE`.

### Optional NVIDIA DLSS Super Resolution

DLSS is **optional** and requires building `victory_dlss.dll` against your own NVIDIA DLSS SDK (see [docs/dlss.md](dlss.md)). Place `victory_dlss.dll` beside your engine binary (or in `bin\`) and place the SDK in `sdk\dlss` or set `VICTORY_DLSS_SDK`.

- When `victory_dlss.dll` is found, the **DLSS Super Resolution** selector (**Off / Quality / Balanced / Performance / DLAA**) is active.
- When `victory_dlss.dll` is absent (or on AMD/Intel GPUs), Vertex automatically disables `VICTORY_UPSCALER=7` and uses **Native Upscaler Render Scale** (`VICTORY_RENDER_SCALE`: `58%`, `67%`, `85%`, `100% Native`), so every Graphics Mode works safely without crashing.

---

## 3. In-Game Menu & Live vs. Restart Settings

Press **`Win+Alt+V`** (customizable under **Settings → Controls**) while playing to summon Vertex Menu over the fullscreen game window.

- **Live Settings (no restart needed):**
  - Keyboard & mouse bindings (`native-keys.txt`) — hot-reloaded by the engine within 1 second after clicking **Apply Changes**.
  - Mouse sensitivity & Invert Y (`keys.txt` `[mouse]`) — hot-reloaded within 1 second.
  - In-Game Menu hotkey & launcher audio.
- **Restart-Required Settings:**
  - Graphics Mode, Output Resolution, DLSS / Render Scale, Texture LOD Bias, Console Profile, Engine & Texture budgets.
  - Changing any of these while playing shows a **Save & Restart Game** / **Save for Next Launch** / **Discard Changes** prompt.

---

## 4. Controls & Scaleform Vector Prompt Glyphs

Under **Settings → Controls**:

1. **Prompt Glyph Mode (`VICTORY_PROMPTS`):**
   - **Auto (KB + Pad):** Automatically shows keyboard/mouse vector key caps when using KB/M and original Xbox icons when using a controller.
   - **Keyboard Caps (`keys`):** Always show keyboard/mouse vector key caps.
   - **Xbox Icons (`gamepad`):** Keep original Xbox controller prompts.
2. **Live Contextual Prompts Preview:** Shows the exact key-cap and mouse SVG glyphs mapped to GTA VI's primary prompts (`A/B/X/Y`, `LB/RB/LT/RT`, `LS/RS`, `D-Pad`, `Menu`, `View`) as you edit.
3. **Modifier Combos & Standalone Modifiers:** Click **Rebind** or **+** and hold `Ctrl`, `Shift`, or `Alt` with another key/mouse button/scroll wheel to bind a combo (e.g. `Ctrl+E`, `Shift+Space`, `Alt+LeftClick`), or tap and release a modifier alone to bind `Shift`/`Ctrl`/`Alt`.
4. **Conflict Resolution:** Binding a key already used in the same category opens an interactive modal to **Swap Bindings**, **Reassign & Unbind Existing**, **Keep Both**, or **Cancel**.
5. **Transactional Apply / Cancel:** Edits stay staged until you click **Apply Changes** (or press `Ctrl+S`). You can also reset individual bindings (`↺` or `Delete`) or **Restore All Defaults**.

---

## 5. Shader Pre-Translation & Pipeline Warming

First-time shader compilation can cause severe gameplay stutters. Under **Settings → Shaders & Tune**:

- **Pre-Translate All Shaders on Disk (`victory.exe shaders <game_dir>`):** Translates console RDNA 2 shaders into DXIL ahead of time.
- **Warm Used Pipelines (`victory.exe warm --used`):** Compiles host D3D12 pipelines recorded by earlier runs using your current Graphics Mode switches.
- **Automatic Mode-Switch Warming:** Vertex records `<cache>\pipelines\warm-mode.txt` keyed by engine binary + Graphics Mode + resolution + upscaler + reflections. When you switch modes or rebuild the engine, clicking **Play** automatically warms pipelines first and launches the game once finished.

---

## 6. Command-Line Interface

`vertex-launcher.exe` supports automation and shortcut flags (validated all-or-nothing before applying):

```text
vertex-launcher.exe [--profile best|<file.json>] [--set name=value ...]
    [--env VICTORY_X=value ...] [--console series-x|series-s]
    [--work cpu|balanced|gpu] [--cache-dir P] [--data-dir P] [--game P]
    [--start] [--prebuild] [--no-music] [--no-save]
```

Examples:

```powershell
# Apply best measured profile for this PC and launch immediately
.\vertex-launcher.exe --profile best --start

# Launch in Performance mode without modifying saved settings
.\vertex-launcher.exe --set preset=performance --set output_resolution=1920x1080 --no-save --start

# Warm pipelines then start the game
.\vertex-launcher.exe --prebuild --start
```

Every `--env` switch is checked against the exact `VICTORY_*` list scanned from engine source at build time (`apps/vertex-launcher/src-tauri/build.rs`), so typos are caught immediately.

---

## 7. In-Game Shortcuts

| Shortcut | Action |
| :--- | :--- |
| **`Win+Alt+V`** | Toggle Vertex In-Game Menu (customizable) |
| **`F3`** | Show / hide the performance overlay (`VICTORY_PANEL=1`) |
| **`F11`** or **`Alt+Enter`** | Toggle fullscreen / windowed |
| **`Alt+F4`** | Exit the game cleanly |
