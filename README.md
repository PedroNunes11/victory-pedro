# BRVIctory-ka1qux

Runs an Xbox Series title natively on Windows, from files the user supplies.
A fork of [victory-fork](https://github.com/fxceless/victory-fork) by fxceless,
itself a fork of [VIctory](https://github.com/Marvisak/VIctory) by Marvisak,
developed and measured on an AMD Radeon card.

This fork collects what it took to get the title's world drawn on one AMD
machine: fixes of its own, open pull requests of victory-fork that were tested
there one by one, and ports from other forks. Every change that affects the
picture was checked in the running title on that machine, most of them against
a counter rather than against a screenshot.

**This repository contains no game files, code or assets.** You need your own
copy of the game. It is not affiliated with or endorsed by the game's developer
or publisher.

## State on the test machine (2026-11-25)

Windows 11, Intel Core i5-13600KF, AMD Radeon RX 7800 XT 16 GB (RDNA3, driver
32.0.32029.2010), 32 GB RAM. Series X profile, diagnostics build.

| Stage | Status | Measured |
|---|---|---|
| Boot, menus, saves | ✅ | work (as upstream) |
| Getting into the world | ✅ | about 3 minutes from launch, with the caches warm and the movies skipped |
| The world | ✅ | drawn from a save: rooms with walls, ceilings and textures, lit by their lamps and by the sun; characters with skin, hair and clothes; exteriors with buildings; HUD and prompts. Also the open world later in the story (a save converted from another console, below): a city of high-rise buildings, palms, pools, a beach and the sea with boats |
| Light in shadow | 🟡 | the title's own ray-traced light runs, on by default (`VICTORY_RAYS=0` turns it off): its structures are built and traced, and shaded faces of buildings, trees and characters get bounced light, where without it they are near black. Four runs on the street, 5 to 11 minutes in the world, with no crash and no device removal; a frame costs about 0.05 to 0.1 s more |
| Play | 🟡 | a 25-minute play session, from the first room on to the next objective, on an earlier build of this branch; a 17-minute session in the open world with victory-fork's speed defaults on, with no crash |
| Frame rate | 🟡 | with the switches below, in the open world on a street with about 6,100 draws a frame: a median of 0.42 s a frame (about 2.4 frames a second), 0.33 s at best; the same street took 0.83 s before this build's fixes to the merge of victory-fork's newest main and 0.53 s before the write marks (below). An earlier build, in a lighter view of about 2,000 draws, took 0.32 s. With victory-fork's speed defaults a frame took 144 to 150 ms in steady open-world views on an earlier build, but the picture is not right with them yet (below) |
| Stability | ✅ | no crash and no device removal in the last fifty runs (4 to 25 minutes each), four of them with ray tracing on |
| Memory | ✅ | in the open world about 12.5 GB of the card's 16 in use, textures the card has no room for made at a lower resolution instead of not at all (below) |
| Tests | ✅ | `cargo test --workspace` runs 739 tests and all pass on the test machine; the CI on `main` (clippy twice and the tests, on a machine with no card) passes |

Known problems:

- The title's ray-traced light is on by default (below) but has been measured
  on one street of the open world only; interiors, night and other scenes are
  not checked yet. With `VICTORY_RAYS=0` a constant stands in for that light,
  so shaded places are lit, but flat.
- In a cutscene at the start of the save used for testing, the main
  character's torso shows the clothes of an earlier scene while the sleeves
  are right: the title bakes the outfit before the new outfit's textures have
  reached its memory. Under investigation.
- Loading is slower than the title's clock. A scene starts before all of it
  has streamed in: the shell of the first room (walls and ceiling) arrives 10
  to 25 seconds into the opening shot, in every launch looked at. An earlier
  version of this page called that "missing in some launches"; it was one
  picture taken at different moments of the shot.
- The characters' baked skin and clothes were right in 2 launches of 5 on the
  previous build and are right in all five since victory-fork's `310c6eb`
  and its pull requests #606 and #632 went in (below). That is still a small
  count; `VICTORY_BAKE_PROBE=1` reads the bake's result back, and its
  sequence of hashes was the same in the launches it was on.
- In the open world the card fills up. Textures it has no room for are now
  made without their largest mips (below); before, thousands waited and walls,
  cupboards and some objects showed black with light speckles. Such a texture
  stays at the lower resolution until it is let go.
- With victory-fork's speed defaults the frame rate doubles, but baked hair
  and skin come out too light (they came out black on an earlier build): the
  bake reads data the card has not received yet while submissions run ahead.
  They stay off in the recommended settings.
- Two of the title's compute programs do not translate, and the programs that
  only shade ray-tracing hits are left out on purpose.
- About 2.4 frames a second on a busy street. Of a 0.42 s frame, about 0.17 s
  is the card, 0.09 s preparing it, 0.17 s the pass over the descriptor heap
  (most of it remaking the buffer views the title copies in as bytes, about
  60,000 a frame) and 0.05 s copying title memory back.
- Tested on one machine only. The switches under "Running it" are the ones the
  state above was measured with.

## How it works

Nothing is emulated. The title's executable is a PE32+ image of x86-64 code:
the loader maps it, applies its relocations, sets up its TLS and binds its
imports to implementations of the console's interfaces (an HLE of the
XGameRuntime, memory (`xmem`), DirectStorage and D3D12.X layers), so its code
runs on the host CPU as it is. Its command lists become host D3D12 command
lists. Its shaders are machine code for the console's RDNA2 graphics
processor; a shader recompiler translates each program to DXIL when the title
makes a pipeline, and caches the result. Textures are detiled from the
console's layouts (swizzle and tiling modes) and BCPack streams are unpacked;
audio is decoded from XMA2 and Opus; the title's Bink movies play through its
own library. [docs/architecture.md](docs/architecture.md) has the crates.

## What this fork adds to victory-fork

Each change is explained in its commit message and in a comment where it is.

**Copies of title memory that went stale** (walls, ceilings and materials
missing in some launches and not in others):

- A diagnostic that measures it, `VICTORY_STALE_CHECK=1`: at every update,
  each page of a copy the write tracking calls unchanged is compared with the
  title's. Before the fixes 5.4 % of those pages differed, 300,000 of them all
  zeros in the copy; with them 0.1 %.
- The fixes are open pull requests of victory-fork applied here: #438 by
  sennecools (write guards see the other views of the same physical pages, and
  the runtime's own writes) and #571 by arminkhaleghirod (DirectStorage page
  writes are told where the pages are mapped).

**Views and textures**

- A raw or structured buffer view that does not begin on 16 bytes of the host
  copy of its memory is made on a copy of its own range. The host's raw views
  begin on 16 bytes and the console's anywhere, so such views read wrong or
  were left null: more than 100,000 view creations a run.
- A command list's reset lets go of the textures its prelude uploaded. Two
  pull requests that were each right alone left every list holding every
  texture it had ever uploaded: no idle texture was freed, the card filled, and
  some 10,000 textures picked by index were never made.
- The queue of textures picked by index: slots never uploaded go ahead of
  re-looks at uploaded ones; re-looks are bounded per frame and back off for
  textures that keep changing.
- Texture views are told apart by the memory their mips really take, uploads
  start at the first mip that is there, and views the queue lost are put back
  (ported from [myokex/victory-amd](https://github.com/myokex/victory-amd)).
- Textures the card has no room for are made without their mips larger than
  256 texels a side, up to 97 % of the card's budget, instead of being
  refused (`VICTORY_TEXTURE_SHORT=0` goes back; `VICTORY_TEXTURE_DROP_MIPS=N`
  is victory-fork's pull request #610). A bake's own sources are made first,
  past the line, so characters keep their skin when the card is full.
- The title bakes its characters' skin and clothes while it loads, from
  textures it picks by index. Before each bake the texture pass runs until
  nothing more is due (it took at most 4,096 slots, once). That alone did not
  settle the bakes: they came out right in 2 launches of 5. What did, as far
  as the launches since show, came from victory-fork: its main at `310c6eb`
  (a newer view makes an older one stale only when it starts within its first
  4 KiB, where every neighbour of a packed texture used to be taken out of the
  queue; a texture with a host copy is uploaded again when the card is full)
  and its open pull requests #606 by laptopnotebook (a remap under an
  uploaded texture uploads it again; this path marked a source of a bake in
  each launch measured) and #632 by ryotdev (a late mapping check no longer
  clears the view that replaced the one it looked at).
- In the open world the card fills up, and late bakes found the textures
  they read still waiting for room (13,000 due, none made): characters came
  out with black skin and white eyes. A bake's own sources now go first and
  may be made past the card's usual line, up to 96 % of its budget
  (`VICTORY_BAKE_SOURCES_FIRST=0` turns it off, `VICTORY_BAKE_ROOM_PERCENT`
  sets the line). In an A/B on the same save, skin and clothes came out right
  with it and black-speckled without it.
- The texture ring of the intro movie no longer shows its frames out of order.
- A descriptor read only for its texture's size is answered by the host.

**Dispatches and pipelines**

- A dispatch of more than 65,535 groups in one dimension is run in parts,
  direct or indirect, with the clamp of indirect arguments on or off. The
  title's final gather of indirect light asks for 115,200. (After
  myokex/victory-amd, extended to any number of parts and to count buffers.)
- Compute programs that only shade ray-tracing hits get no host pipeline while
  the ray-tracing structures are not built: they cost AMD's compiler seconds
  each. Compute pipelines are waited for longer while a world loads.
- Offline warm-up of the shader cache and of the pipelines earlier runs used
  (victory-fork #282 by t4l03 and #237 by fffluffy-dev). The shader cache is
  keyed by the translator's sources, so a rebuild that leaves the translator
  alone keeps it, and a run after a warm-up compiles next to nothing while it
  plays (2 pipelines made, 2,770 loaded, in the run that measured it).

**Loading**

- A DirectStorage queue made for manual submission waits for `Submit`
  (victory-fork #575 by Cuebert), and a page list still being filled is read
  again for a moment before its request fails.
- `VICTORY_SKIP_MOVIES=1` ends the title's non-looping movies on their first
  playback tick (`all`: looping ones too).
- `VICTORY_PATCH=<rva>=<bytes>[:<expected>],...` writes bytes into the title's
  code in memory before it starts, only where the original bytes match; the
  title's files are not changed. Used here, with
  [LilBigJP/ztrx-ps5-save-import](https://github.com/LilBigJP/ztrx-ps5-save-import),
  to load a story save converted from another console and test the open world.

**Frame time** (the street of the open world, ~6,100 draws a frame, 0.83 s a
frame after the merge of victory-fork's main, 0.42 s with these):

- Views' own copies of title memory are let go a few at a time and their
  buffers reused (each new copy used to look at all 12,000 kept ones and make
  a buffer of its own; the pass over the heap took about 850 ms a frame, now
  about 170).
- A copy-back writes the GPU's bytes back under one hold of the write guards
  per stretch instead of one per run of bytes (1.3 million runs a frame):
  275 ms a frame down to about 100.
- **Write marks**: shaders mark the 64 KiB pages they write through views,
  and copies go back by the marked pages instead of comparing snapshots
  (`VICTORY_VIEW_MARKS=0` goes back; with relay and late waits off). Measured
  on the same street, the same build with and without: 0.42 against 0.53 s a
  frame, copy-back 53 against 101 ms, preparing 92 against 137 ms. After
  VIctory's own write marks (`cd94d35`, `940d32f`) and its open pull requests
  #2455, #2437, #2409 and #2474, with two guard fixes of
  [Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD).
- The pass over textures takes at most 50 ms a frame while the card is at
  its line (it could spend 750 ms making 1,500 textures in one frame when a
  little memory was let go), and does not look again at slots it just made.

**Ray tracing** (on by default; `VICTORY_RAYS=0` turns it off):

- The title's ray-tracing structure builder runs whole on the card without
  corrupting the title's memory, and the programs that shade ray hits are
  made. The cause of the crashes was the layer's: the builder dispatches by
  thread counts (`DispatchX` with `USE_THREAD_DIMENSIONS`), which were taken
  for group counts, so its stages ran up to 64 times the work and wrote empty
  nodes over the title's objects. Now the counts make groups and the threads
  past them leave (after victory-fork's pull request #646).
- Writes are declared only where shaders store by address; the address
  directory follows the buffers the title releases or replaces; list upload
  memory sits where shaders can reach it; only the copies a list names are kept
  current; node tests count an absent child or an unmapped page as no hit, and
  nothing hits while no structure is built; sun-shadow traces read structure
  addresses from a per-slot table. After victory-fork's pull request #994 (by
  PiloteCode) and [DexNom/VIctory](https://github.com/DexNom/VIctory), which
  first ran the title's ray tracing on an AMD card.
- While the builder runs, the constant stand-in for indirect light steps
  aside (`VICTORY_AMBIENT_WITH_RAYS=1` keeps it, to compare).

**Light**

- While the ray-traced light does not run, the output of the program that
  resolves the indirect diffuse light is replaced by a constant
  (`VICTORY_AMBIENT=<level>`, 0.6 by default; `off` keeps the program's
  output, which here is magenta on leaves and cloth and blocks of colour
  indoors). After [Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD).
- The constant has a colour (`VICTORY_AMBIENT_TINT=r,g,b`) and less of it
  reaches creases and the underside of things, worked out from the scene's
  depth (`VICTORY_AMBIENT_SHADE`, on by default; `=0` off). Also after Belobug.

**Translator**

- A pixel shader reads the other pixels of its 2x2 quad through quad reads,
  helper pixels included: wave reads give 0 from helper pixels on AMD, so a
  derivative a shader took by hand was wrong along every triangle's edge
  (lines on paint, glass and skin). After Belobug/VIctory-Belobug-AMD.
- Mesh primitive exports take 9-bit vertex indices (ported from upstream).
- The valid-mask bit of a pixel export decides which pixels survive.
- A heap buffer asked only for its size gets a non-uniform handle
  (victory-fork #452 by MalfoyDraco99, for a crash of AMD's compiler).
- Sixteen-bit float outputs of a mesh shader reach the pixel shader as zeros on
  this card, and the 32 KiB limit on mesh outputs is the validator's alone:
  both are measured in tests (`mesh_size_tests`).

**Diagnostics**

- `VICTORY_STALE_CHECK=1` (above; `fix` also copies what it finds) and
  `VICTORY_FULL_REFRESH=1` (every copy copied whole at each update).
- The frame statistics say what became of buffer views that did not fit their
  copy, how the texture queue stands, and what each bake found due.
- `VICTORY_BAKE_PROBE=1`: for the draws of the character bake, the state of
  every texture their data names, a second look at those some seconds later,
  whether materials sample the bake's own target, and a read-back of what the
  bake wrote (blocks and texels with colour, and a hash to compare launches).
- `VICTORY_RENDERDOC=1` loads RenderDoc before the device and captures a
  frame on request.

### Pull requests of victory-fork merged or applied here

Thanks to their authors. Each was read for what it adds, built, and run
through the test suites on the Radeon before it went in; #123 was taken back
out because its tests fail on this card.

#9, #25, #71 and #185 (Noname0202), #24 (NuraadGit), #32 (armin), #38
(spacede), #40 and #52 (jaschinski97-ops), #49 (fever308), #51 (pgm1207), #61
(srcamarda), #67 and #83 (ryotdev), #77 (m335671), #78 and #129
(vicennpacheco), #80, #119 and #506 (Adrito), #87 and #374 (davutsimsekk), #90
(António Mendes), #98 (sillynocodecat), #104 and #112 (avt0m4t), #106
(PastiiJay), #114 (Grégory Bonzon), #121 and #175 (Kamiloj9), #122 (Davi713),
#135 (alexdeliu), #154 (BitWuehler), #169 (HailLucifer666), #186 (Vyacheslav
Venglovsky), #191 (gdawid94), #196 (JaumeMarRib), #237 (fffluffy-dev), #282
(t4l03), #362 (renzowit35), #438 (sennecools), #452 (MalfoyDraco99), #571
(arminkhaleghirod), #575 (Cuebert), #606 (laptopnotebook), #632 (ryotdev).

## Running it

Requirements: Windows 10/11 x64, a CPU with AVX2, a Direct3D 12 GPU with shader
model 6.6 and mesh shaders, Rust (pinned in `rust-toolchain.toml`) with the
MSVC build tools, and several GB of free Windows commit.

```powershell
cargo build --profile diag -p victory --features diagnostics

$g = "<your game folder>"
$env:VICTORY_GAME_DIR  = $g
$env:VICTORY_CACHE_DIR = "<a folder for caches, outside the repository>"
$env:VICTORY_DATA_DIR  = "<a folder for saves, outside the repository>"

# Once after every change to the translator (5 to 15 minutes): translate the
# title's shaders and prebuild the pipelines earlier runs used. Use the same
# executable and the same switches as the run.
target\diag\victory.exe translate --cache --pipelines --used-pipelines `
    "$g\shaders.rpf" "$g\<the title's exe>" "$g\XboxBvhBuilder.dll" "$g\amd_fidelityfx_upscaler_xbox.dll"

# The switches the state above was measured with.
$env:VICTORY_INDIRECT_CLAMP = "0"   # the clamp of indirect arguments has dropped geometry here
$env:VICTORY_SKIP_MOVIES    = "1"
$env:VICTORY_RELAY = "0"; $env:VICTORY_WIDE_BACK = "0"
$env:VICTORY_CARD_BUFFERS_MIN_MB = "16"; $env:VICTORY_CARD_BUFFERS_MB = "512"
$env:VICTORY_LATE_WAIT_LISTS = "128"
# $env:VICTORY_RAYS = "0"            # without the title's ray-traced light (on by default)

target\diag\victory.exe run "$g\<the title's exe>" --execute
```

The last three lines of switches turn off victory-fork's newer speed defaults
(sending submissions ahead of the card). Measured on this card with this build,
they bring a frame from a median of 0.50 s down to 0.29 s (0.17 s at best),
and in the open world from 0.33 s to 0.144 to 0.150 s in steady views (about
7 frames a second), with no failure in a 17-minute session. Baked hair and skin
still come out wrong with them (too light now, black on an earlier build), so
they are off in the recommended settings until that is fixed.

Input: XInput pads; with none connected the keyboard is a pad while the
title's window has focus. Commands, the full list of switches and debugging
notes are in [docs/victory-fork-readme.md](docs/victory-fork-readme.md);
[docs/architecture.md](docs/architecture.md) describes the layers and
[docs/asset-policy.md](docs/asset-policy.md) what may never enter the
repository.

## Credits

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its
  contributors: the loader, the runtime services, the Direct3D 12 back end and
  the shader translator.
- **[victory-fork](https://github.com/fxceless/victory-fork)** by fxceless and
  the authors of its pull requests: the work that first drew the title's world,
  and most of what is merged here.
- **[myokex/victory-amd](https://github.com/myokex/victory-amd)**: fixes found
  on the same graphics card, some ported here.
- **[LilBigJP/ztrx-ps5-save-import](https://github.com/LilBigJP/ztrx-ps5-save-import)**:
  the code-patch option and the save converter used to test the open world.
- **[Belobug/VIctory-Belobug-AMD](https://github.com/Belobug/VIctory-Belobug-AMD)**:
  a fork of upstream VIctory on another AMD card; the quad reads of pixel
  shaders and the stand-in for indirect light are ported from it.
- **[DexNom/VIctory](https://github.com/DexNom/VIctory)** and victory-fork's
  pull request #994 by PiloteCode: the ray-tracing work ported here.
- **[XCL](https://github.com/xcl-dev/xcl)**: a different project with the same
  aim, whose public notes on the console's formats and on driver behaviour
  pointed at several problems. No code of it is used here.
- This fork's own changes were developed with
  [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later, as VIctory and victory-fork (see `LICENSE`). The history of
both is kept below this fork's commits.
