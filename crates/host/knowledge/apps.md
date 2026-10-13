# Apps: live code on the canvas

An app node is a folder of plain web files (`<project>/apps/<app>/`) running live on the
canvas in a sandboxed iframe. Use it for games, tools, simulations, data views, prototypes.

## Tools
- `create_app(sessionId, canvas, name, files?, placement?)` → `{app, node, dir, url, version, check}`.
  `placement.w/h` is the app's viewport in CSS px (default 800×600). Pass all the files you
  have at once.
- `write_app_files(sessionId, app, files[{path, content}], delete?, replace?)`: one atomic
  write = one version (revertable with the chat turn). The node reloads live. Returns
  `check`: a headless load with console errors and uncaught exceptions.
- `read_app(sessionId, app, paths?)`: files and their contents, current errors.
- `app_logs(sessionId, app, rerun?)`: errors / warnings / exceptions of the current version,
  from the headless load and from previews people are playing.
- Writing into `dir` directly (e.g. Codex) works too: the host notices, versions it and
  reloads the node. Call `app_logs` afterwards.

## Loop
1. `create_app` with complete, working files (not a stub).
2. Read `check`. Any error or exception → fix with `write_app_files` → repeat until `check.ok`.
3. Follow-ups ("make the ball faster"): `read_app`, change what's needed, write, check.

## Rules
- Plain web, no build step: `index.html` + `<script type="module" src="./main.js">`, relative
  imports, CSS files. Libraries only from a CDN with an exact version
  (`https://esm.sh/three@0.170.0`, `https://cdn.jsdelivr.net/npm/…@x.y.z/+esm`).
- Fill the viewport: `html, body { margin: 0; height: 100%; overflow: hidden }`; size canvases
  to `innerWidth`/`innerHeight` and handle `resize`. Look good at the node size.
- Sandbox: scripts only, an opaque origin. No `alert`/`confirm`/`prompt`, no form submits,
  no popups, no cookies; `localStorage` is in-memory (gone on reload). `fetch` works for CORS
  endpoints.
- Input: the app gets keyboard and mouse once the user clicks it (Esc gives them back to the
  canvas, so don't bind Esc to anything essential). Show the controls on screen
  ("W/S · ↑/↓", "click to start"). Call `preventDefault` for arrow keys / space.
- Audio starts muted (the user unmutes in the node header); create the AudioContext on the
  first user input.
- Dark, minimal look unless asked otherwise. No external trackers.
