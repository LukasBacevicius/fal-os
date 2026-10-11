# Keyboard shortcuts

The app can be driven without the mouse. Press `?` for this list in the app, and ⌘K for the
command palette, which fuzzy-searches actions, chats and nodes by name. ⌘ means Ctrl on
Windows and Linux. The source of truth is `SHORTCUTS` in `apps/web/src/keys.ts`.

Canvas shortcuts never fire while you type: in the chat input, a note being edited or the
palette. The shortcuts marked "everywhere" work in those places too.

## App
| Keys | Action |
|---|---|
| ⌘K | Command palette: actions, chats, nodes, "connect agent", "reduce motion" (everywhere) |
| `?` | Shortcut list |
| ⌘\\ | Show or hide the chat panel (everywhere) |
| ⌘⇧\\ | Show or hide layers and properties (everywhere) |
| ⌘⇧A / ⌘⇧L | Asset library: left panel / full screen. Arrows move, space quick look, ⏎ or drag places on the canvas, f favorite, t tag, c collect, u upload to fal, l local-only, s similar, esc closes |

## Start a chat from the canvas
| Keys | Action |
|---|---|
| `/` or click `[✦]` | With a selection: open the inline prompt next to it, pre-filled with the selection as chips. Without one: focus the chat input |
| ⏎ | In the ✦ prompt: start a **new chat** |
| ⌘⏎ | In the ✦ prompt: send to the **current chat** |
| ⇥ | In the ✦ prompt: move the draft and chips into the chat panel ("open in chat") |
| Esc | Close the ✦ prompt (the draft is kept for that selection) |
| ⌘L | Add the selection to the chat as context chips |
| Drag a node onto the chat | Attach it as a chip (the node stays where it was) |
| Drop or paste files on the chat | Import them as canvas nodes in empty space near the view and attach them |
| ⌥-click / ⌥-drag | Add a spot or region on an image. On a video it adds a frame at the playhead |

## Chat
| Keys | Action |
|---|---|
| ⌘I | Focus the chat input (everywhere) |
| ⏎ | Send; while the chat runs the message is queued. ⇧⏎ inserts a new line |
| Esc | In the input: stop the run if one is running, otherwise leave the input |
| `@` / `#` | In the input: mention a node / another chat |
| ⌫ | In an empty input: remove the last chip |
| 1…9 | In an empty input: answer the agent's question with that option |
| ⌘. | Agent for the next message: opus / fal / codex (everywhere) |
| ⌘N or ⌃N | New chat (everywhere) |
| ⌘Y or ⌃Y | All chats (history): search, ⏎ open as a tab, `d` delete, `a` archive, Esc back (everywhere) |
| ⌘⇧[ ⌘⇧] or ⌃⇧[ ⌃⇧] | Previous or next tab (everywhere) |
| ⌘1…9 or ⌃1…9 | Switch to tab 1…9; hold ⌘ to see the numbers (everywhere) |
| ← → / ⏎ | On a focused tab: move between tabs / open |
| ⌘⌫ | On a focused tab: delete the chat (asks `[y/n]`) |
| Double-click a tab | Rename. Right-click: rename, close, archive, delete |

