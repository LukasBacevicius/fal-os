# Engine spike results (2026-10-10)

The same Rust/wgpu engine (`crates/engine`) and winit app (`crates/spike`), built natively and as wasm.

**Scene:** 10,000 image nodes and 50 video nodes; procedural pixels stand in for decode.

**Benchmark** (12 s scripted camera path):
1. Overview orbit with every node visible.
2. Dive to 1:1 zoom.
3. Fast pan at 1:1, which stresses detail streaming.
4. Zoom back out.

Machine: M4 Pro, macOS 26.6.2, 5K display at 60 Hz (so 60 fps is the ceiling).

| Target | Viewport | fps avg | frame p99 | worst | CPU/frame p50 | CPU p99 | Detail uploads | Memory |
|---|---|---|---|---|---|---|---|---|
| Native Metal (winit) | 2880×1800 | 60.1 | 20.7 ms | 26.8 ms | 1.2 ms | 7.6 ms | 3,507 | 505 MB phys_footprint (≈194 MB fixed Metal/wgpu baseline) |
| Safari 26 WebGPU (wasm, 690 KB) | 4578×2452 | 60.0 | 19 ms | 28 ms | 3 ms* | 10 ms* | 5,316 | ~405 MB WebContent + part of a shared 161 MB GPU process |
| Built-in browser pane (Chromium) | 800×600 | — | — | — | — | — | — | throttled while the pane is hidden; renders correctly |

\* Safari reports `performance.now()` at 1 ms resolution.

## What we learned
- **The Rust engine works in both places, unchanged.** WebGPU in WebKit 26 (the engine behind Safari and WKWebView on macOS and iPadOS 26) runs it at full frame rate. That makes a single web UI shell (Tauri/WKWebView) viable, which was the main risk in Decision 1.
- **Plenty of CPU headroom** for 10k nodes: median 1–3 ms per frame. Spikes at the 99th percentile come from the *synthetic* decode on the main thread: 16 active videos × 256 px × mips, every frame. Real decode belongs off-thread (VideoToolbox / WebCodecs), feeding zero-copy textures.
- **Memory:** what we control scales as designed: about 21 MB per 4,096 thumbnails and about 0.34 MB per 256 px detail slot (384 slots ≈ 126 MB). There is a fixed ≈194 MB of "graphics" footprint that doesn't change with texture or window size; it looks like Metal driver plus wgpu baseline and needs comparing against a bare Metal app. Native and Safari land in the same range.
- **Progressive cold open** (flat colors → thumbnails, 1,024 per frame) and detail LRU streaming both look right visually.

## Decision 1 — recommendation
**Go with B: one web UI, with the Rust engine as wasm, inside a Tauri v2 (WKWebView) shell on macOS and iPadOS; the same build ships as the web app.**
- One UI codebase.
- Measured performance matches native in this test.
- The Rust host (Loro doc, MCP, agents) runs natively in Tauri and in the cloud.

Keep the engine's native present path (Brilliant's CAMetalLayer approach) as an upgrade if real-world profiling ever demands it. The engine crate already supports it.

## Next spikes
1. Real video: WebCodecs `VideoDecoder` → `importExternalTexture` on web, VideoToolbox → IOSurface natively; 50 clips with a decoder cap.
2. Tauri v2 shell on macOS 26 and an iPad simulator: confirm WKWebView exposes WebGPU by default.
3. Measure the bare-Metal baseline to explain the 194 MB.
4. 120 Hz ProMotion run on a MacBook display.
