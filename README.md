# fal OS

An app for running creative work with agents. A fast native canvas where references, generations, video, audio and small apps live together. It runs fal at full parallelism through an Opus agent team (Claude Agent SDK + fal Agent SDK), and lets your own Claude Code or Codex drive the same projects over MCP.

## Layout

- `crates/engine`: wgpu renderer (native Metal and WebGPU)
- `crates/client`: canvas interaction, compiled to wasm for the UI
- `crates/doc`: Loro document model and provenance
- `crates/host`: `falos-host`: MCP server, sync, projects, assets, ops, export, library
- `crates/figma`: Figma clipboard import
- `apps/web`: React UI
- `apps/desktop`: Tauri macOS app
- `apps/agent-runtime`: agent runtime (Claude Agent SDK, fal Agent SDK, Codex SDK)

## Develop

```bash
cargo build --release -p falos-host
./target/release/falos-host serve
```

In a second terminal:

```bash
cd apps/web && pnpm install && pnpm build:wasm && pnpm dev
```

Then open http://localhost:5173. Needs Rust (with the wasm32 target), Node 22, pnpm, ffmpeg, a signed-in `claude` CLI, and a fal key in `~/.fal-os/fal.env` (`FAL_KEY=…`, AGENT preset).

## Package (macOS)

```bash
sh scripts/package-macos.sh
```

This builds `fal OS.app` and an ad-hoc signed DMG in `target/package/`.

To install:

1. Drag the app to Applications.
2. Run `xattr -dr com.apple.quarantine "/Applications/fal OS.app"`.
3. Open it and follow setup: paste a fal key, then sign in to Claude.
