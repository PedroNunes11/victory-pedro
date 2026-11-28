# Vertex unified build (`vertex/unified`, built on `vertex/axx-best` + `vertex/glyphs`)

The unified branch that gathers the best of AnyXboxX, VIctory-NVIDIA (trustno1-coder), Vertex's own
work (`unify/all-forks`, `vertex/glyphs`, side branches), and community / PR fixes on top of the
AnyXboxX / Vice-Tay engine base.

Status (2026-11-28): everything below builds (`victory.exe` diag+diagnostics, `vertex-launcher.exe`
release, `victory_glyphs.dll`) and all unit test suites pass.
**Next step:** in-game A/B against the previous build (`56f395e1`), then branch/worktree cleanup.
No game files and no NVIDIA binaries are in the tree.

### Unified merges added on top of `vertex/glyphs` (`373689f7`)
| Area | Change | Source / Switch |
|---|---|---|
| Shaders (P0) | Restore AscilCH fixes (`v_div_fixup_f32` `0/x` & `x/Inf`, `wavePrefixOp.f32` signed flag, `v_cvt_{u16,i16}_f16` integer NaN check, MIMG 3D Z/a16 offsets), keep empty-EXEC branches that reactivate lanes, bump `TRANSLATOR_VERSION` to 4 | `76f874c7`, `97b1300e`, `b74b669e`, `194c7364` |
| Shaders (PRs) | `ds_sub_u32` LDS atomic fix, `v_mad_i64_i32`, `s_brev_b64`, pixel `SV_Coverage`, constant 64-bit compares, DXBC signature bounds, spill hybrid-root guard, validator null-blob / `ds_wrap` / `global_addtid` fixes | PRs `#628`, `#629`, `#630`, `#635`, `#638`, `#639`, `#657`, `#658` |
| GPU / Formats (P1) | Pipe/bank XOR swizzle addressing (PastiiJay, MIT) so ocean heightmap / `texback` copy-back tiles properly | `4ee6d153` |
| GPU / Shaders (P1) | Split >65,535-group 1D direct & indirect dispatches on non-AMD hosts (`USER_GROUP_BASE = 0x107`) while preserving AMD's wide path | `0abcd8b4` |
| GPU safety (P0/P1) | Fix AMD `VICTORY_DYNAMIC_SLOTS` regression on uncapped heaps; default `VICTORY_WC_NO_BACK_MIN_KB` to 1024; `held_elsewhere` shared-copy coherence under `WC_NO_BACK` (`VICTORY_WC_NO_BACK_SHARED`); atomic concurrent frame waits; retry failed title copies instead of zero-fill; `inplace.rs` aligned-span overflow, atomic budget, and late-wait mutual exclusion | `1104466a`, `36f033e0` (MIT fix), `59d53a22`, `dd202bc5`, `66f41e05` |
| GPU perf & ordering | `RECORD_LEAN` read-back buffer pool in `texback` (16/size, 128 MB cap; `VICTORY_TEXBACK_POOL=0` disables) + async read-back writer settle flush | `04b90185` |
| AnyXboxX PRs | Diagnostic switches excluded from shader cache key (`#301`), data-dir-inside-cache-dir guard (`#322`), per-core CPU & worker-pool overlay (`#455`) | AnyXboxX `#301`, `#322`, `#455` |
| Audio / HLE | Mono DSP convolutions so TVs, phone calls, and reverb are heard (`VICTORY_DSP_CONVOLVE=0` disables, `VICTORY_DSP_GAIN`); optional Bink movie skipper (`VICTORY_SKIP_BINK=1/all`, off) | VNV `eca45ac2`, `e31f4740` |
| Diagnostics / Launcher | `VICTORY_DLSS_CHECK=n`; poison-safe locks inside launcher `WH_KEYBOARD_LL` callback (`hotkey.rs`) | `5f8ff886` |

## Build

