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
| Tauri 2.12 / WKWebView (wasm, release host) | 2880×1744 | 60.0 | 19 ms | 22–24 ms | 1 ms* | 7–8 ms* | 3,440 | 492 MB total: 310 MB WebContent + 145 MB WebKit.GPU + 28 MB app + 8 MB Networking |
| Built-in browser pane (Chromium) | 800×600 | — | — | — | — | — | — | throttled while the pane is hidden; renders correctly |

\* WebKit (Safari and WKWebView) reports `performance.now()` at 1 ms resolution.

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

## Tauri / WKWebView (macOS 26.6.2, Tauri 2.12.2 / wry, 2026-10-10)
App: `apps/desktop` (see its README). It's a minimal Tauri v2 shell that loads `web/` either from devUrl (`http://127.0.0.1:8789`, served by `web/serve.py`) or bundled as frontendDist (`tauri://localhost`, built with `--features custom-protocol`). It was built with plain `cargo build` using only the Command Line Tools: no Xcode and no tauri-cli.

**WebGPU is on by default in WKWebView on macOS 26. No flags, private WKPreferences or `with_webview` hooks are needed.** Evidence comes from an injected probe (`FALOS_PROBE`) that POSTs to serve.py:
- For both origins (`http://127.0.0.1:8789` and `tauri://localhost`), the probe returned `navigator.gpu` present, `isSecureContext: true`, `requestAdapter()` OK and `requestDevice()` OK.
- The adapter exposes `maxTextureDimension2D` 16384 and the same feature set Safari 26 reports: `shader-f16`, `timestamp-query`, BC/ETC2/ASTC compression, `float32-filterable`/`-blendable`, `core-features-and-limits` and `texture-formats-tier1`.
- `VideoDecoder` (WebCodecs) also exists, which unblocks the real-video spike inside the shell.
- Tauri's custom `tauri://` scheme counts as a secure context, so the bundled app gets WebGPU too, not just the dev server.
- If WebGPU ever needed forcing (an older OS, or a future flag), the route would be `WebviewWindow::with_webview(|w| w.inner())`. That gives the `WKWebView`, and its `configuration().preferences()` can take WebKit's private `_setEnabled:forFeature:` (`WKPreferences._features`). We didn't need it on macOS 26.

**Benchmark** (`?bench`, 10k nodes + 50 videos, 12 s path):
- Setup: 1440×900 pt window (2880×1744 px canvas), release host binary, three runs via devUrl plus one via bundled `tauri://`.
- Results were identical across all runs: **60.0 fps, frame p50 17 ms, p99 19 ms, worst 22–24 ms, CPU/frame p50 1 ms, p99 7–9 ms, about 3,440 detail uploads.**
- The bundled `tauri://` run matched: 60.0 fps, p99 19 ms, CPU p99 9 ms.
- This matches native Metal at a similar viewport (60.1 fps, CPU p50 1.2 ms, p99 7.6 ms) and Safari.

**Memory** (`footprint` 8 s into the bench, i.e. mid-path with the detail cache full; stable through the end; three runs within 2 MB):

| Process | phys_footprint | Notes |
|---|---|---|
| `falos-desktop` (Tauri host, Rust) | 28 MB | peak 31 MB |
| `com.apple.WebKit.WebContent` | 310 MB | ≈236 MB of it is graphics (194 MB IOAccelerator, about 40 MB IOSurface); about 36 MB is wasm/JS/malloc; peak 375–408 MB during cold open |
| `com.apple.WebKit.GPU` | 145 MB | 117 MB owned graphics; peak about 230 MB |
| `com.apple.WebKit.Networking` | 8 MB | |
| **Total** (footprint summary, deduplicated) | **492 MB** | Native Metal: 505 MB. Safari: about 405 MB WebContent + part of a 161 MB GPU process shared with other tabs |

