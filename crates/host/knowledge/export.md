# Export: getting work out

`transform`/`compose` make nodes; `export` makes files.

`export(nodes | frame | "deliverables", presets[], dir?, naming?, reframe?)` →
`{files: [{path, name, node, preset, w, h, bytes, sha256}], dir, warnings}`.

- **Targets:** node ids, a frame id (its media), or `"deliverables"` = nodes with
  `role(final)` or inside a frame named "Deliverables".
- **Presets** (data, versioned; `GET /api/presets`):
  - video: `reel`, `tiktok`, `short` (1080×1920), `ig-feed-45` (1080×1350), `square`,
    `youtube-1080`, `youtube-4k`, `x`, `linkedin` (h264, platform bitrates, -14 LUFS);
  - masters: `master-prores` (ProRes 422 HQ), `overlay-alpha` (ProRes 4444);
  - `gif-web`; images: `web-image` (webp default; avif, jpg, png); audio: `wav`, `mp3`;
  - `original`: the bytes as-is.
- **Rows:** a preset id or `{preset|format, scale, suffix, quality}`; image scale grammar
  `2x | 0.5x | 1080p | 512w | 512h` (1x = the node's size on the canvas), e.g.
  `{"preset":"web-image","format":"avif","scale":"2x","suffix":"@2x"}`.
- **Saved settings:** a node's `export([...])` prop is used when `presets` is omitted (else
  `original`).
- **Aspect mismatch:** a video whose aspect differs from the preset is reframed first
  (`reframe`: pad-blur default, crop-center, pad-color) into a new `role(final)` node, which
  is exported.
- **Never upscales:** smaller sources export at source size, with a warning.
- **Names:** `{project}_{name}_{preset}_{w}x{h}_v{n}.{ext}`; other tokens `{index} {date}
  {node} {scale} {suffix}`. Existing names get ` (1)`, ` (2)` like ~/Downloads.
- **Where:** `<project>/exports/<date>/`; deliverables in `<project>/deliverables/`; `dir`
  (absolute or project-relative) overrides.
- **Integrity:** every file is re-read and its sha256 verified; blank output is refused;
  each file is logged in `<project>/exports/manifest.jsonl` (`{file, sha, asset, node, preset,
  recipe, ts, actor}`).
- In a chat, the export shows as file cards (a `files` event).

Finishing a piece: `role(final)` → `export(nodes:"deliverables", presets)` → `inspect` the
files (`probe`: size, duration, codec, loudness) against the spec → tell the user where they
are.

`package(kind: project|deliverables)` zips the project (canvases + referenced assets) or the
exported deliverables with `manifest.json` (nodes, lineage DAG, recipes, models, costs).
