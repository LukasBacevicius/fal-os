# Keyboard shortcuts

The app can be driven without the mouse. Press `?` for this list in the app, and ⌘K for the
command palette, which fuzzy-searches actions, chats and nodes by name. ⌘ means Ctrl on
Windows and Linux. The source of truth is `SHORTCUTS` in `apps/web/src/keys.ts`.

Canvas shortcuts never fire while you type: in the chat input, a note being edited or the
palette. The shortcuts marked "everywhere" work in those places too.

## App
| Keys | Action |
|---|---|
| ⌘K | Command palette: actions, chats, nodes (everywhere) |
| `?` | Shortcut list |
| ⌘\\ | Show or hide the chat panel (everywhere) |
| ⌘⇧\\ | Show or hide the outline of frames and groups (everywhere) |

## Chat
| Keys | Action |
|---|---|
| ⌘L | Add the selection to the chat as context chips |
| ⌘I or `/` | Focus the chat input (⌘I works everywhere) |
| ⏎ or ⌘⏎ | Send. ⇧⏎ inserts a new line |
| Esc | In the input: stop the run if one is running, otherwise leave the input |
| `#` | In the input: complete a chat title and add that chat as context (`[#title]`) |
| ⌫ | In an empty input: remove the last chip |
| ⌘N or ⌃N | New chat (everywhere) |
| ⌘⇧[ ⌘⇧] or ⌃⇧[ ⌃⇧] | Previous or next chat (everywhere) |
| ⌘1…9 or ⌃1…9 | Switch to chat 1…9 (everywhere) |
| ⌥-click / ⌥-drag | Add a spot or region on an image. On a video it adds a frame at the playhead |

## Canvas
| Keys | Action |
|---|---|
| V | Select tool |
| H, or hold Space | Pan |
| N, or double-click empty canvas | New note at the cursor, in edit mode |
| ⏎ or double-click | Edit the selected note or text. On media, zoom to it |
| Tab / ⇧Tab | Select the next or previous node in reading order (rows top to bottom, left to right) |
| ⌘A | Select all |
| Esc | Deselect |
| ⌘D | Duplicate |
| ⌘G / ⌘⇧G | Group the selection / ungroup. Children keep their place, and a group always hugs its children |
| Click / double-click / Esc | A click on something inside a group selects the group. Double-click enters the group, and Esc goes back up a level |
| ⌘-click | Select inside groups directly |
| ⌘C / ⌘V | Copy or paste nodes. Pasting images or files creates media nodes, uploaded to the host like a drop; pasting text creates a note |
| ⌫ / Delete | Delete |
| Arrows / ⇧Arrows | Nudge 1 px / 10 px |
| ⌘Z / ⌘⇧Z | Undo / redo your own changes (agents' changes are theirs) |
| ⇧1 / ⇧2 | Fit all / zoom to the selection |
| ⌘0 / ⌘+ / ⌘− | Zoom to 100% / zoom in / zoom out |
| Drop files | Import |

## Browser and desktop conflicts
- Browsers keep ⌘N, ⌘1…9 and ⌘⇧[ ] for windows and tabs, so pages never receive them. Use
  the ⌃ aliases, or the palette. In the desktop app (Tauri / WKWebView) the ⌘ forms work.
- ⌘+, ⌘− and ⌘0 are taken over on the canvas, so they zoom the canvas rather than the page.
- ⌘L, ⌘D and ⌘G are taken only while the canvas has focus (not while you type),
  so the browser's own address bar, bookmark and find shortcuts stay reachable from inputs.

## Outline (left panel)
| Keys | Action |
|---|---|
| Click | Select and zoom to the frame or group |
| ⏎ or double-click | Rename |
| ⌫ / Delete | Delete the frame or group with its contents. To keep the children, use ⌘⇧G on the canvas |
| ↑ ↓ / → ← | Move between rows / expand or collapse |
| Drag | Reorder, or drop onto a frame or group to move into it (world position is kept) |
