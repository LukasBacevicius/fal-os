# Assets: provenance and ops

## Provenance
- `src(import|gen|derive|render)` on a row (read-only) says how the asset's bytes came to exist.
- `role(input|candidate|wip|final|rejected)` says what the human means by the node (editable).
- `protected` nodes (imports by default) can't be deleted, overwritten (`asset`), moved to
  another parent or unprotected by agents: "input asset — derive instead". Make a new node
  with an op instead.
- The row ends with a lineage summary, e.g. `← op:reframe{9:16,pad-blur} of 881s9r by @motion`
  (`gen:<model>` for generations). Parents with a role other than input are prefixed
  (`mask:x1y2z3`).

## Rules
1. Never modify or delete protected inputs; derive.
2. Every derivative goes through an op (`transform` / `compose`), or through `import_file` with
   `lineage: {kind: "derive"|"render", op, recipe, parents}` (e.g. for a Bash/ffmpeg result
   or a Remotion render).
3. Variants are `candidate`; after the user picks one, set the others `role(rejected)`.
4. Finishing: mark `role(final)` → `export(nodes:"deliverables", presets)` → `inspect(probe)`
   the files against the spec (`get_knowledge ["export"]`).
5. Uploading user media to fal happens only when a fal op / `mediaInputs` needs it; say so the
   first time.

## Tools (the op is an argument)
- `list_ops()`: specs with arg schemas, engine, cost.
- `inspect(inputs, what[])`: `probe` (codec, fps, duration, color/HDR, alpha), `contact_sheet`
  and `palette` (returned as images), `loudness` (LUFS, true peak), `scenes` (cut times),
  `compare` (SSIM/PSNR of 2 inputs). Cached by sha. Read-only.
- `transform(op, inputs, args, placement?, name?, wait?)`: makes NEW nodes beside the inputs
  (placeholders at once, converted in place). `wait` (default true) returns the results.
- `compose(op: grid|contact_sheet, inputs|frame, args)`.
- `rerun(node, overrides?, on?)`: re-run a recipe with changed args, or apply it to other
  nodes. Generations re-run their fal model with `overrides` merged into the input.

## Inputs
Node ids, asset ids (`sha256:…`), `"selection"`, a frame id (= its media children: a batch,
one output each, laid out as a grid), or chips:
- `{"kind":"region","node","box":[x,y,w,h]}` (0..1): implicit crop first;
- `{"kind":"frame","node","t"}`: that frame as a still;
- `{"kind":"range","node","t0","t1"}`: trimmed first.

## Ops
| op | args |
|---|---|
| crop | `box:[x,y,w,h]` (0..1) / `px:[…]` / `aspect:"9:16"` (+`anchor`) |
| resize | `w`, `h`, `scale`, `fit: contain\|cover\|fill`; never upscales (`allow_upscale` or `upscale`) |
| reframe | `aspect` (or `w`,`h`), `mode: pad-blur\|crop-center\|pad-color\|outpaint` (outpaint = fal-ai/bria/expand, images), `color` |
| convert | `format`: png jpg webp avif gif / mp4 mov webm / m4a mp3 wav flac; `quality` |
| compress | `target_kb` or `target_mb`, `max_w` |
| trim | `start`, `end` or `duration` (s or mm:ss), `copy` |
| concat | inputs in order; `xfade` seconds; normalized to the first input |
| speed | `factor` |
| extract_frames | `times[]` / `count` / `every` → one image node per frame |
| gif | `fps`, `width`, `start`, `duration` |
| audio_normalize | `lufs` (-14), `tp`, `lra` (two-pass loudnorm) |
| audio_mix | inputs `[primary, bed]`; `bed_gain_db`, `duck` |
| audio_extract / strip | `format` / `what: audio\|metadata` |
| remove_bg | fal-ai/birefnet/v2 → transparent PNG |
| upscale | `factor`; fal-ai/seedvr (image and video) |
| inpaint | `prompt`, `mask` (annotation id) or `box`; fal-ai/flux-pro/v1/fill |
| ffmpeg | `args: ["-i","{in0}",…,"{out}"]`, `ext` — escape hatch, recorded as a recipe |
| grid / contact_sheet | `cols`, `cell`, `gap`, `bg` / `count`, `cols` |

Outputs: even dimensions, yuv420p (alpha kept in mov/webm), `+faststart`, bt709, 48 kHz AAC,
`comment=falos:<input sha>`. Empty or fully transparent outputs are refused.

## Generation with references
`generate` / `generate_batch` take `mediaInputs: {"image_url": "<node>", "mask_url": {chip}}`
(lists for `*_urls`): the host uploads local media to fal storage once (cached on the asset)
and records the inputs as lineage parents with roles (mask, start_frame, end_frame, audio,
style, reference).
