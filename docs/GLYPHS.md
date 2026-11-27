# Keyboard key caps in place of controller button glyphs

The console build draws Xbox button icons in its prompts. The glyphs plugin (`victory_glyphs.dll`,
`plugins/glyphs`, MIT) repaints those icons, in the title's own textures, as keyboard key caps showing the
key bound in `native-keys.txt` (A becomes SPACE, RT becomes LMB, RB becomes Q, and so on). The engine
(`crates/victory-gpu/src/glyphs.rs`) hands the plugin each small title texture when it is detiled for the
card, and uploads what the plugin hands back.

**Not known yet: which textures hold the glyphs.** One play session in identification mode finds them
(below). Until `glyphs.txt` names them, the plugin changes nothing.

## Switches (engine)

All off unless set. Nothing is loaded, hashed or written without them.

| Switch | What it does |
|---|---|
| `VICTORY_GLYPHS_PLUGIN=<full path>\victory_glyphs.dll` | Loads the plugin. Each small title texture (see below) is hashed once per content. If `glyphs.txt` names the hash, the plugin paints caps over the glyphs (every level, in the texture's format), and the card gets the painted texture. Answers are cached per content, so a texture costs nothing per frame. |
| `VICTORY_GLYPHS_DUMP=<folder>` | Identification: writes each distinct small title texture once as `<hash>_<w>x<h>_<format>.png` (BC formats decoded), on a background thread, and adds a line to `<folder>\index.txt`: `hash size format levels seconds file` (seconds since the first dump of the run). Works with or without the plugin. |
| `VICTORY_GLYPHS_MAX_SIDE=<texels>` | The largest width or height that counts as small (default 1024). |

The textures that are looked at are 2D, have one slice, and use RGBA8, BGRA8, BGRX8, R8, A8, BC1, BC2, BC3,
BC4, BC5 or BC7, at least 4 and at most `MAX_SIDE` texels a side. Textures with levels on demand are left
out. The hash covers the largest level the engine holds: its texels, format family and size. Typeless, UNORM
and sRGB variants of one format hash the same. BC5 is dumped but cannot be repainted.

## Files (plugin)

- `glyphs.txt`: `VICTORY_GLYPHS_FILE`, else `<data>\glyphs\glyphs.txt`.
  `<data>` is `VICTORY_DATA_DIR` (`D:\dev\Vertex-AXX\data` in the test build), else `%LOCALAPPDATA%\victory\data`.
- The keys: `VICTORY_NATIVE_KEYS`, else `<data>\native-keys.txt`.

The plugin rereads both files within a second of a change. Textures already uploaded change the next time the
game uploads them (a new area, a menu, a reload); a restart always applies everything.

### glyphs.txt

```text
# <hash or dump file name>  <glyph>  [x y w h]  [menu] [premul]
3fa2c9d1e0b4a785_64x64_BC3.png   A                 the whole texture is the A button
9c01d2e3f4a5b6c7_256x64_BC7.png  A    0 0 64 64    an atlas: one line per glyph
9c01d2e3f4a5b6c7_256x64_BC7.png  B   64 0 64 64 menu  ...showing B's menu key
9c01d2e3f4a5b6c7_256x64_BC7.png  key=Tab 128 0 64 64  a key you choose
9c01d2e3f4a5b6c7_256x64_BC7.png  label=HOLD 192 0 64 64  a cap that says HOLD
map LB Tab                       LB's caps show Tab, whatever native-keys.txt says
map A Enter menu                 ...in menus only
```

- The first token is the hash: the first 16 hex digits of a dump file name. Pasting the whole file name is fine.
- Glyph names: `A B X Y LB RB LT RT View Menu L3 R3 DUp DDown DLeft DRight`, plus `DPad`, `LS`, `RS`, `LX`,
  `LY`, `RX`, `RY`, `LSUp` ... `RSRight`. Aliases: `Back`/`Select` for View, `Start` for Menu, `LSB` for L3,
  `RSB` for R3.
- `x y w h` is the glyph's rectangle in the dumped PNG, in pixels. Paint shows pixel coordinates in its status
  bar when you hover. Leave it out when the whole texture is one glyph. Lower levels are scaled to match.
- `menu` uses the key the button stands for in menus. `premul` is for textures with premultiplied alpha:
  try it if a cap's edge looks dark or has a halo.
- `plugins/glyphs/glyphs.example.txt` is a commented template.

Mistakes are reported in the session log with their line numbers (`glyphs: ... line N: ...`).

### Which key a button shows

For each pad button, the plugin takes every title action the console binds to it (`plugins/glyphs/src/actions.rs`,
from `axx-audit/controls-research/action_table.txt`). It then picks the key in `native-keys.txt` whose play
actions (or menu actions, after the `|`) cover them most. The generic prompt actions count most: `INPUT_CONTEXT*`
counts 6 and `INPUT_SCRIPT_*` counts 2. Vehicle, parachute, minigame, shop, frontend and cinematic variants
count 0.5, so six kinds of vehicle attack on RB do not outvote taking cover. Every other action counts 1. A tie
goes to the earlier line. With the current layout:

| Button | Play | Menus |
|---|---|---|
| A | SPACE | SPACE |
| B | X | ESC |
| X | R | R |
| Y | F | TAB |
| LB | MB4 | Q |
| RB | Q | E |
| LT / RT | RMB / LMB | PGDN / PGUP |
| View / Menu | V / ESC | V / P |
| L3 / R3 | SHIFT / C | SHIFT / CTRL |
| D-pad up/down/left/right | ↑ ↓ Q E | W S A D |
| LS / RS | WASD / MOUSE | WASD / MOUSE |

The plugin logs this table at start (`glyphs: in play A=SPACE B=X ...`). Use `map` lines for anything you
would rather see (for example `map LB Tab`). When one pad button stands for several keys in different
situations, the cap shows only one of them.

Caps are drawn procedurally: a dark rounded key with a light rim and a white label, in a 5x7 font drawn for the
plugin. Long names get shorter on small caps (SPACE becomes SPC, SHIFT becomes SHF). Mouse buttons read LMB,
RMB, MMB, MB4 and MB5. On one-channel textures (R8, A8, BC4), which the game tints itself, only the rim and the
label are drawn.

## The identification session (once)

You need an engine built from branch `vertex/glyphs`
(`cargo +stable build --profile diag -p victory --features diagnostics`), for example
`D:\dev\unify\target-glyphs\diag\victory.exe`. The test build's `Play-Vertex-AXX.ps1` runs `bin\vertex-axx.exe`,
so put the new exe there yourself, after copying the old one somewhere safe.

1. Pick an empty folder on D: (C: is nearly full), for example `D:\glyph-dump`. The dump can grow to a few GB
   in a long session, so keep the session short (10 to 15 minutes is plenty).
2. Start the game with the dump on:
   ```powershell
   cd D:\dev\Vertex-AXX
   .\Play-Vertex-AXX.ps1 -NativeKeys -More @{ VICTORY_GLYPHS_DUMP = 'D:\glyph-dump' }
   ```
3. Bring up as many kinds of button prompt as you can, a few seconds each:
   - walk up to a car (enter prompt), a door or a person (context prompt)
   - the phone, the pause menu and the map (the button bar at the bottom of menus)
   - any tutorial or help box with buttons in it
   - get into a car, and aim a weapon
4. Quit the game.
5. Open `D:\glyph-dump` in Explorer: View > Extra large icons. The button glyphs look like Xbox buttons: A, B, X
   and Y circles (green, red, blue, yellow, or white), LB/RB/LT/RT shapes, sticks, the D-pad, and the View
   and Menu icons. They can be single small textures or one sheet (an atlas) holding many icons. Sorting by
   *Date modified* groups what loaded together. `index.txt` gives each file's seconds since the first dump,
   if you noted when a prompt was on screen.
6. Create `D:\dev\Vertex-AXX\data\glyphs\glyphs.txt` from `plugins\glyphs\glyphs.example.txt`. For each glyph
   file, add a line `<file name> <button>`. For an atlas, add one line per icon with its rectangle
   `x y w h`, read in Paint.
7. Play with the plugin (the dump can stay off):
   ```powershell
   .\Play-Vertex-AXX.ps1 -NativeKeys -More @{ VICTORY_GLYPHS_PLUGIN = 'D:\dev\unify\target-glyphs\plugin\release\victory_glyphs.dll' }
   ```
   In `logs\session-*.log`, `glyphs:` lines show the files read, the key table and each texture repainted
   (`glyphs: <hash> (64x64 BC3, 1 levels): caps SPACE`).

If no glyph turns up in the dump:
- They may be larger than 1024: run again with `VICTORY_GLYPHS_MAX_SIDE = '2048'`.
- They may be in a format this does not read (the log names none), or reach the card without passing the
  engine's detiling (GPU copies). Report what you saw. The next step would then be a frame capture of a
  prompt.

## Building the plugin

```powershell
plugins\glyphs\build.ps1            # -> target\glyphs-plugin\release\victory_glyphs.dll
```

The plugin has no SDK or download. Its one dependency is `crates/victory-bc` (MIT): BC1-BC5 and BC7
decoding, and BC1-BC4 plus BC7 (modes 5 and 6) encoding. Tests: `cargo +stable test` in `plugins/glyphs`,
`cargo +stable test -p victory-bc`, and `cargo +stable test -p victory-gpu --lib glyphs`.
`VICTORY_GLYPHS_TEST_PLUGIN=<dll> ... -- --include-ignored` also loads the built DLL through the engine's loader.

## Interface (plugin version 1)

Plain C functions; all are thread-safe:

```c
uint32_t victory_glyphs_version(void);                  // 1
uint32_t victory_glyphs_init(const Init*);              // 0: started.  Init { const wchar_t* data; void (*log)(const char*); }
uint32_t victory_glyphs_generation(void);               // changes when its files were reread
uint32_t victory_glyphs_wants(const Texture*);          // 1: repaint this content.  Texture { u64 hash; u32 dxgi_format, width, height, levels; }
uint32_t victory_glyphs_replace(const Texture*, uint32_t level, uint8_t* bytes, size_t len, uint32_t pitch); // 1: changed in place
```
