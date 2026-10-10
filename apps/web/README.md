# fal-os web app

The canvas UI (Vite + React + TypeScript) over the Rust client (`crates/client`, wasm on
WebGPU). The same build runs in the browser and in the Tauri shell (`apps/desktop`).

## Run
Prerequisites: `rustup target add wasm32-unknown-unknown`, `rustup component add llvm-tools`
(Apple's `ar` can't archive loro's wasm objects), wasm-bindgen CLI 0.2.129, pnpm.

```sh
pnpm --dir apps/web install
pnpm --dir apps/web samples      # optional: sample media into public/samples (needs ffmpeg)
pnpm --dir apps/web dev          # builds the wasm (fast profile) and serves http://127.0.0.1:5173
```
`pnpm build:wasm` builds the optimized wasm into `src/wasm/pkg`; `pnpm build` produces `dist/`
(what the desktop shell bundles). `?samples` seeds an empty canvas with the sample media.

## Layout
- `src/components/Stage.tsx`: owns the `CanvasApp`, forwards pointer/wheel/gesture/key input,
  file drops, and hosts the DOM overlays (annotation composer, agent cursor labels).
- `src/controller.ts`: wasm events → store, IndexedDB persistence (`persist.ts`), import/export.
- `src/components/RightPanel.tsx`: inspector and annotation threads.

Local-only for now: the doc lives in the page (IndexedDB + `.falos.json` export). A host
connects through the client's `SyncTransport` (`crates/client/src/sync.rs`).
