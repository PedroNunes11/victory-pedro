# Keyboard Input, Prompt Glyphs & Settings Architecture Comparison

> Comparison between the reference installation (`D:\GTA6-PC`, branch `nvidia`, HEAD `5f322bc`)
> and Victory / Vertex (`D:\Vertex\source`, branch `main`, HEAD `b6a38292`).
>
> **Engineering rule:** Choose the cleanest, most maintainable architecture per subsystem —
> reuse existing Victory infrastructure wherever it is superior, import only compatible
> reference capabilities that strictly improve the system, and avoid duplicate input handlers
> or conflicting configuration files.

---

## 1. Executive Summary

| Area | Reference (`D:\GTA6-PC`) | Victory / Vertex (`D:\Vertex\source`) | Superior Implementation | Integration Decision |
|---|---|---|---|---|
| **In-Game Prompt Glyphs** | Scaleform GFx vector movie rewriting (`victory-formats/gfx.rs` + `victory-game/prompts/`), hooking `movie_address_builder` (`+0x62f3388`) and redrawing 38+ vector button icons via host GDI `Segoe UI` / `Segoe UI Symbol` outlines + geometric mouse icon. Crisp at any resolution/DPI; zero game assets. | Raster texture-upload hook (`victory-gpu/glyphs.rs` + `plugins/glyphs` DLL + `victory-bc`). GTA VI prompts are Scaleform GFx vectors, not raster textures, so raster dump captures 0 prompt icons. However, `plugins/glyphs/src/actions.rs` + `keys.rs` already has a weighted GTA VI `action_table` -> pad-button -> `native-keys.txt` resolver. | **Reference (`D:\GTA6-PC`)** for rendering/hooking; **Vertex** for dynamic action-to-key resolution. | **Port & supercharge** Scaleform GFx vector rewriting into Vertex, expand glyph coverage to all PC keys & modifier combinations, and wire it dynamically to `native-keys.txt` + `keys.txt` + active-device switching. |
| **Input Interception** | `gameinput.rs` stand-in pad + `VICTORY_KEYMAP=pc` (`PcControls`), mapping keyboard/mouse onto a virtual Xbox gamepad. | **Native RAGE event relay** (`victory-loader/keyboard_events.rs`) subscribing to the title's keyboard device (`+0x9ab14c8`) and translating keys directly into active GTA VI `INPUT_*` actions (`+0xab77bd4/+0xab77bd8`), plus `gameinput.rs` for mouse look / map / wheel / controller pass-through. | **Victory / Vertex** | **Keep Vertex** as the sole input handler. Extend `keyboard_events.rs` to support **modifier combinations** (`Ctrl+Key`, `Shift+Key`, `Alt+Key`) cleanly without breaking single-key or held-key behavior. |
| **Keyboard Mapping & Contexts** | Static hardcoded `PcControls` (on-foot vs driving toggled manually via `F`/`CapsLock`), plus scancode-based `keymap.rs` (AZERTY/QWERTZ). | Automatic live context detection from RAGE action bitflags (`IN_MENU`, `IN_MAP`, `IN_WHEEL`, `IN_VEHICLE`), smooth vehicle steering ramp (`VICTORY_STEER_RAMP_MS`), walk toggle/hold, frame-locked mouse look (`FrameLook`), distance-based `MapCursor`, radial `WheelStick`, `MenuStepper`, plus `keymap.rs`. | **Victory / Vertex** | **Keep Vertex.** No manual foot/car toggle needed. Expose live context & active-device state cleanly via `victory-core::pad`. |
| **Keybinding Storage** | Hardcoded Rust constants (`victory-core/pad.rs` + `gameinput.rs`). No user-editable binding file or persistence. | Two files in `<data>`: `native-keys.txt` (primary: `KEY = play actions \| menu actions`, hot-reloaded every <=1 s) and `keys.txt` (legacy stand-in pad + `[mouse]` sensitivity/invert, hot-reloaded every <=1 s). | **Victory / Vertex** | **Keep `native-keys.txt` + `[mouse]` in `keys.txt`** as the single persisted config authority. Add modifier combo syntax (`Ctrl+E = ...`) to `native-keys.txt` and drive both input AND GFx prompt glyphs from it. |
| **Configuration Management** | Flat env file (`play/settings-nvidia.env`) + CLI flags (`--prompts keys`). | `victory-core::Config` (`victory.toml` + `VICTORY_*`), Vertex Launcher JSON profile (`%LOCALAPPDATA%\Vertex\launcher.json`), automatic `ENGINE_SWITCHES` build scanner (`apps/vertex-launcher/src-tauri/build.rs`), atomic temp-file-rename saves. | **Victory / Vertex** | **Reuse Vertex** config management. Add `VICTORY_PROMPTS` (`auto`/`keys`/`gamepad`) and `--prompts` CLI flag so Launcher, CLI, and `Play-Vertex-AXX.ps1` all control it uniformly. |
| **UI Infrastructure & Keyboard Settings Menu** | None (no settings UI, no rebinding, no conflict detection). | **Vertex Launcher** (`apps/vertex-launcher`, Tauri desktop UI + `Win+Alt+V` in-game overlay) with categorized `native-keys.txt` list, search, mouse sliders, and capture modal; plus legacy F1 GDI overlay (`gameinput/menu.rs`, off by default). | **Victory / Vertex** (but needs key UX upgrades) | **Upgrade Vertex Launcher's Controls Settings UI** with: Apply/Cancel staging, interactive conflict resolution (Swap/Reassign/Keep Both), modifier-combo capture, per-action Reset + global Restore Defaults, contextual visual keycap glyphs + live in-game prompt preview, and full keyboard navigation. |
| **Input-Device Detection** | XInput slot polling (slots 0–3); stand-in pad only active when no controller is connected (`fallback` replaced by host pad). | `VICTORY_ONE_PAD=1` merges XInput (0–3) + DualSense HID + KB/M into one persistent pad (avoids GTA VI's "Controller Disconnected" modal), while keyboard events flow natively via `keyboard_events.rs`. No live KB/M vs Gamepad prompt-switching signal yet. | **Victory / Vertex** (needs active-device tracker) | **Add atomic `ActiveDevice` tracking** (`KeyboardMouse` vs `Gamepad`) in `victory-core::pad`, updated on non-idle input in `gameinput.rs` + `keyboard_events.rs`, consumed by `prompts/hook.rs` in `auto` mode. |
| **Debugging Tools** | `victory key-caps <folder>` (renders every vector prompt cap to PNG/SVG + HTML gallery); `tools/nvbench` automated screenshot & benchmark scripts. | `input_spy.rs` (`VICTORY_INPUT_SPY`), `input_watch.rs` (`VICTORY_INPUT_WATCH`), `VICTORY_KEYBOARD_EVENTS=log`, `VICTORY_GLYPHS_DUMP`, D3D12/GDI perf `overlay.rs` (F10/F12 screenshots), `Controls.ps1`. | **Both complement each other** | **Port & extend `victory key-caps <folder>`** into Vertex CLI (supporting `--all` full glyph matrix + `--native-keys <path>` live layout export), keeping all existing Vertex dev tools intact. |
| **Controller Compatibility** | Host XInput pads work, but plugging/unplugging swaps devices and prompts stay permanently rewritten to keyboard keys. | XInput + DualSense HID + KB/M coexist seamlessly under `VICTORY_ONE_PAD=1`; prompts remain Xbox icons until plugin/GFx hook is enabled. | **Victory / Vertex** (with GFx auto-switching) | **Preserve `VICTORY_ONE_PAD=1` coexistence** and let `prompts` switch dynamically between keyboard caps and original Xbox icons in `auto` mode when a controller is used. |

---

## 2. Deep-Dive Findings on Reference (`D:\GTA6-PC`)

### 2.1 How Reference Prompt Glyphs Actually Work

GTA VI renders its on-screen button prompts (menus, HUD help boxes, interaction prompts, map legend) using an uncompressed **Scaleform GFx vector movie** (~28 KB) rather than static raster textures. This explains why raster texture dumping (`VICTORY_GLYPHS_DUMP`) found zero prompt sheets.

`D:\GTA6-PC` intercepts this movie cleanly in four stages:

1. **PE Hook Slot (`crates/victory-loader/src/windows.rs` + `tls.rs`)**
   - Adds validated `LoadedImage::code_address`, `read_pointer`, and `write_pointer` (rejecting out-of-bounds, misaligned, or wrong-section RVAs via `SlotError`).
2. **Build-Specific Hook Offsets (`crates/victory-game/src/lib.rs`)**
   - Identifies `Series 1.0.2.1` by BLAKE3 (`b99f876b74d4a77f03af5f878223b27bd647b5c35a79d5decd9f716504239e93`, matching `D:\Game\V6\payload\V6\master.exe`).
   - Hooks `movie_address_builder` (`+0x62f_3388`) and calls `string_assign` (`+0xeb_ba60`).
3. **In-Memory Movie Interception (`crates/victory-game/src/prompts/hook.rs`)**
   - When the streaming store delivers a movie (`Request::HandedOver`, `kind == 0`), it parses `memory:$<hex_addr>,<len>,<num>:<tag>`, checks the `GFX` header (`<= 256 KiB`), and detects the icon movie structurally (`>= 20` known `icon_<N>` sprite class names including all 4 face buttons A/B/X/Y).
   - When another UI movie loads the icon movie by name (`Request::LoadedByAMovie`, `kind == 3`), it rewrites the UI string to `memory:$<redrawn_addr>,<len>,0:victory:<name>`.
4. **Vector Shape Redrawing (`crates/victory-formats/src/gfx.rs`, `crates/victory-game/src/prompts.rs`, `prompts/font.rs`)**
   - Parses `DefineShape3`, `DefineSprite`, `PlaceObject2/3`, `SymbolClass` tags.
   - Replaces each icon sprite's back shape with a white rounded-square vector cap (1280×1280 twips cell, 50-twip margin, 220-twip corner radius) and front shape with black glyph contours.
   - Extracts TrueType quadratic Bézier outlines at runtime from host Windows fonts (`Segoe UI` Bold + `Segoe UI Symbol`) via GDI `GetGlyphOutlineW(GGO_NATIVE | GGO_UNHINTED)` — **zero redistributed font or game assets**.
   - Draws a custom geometric vector mouse (outline + highlighted LMB/RMB/Wheel/Move states) and multi-cap clusters (2-cap horizontal or 2×2 grid for 4-way clusters like `W A S D` / arrows) scaled cleanly into the 1280-twip cell.

### 2.2 Limitations Found in Reference (`D:\GTA6-PC`)

1. **Incomplete key coverage:** `prompts.rs::key_label` only maps `A–Z`, `Backspace`, `Tab`, `Enter`, `Esc`, `Shift`, `Ctrl`, `Space`, `Arrows`, `LMB/RMB/MMB`, and `MouseMoves`. It lacks digits `0–9`, `F1–F12`, `Alt`, `CapsLock`, `Insert/Delete/Home/End/PageUp/PageDown`, `Numpad 0–9` & numpad operators, punctuation/symbols, `Mouse4/Mouse5/WheelUp/WheelDown`, and **modifier combinations** (`Ctrl+E`, `Shift+R`, `Alt+F`, etc.).
2. **Static bindings only:** `prompts.rs` reads compile-time `victory_core::pad::prompt_key(control, pc)`. Rebinding keys or using `native-keys.txt` has zero effect on displayed prompts.
3. **No live refresh or device switching:** Once redrawn, a cached movie is static for the lifetime of the process, even if the user changes bindings or switches to a gamepad.
4. **No keyboard settings UI:** Zero rebinding UI, zero conflict detection, zero persistence.
5. **Inferior input model:** `VICTORY_KEYMAP=pc` relies on manual `F`/`CapsLock` mode toggling between on-foot and driving because it only emulates a virtual gamepad rather than reading GTA VI's live action context.

---

## 3. Deep-Dive Findings on Existing Victory / Vertex (`D:\Vertex\source`)

### 3.1 Strengths to Reuse

1. **Native RAGE Action Injection (`crates/victory-loader/src/keyboard_events.rs`)**
   - Subscribes to the title's keyboard device (`+0x9ab14c8`) and resolves pressed keys against GTA VI's live active action table (`+0xab77bd4` / `+0xab77bd8`).
   - Automatically detects `IN_MENU`, `IN_MAP`, `IN_WHEEL`, and `IN_VEHICLE` from live action flags — zero manual mode switching.
   - Handles overlapping held keys (`W+S`, `A+D`), smooth vehicle steering ramp (`SteerRamp`), walk speed toggle/hold, and hot-reloads `native-keys.txt` within 1 second.
2. **Unified Gamepad + Mouse Coexistence (`crates/victory-hle/src/modules/gameinput.rs` + `mouse.rs`)**
   - `VICTORY_ONE_PAD=1` merges XInput (0–3), DualSense HID, and KB/M into one persistent pad.
   - Frame-locked camera look (`FrameLook`), distance-based map panning (`MapCursor`), radial weapon/radio wheel stick (`WheelStick`), and menu stepping (`MenuStepper`).
3. **Vertex Launcher Settings UI (`apps/vertex-launcher`)**
   - Modern Tauri desktop UI with global in-game overlay hotkey (`Win+Alt+V`), categorized `native-keys.txt` editor (`native_keys.rs` + `app.js`), live `[mouse]` writing to `keys.txt`, search, bound-only filter, and atomic file persistence.
4. **Developer Toolkit**
   - `input_spy.rs`, `input_watch.rs`, `overlay.rs`, `victory-bc` + `victory-gpu/glyphs.rs` + `plugins/glyphs` (including MIT `actions.rs` GTA VI action-to-pad table).

### 3.2 Gaps in Existing Victory / Vertex to Close

1. **In-game prompt glyphs:** Currently only hooked on raster texture uploads (`victory-gpu/glyphs.rs`), which misses GTA VI's Scaleform GFx vector prompts.
2. **Modifier combinations:** `keyboard_events.rs` and `native_keys.rs` only parse single key names (`E`, `Shift`), not modifier chords (`Ctrl+E`, `Shift+Space`, `Alt+1`).
3. **Active input-device tracking:** Neither `gameinput.rs` nor `keyboard_events.rs` publishes whether the player is currently using Keyboard/Mouse vs Gamepad.
4. **Keyboard Settings UX gaps in `apps/vertex-launcher`:**
   - Immediate-save on every edit — lacks **Apply / Cancel** transactional staging with unsaved-changes tracking.
   - Conflict banner is passive — lacks **interactive Conflict Resolution** (Swap / Reassign & Unbind / Keep Both / Cancel).
   - No **Reset Individual Binding** button (only global Reset All).
   - Capture modal fires on `keydown` immediately for `Shift`/`Ctrl`/`Alt`, preventing **modifier combination capture** (`Ctrl+E`, `Shift+R`, `Alt+Tab`-safe chords) while still needing standalone `Shift`/`Ctrl`/`Alt` binding on keyup.
   - Plain text chips (`LeftClick`, `Backspace`, `ArrowUp`) rather than **contextual keyboard/mouse glyphs** matching in-game prompts, and no live **In-Game Prompt Glyph Preview** strip.
   - Incomplete **keyboard navigation** (arrow keys / Enter / Backspace / Esc) across the controls list and dialogs.

---

## 4. Asset Licensing & Policy Verification

- **Code license:** Both `D:\GTA6-PC` and `D:\Vertex\source` are licensed under **GPL-3.0-or-later** (`LICENSE`), making direct porting of `gfx.rs`, `prompts/`, and loader slot helpers 100% license-compatible.
- **Asset policy (`docs/asset-policy.md`):** Zero copyrighted game assets, textures, fonts, or extracted binaries are imported. All glyph shapes are generated procedurally at runtime from:
  - Pure geometric vector contours (rounded key caps, mouse outline/buttons/wheel, modifier `+` separator), and
  - Host Windows system font outlines queried dynamically via GDI (`Segoe UI` Bold + `Segoe UI Symbol`).
- **Screenshots & game files:** All game binaries remain in `D:\Game\V6` (outside the repository), and all before/after verification screenshots are stored outside the Git repository.

---

## 5. Chosen Integration Architecture

1. **Shared Input & Device Core (`crates/victory-core/src/pad.rs`)**
   - Define `PadControl`, `ActiveDevice` (`KeyboardMouse` / `Gamepad`), `PromptMode` (`Auto` / `Keys` / `Gamepad`), and thread-safe atomic active-device state updated by `gameinput.rs` and `keyboard_events.rs`.
   - Define `KeySpec` (key + optional `Ctrl`/`Shift`/`Alt` modifiers) shared across input and prompt rendering.
2. **Scaleform GFx Vector Engine (`crates/victory-formats/src/gfx.rs` + `crates/victory-loader/src/windows.rs`)**
   - Port `gfx.rs` and `LoadedImage::{code_address, read_pointer, write_pointer, SlotError}` from `D:\GTA6-PC`.
3. **Dynamic Full-Coverage Vector Prompt Glyphs (`crates/victory-game/src/prompts/`)**
   - Port `prompts.rs`, `font.rs`, `hook.rs`, `export.rs` from `D:\GTA6-PC` and upgrade them:
     - Full key coverage: `A–Z`, `0–9`, `F1–F12`, `Shift/Ctrl/Alt` (L/R), `Enter/Esc/Tab/Backspace`, `Space`, Arrows + `Ins/Del/Home/End/PgUp/PgDn/CapsLock`, `Numpad 0–9` + `Num+/-/×/÷/.`, Punctuation `, . - = [ ] ; ' / \ `` ` ``, Mouse (`LMB/RMB/MMB/MB4/MB5/WheelUp/WheelDown/MouseMove`), and **Modifier Combinations** (e.g., `Ctrl+E`, `Shift+R`, `Alt+1` drawn cleanly as stacked/paired caps inside the 1280-twip icon cell).
     - **Dynamic binding resolver (`prompts/bindings.rs`)**: derives per-pad-button keys directly from `native-keys.txt` (reusing the weighted GTA VI action table) with fallback to `keys.txt` and sensible PC defaults.
     - **Live reload & device-aware `hook.rs`**: retains the original GFx movie bytes, checks `native-keys.txt` / `keys.txt` mtime (throttled to 1 Hz), redraws on binding changes (deduplicated cache), and respects `VICTORY_PROMPTS=auto|keys|gamepad` (in `auto` mode, serves original Xbox icons while `ActiveDevice::Gamepad` is active and redrawn keyboard caps while `ActiveDevice::KeyboardMouse` is active).
   - Add `--prompts auto|keys|gamepad` to `victory run` (defaulting to `auto` when `VICTORY_PROMPTS` is unset and native keys are on, or controlled by `VICTORY_PROMPTS`) and `victory key-caps <folder>` CLI subcommand (with `--all` full glyph matrix and `--native-keys <path>`).
4. **Modifier Combinations in Native Input (`crates/victory-loader/src/keyboard_events.rs`)**
   - Parse `Ctrl+Key`, `Shift+Key`, `Alt+Key` (and multi-modifier combos like `Ctrl+Shift+Key`) in `native-keys.txt`.
   - Track live modifier state on the title's keyboard relay; when a key is pressed, prefer a matching modifier-combo binding over a bare-key binding, and release cleanly on keyup.
   - Notify `victory_core::pad::note_keyboard_mouse_input()` on keyboard events (and `gameinput.rs` notifies `note_gamepad_input()` on non-idle XInput/DualSense input, and `note_keyboard_mouse_input()` on mouse/keyboard activity).
5. **Polished Keyboard Settings Menu (`apps/vertex-launcher`)**
   - Upgrade `native_keys.rs`, `main.rs`, `settings.rs`, `ui/app.js`, `ui/strings.js`, `ui/style.css`:
     - Complete categorized overview, search, bound-only toggle, keyboard navigation (`↑/↓/←/→/Enter/Backspace/Esc`).
     - Transactional **Apply / Cancel** staging bar with unsaved-changes badge & guard.
     - Modifier-combination & standalone modifier key capture (`Ctrl+E`, `Shift+F`, standalone `Shift`, mouse buttons, wheel, Numpad, F1–F12, punctuation).
     - **Duplicate-binding detection + Interactive Conflict Resolution modal** (**Reassign & Unbind Existing** / **Swap Bindings** / **Keep Both** / **Cancel**).
     - **Reset Individual Binding** per action + **Restore All Defaults** with confirmation.
     - **Contextual Keyboard Glyphs** rendered as crisp keycap / mouse-SVG badges on every binding row + live **In-Game Prompt Preview** showing exact resolved glyphs for every GTA VI prompt icon (`A/B/X/Y/LB/RB/LT/RT/LS/RS/D-Pad/Menu/View`) plus Prompt Glyphs mode selector (`Auto` / `Keyboard` / `Gamepad`).
     - Updating a binding and clicking **Apply** immediately updates: (1) `native-keys.txt` on disk, (2) `keyboard_events.rs` live input mapping, (3) `victory-game/prompts` live GFx prompt glyphs, and (4) the Launcher's own displayed glyphs and prompt preview.
