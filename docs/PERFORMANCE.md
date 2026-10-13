# Why fal-os is fast

The short answer: **the hot path is Rust on the GPU, and the browser and React stay out of it.** The canvas is one Rust renderer (wgpu) that runs as WebGPU/wasm in the app and as Metal natively. It draws every node in a single instanced draw call and only spends memory on pixels that are actually on screen. React draws the chrome around it. A native Rust host does the heavy lifting (files, thumbnails, the asset index, fal jobs) off the UI thread.

```
 Tauri shell (WKWebView, ~28 MB host process)
 ┌──────────────────────────────────────────────────────────────┐
 │  React chrome: panels, chat, library grid, DOM text/video UI │  re-renders on state, not per pixel
 │        │ calls (pointer, wheel, apply)    ▲ events (JSON)    │
 │        ▼                                  │                  │
 │  wasm CanvasApp (crates/client)                              │
 │    Loro doc ─► ChangeFeed ─► Scene diff ─► Renderer (engine) │  rAF loop, renders only when dirty
 │                                              │ 1 instanced draw, texture tiers, LRU
 │                                              ▼               │
 │                                     WebGPU ─► Metal          │
 └───────────────▲──────────────────────────────────────────────┘
                 │ WebSocket: Loro update bytes, presence, jobs
 ┌───────────────┴──────────────────────────────────────────────┐
 │  falos-host (Rust, tokio): canvas authority, MCP, fal batches,│
 │  SQLite library index, WebP thumbs, Range-served assets      │
 └──────────────────────────────────────────────────────────────┘
```

## 1. Renderer (`crates/engine`)
- **One instanced draw for the whole scene.** Every node is one instance. The node shapes, rounded corners (SDF), generating-tile animations and the canvas UI (selection, handles, pins, cursors) are all drawn by shaders, with no per-node draw calls and no DOM.
- **Panning and zooming are nearly free.** The engine owns the camera. A camera-only frame writes one uniform. Instance data is re-uploaded only for the range that changed.
- **No frames when nothing changes.** The rAF loop renders only when the scene, overlay, streaming or video playback needs it (`needs_redraw`). Slow animations are throttled. The animation clock pauses during drags and camera moves.
- **Memory goes to what is visible.** Texture tiers live in 2048 px array pages:
  - a 32 px mipmapped tier for every node, always resident (4,096 tiles per page);
  - 384 slots of 256 px detail (about 126 MB with mips);
  - 8 slots of 1024 px for the few nodes that are big on screen.

  Detail and hires tiles are requested only for nodes in view (plus 25% overscan) and above a minimum on-screen size. They are evicted by LRU.
- **Budgets on everything:** a cap of 12 detail uploads per frame, a progressive cold open (flat colors, then thumbnails), and at most 16 videos decoding at once (the rest keep their last frame). Each budget can be switched off for measurement.
- **Zero-copy video on the web:** `importExternalTexture(<video>)` blits straight into a GPU slot, at about 6 µs per video against about 0.8 ms for the copy path (measured, `docs/spike-results.md`).
- **Crisp text without a text engine in the hot path.** Text nodes are rasterized into tiles, and greeked when tiny. Once a node is readable on screen, a DOM text layer crossfades in over it (`TextLayer.tsx`), so text is sharp and editable without the engine shaping glyphs every frame.

## 2. React stays out of the hot path (`crates/client`, `apps/web`)
- Pointer and wheel input go straight into wasm (`pointerDown`, `wheel`, …). Selection, drag, marquee and hit testing happen in Rust, and React hears about the results as events.
- **Doc changes are incremental.** A Loro subscription records *which* nodes changed. The scene re-reads only those nodes and sends minimal add/update/remove calls to the engine. Local edits and remote imports take the same path, so the canvas never rebuilds everything.
- DOM overlays (text, video transport, audio) are mounted only for in-view nodes and positioned with one CSS transform.
- **The library grid is virtualized** in bands with hysteresis: it renders 1.5 screens around the viewport and re-bands only after scrolling a quarter screen. It only ever loads 256 px WebP thumbnails, never originals.

