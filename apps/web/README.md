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

## Host (live collaboration)
With a fal-os host running (`cargo run -p falos-host -- serve --project <dir>`), the page
connects to it and humans and agents edit the same canvas live (`src/sync.ts`, protocol in
`crates/host/PROTOCOL.md`). Discovery (`src/host.ts`), first match wins:
1. `?host=127.0.0.1:7317&token=…` in the URL (`?local` forces local-only).
2. Tauri: the `host_info` command reads `~/.fal-os/host.json` (`FALOS_HOME` honored).
3. `pnpm dev`: the Vite plugin serves that file at `/__falos/host` (dev server only, loopback
   clients only). Run `FALOS_HOME=<dir> pnpm dev` to use another host; `FALOS_WEB_PORT`
   changes the dev port.

Connected: the host's snapshot replaces the local canvas on first contact (later reconnects
merge, so offline edits go out), agent cursors/status come from host presence (they fade after
10 s idle), claims are outlined, dropped media is uploaded to `/assets/{sha256}` and
remote-only assets are loaded from it (videos stream with byte ranges). The top-bar pill shows
the connection. Without a host the doc lives in the page (IndexedDB + `.falos.json` export).
Doc positions are parent-relative (children move with their frame).
