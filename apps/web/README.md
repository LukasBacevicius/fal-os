# fal-os web app

The canvas UI (Vite + React + TypeScript) over the Rust client (`crates/client`, wasm on
WebGPU). The same build runs in the browser and in the Tauri shell (`apps/desktop`).

## Run
Prerequisites: `rustup target add wasm32-unknown-unknown`, `rustup component add llvm-tools`
(Apple's `ar` can't archive loro's wasm objects), wasm-bindgen CLI 0.2.129, pnpm.

```sh
pnpm --dir apps/web install
pnpm --dir apps/web dev          # builds the wasm (fast profile) and serves http://127.0.0.1:5173
```
`pnpm build:wasm` builds the optimized wasm into `src/wasm/pkg`; `pnpm build` produces `dist/`
(what the desktop shell bundles). `CARGO_TARGET_DIR` is honored. `?theme=light` switches to the
light variant (remembered), `?debug` shows node count and fps.

The look follows `docs/UI-SPEC.md` (plain mono terminal: canvas + chat, nothing else unless it
needs attention). Keyboard: `docs/SHORTCUTS.md` (`?` in the app, ⌘K palette).

## Layout
- `src/components/Stage.tsx`: owns the `CanvasApp`, forwards pointer/wheel/gesture input and
  file drops, sizes the canvas without flashing (over-allocated backing store + `resizePx`
  in the ResizeObserver), and hosts the DOM layers: note/text (`TextLayer.tsx`, crisp and
  editable), frame titles / generation info (`NodeOverlays.tsx`), agent cursor labels.
- `src/components/ChatPanel.tsx` + `src/chat.ts`: the chat panel (docs/CHAT-PROTOCOL.md),
  several chats, context chips. `src/components/Outline.tsx`: frames/groups tree.
- `src/keys.ts` (all shortcuts), `src/actions.ts` (duplicate, group, copy/paste, Tab order),
  `src/components/Palette.tsx` (⌘K and the `?` list).
- `src/controller.ts`: wasm events → store, IndexedDB persistence (`persist.ts`), file import.

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
merge, so offline edits go out), agent cursors/status come from host presence (white arrow +
mono label, fade after 10 s idle), claims are outlined, dropped media is uploaded to `/assets/{sha256}` and
remote-only assets are loaded from it (videos stream with byte ranges). Connection state shows
only while something is wrong ("offline — reconnecting"). Without a host the doc lives in the
page (IndexedDB) and chat is unavailable.
Doc positions are parent-relative (children move with their frame).
