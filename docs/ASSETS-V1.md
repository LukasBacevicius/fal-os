# Assets v1: bring it in, generate, take it out

> fal-os is the central piece of your creative work. Everything comes in (your files, references, fal library, Figma), agents generate and transform, and everything goes out to whatever app you need. Research: [atlas](research/atlas.md), [fal-assets](research/fal-assets.md), [asset-ops-export-design](research/asset-ops-export-design.md), [paper-brilliant-io](research/paper-brilliant-io.md).

## 1. Provenance: what's an input, what's generated
Two orthogonal fields:
- **`asset.source`** (immutable, how the bytes came to exist):
  - `import`: user file, paste, Figma, fal library upload
  - `generate`: a fal model or fal Agent
  - `derive`: a local or fal op applied to other assets (crop, trim, upscale…)
  - `render`: Remotion, HyperFrames, frame render, compose
- **`node.role`** (mutable, what the human means by it):
  - `input`, the default for imports
  - `candidate`, the default for generations
  - `wip`
  - `final`
  - `rejected`

Further rules:
- **Protection:** `node.protected` defaults to true for imports. Agents can't delete, overwrite or move protected nodes; they derive instead.
- **Lineage:** `{kind, op, recipe (canonical args, enough to re-run), parents:[{asset, node?, role: input|reference|mask|style|audio|start_frame|end_frame}], seed, fal_request_id, cost, actor, deterministic, engine_version}`.
- **Migration:** old `parents: [sha]` maps to role `input`; `model` maps to `op`.
- **Origin** (for imports): `{path?, url?, filename, app?, imported_by, at}`. **Media** (from probe): `{w, h, duration, fps, codec, alpha, color}`.
- **Text projection:** read-only `src(import|gen|derive|render)`, editable `role(final)`, and a lineage summary such as `← op:reframe{9:16} of 881s9r by @motion`.

## 2. Asset library (Atlas-grade, backed by fal.ai/assets)
**Host index:**
- **Storage:** SQLite at `~/.fal-os/library.db` (workspace-wide), keyed by sha256.
- **Fields:** `{sha, mime, media, source, origin, lineage, prompt, caption, tags, palette, fal: {assetRecordId, url, requestId, synced}, projects: [ids], favorite, created}`.
- **Placements are separate from assets:** one asset can be on many canvases or projects.

**Thumbnails:**
- **Sizes:** 256 and 1024 px WebP, plus a video poster (smart frame) and a 4×3 hover strip for videos.
- **Location:** generated on import or first sight into `~/.fal-os/thumbs/<sha>-<size>.webp`.
- **Repair:** a background job fixes missing thumbnails.
- **Originals** are never decoded in the grid.

**fal sync:**
- **Source of truth:** fal is the master for identity and metadata; local files are a cache.
- **Collections:** each project has a manual fal collection (nested under a "fal-os" parent) attached to the project's fal Agent project.
- **Imports:** pushed (upload, then `POST /v1/assets/uploads` with `Idempotency-Key = sha`), unless the asset is marked **local-only**.
- **Generations:** matched to their auto-indexed fal asset.
- **Pull:** an incremental pull of the user's fal library (newest first) plus a periodic reconcile.
- **Offline:** writes queue in an outbox.
- **Uploading a user input** is explicit and visible: a `↑ fal` badge, and the first upload in a project asks.

**Search:**
- metadata: prompt, caption, tags, name, model;
- fal semantic search (`q`, and `searchImageUrl` for "find similar");
- color: palette, with hex query;
- filters: source, role, type, model, date, project, favorite.
- Smart filters are rule JSON shared by the UI filter bar and agent tools.

**Library UI:** a left-panel tab (⌘⇧A) or full-screen view (⌘⇧L).
- **Grid:** a virtualized justified grid using the engine's tile renderer, with hysteresis bands and loads triggered by scroll distance.
- **System views:**
  - **This project:** inputs, generated, derived, finals;
  - **All:** across the workspace and the fal library;
  - **Favorites**, **Uploads**, **Generated**.