## Canvas
| Keys | Action |
|---|---|
| V | Select tool |
| H, or hold Space | Pan |
| N, or double-click empty canvas | New note at the cursor, in edit mode |
| ⏎ or double-click | Edit the selected note or text. On a video, open the focus view; on other media, zoom to it |
| Tab / ⇧Tab | Select the next or previous node in reading order (rows top to bottom, left to right) |
| ⌘A | Select all |
| Esc | Deselect |
| ⌘D | Duplicate |
| ⌘G | Group the selection. A group always hugs its children |
| ⌘⌥G | Frame the selection (a titled frame with padding) |
| ⌘⇧G | Ungroup (children keep their place) |
| Click / double-click / Esc | A click on something inside a group selects the group. Double-click enters the group, and Esc goes back up a level |
| ⌘-click | Select inside groups directly |
| ⌘C / ⌘V | Copy or paste nodes. A Figma selection (⌘C in Figma) pastes as real layers: frames, groups, editable text, images (downloaded by the host) and vector art (rendered to images), one undo step. Pasting images (also Figma's Copy as PNG) or files creates media nodes, uploaded to the host like a drop; pasting text creates a note |
| ⌫ / Delete | Delete |
| Arrows / ⇧Arrows | Nudge 1 px / 10 px |
| ⌘Z / ⌘⇧Z | Undo / redo your own changes (agents' changes are theirs; revert an agent turn with `[revert]` in the chat) |
| ⇧1 / ⇧2 | Fit all / zoom to the selection |
| ⌘0 / ⌘+ / ⌘− | Zoom to 100% / zoom in / zoom out |
| Drop files | Import |

## Video
Videos play muted while hovered or selected (tiles big enough on screen show `▶ 0:30`). The selected video gets a transport under it: `[▶] 0:07 / 0:30 ───── [mute] [⤢]` (click or drag the line to scrub; hover shows the time). Arrows keep nudging, so seeking uses J / L and , / .

| Keys | Action |
|---|---|
| Space (tap) or K | Play / pause the selected video. Holding Space still pans |
| J / L | Back / forward 5 s |
| , / . | One frame back / forward (pauses) |
| M | Sound on / off (back to muted when you deselect) |
| ⏎ or double-click | Focus view: the video near-fullscreen with sound. ← / → previous / next video on the canvas, Space / K, J / L, , / ., M work there too; Esc or a click on the backdrop closes |

Agents never move your camera; only your own clicks (a node id, a chip, a batch line) pan.

## Out: files into other apps
Selected media can go to Finder, Premiere, Slack, Mail and other apps as real files
(docs/ASSETS-V1.md §4). The desktop app hands out human-named copies from `<project>/.out/`
(APFS clones, never hardlinks into the asset store).

| Keys | Action |
|---|---|
| Drag a node out of the window | Desktop: a native file drag of the selection (the move on the canvas snaps back) |
| Drag `[⇲]` by the selection | The same from a handle. Web build: Chrome drags the file (`DownloadURL`), a click downloads |
| Right-click a node | Menu: copy file, copy image, open in… (pinned creative apps first), reveal in Finder, export…, mark final, show lineage, more… (⌘K) |
| ⌘⇧C | Copy the selection as files (⌘V pastes them in Finder, Slack, Premiere, Mail). Web: links |
| ⌘⇧E | Export panel for the selection: rows of preset · scale (`2x`, `1080p`, `512w`) · format, naming tokens, destination, files / zip / pdf, "save on node". ⏎ exports. With nothing selected: export every node with saved export settings |
| ⌘K | Also: copy fal URL / prompt / recipe, mark candidate / rejected, rerun, rerun with changes, apply recipe to selection, "export as <preset>", and ops on the selection (remove background, upscale, reframe to 9:16 / 1:1 / 4:5, trim, extract frames, make GIF, convert) |
| Esc | Leave the lineage view (deselects) |

Chat file cards (an agent's export) drag out like nodes and have `[reveal]`, `[open in]` and
`[copy]`. Dropping a node into a frame named "Deliverables" marks it final. Badges at a tile's
top-left show where it came from (`↓` import with a dot when uploaded to fal, `✦` generated,
`ƒ` derived, `▶` render, `★` final), on hover, on selection or on big tiles.

## Layers and properties (left panel)
| Keys | Action |
|---|---|
| Click / ⇧-click / ⌘-click | Select / add to the selection (synced with the canvas both ways) |
| Hover | Highlight the node on the canvas |
| ↑ ↓ | Move the selection between rows |
| → ← | Expand / collapse (← on a child selects its parent) |
| ⏎ or double-click | Rename |
| ⌫ / Delete | Delete the selection |
| ⌘F | Filter layers by name |
| inputs · generated · derived · finals · rejected | Filter chips under the header (combine with the name filter) |
| Properties → role | Set input / candidate / wip / final / rejected on the selection |
| Properties → provenance | Source and the `← op of <parent>` chain, "upstream changed", `[rerun] [rerun with changes] [copy recipe] [apply to selection]` |
| Drag | Reorder, or drop onto a frame or group to move into it (world position is kept); drop on the chat to attach |
| Properties fields | ⏎ applies, Esc reverts, ↑ ↓ step by 1 (⇧: 10) |

## Browser and desktop conflicts
- Browsers keep ⌘N, ⌘Y, ⌘1…9 and ⌘⇧[ ] for windows and tabs, so pages never receive them.
  Use the ⌃ aliases, the `+` / `≡` buttons, or the palette. In the desktop app (Tauri /
  WKWebView) the ⌘ forms work.
- ⌘+, ⌘− and ⌘0 are taken over on the canvas, so they zoom the canvas rather than the page.
- ⌘L, ⌘D and ⌘G are taken only while the canvas has focus (not while you type),
  so the browser's own address bar, bookmark and find shortcuts stay reachable from inputs.