The user's working builds are made with the **stable** toolchain (rustc `0ef1bc38`), not the
repository's pinned 1.98.1. Build with `+stable` or every crate rebuilds and the binary differs.

```
set TMP=D:/dev/Victory/tmp
set CARGO_TARGET_DIR=D:/dev/unify/target-best
cargo +stable build --profile diag -p victory --features diagnostics      # engine -> target-best\diag\victory.exe
cargo +stable test  --profile diag -p victory-gpu -p victory-shader -p victory-hle -p victory-loader --lib

cd apps/vertex-launcher/src-tauri
set CARGO_TARGET_DIR=D:/dev/unify/target-launcher
cargo +stable build --release                                           # launcher -> target-launcher\release\vertex-launcher.exe
```

While the game runs, build at low priority (`axx-audit/best-of/lowbuild.cmd`: `start /belownormal`, 8 jobs).

Install: engine as `D:\dev\Vertex-AXX\bin\vertex-axx.exe`, launcher as `D:\dev\Vertex-AXX\Vertex-Launcher.exe`.
A running exe can be renamed but not overwritten: rename it into `bin\` as a backup, copy the new one in.
The launcher always starts `bin\vertex-axx.exe` (by name; other `.exe` files in `bin\` are backups).

## What is in it

### Vertex (this session)
| Change | Switch | Notes |
|---|---|---|
| 4K native: the title's render target heaps, pool and page array grow past the console's 2560x1440 render size | auto above 2560x1440 rendered; `VICTORY_RT_HEAP_SCALE` overrides | heap 3 (2199 MiB) and heap 1 grown, capped at 4094 MiB (32-bit allocator); RT pool 710 -> 1598 MiB with its static page array moved. Boots and renders at 4K DLAA; 2.5-6 fps on 12 GB (VRAM over budget) |
| Map cursor follows the mouse by distance at any frame rate; WASD pan the map | `VICTORY_MAP_CURSOR_SPEED` (0.6), `VICTORY_MAP_DEAD_ZONE` (0.24), `VICTORY_MAP_CURSOR_COUNTS` (1600) | speed and dead zone are estimates, to calibrate in one session |
| native-keys.txt: an axis action may carry a direction (`INPUT_FRONTEND_MAP_MOVE_UD-`) | | |
| Launcher: 4K Native mode (High Quality's settings at 3840x2160) | | |
| Launcher: engine is `bin\vertex-axx.exe` by name, never the newest file | | |
| Launcher: Play warms first when the mode **or the engine** changed since the last good warm | `<cache>\pipelines\warm-mode.txt` | |
| Launcher: 8 landing threads on 24+ logical CPUs | `VICTORY_LAND_THREADS=8` | AnyXboxX #469 measured 17.1 -> 22.0 fps |
| Dynamic descriptor slots on NVIDIA's capped heap | `VICTORY_DYNAMIC_SLOTS=393216` (launcher) | base dropped ~115 draws a frame (unify 5a291f4d) |
| SPREAD thread placement | `VICTORY_SPREAD` (off) | weak gain, conflicts with CACHE_CORES; A/B only |

### AnyXboxX (LooperSalty/AnyXboxX, merged upstream)
91919d3, #306, #321, #327, #295 (NVIDIA shader correctness; translator version 3), #443 (one texture
lookup per draw read), #506 (`VICTORY_TEXTURE_MAX_SIZE`), #320, #314, #305, #288 (table caches per
heap: stale textures / flicker).

### AnyXboxX open PRs (unmerged upstream at bebcfb1)
On: #457 (refused PSOs), #520 (lists left open: submit thread 1.4 -> 0.2 ms), #518, #514, #515, #554,
#527 (purple blotches), #526, #458, #480, #474, #549.
Off by default, for A/B: #512 `VICTORY_SETTLE_ROOM`, #511 `VICTORY_PAR_REFRESH`, #516
`VICTORY_THIN_SPLIT`, #521 `VICTORY_COPIED_AT_ONCE` / `VICTORY_TABLED_ORPHANS`.

### VIctory-NVIDIA (trustno1-coder / readmet3xt-lab / Indy2l / Yorivel)
| Commit | What | Switch (launcher value) |
|---|---|---|
| 50c15a1 + 2f81d0c | write-combined copies keep shader writes on the card (4070 SUPER: 38.3 -> 33.3 ms gameplay) | `VICTORY_WC_NO_BACK=1`, `VICTORY_WC_NO_BACK_MIN_KB=1024` |
| 7fa0e95 | pipelines with static samplers not loaded from the library (sky / grading banding) | always |
| 672442b | `VICTORY_LEAVE_OUT` in every build | `731a39171b568298` (heat haze: block smear, bad motion vectors for DLSS) |
| 35c1560 (make.rs part) | water's tiled light left out on 32-lane hosts: no dark blocks, +5 fps on water | `VICTORY_WATER_LIGHT=1` restores |
| 2203538 | card copies sized from the VRAM budget | |
| 3cb7b8f | exact texture refreshes read through the kernel first | `VICTORY_EXACT_NO_WAIT` |
| b3d49db | bake sources brought up to date first | |
| 9cb55c7, f12996c, fb838e8 | page-mapping copies; lazy views; soft views (no freezes at scene cuts) | `VICTORY_SOFT_VIEWS=copies` |
| 80c70b7 | submitting thread's helpers at its priority | `VICTORY_HELPER_PRIORITY` (on) |
| 8c1fa5d, 441446d, 30b979e | spread wake-ups, touched copies by list, address arena | |
| 21b040d | render-target reserve given back once targets settle (more room for textures) | `VICTORY_CARD_ROOM_FOR_TARGETS_SETTLE_MS` (20000) |
| 2d1dc38 | texture read-backs on a thread of their own | |
| 41b4a05 | pixel programs keep the console's early depth test (glass over heads, DLSS smear) | `VICTORY_EARLY_DEPTH=1` (off) |
| 2db59d4 chain (+f89b79d, d226814, bda6e59, 9b91d27, 9f6fb77) | reflections stand-in without rays | `VICTORY_AMBIENT_SPECULAR=<share>[,<sky>]` (off) |

Launcher also sets: `VICTORY_RIPE_BLOCKS=0` (768 MiB of VRAM back), `VICTORY_TEXTURE_STAGE_MS=16`,
`VICTORY_TEXTURE_CATCHUP=0`, `VICTORY_CARD_AWAKE=1`, `VICTORY_SYNC_FINE=1`.

### Community
- Whiplash floor / glossy-surface fix (Trexies, `floor-shimmer-fix.zip`, ported by hand): reflections
  held to half the light around them. Launcher: `VICTORY_REFLECT_SCREEN=1`, `VICTORY_AMBIENT_REFLECT=0.5`
  (needs `VICTORY_AMBIENT`). `VICTORY_AMBIENT_REFLECT_TURNS=<s>` flips it by turns to compare.

Every launcher switch above can be turned off in the launcher's extra switches (e.g. `VICTORY_WC_NO_BACK=0`).
The modes' texture budgets (room percent, spares, partial frames, LOD bias) are the user's, unchanged.

## What is missing

| Item | Why not yet | Next step |
|---|---|---|
| **In-game A/B** of this branch vs `56f395e1` | built while the user played | same scene, second run of the same exe (the driver caches pipelines per exe path) |
| **Lazy aliases** (Vertex `vertex/lazy-alias`) | no patch applies: old unify base; mapping, safecopy and watch.rs differ | add counters first (title-thread ms in copy_page_mappings, alias pages and Windows calls per frame); port only if > ~5 ms a frame |
| RECORD_LEAN read-back reuse (unify 04b90185) | hand port | ~3-4 ms a frame measured on unify |
| VNV 19720fe (submission batch kept) | 6 conflicts in the submit path AnyXboxX #518/#520/#554 reworked | ~2.4 % of the submit thread |
| VNV 082ebaf / 2983a71 (bake-source logging) | code differs on this base | diagnostics only |
| VNV a7b8667 (lists left open), aec85ef (raw view clamp) | duplicates of AnyXboxX #520 / #526, already in | none |
| VNV 826ba15 (violet probe filter) | overlaps AnyXboxX #527, already in | only if purple blotches remain |
| AnyXboxX #517 `VICTORY_EARLY_HEAPS` | no longer applies after the descriptor changes | re-port by hand if heap waits show |
| AnyXboxX #524 `VICTORY_WAVE_PAIRS`, #531 `VICTORY_THIN_FAST` | #524 experimental; #531 needs #519 first | wait for upstream |
| DLSS Frame Generation | Streamline integration (2.14.16 downloaded, D:/sdk/streamline-dl) not started; costs 0.5-2 GB VRAM | after VRAM pressure is solved |
| Ray-traced reflections (BVH builder) | builder left out; `-Reflections` blows out | VNV's stand-in (`VICTORY_AMBIENT_SPECULAR`) as the interim |
| Map cursor exact placement | the title only takes stick input; current tracker is distance-based | find the map cursor's position in memory for closed-loop control |
| **Keyboard key icons instead of controller icons** | built on `vertex/glyphs`: plugin DLL + engine hook, off by default | one identification session (`docs/GLYPHS.md`) |
| Ultrawide | `resolution.rs` MODES is a 16:9 whitelist | relax `parse_output`, add the launcher option |
| Pause-menu / settings mouse cursor | menus step by D-pad from mouse motion | the title's CURSOR actions have no bindings |
| `VICTORY_NAN_SCRUB` (rorodev6 megafork) | not in our tree | recommended by the floor fix's author for banding rings and eyes |

### Keyboard icons (button prompts)
The console build draws Xbox button glyphs; the title has no keyboard prompt path we know of (it only
registers a gamepad and a keyboard with GameInput, no mouse, no key-name strings). Two routes:
1. **Glyph texture swap** (practical): find the button glyph textures (font/scaleform atlas or texture
   dictionary) by hash when they upload, and replace each Xbox glyph with a key cap image of the key
   bound to that pad button in `native-keys.txt` (play-mode primary key). Needs: one capture session to
   find the glyph textures, a set of key cap images, a replacement table in the texture upload path.
   Context-dependent bindings (one pad button = several keys) show the primary key only.
2. **The title's own PC prompt code** (if any survives in the console binary): research, uncertain.

Route 1 is built on branch `vertex/glyphs`: the hook sits in the engine's texture upload path
(`VICTORY_GLYPHS_PLUGIN`, `VICTORY_GLYPHS_DUMP`), and the key caps come from a plugin DLL
(`plugins/glyphs`), as the user asked. See `docs/GLYPHS.md`.

## Research and tools
- Reports: `D:/dev/unify/axx-audit/best-of/anyxboxx.md`, `victory-nvidia.md`, `vertex-features.md`.
- Patches and apply checks: `best-of/patches`, `open-prs`, `vnv-patches`, `vertex-patches`.
- Cherry-pick helper `best-of/pick.sh`; low-priority builds `best-of/lowbuild.cmd`, `lowbuild-launcher.cmd`.
- 4K native and input background: `D:/dev/Vertex-AXX-Handoff/HANDOFF.md`, `controls-research/NOTES.md`.

## Incident 2026-11-28 00:41
The shared repository's `.git/config` and wt-best's index were found zero-filled (written at 00:41:32,
when a session ended). Objects and refs were intact (991 recent objects checked, fsck clean). The config
was rebuilt from the session's record of the remotes (push URLs `DISABLED` as before); the zeroed copy is
`.git/config.zeroed-2026-11-28`. The index was rebuilt from HEAD.
