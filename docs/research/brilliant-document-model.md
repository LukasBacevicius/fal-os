# Brilliant teardown — document model, persistence, sync, motion

Sources: Dart AOT strings (941 `package:brilliant/` URIs), Rust dylib strings, bundled `motion/*.mjs`, knowledge docs via read-only MCP. **[I]** = inference.

## Architecture
- Flutter/Dart: UI, document model, sync, AI, MCP server. **No riverpod/bloc, no database** — Manager/Controller/Store classes on ChangeNotifier; ~300 `*Command` classes with a VS Code-style keybinding/`when` registry.
- Rust wgpu engine behind `bn_*_v1` delta FFI (see `brilliant-renderer.md`); also parses Blueprint and evaluates motion natively.
- Module areas: managers (193), mcp_tools (40), mcp_connections (7), state + dsl, realtime (19), platform/cloud (103), providers (54: Anthropic/OpenAI/Google HTTP + claude/codex/cursor CLI + ACP), native, import (Figma Kiwi clipboard, SVG), export (HTML/React/SVG/PDF/video).

## The core idea: the document *is* text
`.bl` canvas file:
```
bp:v1
canvas: { background, guides, ds binding, zoom/pan }
---
0bdf0b86f6e9178a al(v,x(c),g(30)) p(0,1100) s(1080,hug:172) f[] "Details" #details
  <children indented 2 spaces>
```
- One element per line, stable 16-hex ids; **paints have ids too**.
- Stores intent *and* resolved value (`hug:172`, `$token` + value) → agents/exporters never re-run layout.
- Deterministic, byte-stable writes → clean git diffs. Partial-load tolerant (bad lines dropped, unknown props preserved).
- The same row format is: file on disk, agent read/write API, clipboard, diff unit, 3-way merge unit, realtime unit ("the room stores rows only").
- Parsers exist in Dart, Rust and JS.

Node types: `fr` frame (top-level = artboard = motion state), `gr`, `al()` auto-layout, `r`, `c`, `t()`, `v()`, `svg(icon:)`, `mask`, `bool()`, `comp`/`inst()` with variants, overrides, slots, cross-canvas and library instances. **Images are paints, not nodes. No video node.**

Project = folder, no index: `*.bl`, `Styles/*.ds` (token language with modes, generators; comments written as prompts for AI), `Motion/*.bm`, `Assets/`, `.brilliant/` metadata, `libraries.yaml`.

## Persistence & undo
Debounced atomic background auto-save; identical-write skip; plausibility check refuses destructive saves; saving blocked after failed load; quit flush; crash sentinels; FSEvents watcher for external edits; agent shell writes to `.bl` validated via a Claude Code PostToolUse hook. Undo per canvas, named session-scoped checkpoints, "undo all changes from this chat message", undo inverses rebased over concurrent edits.

## Sync — no CRDT
- **File lane:** git-like store on `api.brilliant.design` (blobs by sha, tip, commits), element-wise 3-way merge, loser kept as checkpoint.
- **Live lane:** `wss://rooms.brilliant.design` — seed/basis snapshot, `seq`/`baseSeq` server-sequenced ops, 409 rebase, local op journal for offline ("LOCAL WINS"), presence ≤25 cursors.
- "Cutover" hands a file between lanes — a complexity tax we should avoid.
- Auth: Google OAuth PKCE (loopback `127.0.0.1:19876`), keychain.

## Motion
`.bm` films: list of states (top-level frames) + timing windows (`hold`, `to(#s02) sec(0.6) spring(...)`), per-element enter/stagger, camera, behaviors, sfx. Elements pair across states by name. Evaluated natively in Rust → preview == mp4 export. `remotion.rs` imports Remotion comps.
`Resources/motion/*.mjs` is **not** run in-app: it's a node+ffmpeg CLI runner for agents (procedural `frame(t) → patch lines`, rhythm audit, preflight lint, contact sheets, LUFS sound mix) that drives the app over MCP.

## Lessons for fal-os
1. A line-oriented, id-keyed text projection of the canvas is an outstanding **agent interface** (and diff/merge unit). Give ids to sub-objects (generation params, keyframes).
2. Store intent + resolved values side by side.
3. Folder = project, content-addressed assets, defensive persistence.
4. Server-sequenced ops + local journal works without a CRDT — but don't run two sync lanes.
5. Agents need first-class sessions: refs, checkpoints, cursor, attribution, idempotency, undo rebase.
6. Stream edits onto the canvas while the model generates; jobs appear as placeholders immediately.
7. Motion = states + timing, deterministic evaluation; give agents measurable QA tools.
8. **Diverge:** fal-os is media-first — image, video, audio, 3D must be first-class nodes, with generation lineage.
