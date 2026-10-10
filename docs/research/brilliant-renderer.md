# Brilliant teardown — canvas renderer

Static analysis of `brilliant-1.0.0-beta.102` (macOS). Nothing executed. **[E]** = seen in strings/symbols/files, **[I]** = inference.

## Stack
- **[E]** Flutter (Impeller on) draws **only the app chrome**. The canvas is `libbrilliant_native.dylib`: a Rust workspace (`crates/brilliant_native`, `crates/brilliant_spell`) on **wgpu 29 (Metal) + naga**, exposed via a plain C ABI (`bn_*`, ~170 exports, versioned `_v1`). No flutter_rust_bridge.
- **[E]** Crates: lyon (tessellation), kurbo, i_overlay (booleans), etagere (atlas), cosmic-text, harfrust, swash, skrifa, vendored glyphon, spellbook, rayon, slotmap, lru, ruzstd, png. **No** Skia / Vello / tiny-skia / resvg / CRDT crate.
- **[E]** `web_*.rs`, `BN_WEB_*`, `brilliant_engine_bg.wasm` → the same engine ships to WebGPU/wasm.
- **[E]** Two present paths: Rust → IOSurface → Flutter texture; or `DirectPresentController` (Swift) owns a `CAMetalLayer` + `CADisplayLink`, and native scroll/pinch drive the engine camera directly (`bn_gesture_event_v1`, `camera_authority.rs`) — **pan/zoom never touch Dart**.

## Pipeline [I from E]
Dart doc model → binary deltas per frame batch (`bn_apply_deltas_v1`) → retained Rust scene (slotmap, `layer_tree.rs`) → parallel walk (cull, LOD, tessellate, shape text) → instance SSBOs + mesh arena + glyph coverage pages → passes: unified content draw → layer/effect/composite (blend, blur, shadow atlas, glass) → pixel grid → instanced overlay chrome (selection, handles, guides) → labels → retained "content plane" texture → present on a dedicated render thread paced by display link.

**Unified draw [E, WGSL comments]:** one vertex-pulled draw over a z-ordered u32 stream with 4 entry kinds: SDF shape quads (rect/rrect/ellipse), lyon meshes (AA fringe extruded by 1/zoom in VS → *"no re-tessellation, ever"*), coverage text (Wallace/Figma-style winding into R16F; one glyph mesh for all sizes), emoji quads. Shader-side clip chain (≤7 rounded-rect clips). Images: bindless texture arrays or mipmapped atlas, mip from zoom. Shadows = Gaussian of SDF. Glass = jump-flood DF baked once in world space.

## Performance techniques [E unless noted]
| Technique | Evidence |
|---|---|
| Camera-only frames write only a uniform | "camera-only frames still write globals ONLY" |
| Zoom-independent AA (SDF, VS fringes, coverage text) | WGSL comments |
| Draw-list shape stability: culled = zero-area entries, in-place span patches | `HIDDEN_SHAPE_KIND=255`, `BN_DISABLE_STREAM_SPANS` |
| LOD cull with restore debounce | `BN_DISABLE_LOD_CULL`, `BN_CULL_RESTORE_DEBOUNCE` |
| Viewport banding + overscan | `BN_DISABLE_VIEWPORT_BANDING`, `BN_DISABLE_MARGIN_OVERSCAN` |
| Damage compositor into retained plane | `damage.rs`, `BN_DAMAGE_PAN` |
| Subtree carry (reuse unchanged subtrees) | `subtree_carry.rs` |
| GPU compute drag (affine table per record) | WGSL compute pre-pass |
| Mesh arena, idle-time compaction | `mesh_arena.rs`, `BN_ARENA_COMPACT_*` |
| Parallel tessellation/walk + retess cache | `BN_TESS_THREADS`, `BN_DISABLE_RETESS_CACHE` |
| Time/MB budgets for everything | `BN_SPLICE_BUDGET_MS`, `upload_budget.rs`, `bn_shed_v1` |
| Progressive cold open ranked by viewport distance | `BN_COLD_VIEWPORT_RANK`, `BN_COLD_BUILD_*` |
| Half-res during heavy gestures | `present_downscale/upscale.wgsl` |
| Animated-shader quality ladder, paused during gestures | `shader_ladder.rs` |
| Idle frame skip | `BN_DISABLE_IDLE_FRAME_SKIP` |
| Image LRU under budget + incremental mips | `BN_IMAGE_BUDGET_MB`, `bn_evicted_images_v1` |
| Retained GPU scene per canvas (instant tab switch) | `BN_SCENE_RETAIN_BUDGET_MB` |
| Tiled export readback | `readback_tile_cap.rs` |
| Device-loss recovery | `bn_device_loss_record_v1` |
| Kill-switch env var per optimization + GPU timestamps/frame stats | `BN_*` |

No tile cache for the live view. Spatial index lives Dart-side (`utils/spatial_index.dart`), structure unknown.

## Shaders
14 GLSL effects shipped as source (color_adjust, dither, dithering, duotone, halftone, holographic, liquid_metal, liquid_stainless_steel, metaballs, activation_metaballs, noise_grain, pixelate, posterize, reactive_grid) with a fixed uniform contract; also embedded in the dylib with naga's GLSL frontend [I: one source, runs in Flutter and engine → user-extensible effects].

## Lessons for fal-os
1. Separate canvas engine from UI behind a narrow, versioned, binary delta protocol.
2. Engine owns the camera during gestures; pan/zoom = uniform update.
3. Zoom must be free: SDF primitives, mip-from-zoom textures.
4. One instanced/vertex-pulled draw; edit buffers in place, don't rebuild.
5. Retained frame + damage rects + idle skip.
6. **Budget everything** — image LRU, incremental mips, upload budgets, progressive open. Critical for an AI-media canvas with thousands of images/videos.
7. Degrade under load (half-res during gestures, pause animated content).
8. Diagnostics + kill switch per optimization from day one.
