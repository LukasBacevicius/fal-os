# Asset ops, export targets, provenance: design

From a design agent that read the current code (2026-10-11).

**Core idea:** one **op registry in the host**. The UI context menu, the Opus swarm and external MCP agents all call the same ops. Every op writes a **new canvas node with a structured, re-runnable recipe**. Export is a separate step that writes files plus a manifest. The desktop shell adds drag-out, clipboard and "Open in…".

## Where we are
- **Model:** `Asset{id, mime, uri, lineage}`, where `Lineage{model, params, seed, parents: Vec<String>, fal_request_id, cost, actor}` is generation-shaped: no source, no parent roles, no recipe.
- **Import:** `import_file` covers only 13 extensions.
- **Not built:** `render_viewport`, `export` and `search_assets`, all listed in ARCHITECTURE.
- **Opus today** does asset work through Bash + ffmpeg + `import_file`, with no consistent lineage and nothing re-runnable.
- **Desktop shell:** Tauri with only `host_info`: no opener, clipboard or drag plugins. Nothing goes out.
- **Tools on the machine:** ffmpeg, ffprobe, `sips`, chrome-headless (via Remotion/HyperFrames), `motion-qa.mjs`.

## 1. Ops
**Architecture:**
- **Registry:** `crates/host/src/ops/` holds `OpSpec{name, inputs, args schema, engine, output_kind, deterministic, cost_hint}`.
- **Run flow:** a placeholder node appears next to the input (`find_space`) → the engine runs (ffmpeg `-progress` shown on the placeholder) → the output is imported in place with structured lineage.
- **Engines:**
  - ffmpeg for audio and video;
  - in-process Rust for images (`image`, `fast_image_resize`, webp/avif encoders, `resvg`, k-means);
  - fal models via the host's queue path, so external agents never need FAL_KEY;
  - chrome-headless HTML templates for layout and PDF, run by the runtime.
- **Escape hatch:** `op:ffmpeg {inputs, args with {in0}/{out}}` is recorded as a re-runnable recipe. `import_file` also accepts `lineage.kind:derive + recipe + parents`, so Bash output can declare where it came from.
- **Chips as inputs:**
  - a region becomes an implicit crop;
  - a frame becomes a still;
  - a range becomes a trim;
  - an annotation becomes a mask.
- **A small MCP surface** (`op` is an argument, not a separate tool):
  - `inspect`, `transform`, `compose`, `rerun`, `export`, `package`, `open_with`/`reveal`, `list_ops`;
  - the reference lives in knowledge docs `assets.md` and `export.md`.
  - Results return MCP image blocks.

**Inspect** (cached by sha in `.cache/inspect/`):
- `probe`: codec, fps, duration, color/HDR, EXIF, alpha.
- `describe`: caption, tags, OCR.
- `contact_sheet`: ffmpeg `tile` with timecodes.
- `transcript`: fal ASR, producing SRT/VTT.
- `scenes`: scene detection.
- `loudness`: ebur128.
- `beats`: from motion-qa.
- `palette`: k-means → `style:palette` node.
- `compare`: SSIM/PSNR plus a diff.

**Transform:**
- **Geometry:**
  - `crop`, `resize` (Lanczos);
  - `pad`/`reframe` (crop-center / pad-color / pad-blur / outpaint via a fal expand model / smart subject-tracking later);
  - `outpaint_canvas`.
- **Encoding:**
  - `convert`: formats, color, HDR→SDR;
  - `compress`: a target size.
- **Timing:**
  - `trim` (accurate or copy), `concat` (normalized, with xfade);
  - `speed` (setpts/atempo, or ML interpolation);
  - `extract_frames`, `gif` (palettegen).
- **Audio:** `audio_normalize` (two-pass loudnorm), `audio_mix` (amix + ducking), extract/replace/strip.
- **Look and captions:** `burn_captions` (ASS), `grade` (LUT/eq/match), stabilize/denoise.
- **fal-backed:** `remove_bg`, `upscale` (image and video), `vectorize`, `inpaint` (annotation mask). Verify model ids with `fal_model_schema` before wiring.
- **Output defaults:** even dimensions, yuv420p, `+faststart`, bt709/sRGB tags, keep alpha, 48 kHz AAC, never upscale silently, and `comment=falos:<sha>` written into the file.

**Compose:**
- grid/collage (Rust compositor), contact_sheet;
- storyboard (HTML → PDF + PNGs, via chrome-headless);
- moodboard (render a frame);
- deck (pptxgenjs);
- timeline_render (ffmpeg or Remotion).