- **Collections, tags and favorites** are shared with fal.
- **Browsing:** hover plays video; Space opens a quick look; arrow keys navigate.
- **Placing:** drag onto the canvas, or `⏎`, places the asset (new placement, same asset).
- **Info panel:** prompt, model, lineage chain, palette, source and size.
- **Agent tools:** `search_assets`, `get_asset`, `place_asset`, `tag_assets`, `collect_assets`, `show_assets` (puts results in the user's library view).

## 3. Ops: Opus and everyone get real asset powers
A host op registry: one implementation for the UI context menu, the Opus swarm and external MCP agents.
- **How an op runs:** placeholder node beside the input → engine (ffmpeg / in-process Rust image / fal model / chrome-headless template) → output node with lineage → `turn`, `activity` and `cost` events.
- **Inputs:** nodes, `"selection"`, a frame id, or **chips**. A region chip is an implicit crop; a frame chip extracts that frame; a range chip trims; an annotation becomes a mask.

**MCP tools** (small surface; the op name is an argument; reference in knowledge `assets.md`, `export.md`):

| Tool | Purpose |
|---|---|
| `list_ops()` | specs with arg schemas |
| `inspect(sessionId, canvas, inputs, what[])` | probe · contact_sheet · palette · loudness · scenes · transcript · describe · compare. Cached by sha; returns MCP **image blocks** where visual |
| `transform(sessionId, canvas, op, inputs, args, placement?, name?)` | geometry · encoding · timing · audio · look · fal-backed (see below) |
| `compose(sessionId, canvas, op, inputs\|frame, args)` | grid / collage · contact_sheet · storyboard (PDF + PNG) · moodboard (frame render) |
| `rerun(sessionId, canvas, node, overrides?, on?)` | re-run with changes, or apply the recipe to other nodes |
| `export(sessionId, canvas, nodes\|frame\|"deliverables", presets[], dir?, naming?)` | → `{files:[{path, node, preset, w, h, bytes, sha256}], dir}` |
| `package(sessionId, canvas, kind: project\|deliverables)` | → `.zip` with a manifest |

**`transform` ops** (phase 1 = MUST):
- **Geometry:** `crop`, `resize`, `reframe` (crop-center / pad-blur / pad-color / outpaint via fal).
- **Encoding:** `convert`, `compress` (target size).
- **Timing:** `trim`, `concat` (normalize, xfade), `speed`, `extract_frames`, `gif`.
- **Audio:** `audio_normalize` (LUFS target), `audio_mix` (ducking), `audio_extract` / `strip`.
- **Captions:** `burn_captions` (from a transcript).
- **fal-backed:** `remove_bg`, `upscale`, `inpaint` (mask from an annotation or chip).
- **Escape hatch:** `ffmpeg` (raw args with `{in0}`/`{out}` placeholders, recorded as a recipe).

**Output defaults:** even dimensions, yuv420p, `+faststart`, bt709/sRGB, keep alpha, 48 kHz AAC, `comment=falos:<sha>`, never upscale silently.

**Other host changes:**
- `generate` and `generate_batch` accept **`mediaInputs: {param: node|chip}`**, uploaded host-side and cached in `uri`.
- `import_file` accepts lineage (`kind:derive, recipe, parents`) and the wider types: svg, pdf, glb/usdz, heic/avif/tiff/exr (proxy plus original), srt/vtt, cube → lut, fonts, md/txt.

## 4. Out: get it into any app
**Export:**
- **Presets** are data in `crates/host/presets/export.json`, with a version date:
  - social video: `reel`/`tiktok`/`short` (1080×1920), `ig-feed-45`, `square`;
  - `youtube-1080`, `youtube-4k`, `x`, `linkedin`;
  - masters: `master-prores`, `overlay-alpha`;
  - `web-image` (webp/avif/jpg/png at @1x/@2x/@3x, Paper's scale grammar `2x|1080p|512w`), `gif-web`;
  - audio: `wav`, `mp3`.
- **Aspect mismatch:** `reframe` creates a `role:final` node first.
- **Defaults:** naming `{project}_{name}_{preset}_{w}x{h}_v{n}.{ext}` with Downloads-style collision suffixes; destination `<project>/exports/<date>/`, with deliverables in `<project>/deliverables/`.
- **Integrity:** sha256 is verified after writing; refuse rather than write blank output.
- **Manifest:** every file is recorded in `exports/manifest.jsonl` as `{file, sha, asset, node, preset, recipe, ts, actor}`.
- **Per-node saved export settings** (the `export` prop): "Export all marked" with ⌘⇧E, and an Export panel (rows of preset × scale × format, naming, destination, progress, Reveal).
- **Chat event:** `{kind:"files", files:[…], dir}` renders as **file cards**: drag out, Reveal, Open in…, Copy.

**Desktop (Tauri):**
- **Drag-out of nodes and cards** to Finder and other apps:
  - `tauri-plugin-drag` (file URLs);
  - human-named **APFS clonefile** copies in `<project>/.out/` (**never hardlinks**);
  - triggered when the pointer leaves the window with the button down, or by a selection grip.
- **Copy:** image (PNG) and **file URLs** (objc2 `NSPasteboard writeObjects`), so ⌘V works in Finder, Slack, Premiere and Mail. Also Copy fal URL and Copy prompt.
- **Open in…:** apps from `NSWorkspace URLsForApplicationsToOpenURL`, plus pinned bundle ids (Premiere, After Effects, Resolve, Final Cut, Photoshop, Figma, Keynote), and **Reveal in Finder**.
- **Editors:** **OTIO** + **FCPXML** timeline export for `timeline` nodes and Remotion renders, so the motion agent emits OTIO next to `ad.mp4`.
- **Watch folder:** `<project>/inbox/` round-trips renders from other apps back in as `source:import`.
- **Fixes:** audit `project.duplicate` hardlinks, which should be clonefile.

## 5. UI provenance
- **Badges:** on nodes and library tiles, drawn by the engine, mono and minimal: `↓` import (plus a `↑` dot if uploaded to fal), `✦` generated, `ƒ` derived, `▶` render, `★` final.
- **Filter chips** in the layers panel and library: inputs / generated / derived / finals / rejected.
- **Show lineage:** dims everything outside the selected node's ancestors and descendants, with connectors on hover.
- **Properties → provenance:** the chain with `[rerun] [rerun with changes] [copy recipe] [apply to selection]`, and an "upstream changed" marker.
- **Deliverables frame:** dropping a node into it marks it `final`.

## 6. Agent rules
Prompts plus host enforcement:
1. Never modify or delete protected inputs; derive.
2. Every derivative goes through an op, or through `import_file` with lineage.
3. Variants are `candidate`; after a pick, the losers become `rejected` (preference data for memory).
4. Finishing a piece: mark `final` → `export(deliverables, presets)` → `inspect(probe)` against the spec → a `files` event with cards.
5. Upload user inputs to fal only when needed, and say so on the first upload.
6. Use library search before generating something that may already exist; reuse the user's references and characters (fal entities).

## 7. Contracts between work streams (build swarm)
- **Host REST, for the UI** (token auth, `?project=<id>`, actor = the human), mirroring the MCP tools without a sessionId:
  - `GET /api/ops` — list ops;
  - `POST /api/inspect {inputs, what}`;
  - `POST /api/transform {op, inputs, args, placement?, name?}`;
  - `POST /api/compose {op, inputs|frame, args}`;
  - `POST /api/rerun {node, overrides?, on?}`;
  - `POST /api/export {nodes|frame|"deliverables", presets, dir?, naming?}` → `{files, dir}`;
  - `POST /api/package {kind}`;
  - `GET /api/presets`.

  Long ops return immediately with placeholder node ids; progress arrives as `job` / `batch` events on `/sync`.
- **Library over `/workspace` WS:**
  - client → server: `{"type":"library.query","id","query":{text?, similar_to?:sha|node, color?:"#hex", source?:[], role?:[], kind?:[], project?:id|"*", favorite?:bool, collection?:id, sort?, cursor?, limit?}}`;
  - server → client: `{"type":"library.results","id","items":[{sha, kind, mime, w, h, duration, source, role?, prompt?, model?, caption?, palette?, favorite, fal?:{assetRecordId,url,synced}, projects, thumb:"/thumbs/<sha>-256.webp", thumb_lg:"/thumbs/<sha>-1024.webp", created}],"next"}`;
  - `{"type":"library.changed","shas":[…]}` on any change;
  - client → server writes: `library.tag {shas, add?, remove?}`, `library.favorite {shas, value}`, `library.collect {shas, collection}`, `library.place {sha, project, x?, y?}` (creates a node), `library.upload {shas}` (push to fal), `library.local_only {shas, value}`.
- **Thumbnails:** `GET /thumbs/<sha>-<256|1024>.webp` (token), generated on demand if missing.
- **Library MCP tools:** `search_assets(sessionId, query)`, `get_asset(sessionId, sha|node)`, `place_asset(sessionId, canvas, sha, placement?)`, `tag_assets`, `collect_assets`, `show_assets(sessionId, shas)` (emits `{"type":"library.show","shas"}` to clients).
- **Chat event `files`:** `{kind:"files", files:[{path, name, node?, preset?, w?, h?, bytes, sha256}], dir}`.
- **Node props** (doc): `role`, `protected`, `export` (array of `{preset|format, scale, suffix}`). Asset fields: `source`, `origin`, `media`, `lineage` (new shape). The text projection shows `src()`, `role()` and the lineage summary.
- **Desktop commands** (Tauri invoke): `drag_out(paths[], icon?)`, `copy_files(paths[])`, `copy_image(path)`, `open_with_list(path) → [{name, bundle_id, path}]`, `open_with(path, app_path|bundle_id)`, `reveal(path)`, `materialize(sha|node, name?) → path` (clonefile into `<project>/.out/`). The host also exposes `POST /api/materialize {node|sha, name?}` → `{path}` for the web build.