## 3. Data and sync (`crates/doc`, `crates/host`)
- **Loro CRDT, one doc per canvas, metadata only.** Pixels never go into the doc. Sync sends binary Loro *update* bytes since the peer's version vector over a WebSocket, not snapshots or JSON. The host autosaves with a 400 ms debounce.
- **Agents read a compact text projection** (`text.rs`, `FORMAT.md`), filtered by subtree, depth or type, instead of dumping JSON.
- **Library:** a SQLite index (WAL, FTS5) keyed by sha256. Thumbnails are generated in the background on blocking threads: 256/1024 px WebP, a 12-frame video strip with a best-frame poster, and audio waveforms.
- **Assets are served with HTTP Range** (206), so video seeks and scrubs fetch only the bytes they need.
- **Copies are free:** duplicates and exports use APFS `clonefile` (copy-on-write) with a byte-copy fallback.

## 4. Generation is parallel by design (`batch.rs`, `pipeline.rs`, `apps/agent-runtime`)
- `generate_batch` creates all N placeholders in **one transaction**. It submits up to 64 jobs in flight to the fal queue (where they run in parallel), polls them all from one shared poller, and downloads results 16 at a time. Results land in coalesced transactions every 150 ms, so 200 completions cost a handful of syncs. Identical requests are packed via `num_images`.
- **Per-item pipelines** (`then`, up to 6 steps) chain still → video → upscale inside the host, with no agent round-trip between steps.
- The director agent is prompted to plan a dependency graph and issue every independent tool call or subagent in one message, so the branches run concurrently.

## 5. Shell: Tauri, not Electron
- The app uses the system WKWebView with WebGPU on by default on macOS 26. It ships no bundled Chromium. The shell's host process measured **28 MB**.
- Most of the 63 MB DMG is the bundled Node 22 that the agent runtime needs. The web UI is about 5 MB and `falos-host` about 21 MB stripped (`docs/PACKAGING.md`).

## Numbers
All measured on an M4 Pro, macOS 26. The source column says where each comes from.

| What | Result | Source |
|---|---|---|
| 10k image nodes + 50 videos, scripted pan/zoom | 60 fps (display cap), CPU/frame p50 1 ms, p99 7–9 ms | spike-results.md (Tauri/WKWebView) |
| Same scene, native Metal | 60.1 fps, CPU p50 1.2 ms | spike-results.md |
| Memory, same scene | 492 MB total in Tauri vs 505 MB native Metal | spike-results.md |
| 1,000 real videos, zero-copy import | 59.6–59.8 fps, main-thread CPU p50 1 ms | spike-results.md |
| Video ingest per video | ~0.006 ms import vs ~0.8 ms copy | spike-results.md |
| Library: 7.7k real assets | opens in ~25 ms, scrolls at 120 fps | RESULTS-ASSETS-V1.md |
| DMG / host process | 63 MB / 28 MB | RESULTS-ASSETS-V1.md, spike-results.md |
| Web bundle (Oct 11 build) | wasm 4.8 MB (1.6 MB gzip), JS 395 KB (129 KB gzip) | measured from `apps/web/dist` |
| Canvas at 120 Hz ProMotion, cold open of a real large canvas, RSS of a real project | not measured yet | — |

## Honest caveats
- The canvas benchmarks ran with synthetic pixels and real `<video>` on a 60 Hz display. A real-asset canvas profile and a 120 Hz run are still to do.
- Not built yet (from the architecture plan): damage-region partial redraw, half resolution during gestures, and native VideoToolbox → IOSurface video. Native uses synthesized frames today.
- Hit testing is a linear scan over nodes, which is fine at 10k but has no spatial index. The React node list is re-read in full on each doc change. Camera moves re-render the in-view DOM overlays.
- About 194 MB of the memory is a fixed Metal/wgpu baseline that hasn't been explained yet. There is an active-video cap but no LRU on created `<video>` elements.