**Batch and recipes:**
- ops accept `inputs: selection | frame | ids`, with one placeholder per input in a grid;
- `rerun(node, overrides?, on?)`: re-run with changes, or apply a recipe to other nodes;
- named recipes saved to `.fal-os/recipes`;
- `propagate(node)`: re-derive stale descendants; only deterministic ops re-run automatically.

**Generation with reference:**
- host `generate`/`generate_batch` gain `mediaInputs` (node or chip) with host-side upload and uri caching;
- parents record a role (input/reference/mask/style/audio/start_frame/end_frame);
- an annotation becomes an inpaint mask.

## 2. Export and "out"
**Rule:** transform/compose create **nodes**; export creates **files**. Every file is logged in `<project>/exports/manifest.jsonl` as `{file, sha, asset, node, preset, recipe, ts, actor}`.

**Presets** (data in `presets/export.json`):

| Preset | What it means |
|---|---|
| reel / tiktok / short | 1080×1920 h264, around 10–16 Mbps, -14 LUFS, safe-zone check |
| ig-feed-45 / square | 1080×1350 / 1080×1080 |
| youtube-1080 / youtube-4k | 16:9 h264 at the platform's bitrates |
| x / linkedin | 1920×1080 or 1080×1920 h264 |
| master-prores | ProRes 422 HQ |
| overlay-alpha | ProRes 4444 / VP9 with alpha |
| web-image | webp / avif / jpg / png at @1x/@2x/@3x |
| gif-web | small looping GIF |
| audio | wav 48k/24-bit, mp3 320 |

When the aspect ratio doesn't match, `reframe` (pad-blur by default) creates a `role:final` node first. Platform limits drift, so keep presets as data with a version date.

**Surfaces:**
- **Agent tool:** `export(nodes|frame|"deliverables", presets[], dir?, naming?, reveal?)`.
- **Naming tokens:** `{project}_{name}_{preset}_{w}x{h}_v{version}_{index:03}.{ext}`.
- **Folders:** default `<project>/exports/<date>/`; deliverables go to `<project>/deliverables/`.
- **Saved settings:** Figma-style per-node `export([...])` presets, with ⌘⇧E for "Export all".
- **Export panel:** rows of preset × scale × format, naming, destination, progress, then Reveal.
- **Chat event:** `{kind:"files", files, dir, actions:[reveal, open_with, copy, drag]}` renders as file cards.
- **Reveal and open:** `tauri-plugin-opener`.

**Rendering frames:**
- Phase 1: the wasm engine renders offscreen in the client, and the host relays requests over `/sync`.
- Phase 2: a headless native wgpu engine in the host, which also provides `render_viewport`.
- PDF: one page per frame (`pdf-writer`).

**Drag-out (Tauri/WKWebView):**
- Use `tauri-plugin-drag` (drag-rs). It works with file URLs only, not file promises.
- Trigger it from the pointer leaving the window with the button down, a selection grip, or chat cards.
- Materialize human-named copies with **APFS `clonefile`** into `.out/`.
- **Never hand out hardlinks:** the receiving app could edit the file and corrupt the content-addressed store. Audit `project.duplicate`, which uses hardlinks.
- Lazy renders: render first, then drag. Phase 3: an objc2 `NSFilePromiseProvider`, for Finder only.
- Web build: `<a download>`, or Chrome's `DownloadURL`.

**Clipboard:**
- PNG via `tauri-plugin-clipboard-manager`.
- File URLs via a small objc2 command (`NSPasteboard writeObjects`).
- Also: Copy fal URL, Copy as SVG (Figma pastes it as layers), Copy prompt/recipe.

**Open in…:**
- Use the `NSWorkspace URLsForApplicationsToOpenURL` list, plus pinned bundle ids: Final Cut, Resolve, Premiere, After Effects, Photoshop, Figma, Keynote.
- **Editable timelines:**
  - **OTIO** writer in native Rust (imports into Resolve 18.5+ and Premiere 25.6+; possibly still beta there);
  - **FCPXML** (`.fcpxmld`) for Final Cut and Resolve;
  - FCP7 XML / CMX3600 EDL via OTIO adapters.
  - The motion agent emits OTIO through Remotion's opentimeline skill, so "open in Premiere" gives an editable cut.
- Layered PSD (ag-psd) and pptx come later.

**Share, package, fal:**
- **Share link:** the fal CDN `uri`. Uploading a **user input** must be explicit and visible (an "uploaded to fal" badge).
- **`.falos.zip` package:** project, canvas snapshot, referenced assets, a manifest (node → files, lineage DAG, recipes, models, costs, licenses), optional chats, and an offline viewer.
- **Deliverables package:** exports, contact sheet, credits/provenance sheet, CSV.
- **fal library:** project ↔ collection, characters ↔ entities, "Save to fal", import from the library and request history.

