# fal-os — architecture (v1)

> Status: v1 direction, 2026-10-10. Background: [`research/`](research/).

## What v1 is
**A collaboration canvas: the central place in fal where you and your agents get creative work done, and where you can see all of it at once.**

- **Projects** contain infinite **canvases** of media in every modality.
- Humans and **many agents at once** work on the same canvas: fal Agent, the user's Claude Code, Codex, Gemini and Cursor, and anything that speaks MCP.
- **Precise annotation**: pin a comment to a region of an image, a single video frame, an audio range or a point on a 3D model, then hand it to an agent.
- **Central overview**: who is working on what, live jobs and spend, and an activity history of every change.
- Internal experiment: no marketing site. Performance and RAM come first.

**Not in v1:** our own bridges into Premiere, Resolve and other apps. If you want an agent to work across fal-os and another app, your own agent (e.g. Codex with computer use) does it. We make fal-os easy for such agents to use: MCP first, an accessible UI second.

## Platform strategy
**Rust core, shared by every platform.** No Flutter and no Electron.

```
crates/
  engine     wgpu renderer: Metal natively, WebGPU in wasm (WebGL2 fallback)
  doc        Loro document, node schema, command API, text projection, annotations
  media      media pipeline interfaces: decode sources, thumbnails, waveforms, mips
  host       canvas authority: actors, claims, undo, jobs, MCP server, agent supervisor
             (runs native in the app, and as the cloud room server)
  ffi        C ABI / UniFFI for the native shells; wasm-bindgen for web
```

- **Engine:** follows Brilliant's design.
  - The engine owns the camera, so pan and zoom only update a uniform.
  - One instanced draw for the whole scene, with SDF shapes and mip levels chosen by zoom.
  - *(Planned, not built yet: only redrawing damaged areas.)*
  - Budgets for everything, an image LRU, and progressive open.
  - *(Planned, not built yet: half resolution while gesturing, and a kill switch for each optimization.)*
  - Canvas-level UI (selection, handles, cursors, annotation pins, labels) is drawn by the engine, so it stays native-fast on every platform.
- **Media decode is platform-native and zero-copy**, behind one engine trait.

  | | Native (macOS / iPadOS) | Web |
  |---|---|---|
  | Video | AVFoundation / VideoToolbox → IOSurface → Metal texture | `<video>` → `importExternalTexture` (measured; WebCodecs `VideoFrame` for scrubbing) |
  | Audio | AVAudioEngine | WebAudio |
  | Images | ImageIO, including HDR/EXR | `createImageBitmap` |

