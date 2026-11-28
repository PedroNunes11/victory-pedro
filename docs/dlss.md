# DLSS Super Resolution, with your own SDK

Optional and off. Nothing of NVIDIA's (no header, library or DLL) is in this
repository, linked into `victory.exe` or downloaded by it. Someone who has the
DLSS SDK can build a small plugin against it and name that plugin at run time.

## What you do

1. Get the DLSS SDK from NVIDIA and accept its licence yourself. It was written
   against 310.9.
2. Build the plugin (Rust and the Visual C++ build tools, as for the program):

   ```powershell
   $env:VICTORY_DLSS_SDK = 'D:\sdk\dlss'      # holds lib\Windows_x86_64
   plugins\dlss\build.ps1
   ```

3. Run the Series S profile with both set:

   ```powershell
   $env:VICTORY_DLSS_PLUGIN = '<full path of victory_dlss.dll, printed by the build>'
   victory.exe run --console series-s <the title's exe>
   ```

   `nvngx_dlss.dll` is looked for beside the plugin, then in
   `%VICTORY_DLSS_SDK%\lib\Windows_x86_64\rel`.

| Variable | Effect |
|---|---|
| `VICTORY_DLSS_PLUGIN=<full path>` | turns it on |
| `VICTORY_DLSS_SDK=<folder>` | for the build, and where the feature's DLL is found |
| `VICTORY_DLSS_PRESET=J`, `K`, `L` or `M` | the model; the SDK's own choice if not set |
| `VICTORY_DLSS_FLAGS=<hex>` | FidelityFX context flags to use in place of the ones the title made its context with |

The log says whether the plugin loaded, whether DLSS is up on the card, what
the title made its upscale context with, which feature was made, and once for
each reason why a dispatch was stretched instead. A dispatch DLSS cannot take is
always stretched (`victory_gpu::over`), never left out. The plugin writes no
log files of NVIDIA's; their lines are in our log with
`VICTORY_LOG=victory_gpu::dlss=debug`.

## What it changes

- With the variable unset, or the plugin not loading: nothing.
- On a Series S with it loaded: the known build's two "upscaler choice" patches
  are left out, so that the title uses FidelityFX and dispatches it. With them
  it uses its own upscaler and dispatches nothing DLSS could take the place of.
  This is the state the plain stretch was measured in by its author (`19a0240`).
- The title's `ffxCreateContext` is looked at and passed on.
- On a Series X: nothing (below).

- The plugin starts on a thread of its own when the title makes its upscale
  context; dispatches are stretched until it is up.

## What was measured

On an RTX 5070 (driver 621.40, SDK 310.9.16, Windows 11, 2026-11-26), by the
ignored tests in `crates/victory-gpu/src/dlss/card.rs`, with images made there
and no title. **No run of the title has used it yet.**

- A title's own list and textures, as the dispatch gives them (R9G9B9E5 colour
  and output, D32 depth, R16G16 motion): made, evaluated four times, the output
  holds the input's colour.
- 1280x720 to 1920x1080: 0.62 to 0.77 ms on the card, 60 to 160 microseconds to
  record, about 85 MiB of card memory for a feature (138 for the first).
- Starting the plugin: 2.1 s; a feature: 70 ms, up to 2 s the first time with
  a preset (L). The first test ever run took 27 s, start and first feature
  together; the two were not timed apart then, and it did not happen again.
- Closeness to the scene at the output's size, higher is better (the stretch is
  17.4 to 17.5 dB):

  | Scene | as passed on | x and y turned | x turned | y turned | motion 0 |
  |---|---|---|---|---|---|
  | still, jitter's sign | 28.9 dB | 17.1 | 17.2 | 25.1 | |
  | moving, motion's sign | 27.4 dB | 13.5 | 13.5 | 13.4 | 13.5 |

  Motion in parts of the image with its scale: 27.3 dB. Along the edge of a
  square in front: 22.0 dB told the depth's direction, 21.6 told the other
  (both directions).
- Depth as D32, R32, R32G8X24, R24G8 and R16 (typeless), motion as R16G16,
  R32G32 and R16G16B16A16 (typed and typeless), typeless colour and output,
  R11G11B10 output, an exposure texture and a dynamic size: all taken, with the
  same result.
- Without `nvngx_dlss.dll` the plugin does not start and nothing crashes.

## How the two interfaces differ

Written down in `crates/victory-gpu/src/dlss.rs` with the code that converts.
Motion and jitter have the same units and signs in both, so they are passed on
as they are. The card tests measure that on synthetic scenes: each other choice
of signs is worse than the one used. Reactive masks and sharpening have no
counterpart and are left out.

## Series X: what it would take

On a Series X the title is told of a 3840x2160 display (`dxgi.rs`,
`display_size`), renders below that (a 2560x1440 depth texture is in the
debug layer's messages of run 205) and upscales by its own passes ("type 6").
`VICTORY_DISPLAY_MODE` changes what it is told and not what it presents (run
20).

**At the present** (`host.rs`, `present_now`) there is a finished image only.
DLSS needs, and that point lacks: motion vectors, depth, the jitter of the
frame, the rendered size, and colour before tone mapping and the interface.
None can be made up there. DLSS is not possible at that point.

**In the title's own passes** most of it must exist, since its upscaler is
temporal too (inferred, not seen):

| Input | Where | State |
|---|---|---|
| colour, rendered size | a target the upscale pass reads | which target and which pass is not known |
| depth | the 2560x1440 depth texture | can be recognised by size and use; several passes have one |
| motion | a two-channel float target of that size | not identified; unit and sign not known |
| jitter | constants of the pass or the projection matrix | not identified; nothing in our interface names it |
| exposure | probably a 1x1 texture | automatic exposure does without |
| reset | a flag of the title's | not visible; cuts would have to be guessed |
| output | what the pass writes | to be replaced, with every step of the pass before it left out |

Each would be found by hand from captures, for one build of the title, and a
wrong guess about jitter or motion gives a smeared picture and no error. This
is not implemented.

**By the title's own FidelityFX path** all of it comes with the dispatch, as on
a Series S. `VICTORY_UPSCALER=7` makes both choice functions return
7 on a Series X. KLBF Lab (GPL-3.0, commit `b24cb52` and `run.rs`, `ffx.rs` of
its main) went that way with AMD's upscaler and says what is in the way:

1. The console's upscaler DLL has tables for a 1920x1080 output only: context
   creation for 3840x2160 fails and the title stops. `ffxCreateContext`,
   `ffxDestroyContext`, `ffxQuery` (versions, jitter phases and offsets) and
   `ffxConfigure` would have to be answered by us.
2. The title's scale for this path is 2/3 on a Series S and 1.0 on a Series X:
   it would render at 3840x2160 and overflow its memory plan. A second patch
   of title code has to set 2/3 or 1/2 (theirs is at RVA 0x28EF680, not checked
   on this build by us).
3. DLSS at 3840x2160 takes more card memory than at 1920x1080, on a card that is
   full in the world.

This is the way to go if it is wanted: a few hundred lines, and runs of the
title to check each step. It is not implemented, since no step of it could be
checked without the title.

What it could gain is bounded: on the owner's PC the usual frame (123 ms) is
held up by the submitting thread (108 ms), and the wait for the card is 23 ms
of it.