**Bring-in gaps:**
- Widen import to svg, pdf, glb/usdz, heic/avif/tiff/exr (proxy plus original), srt/vtt, .cube → `lut`, fonts, md/txt.
- A **watch folder** `<project>/inbox/`: the cheapest round-trip with Premiere and other apps.

## 3. Provenance
```rust
Asset { id, mime, uri,
  source: Import | Generate | Derive | Render | Capture,       // immutable: how these bytes came to exist
  origin: Option<Origin{path,url,filename,app,imported_by,at}>,
  media: Option<Media{w,h,duration,fps,codec,alpha,color}>,
  lineage: Option<Lineage> }
Lineage { kind: generate|derive|render, op, recipe: Json, parents: Vec<Parent{asset,node?,role}>,
  seed, fal_request_id, cost, actor, deterministic, engine_version }
```
- **Migration:** old `parents: Vec<String>` becomes `role: input`; `model` becomes `op`.
- **Node intent** (mutable) is `role: input | wip | candidate | final | rejected` plus `protected` (defaults to true for imports). **Source** says where bytes came from; **role** says what the human means by them.
- **Text format:** read-only `src(...)`, editable `role(...)`, and a lineage summary such as `← op:reframe{9:16,pad-blur} of 881s9r by @motion`.
- **UI:**
  - **Badges:** ↓ import (with a local-only / uploaded dot), ✦ generated, ƒ derived, ▶ render, ★ final.
  - **Filters:** Inputs / Generated / Derived / Finals / Rejected.
  - **"Show lineage":** dims everything outside the selected node's ancestors and descendants, with connectors on hover.
  - **Provenance tab:** a chain with Re-run, Re-run with changes, Copy recipe, Apply to selection, and an "upstream changed" badge.
  - **Deliverables frame:** dropping a node there sets `role:final`.
- **Agent rules:**
  1. Never modify or delete inputs. The host rejects delete/move on `protected` nodes from agents.
  2. Every derivative goes through an op or through `import_file` with lineage.
  3. Finals are marked `role:final` → `export(deliverables)` → probe against the spec → a `files` event.
  4. Variants are candidates; losers become rejected (preference data).
  5. Uploading user inputs to fal is logged and shown.

## 4. Phases
- **Phase 0 (foundations):**
  - provenance model plus migration;
  - the ops registry and runner;
  - `inspect` (probe, contact_sheet, palette, loudness) with image blocks;
  - `op:ffmpeg`, wider import types;
  - knowledge docs and prompt rules.
- **Phase 1 (generate → usable file in another app):**
  - core ops (crop / resize / reframe / convert / compress / trim / concat / extract_frames / gif / audio_normalize / burn_captions / transcript / remove_bg / upscale) with batch;
  - export presets, naming, manifest and Reveal; the Export panel; `files` chat cards;
  - drag-out, clipboard, Open With;
  - `generate` with `mediaInputs`.
- **Phase 2:**
  - compose (grid, storyboard PDF, moodboard);
  - rerun, apply recipe, propagate;
  - OTIO and FCPXML;
  - packages;
  - fal library push and import;
  - provenance UI;
  - `search_assets`.
- **Phase 3:**
  - headless native render;
  - file promises;
  - Copy as SVG, PSD, PPTX;
  - smart reframe;
  - LUT and grade-match;
  - review share page;
  - watch folder.

## Risks
- **Drag-out:**
  - drag-rs needs a live mouse event, so trigger it synchronously and prepare the file paths ahead of time.
  - It works with file URLs only, so anything unrendered needs a render first.
  - Pointer capture by the wasm canvas may conflict with WKWebView's drag tracking; prototype this first.
  - File promises have a timing race with some destinations.
- **Clipboard:** the Tauri plugin has no file URLs, so that needs objc2.
- **Bundling ffmpeg:** if bundled, use an LGPL build with VideoToolbox encoders and set `-b:v` explicitly.
- **Storage growth:** derived assets multiply fast; needs garbage collection of unreferenced assets, and clones for exports.
- **Color:** HDR and P3 phone footage needs tone-mapping and tagging.
- **Spec drift:** platform limits change, so keep presets as data.
- **Interchange limits:** OTIO and FCPXML don't carry Remotion effects, so those get baked as overlays.
- **Tool sprawl:** keep ops behind `transform`/`compose`.
- **Cost and privacy:** fal-backed ops need cost events, the upload badge and the circuit breaker.