- **Shell:** see [Decision 1](#decision-1-shell) below.

## Modalities — every node type
Every media node has an asset (content-addressed, stored in the local cache and on the fal CDN) and optional **lineage** (model, params, seed, parent assets, fal request id, cost, actor).

| Modality | Node | Rendering / interaction | Annotation anchor |
|---|---|---|---|
| Image | `image` (alpha, HDR/EXR, layers as stacked children) | mipmapped / virtual texture | point, box, polygon, brush mask |
| Video | `video` | GPU frames, hover-scrub, poster when idle, in/out, loop | frame or time range, plus a region on that frame |
| Audio | `audio` (music, voice, SFX) | waveform / spectrogram tile, playhead | time range (optionally a frequency band) |
| Text | `text` (rich text / markdown docs, prompts, scripts) | engine text (cosmic-text) | text range |
| 3D | `model3d` (glTF/GLB, USDZ, Gaussian splats, point clouds) | turntable thumbnail; live orbit viewport when focused | 3D point plus camera |
| Vector | `vector` (SVG, logos, icons) | lyon tessellation | point, box |
| Document | `document` (PDF, slides) | per-page textures | page plus region |
| Web / code | `embed` (generated HTML apps, URLs, live prototypes) | snapshot texture; live webview when focused | point, box |
| Sequence | `timeline` (clips and audio layers, matching fal Agent's timeline) | strip view, scrubbable | time range on a track |
| Live stream | `stream` (fal realtime models, webcam) | streamed frames to texture | frame or time |
| Generation | `generation` (a live fal job) | placeholder with progress → becomes the media node | — |
| Data / styling | `palette`, `lut`, `font`, `character`, `style_ref`, `lora` | swatch / card | — |
| Structure | `frame`, `group`, `note` (sticky), `connector` (lineage / reference / flow) | engine | — |

Image derivatives (depth, normals, masks, segmentation) are attached as **layers** of the image node, not separate nodes.

## App nodes (live code)
Ask any agent for "a ping pong game" and a playable app lands on the canvas, which you then iterate on by chat. Code: `crates/host/src/apps/`, `apps/web/src/components/AppLayer.tsx`.
- **Storage.** An app is a folder of plain web files, `<project>/apps/<app>/`: `index.html` plus ES modules, CSS and assets. Libraries come from CDNs at exact versions. There's no build step in v1; an optional one can come later.
- **Doc node.** Kind `app` with props `app` (the folder id), `entry`, `version`, `asset` (a snapshot still) and `chat` (the chat that wrote it). `created_by` / `updated_by` record who wrote it.
- **Versions.** Every write copies the folder to `apps/.history/<app>/<n>/` and sets `version = n` as the writing agent's actor.
  - Undo, chat-turn revert and redo move that prop like any other change. The host watches doc changes and restores that version's files.
  - Writes are atomic: a staged copy is swapped in with `renamex_np(RENAME_SWAP)` / `renameat2(RENAME_EXCHANGE)`.
- **Serving.** A second loopback listener runs on the host port + 100 (or an ephemeral port): `http://127.0.0.1:<p>/<project>/<app>/<path>`.
  - It needs no token, because that origin holds no privileges. Responses use correct MIME types, `no-store` and CORS `*` (opaque-origin iframes load modules cross-origin).
  - HTML gets a guard script first:
    - audio starts muted (`AudioContext` suspended, media muted) until the node's `[mute]` toggle is flipped;
    - console errors, exceptions and failed loads are beaconed to `__falos/log`;
    - Esc is forwarded to the canvas;
    - `localStorage` gets an in-memory shim.
- **Live reload.** Tool writes, and edits made directly in the folder (Codex, an editor; a 500 ms poll), commit a version. They also broadcast `{"type":"app","app","node","version","what":"files"|"logs","errors"?}` on the project's sync sockets.
- **Check.** After a change, a headless chrome-headless-shell (DevTools protocol, `--mute-audio`) loads the app at the node's size. It collects console errors and exceptions and takes a screenshot, which becomes the node's still. `app_logs` merges these logs with what live previews report.
- **MCP tools.** `create_app`, `write_app_files` (multi-file, atomic; returns `check`), `read_app`, `app_logs`; reference in knowledge `apps`. They're available to the Opus director (small apps), the `coder` subagent, the Codex driver (its sandbox can also write `<project>/apps/`) and external agents over the local MCP.
- **Canvas.**
  - Apps big enough on screen (at most 3) run in a sandboxed iframe (`sandbox="allow-scripts"`, opaque origin) inside the camera-transformed overlay. The others show the engine-drawn still.
  - A new version loads in a hidden second iframe and swaps in on load, so it never flashes.
  - A click (no drag) or ⏎ focuses the app, giving it keyboard and mouse; Esc returns them to the canvas.
  - Header: `app · name · [src] [reload] [open] [mute]`. `[src]` is a read-only file list and code view; `[open]` is the full-screen preview.
- **Chat.** Tool writes add an `app` card to the chat, linking to the node. An app-node chip resolves with `app: {app, dir, url, files, errors}` and its still as an image.

## Annotations & comments
The killer feature. A first-class part of the doc model, not an overlay.

```
Annotation {
  id, nodeId, author (human | agent), createdAt, status (open | in_progress | resolved),
  anchor: point | box | polygon | mask(assetId)
        | time(t) | range(t0,t1) | frame(t) + region
        | textRange(start,end) | page(n) + region | point3d(xyz, camera)
  thread: Message[]            // humans and agents reply in place
  assignee?: actor             // "@codex", "@fal-agent", "@claude"
  outcome?: { nodeIds[] }      // what the agent produced in response
}
```

- **Tools:** pin, box, lasso, brush over images; scrub to a frame and draw on a video; select a range on an audio waveform; click a point on a 3D model; highlight text.
- **Forwarding to agents:**
  - Assigning an annotation, or typing `@agent` in a thread, creates a task for that agent.
  - The agent receives it as an MCP event or a hosted-agent prompt.
  - It gets a context bundle via `get_annotation`: the crop, the exact frame, or the audio clip, rendered on demand, plus coordinates, the thread and lineage.
- **Agents annotate back** ("changed this region", "v2 here"), and their results link to the thread through `outcome`.
- **Review loop:** resolve, reopen, or "try again" on the same anchor.

## Agents as collaborators
- **Actor** on every transaction. Everything is attributed: "created by @claude via flux-2 · $0.04".
- **Presence:** cursor, viewport, working box, and a status line (e.g. "generating 4 variations…"), with an optional live snippet of the agent's thinking.
- **Soft leased claims** on nodes or regions. Humans can always override them, and the agent receives a `claim_revoked` event.
- **Undo per actor:** "undo Claude's last change" or "revert everything Codex did this session". One tool call is one transaction and one undo step.
- **Review mode per agent:**
  - direct: changes apply immediately
  - proposal: changes appear as a ghost layer you accept or reject
  - sandbox frame: the agent works in its own area
- **Spend:** budgets per agent and per project, with approval required above a threshold.
- **Overview panel:** agent roster, active claims, a job queue with progress and cost, and an activity feed you can filter by actor, linked to canvas locations.
- **Safety:** canvas text and comments are passed to agents as untrusted data. Circuit breakers stop no-progress loops.

## How agents connect (v1)
1. **Localhost MCP server** in the app.
   - Streamable HTTP, with a per-install token and Origin/Host checks; plus a stdio shim.
   - The app auto-registers itself in Claude Code, Codex, Cursor and Gemini configs, treating each config file as the source of truth.
   - Tools:
     - `session(agentName)` → session id plus cursor
     - `read` / `edit` (symmetric text projection)
     - `generate` (async MCP Task; creates a placeholder node immediately)
     - `render_viewport` / `export` (vision)
     - `get_selection`
     - `list_annotations` / `get_annotation` / `reply` / `resolve`
     - `claim` / `release`
     - `search_assets`
   - `subscriptions/listen` pushes annotation assignments and changes.
   - Reference docs are loaded on demand rather than packed into tool descriptions; edits are idempotent via `requestId`.
2. **`fal-os` CLI + SKILL.md:** a thin client over the same API, for any shell agent.
3. **fal Agent (hosted)** as a built-in collaborator, via the [Agent SDK](https://fal.ai/docs/documentation/agent/sdk):
   - Selected nodes and annotations are passed in as task context.
   - Its plan renders as a card on the canvas.
   - `fal.activities` drive its presence and status line.
   - `artifacts` land as nodes with lineage.
   - `pending_inputs` (questions and approvals) appear as approval cards.
   - Its projects, characters and skills map onto ours.
   - Caveats: the SDK is JS-only (Node ≥ 22), alpha, and needs an AGENT-scoped key kept server-side. Its REST surface isn't documented, so ask the fal team, or run the SDK in a small Node sidecar or web worker behind the host.
4. **Later:** ACP client and Codex app-server so external agents run inside our UI; remote MCP with OAuth for claude.ai and ChatGPT; MCP Apps views.

**Easy for computer-use agents to drive:** an accessibility mirror of canvas nodes with semantic labels, a command palette for everything, a status bar showing coordinates, zoom and selection, `falos://` deep links, and an MCP endpoint they can discover.

## Data & performance
- **Loro, one doc per canvas.**
  - Rust-native, with a movable tree for z-order, per-peer undo, version checkout, shallow snapshots and fast import.
  - The doc holds only metadata. A canvas with 10k nodes is a few MB, so **the CRDT is not where RAM goes**.
  - We revisit only if profiling says so; the fallback is a server-sequenced op log like Brilliant's.
- **RAM goes to pixels, so that is where the budget lives:**
  - Thumbnail tiers (64 / 256 / 1024 / full), with mip level chosen by zoom.
  - Virtual textures for huge images, and a global GPU texture budget with LRU.
  - Cap on simultaneously active video decoders; poster frames otherwise.
  - Audio stays compressed; waveforms are precomputed.
  - 3D shows a thumbnail until focused, then uses one live viewport at a time.
  - Media is decoded lazily, ranked by distance from the viewport.
- **Targets:**
  - 120 fps pan/zoom with 10k image nodes and 50 videos on canvas.
  - Under 2 s cold open of a large canvas.
  - Under 500 MB RSS for that scene on the native shell.
- **One authority:** the host (local or a cloud room). Files and snapshots are projections of it; no dual sync lanes.

## Decision 1: shell
| Option | How | Pros | Cons |
|---|---|---|---|
| **A. Native shells** | SwiftUI (macOS + iPadOS) hosts the native engine in a `CAMetalLayer` (Brilliant's direct present). The web build is a separate thin TS UI over the wasm engine. | Best performance, RAM and feel. Zero-copy video. iPad comes free with SwiftUI. | Two chrome UIs (Swift and web) |
| **B. One web UI everywhere** | Tauri v2 (WKWebView on macOS/iPadOS) + Rust host natively; engine as wasm/WebGPU inside the webview. Same UI in the browser. | One UI codebase, fastest iteration, Tauri has iOS support, far lighter than Electron. | Engine runs as wasm in the app. WebGPU/WebCodecs inside WKWebView needs macOS/iPadOS 26 and must be verified. Video less zero-copy. |
| C. Hybrid | Native Metal engine layer under a transparent web chrome webview | Native engine, single UI | Messy input and compositing; not worth it |

Either way, the engine, doc and host crates are identical. Only the chrome differs, and the canvas-level UI is drawn by the engine in both, so the chrome stays thin.

**Background layer (desktop).** The app is an always-there layer, not just a window: closing
the window only hides it, so the host, the agent runtime, chat runs and generation batches keep
going. A menu bar item (the LED mark as a template image; one more LED lights while agents
work) lists the active runs, in-flight generations and recently finished runs from the host's
`GET /api/status`, and quiet native notifications say when a run finishes or needs the user.
No global hotkey. Details: [PACKAGING.md §4](PACKAGING.md#4-proof-of-concept-this-repo).

## v1 milestones
1. **Spike (week 1):** engine renders 10k image quads plus video in both native Metal and wasm/WebGPU (inside WKWebView and Chrome). Measure fps and RAM to settle Decision 1 with data.
2. **Canvas core:** projects and canvases, drag and drop import of all modalities, select/move/resize, frames, connectors, Loro doc, undo, local project folder.
3. **Generation:** `generation` nodes via the fal queue/realtime, with lineage and spend.
4. **Agents:** localhost MCP, auto-registration, presence, attribution, claims, per-actor undo, overview panel.
5. **Annotations:** anchors for every modality, threads, assignment to agents, context bundles, review loop.
6. **fal Agent** as a built-in collaborator.
7. **Demo:** you annotate frame 112 of a video ("make the sky stormier"), assign it to Codex. Meanwhile fal Agent builds a moodboard and Claude Code writes the script into a text node. All of it is visible live with costs.
