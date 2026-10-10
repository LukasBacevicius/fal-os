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

## Next spikes
1. Real video: WebCodecs `VideoDecoder` → `importExternalTexture` on web, VideoToolbox → IOSurface natively; 50 clips with a decoder cap.
2. Tauri v2 shell on an iPad simulator and device (macOS done, see above; needs Xcode).
3. Measure the bare-Metal baseline to explain the 194 MB.
4. 120 Hz ProMotion run on a MacBook display.
