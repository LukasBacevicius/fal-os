# fal-os — architecture proposal (v0)

> Status: draft for discussion, 2026-10-10. Built on the Brilliant teardown and options research in [`research/`](research/).

## Product in one paragraph
fal-os is the hub for creative work. A **project** holds one or more **infinite canvases** of media — images, video, audio, 3D, text, references — along with each item's generation lineage. Humans and **many agents at once** work on the same canvas: fal's own agents, the user's Claude Code, Codex, Gemini or Cursor, and internal agents that drive Premiere, Resolve, Blender and other apps via scripting bridges or computer use. Desktop app first, with a web build of the same canvas that doubles as the website.

## What we take from Brilliant, and where we diverge
| Brilliant does | We do |
|---|---|
| Native Rust/wgpu engine behind a narrow, versioned delta protocol; UI toolkit draws only chrome | **Same principle.** Engine is a separate package behind a binary delta API. v0 is TypeScript + WebGPU (our canvas is mostly textured quads and video, not vector editing); Rust/wasm is possible later behind the same protocol |
| Engine owns the camera; pan/zoom only updates a uniform; budgets everywhere; image LRU; progressive open | **Copy all of it** — essential for thousands of generated assets |
| Document is id-keyed text rows (file = API = diff = merge unit) | Source of truth is a **Loro CRDT doc**. A **line-oriented text projection** of it is the agent read/write format (Brilliant's best idea, kept) |
| Images are paints; no video node | **Media-first nodes**: image, video, audio, 3D, text, note, frame, group, connector, **generation (live job)** |
| One MCP server, `edit` + `read` in one format, knowledge on demand, session ids, agent cursors, staleness reports, per-agent undo, idempotency, spend breakers, untrusted-content taint | **Copy**, and add what they lack: auth token on localhost, **async jobs (MCP Tasks)** for generations, MCP annotations, claims, proposal/review mode |
| Drives Claude CLI (stream-json), Codex app-server, Cursor ACP, HTTP providers | Same multi-door idea, but standardized: **ACP client + Codex app-server**, plus our own fal agent; no home-grown coding harness |
| No computer use | **App Bridges + computer-use fallback**, and fal-os itself is built to be easy for agents to drive |
| Two sync lanes (file git-store + live rooms) with cutover | **One authority**: the Canvas Host (local, or as a cloud room); files and snapshots are projections |

## System diagram
```
┌──────────────────────────── fal cloud ────────────────────────────┐
│ Model APIs (queue/realtime) · Assets/Collections · Canvas rooms   │
│ (same Host code) · Remote MCP (OAuth) · fal agent runtime         │
└──────────────▲──────────────────────────────▲─────────────────────┘
               │ Loro sync                    │ fal APIs
┌──────────────┴──────── fal-os desktop (Electron) ─────────────────┐
│ Renderer process (shared with web)                                │
│   React UI · WebGPU canvas engine (worker, OffscreenCanvas)       │
│   agent cursors/presence · proposal layer · comments · a11y mirror│
│ Host (Node utility process; same code runs in the cloud)          │
│   Canvas Host: Loro doc · Command API (tx, actor, claims,         │
│     expectedVersion) · per-actor undo · permissions/budgets · bus │
│   Adapters: MCP (localhost Streamable HTTP + stdio shim) ·        │
│     CLI socket · text projection (read/edit) · render/export      │
│   Agent Supervisor: fal agent · ACP client · Codex app-server     │
│   App Bridges: UXP · ExtendScript · Resolve py · Blender · CU     │
│   Media: content-addressed store, ffmpeg thumbs/proxies/waveforms │
└──────────────▲────────────────────────────────────────────────────┘
               │ MCP / fal-os CLI + SKILL.md
     User's own agents: Claude Code · Codex · Cursor · Gemini · …
```

## Key decisions (proposed)
1. **Shell: Electron.** Bundled Chromium gives consistent WebGPU, WebCodecs and codecs on every OS, and the same runtime as the web build. Tauri's WKWebView only has WebGPU on macOS 26+, and its Linux codec support is weak.
2. **Language: TypeScript monorepo** (pnpm + turbo). One language across engine, host, MCP (official SDK), CLI and web. Rust only when profiling demands it, behind the engine protocol.
3. **Canvas engine: custom WebGPU** in a worker with OffscreenCanvas, plus a WebGL2 fallback later.
   - Instanced quads, SDF shapes, mipmapped/virtual textures picked by zoom level.
   - Video via WebCodecs `VideoFrame` → `importExternalTexture`; cap active decoders; poster frames when idle.
   - Brilliant's performance list as the checklist: uniform-only camera, retained frame, idle skip, budgets, LRU, progressive open, half-res during gestures, a kill switch per optimization.
   - Text and chrome start as a DOM overlay.
4. **Data: Loro doc per canvas.** It gives us a movable tree for z-order, per-peer undo, version checkout and a Rust core for later. Media is content-addressed (local cache + fal CDN); the doc holds only references, metadata and lineage. One Canvas Host is the authority, locally or as a cloud room.
5. **Agents are actors.** Every transaction carries `actor`. Agents get presence (cursor, viewport, working box, status line), soft leased claims, per-actor undo/revert, and a review mode per agent (direct / proposal / sandbox frame). Spend is gated by budgets.
6. **Agent interface: three tiers.**
   - (a) Semantic tools: `generate`, `place`, `arrange`, `find_free_space`, `comment`.
   - (b) `read`/`edit` on the canvas text projection, symmetric like Brilliant's.
   - (c) Later: sandboxed code mode against a `@fal-os/canvas` API.

   Reads come in two depths: `blurry` (bounds, type, label) and `full`, plus `render_viewport` for vision. Generations are MCP Tasks that create a placeholder node immediately and stream progress. A requestId provides idempotency. Knowledge is loaded on demand.
7. **Integration priority.**
   - P0: localhost MCP with token auth + `fal-os` CLI/skill + auto-registration into Claude Code/Codex/Cursor/Gemini configs + our fal agent.
   - P1: ACP client and Codex app-server so external agents run inside our UI, remote MCP for claude.ai/ChatGPT, Photoshop/Premiere/Blender/Resolve bridges, computer-use fallback.
   - P2: After Effects/Final Cut/Figma depth, learned bridges.
8. **Billing.** Bring-your-own-agent first: the agent runs on the user's subscription and fal bills only generations. Codex via "Sign in with ChatGPT"; Claude via API key unless Anthropic approves claude.ai login; Gemini via API key.
9. **Built for computer use:** ARIA mirror of canvas objects, stable labels, command palette for everything, coordinate/zoom/selection status bar, `falos://` deep links, and a discoverable MCP endpoint (agents prefer it to clicking).

## Canvas node model (first cut)
```
Project { id, name, canvases[], assets[] }
Canvas  { id, name, root: Tree<Node> }          // Loro movable tree
Node    { id, type, x, y, w, h, rot, z, locked, name, createdBy, updatedBy }
  image | video | audio | model3d   → assetId, crop, playback{in,out,loop,muted}
  text | note                        → richtext (Loro text)
  frame | group                      → children
  connector                          → from, to, kind (lineage | reference | flow)
  generation                         → status, jobId, model, params, progress → becomes media node
Asset   { id (content hash), mime, w, h, duration, url, thumbs, lineage{model, params, seed,
          parents[], falRequestId, cost, actor} }
```
Example text projection (agent read/edit format, illustrative):
```
fal:v0 canvas "Moodboard"
a1b2 frame "Hero shots" p(0,0) s(2400,1400)
  c3d4 image asset(sha:9f…) p(40,80) s(1024,1024) "red car v3" by(@claude)
  e5f6 gen flux-pro "red car, dusk, 35mm" p(1100,80) s(1024,1024) status(running 40%)
```

## MVP (phase 1) — "canvas + agents can play"
1. Monorepo scaffold: `apps/desktop` (Electron), `apps/web`, `packages/engine`, `packages/doc`, `packages/host`, `packages/mcp`, `packages/cli`.
2. Engine: pan/zoom at 120fps with 5k images and 50 videos (poster + hover play); select/move/resize; drop files.
3. Doc + Host: Loro canvas, local project folders, undo, actors.
4. fal generation node: prompt → placeholder → result, with lineage.
5. Localhost MCP (`session`, `read`, `edit`, `generate`, `render_viewport`, `get_selection`) + auto-register into Claude Code/Codex. Agent cursors and attribution on the canvas.
6. Demo: Claude Code and Codex building a moodboard together, while the user drags things around.

## Open questions
- Is there an internal "fal agent SDK" we should build on? Public research found the MCP server, genmedia CLI, clients and Platform APIs, but no product with that name.
- Website scope: marketing site only, or the full web canvas at launch?
- Do we accept Loro, or start with a simpler server-sequenced op log like Brilliant's? (I lean Loro: multi-agent is core, and per-peer undo and versioning come free.)
- Auth: fal account OAuth everywhere (app, remote MCP, CLI)?