With WKWebView, the app gets its own private GPU and WebContent processes, so its memory is fully attributable (unlike Safari's shared GPU process). The shell itself costs about 28 MB plus 8 MB networking. That's tiny next to Electron.

**Caveats found:**
- **Occlusion stops rendering.** A WKWebView whose window is fully covered (or launched in the background behind a fullscreen app) reports `visibilityState = hidden` and pauses `requestAnimationFrame`. Our first unattended run stalled on "loading…" for that reason. It's normal browser behavior, and good for battery. For unattended benches, use `FALOS_ON_TOP=1`, which sets always-on-top. If the product ever needs agents to render while hidden, it needs either an engine tick that doesn't depend on rAF (rendering to an offscreen texture) or WebKit's private `_setWindowOcclusionDetectionEnabled:NO`.
- The user agent is a generic `Macintosh; Intel Mac OS X 10_15_7 … AppleWebKit/605.1.15` with no Safari/version token. Use feature detection, not UA sniffing.
- The `?bench` POST to `/bench` only reaches serve.py on the devUrl origin. Under `tauri://`, the result was captured through the probe, which reads `window.__falosBench`. Production telemetry should go through a Tauri command / IPC.
- Not tested: a `.app` bundle made by `cargo tauri build`, code signing and notarization, and 120 Hz ProMotion. The bundler doesn't need Xcode for a macOS `.app`/`.dmg`; `codesign` and `notarytool` ship with the CLT.

**iPadOS feasibility (not attempted):**
- Building for iPad needs full Xcode, not just the CLT. `cargo tauri ios init` generates an Xcode project, and `cargo tauri ios dev|build` drives `xcodebuild` and the Simulator.
- It also needs the `aarch64-apple-ios` and `aarch64-apple-ios-sim` Rust targets, CocoaPods (listed in the Tauri v2 iOS prerequisites), and an Apple Developer team ID for running on a device.
- WebKit is the same engine on iPadOS 26, where Safari ships WebGPU on by default, so WKWebView should behave the same. Verify with the same probe in a simulator once Xcode is installed. Note that the simulator's GPU path differs from a device; final numbers need a real iPad.
- Expect tighter WebContent memory limits (jetsam) on iPad. The roughly 310 MB WebContent footprint is fine on M-series iPads but should be budgeted.

**Verdict for Decision 1:** Option B is confirmed on macOS 26. Tauri v2's WKWebView runs the wasm engine at full frame rate, with CPU and memory matching native Metal (492 vs 505 MB) and a host process of about 28 MB, with zero WebKit configuration. The remaining risk is iPadOS, which should behave the same but needs Xcode to verify.

## Real video (web)
Real `<video>` playback replaces the synthesized frames on the web path. Run it with `?realvideo` (zero-copy, the default) or `?realvideo=copy`. Clips come from `web/make-media.sh`: four 640×360 H.264 lavfi sources (testsrc2, mandelbrot, smptebars, life), 30 fps, 10 s.

**What wgpu 30 offers on its webgpu backend** (checked in the `wgpu-30.0.1` source):
- `ExternalTexture` and `texture_external` bindings are `unimplemented!()`. Creating one, binding one, and even its `Drop` all panic on web.
- `Queue::copy_external_image_to_texture` is implemented, for `HTMLVideoElement`, `VideoFrame`, `ImageBitmap` and canvases.
- `Device::as_webgpu()` and `Texture::as_webgpu()` return the raw `GPUDevice` and `GPUTexture`. That's the escape hatch the zero-copy path uses.

**Approach.**
- The engine has a `media::VideoSource` trait that the platform layer provides:
  - `set_active(id, bool)` starts or pauses a video.
  - `update(id, force, slot)` delivers a frame in one of two ways. It returns `Texture(view)` and the engine GPU-blits it into the slot, or it returns `Written` because the source drew into the slot itself.
- The engine owns the policy:
  - Only the biggest visible videos play, up to `max_active_videos` (16); the rest are paused and keep their last frame.
  - A slot that is waiting for its first frame shows the low tier.
  - Every blit fills all three detail mips with a 4×4-tap box filter.
- With no source attached, nothing changes: native still synthesizes frames. VideoToolbox → IOSurface can plug in later through `Texture(view)`.
- The web source (`crates/spike/src/web_video.rs`):
  - Creates one hidden `<video>` per video node (muted, loop, playsinline), lazily, the first time that video becomes active.
  - Counts new frames with `requestVideoFrameCallback`, so it only ingests when a frame actually changed.
  - **import** (default): calls `importExternalTexture(video)` and draws a small blit straight into the detail slot, through the raw handles plus about 40 lines of inline JS. No copy and no CPU pixels.
  - **copy**: wgpu `copy_external_image_to_texture` into a per-video 640×360 texture, which the engine then blits.
- `web/serve.py` now answers HTTP Range requests; Safari won't play `<video>` without them.

**Measurements**
- Safari 26.6.2, M4 Pro, 5K at 60 Hz, viewport 4578×2452.
- Same 12 s camera path, 10k nodes.
- *Video CPU* is the main-thread time the engine spends on video each frame (the source calls plus encoding blits, or synth plus upload), averaged over frames that had at least one active video.
- Safari's timer has 1 ms resolution, so per-frame percentiles are coarse; the video averages are taken over about 500 frames.
- In the 1,000-video scene the 16-video cap is hit throughout the 1:1 phases.

| Scene | Video path | fps avg | frame p99 / max | CPU p50 / p99 | Video CPU / active frame | Frames ingested |
|---|---|---|---|---|---|---|
| 50 videos | simulated (CPU synth + `write_texture`) | 60.0 | 19 / 19 ms | 3 / 11 ms | 2.35 ms | — (uploads every frame) |
| 50 videos | real, **import** | 59.8 | 19 / 53 ms | **1** / 8 ms | **0.13 ms** | 1,285 (1,350 presented) |
| 50 videos | real, copy | 59.8 | 19 / 38 ms | 3 / 12 ms | 2.60 ms | 1,299 |
| 1,000 videos | simulated | 59.9–60.0 | 18–19 / 20–27 ms | 6 / 10–12 ms | 5.2–5.4 ms | — |
| 1,000 videos | real, **import** (2 runs) | 59.6–59.8 | 21–28 / 53–61 ms | **1** / 5 ms | **0.17–0.21 ms** | ≈3,850 (≈3,900 presented) |
| 1,000 videos | real, copy | 59.7 | 24 / 53 ms | 7 / 13 ms | 6.28 ms | 3,819 |

*Presented* counts `requestVideoFrameCallback` firings across all elements, which is proof that the videos really played; *ingested* counts frames that reached a detail slot.

**Microbenchmark** (`web/videobench.html`): 16 playing 640×360 videos, every frame ingested and blitted. Cost is main-thread CPU.

| Path | fps | CPU per video per frame |
|---|---|---|
| `copyExternalImageToTexture(<video>)` (wgpu's path) | 45.7 | 0.81 ms |
| `copyExternalImageToTexture(new VideoFrame(<video>))` | 58.2 | 0.81 ms |
| `importExternalTexture(<video>)` + bind group + blit | 60 | **0.006 ms** |
| `importExternalTexture(new VideoFrame(<video>))` + blit | 60 | 0.085 ms |
| blit only (no ingest) | 60 | 0.005 ms |

**What we learned**
- **Zero-copy is the only acceptable web path, and it works in WebKit 26.**
  - `copyExternalImageToTexture` is GPU-side in name only. WebKit spends about 0.8 ms of main-thread time per call, so 16 videos at 30 fps cost about 6 ms per frame, worse than our CPU *simulation*.
  - `importExternalTexture` costs about 6 µs per video; 16 real videos cost about 0.2 ms per frame.
- **wgpu doesn't expose it yet,** but the raw-handle escape hatch is small and contained: one JS function in the platform layer, drawing into a wgpu-owned texture. Replace it when wgpu's webgpu backend implements `ExternalTexture`.
- **Real decode lives off our thread.** The browser's hardware decoder does the work. Main-thread CPU p50 drops from 3–6 ms (simulated) to 1 ms at the same 60 fps.
- **Import directly from the `<video>` element; it beats WebCodecs `VideoFrame`** (0.085 ms per video, from constructing the frame). A WebCodecs `VideoDecoder` is still the route for frame-accurate scrubbing and for decoding in a worker, but it isn't needed for playback.

**Limitations and caveats**
- The cap counts *playing* elements; there's no cap on *created* elements yet. Every video that has ever been active keeps its `<video>` element and buffered media. That needs an LRU that drops `src` on long-paused elements, and memory hasn't been measured.
- The first activation of a video shows the low tier until its element has a frame (about 100 ms), and creating an element costs a frame-time spike: the import runs have a worst frame of 53–61 ms, against 19–27 ms simulated.
- The low tier (32 px thumbnails) is still synthesized, so a paused video that has never been activated shows a fake thumbnail. Real posters would come from the asset pipeline.
- Frames are stretched into the square 256 px slot (no aspect-correct crop) and capped at the detail-slot resolution. A focused or full-screen video should sample the external texture directly at native resolution.
- Video color is assumed sRGB (`importExternalTexture` default). HDR/P3 sources need a color-managed path.
- WebKit may pause media it doesn't consider rendered. The elements are kept as 4 px, nearly transparent boxes behind the canvas. Chromium pauses all of them while the page is hidden (seen in the built-in browser pane); that's expected and harmless.
- Chrome wasn't measured: it isn't installed on this machine, and the built-in Chromium pane is throttled while hidden. That pane did confirm the import path renders correctly.
- Native still uses synthesized frames. VideoToolbox → IOSurface → `Texture(view)` is the next native step.

## Next spikes
1. Real video, native: VideoToolbox → IOSurface-backed Metal texture behind `VideoSource`. Web: an element LRU, real posters, and aspect-correct sampling.
2. Tauri v2 shell on an iPad simulator and device (macOS done, see above; needs Xcode).
3. Measure the bare-Metal baseline to explain the 194 MB.
4. 120 Hz ProMotion run on a MacBook display.
