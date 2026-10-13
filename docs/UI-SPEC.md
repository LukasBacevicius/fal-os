# UI spec v0: plain mono terminal

> A POC look. Plain, monochrome and functional, like a 90s terminal: no branding, no color, no ornament. We'll redesign properly later. If in doubt, remove it.

## Visual language
- **Type:** monospace everywhere (`ui-monospace, "SF Mono", Menlo, monospace`), 12–13px, regular weight; 500 for headings only. No icon fonts. Use text labels such as `[sel]` `[hand]` `[+ file]`, or plain ASCII glyphs.
- **Color:** black, white and grays only.
  - CSS variables: `--bg #0a0a0a`, `--panel #0f0f0f`, `--line #262626`, `--text #e6e6e6`, `--dim #8a8a8a`, `--faint #4a4a4a`.
  - Provide a light variant through the same variables (`[data-theme=light]`: white background, black text). Dark is the default.
  - **No accent colors, gradients, shadows, glows, blur, or rounded "pill" chips.** Corners are 0–2px; borders are 1px `--line`.
- **Selection** is a 1px white outline with small square handles: white fill, black border.
- **Motion** is only functional: 120ms fades. No bounces.

## Layout
```
┌ fal-os / demo / main ───────────────────────── ● host · lukas@fal ┐
│                                                 │ chat             │
│                    canvas                       │ ...messages...   │
│                                                 │                  │
│                                                 │ agents: ...      │
│                                                 │ [chips]          │
│ 43% · 7 nodes                                   │ > input_         │
└─────────────────────────────────────────────────┴──────────────────┘
```
- **Top bar** (28px):
  - Left: a text breadcrumb, `fal-os / <project> / <canvas>`.
  - Right: a host connection dot (white = connected, `--faint` = offline, as a text label rather than a color) and the signed-in fal account as text.
  - A `[≡]` text button opens the app menu (projects, panels, chats, import / export, connect agent, undo / redo, theme, reduce motion, shortcuts, palette). Every action with a key also has a click path (docs/SHORTCUTS.md, "Click paths"); tooltips show the key.
- **Canvas** fills the rest. The bottom-left status is plain dim text: `43% · 7 nodes`. Show fps only with `?debug`.
- **Chat panel** on the right: 360px, resizable, collapsible with ⌘\\. It implements [CHAT-PROTOCOL.md](CHAT-PROTOCOL.md):
  - Messages are plain text blocks. User lines are prefixed `>`, agent lines `director:`, `generator:` and so on.
  - Tool and agent activity appears as single dim lines (e.g. `· generator  generate flux/schnell → 3 nodes`), collapsible. Node ids are clickable and pan to the node.
  - The swarm strip above the input has one line per active subagent (`motion   rendering 0:07/0:15`). It appears only while agents run.
  - Context chips are bracketed text: `[img hero-2]`, `[spot hero-2 0.42,0.31]`, `[frame clip-1 @3.24s box]`, each with an `×`.
  - The input is a textarea with a `>` prompt. Enter sends, Shift+Enter adds a newline, Esc stops a run.
- **Adding context:** select nodes and press ⌘L (or click "add to chat" in a tiny text button by the selection). Alt-click a spot on an image or video to add a spot chip; Alt-drag to add a region chip; on a video this uses the current playhead frame.

## Remove
- The F logo, product branding, colored avatars, and the "Live agents" demo.
- **All fake or demo cursors.** Agent cursors are shown only for real host sessions with activity in the last ~10 s: a small white arrow plus a mono text label (`motion: rendering`), with no colored bubble. They fade out when idle.
- The annotations panel, the annotate tool and annotation pins and boxes on the canvas. Keep the doc data model, just don't render it.
- The left layers/assets panel and the inspector panel. Lineage is shown later via chat or hover.
- The bottom floating toolbar. Tools are keys: V select, H/space hand, and drop files to import.

## Generating state (important)
A `generation` node renders as a dark tile (`#1a1a1c`) with **the fal mark as LED dots** centred on it: the 13×13 grid of `assets/brand/fal-mark.grid`, 2–3 px square dots on a 3–6 px pitch. Cells sit dim and light in discrete steps (about 2 per second, each cell on its own phase), so nothing slides, fades smoothly or flashes as a whole. No gradients, glows or sweeps. Tiles too small for the mark stay plain.
- Draw it in the engine as one shader effect on the tile: O(1) per pixel, no per-frame CPU work, redrawn at ~10 Hz only while a tile is visible.
- Pause the animation during heavy gestures.
- **Progress:** if known, the mark lights in reading order up to the progress, and a 1px bottom bar in `--dim` shows it too. Hovering shows `<model> · <elapsed> · <progress>` and the prompt snippet as tiny dim mono text.
- **When it completes:** a 200ms crossfade from dots to the media.
- **Failed:** the tile shows `failed` in dim mono text with a retry affordance. The text goes in the chat.

## Node rendering (agent content)
- **frame:** a 1px `--line` outline with the title in dim mono above its top-left corner. No fill.
- **note / text:** a plain mono text block on `--panel`, with a 1px border and no sticky colors (ignore `color` props for now).
- **image / video:** the media, edge to edge. Videos play muted on hover, or while selected.
  - **Poster:** the most representative of a few sampled frames (15/35/55/75 %), never a black or flat opening frame.
  - **Affordances:** a tiny `▶ 0:30` badge bottom-left (only when the tile is big enough on screen); while hovered it shows `0:07 / 0:30`.
  - **Selected:** a one-line mono transport under the tile, `[▶] 0:07 / 0:30 ───── [mute] [⤢]`. Space tap / K, J / L, , / . and M drive it (see SHORTCUTS.md).
  - **Focus view** (⏎ or double-click): a near-fullscreen real `<video>` with sound on a near-black backdrop, the same transport, ← / → for other videos, Esc closes.
- **audio:** a waveform in `--dim` on `--panel`, with the name.
- **other types:** a bordered box with `[3d] name`, `[pdf] name` and so on.
