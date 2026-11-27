# Third-party ideas

Ideas taken from other projects, each rewritten here rather than copied, with where it came from.

## victory-scarlett (Op-lite), GPL-3.0

- `VICTORY_FILL_ONCE` (`crates/victory-gpu/src/host.rs`, `HostList::fill_scratch`): a root constant buffer
  copied to a list's scratch once per list and per range, not once per draw. Idea from victory-scarlett commit
  `c96525dbb2`, which credits Belobug (84ea9c12) and romainhedouin.
- `VICTORY_TABLE_MEMO` (`crates/victory-gpu/src/draw.rs`, `State::table_entry`): a table entry a list made already,
  with the same descriptor bytes, read the same way and nothing changed in the list since, is not made again; `=2`
  verifies. Idea from victory-scarlett commit `c45b43e96a`; written anew here.
- `VICTORY_SOFT_VIEWS` (`crates/victory-gpu/src/softview.rs`; hooks in `crates/victory-hle/src/modules/xmem.rs`
  `map_alias`, `crates/victory-gpu/src/texture.rs`): the views the title's page-mapping batch copies make are kept in
  the books and read through a lasting view of the whole section, a window, instead of a map and unmap call per
  64 KiB page; `=verify` checks the window's bytes against the mapped views. Idea from victory-scarlett branch
  `scarlett-fox`, `crates/victory-hle/src/modules/xmem.rs` (`mod soft`, `VICTORY_SOFT_VIEWS=copies`), read on
  2026-11-26: commit `bc264a4174` (author "scarlett-local", 2026-11-27); written anew here.

## JMAN730/project-flamingo, GPL-3.0

- `VICTORY_TEXTURE_BACK` (`crates/victory-gpu/src/texback.rs`, hooks in `HostList::texture_written` /
  `textures_back` and `read_backs`): textures shaders write that the title's CPU code reads (the ocean's height and
  displacement maps boats float on) are copied back into title memory, in the console's layout, once their list
  has run. Rewritten here after commit `cdc03a8fe32f` (fever308), which ports fxceless pull request 999 with that
  fork's later fixes (only the subresources a view writes, no unaddressable layouts, the texture's state kept).

## fxceless/victory-fork, GPL-3.0

- `crate::texture::alias` (`crates/victory-gpu/src/texture.rs`, `HostList::alias_owed` in `host.rs`): a view placed in the
  memory of a texture the host writes is copied from it once per list and source, even when the same list filled the view
  from title memory first. Idea from commit `19a45d1` (fork PR #1241); written anew here, with our own test
  (`resource.rs`, `a_placed_view_shows_what_the_host_wrote_after_an_upload_in_the_same_list`).

- `VICTORY_TEXTURE_BACK`, as above: the idea and the read watch that found ~30 title addresses reading those
  texture kinds come from pull request 999 by Geckomacabre (commits `895c0b8f7`, `04df43508`, merged as
  `f5c03f3b`). Rewritten here, not copied.

## rorodev6/victory-nvidia-megafork, GPL-3.0

- `VICTORY_PIPELINE_WAIT_ONCE` (`crates/victory-gpu/src/pipeline/make.rs`, `wait_once`): a pipeline the driver
  has not made gets one frame's budget of waiting over the whole run, not one budget in every frame a draw asks
  for it. Idea from commit `77e13a9` (trustno1-coder, for the defect of fxceless issue 1181); written anew here.
- `VICTORY_NAN_SCRUB` (`crates/victory-shader/src/emit/lower.rs`, `Lower::scrub_nan`): the colours pixel shaders
  write and float image stores are 0 where they are NaN or an infinity, so one bad value does not spread through
  the title's blur and temporal passes (black blobs on faces and in the air; the whole picture white at some angles
  with only NaN scrubbed). Ported from commit `77e13a9` (trustno1-coder, 2026-11-25), read on 2026-11-27; the test
  (`emit/nan_scrub_tests.rs`) is ours.
- `VICTORY_INDIRECT_REST` and the refused-record counter (`crates/victory-gpu/src/indirect.rs`, the clamp's
  `REST`; `crates/victory-gpu/src/host.rs`, `BUDGET_NOTES`): with `=0`, an indirect record that does not fit in what
  is left of its list's budget takes nothing, so the records after it that fit still run; the frame line says how
  many records a frame ran empty and the largest cost. Ported from commit `449830f` (2026-11-25), which credits
  Belobug (76387c2d); read on 2026-11-27. The list counter (`BUDGET_RUNS`) stays ours.
- `VICTORY_CONSTANT_MARKS` (`crates/victory-shader/src/emit/shader.rs`, `SLOT_STRIDE_CONSTANT`; `emit/lower.rs`,
  `Lower::slot_is_constant`; `crates/victory-gpu/src/descriptor/host.rs`, the table of strides by slot): a heap slot
  that holds a constant buffer view is marked in the host's table, and a program reads a slot it picks from the heap
  as a constant buffer only where that mark is (a texture's descriptor read as one gave a made-up address in the
  meshlet culling `_Prev` passes). Ported by way of commit `77e13a9`, which ports fxceless/victory-fork pull
  request 1216 (GamingRipperDoc); rewritten for this tree's table, which holds strides only. The test
  (`emit/constant_marks_tests.rs`) is ours.

## jamesn-xt/NZX-Recomp, GPL-3.0

- `TextureQueue::partly_less` (`crates/victory-gpu/src/descriptor/host.rs`, called from `draw.rs`): a texture uploaded in
  part whose memory goes in part keeps the smaller count, so its memory coming back is seen as more and it is uploaded
  again. Idea from commit `e10b1b5a1` ("Refresh partially evicted textures when backing returns"); written anew here.
