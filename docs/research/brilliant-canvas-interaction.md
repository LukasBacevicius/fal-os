# Brilliant: canvas interaction spec (polish checklist for fal-os)

Sources: Brilliant's built-in docs written for its agent (`.claude-prod/knowledge/reference/*`: canvas, editing, frames, crop, tools, text, shortcuts, ui), its 553 `*Command` classes, and read-only renders.

- **[D]** = documented by Brilliant.
- **[I]** = suggested value; Brilliant doesn't document it.
- **Colors:** fal-os stays monochrome (UI-SPEC). Selection is white, hover dim, guides theme-gray. Chat colors are used only for agent activity.

## Camera
- **Zoom limits:** 2%–25600%. ⌘= / ⌘- step ×2 / ×0.5, centered on the viewport.
- **Digit keys:** 1–9 = 100–900% (Move/Hand only, with no text focus). Pressing the same digit twice resets to 100%. 0 toggles between 100% and the previous zoom (200% if there is none).
- **Pinch / ⌘-scroll:** zooms around the cursor. Animated zooms (fit, to-selection, center) ease and can be retargeted mid-flight.
- **Fit and center:** fit all ⌘⌃A; zoom to selection ⌘⌃F (falls back to fit-all); center ⌘⌃C. All of them frame content inside the area left visible by the panels.
- **Per-canvas memory:** each canvas remembers its camera, and a canvas with content auto-fits the first time it opens.
- **Pan:**
  - Wheel pans vertically, ⇧wheel horizontally.
  - Two-finger trackpad drag pans.
  - Space, middle mouse or H gives a temporary hand; the previous tool comes back on release.
  - Trackpad and hand-flick pans carry **momentum**, stopped by any click or scroll. The mouse wheel has no momentum.

## Hit testing
- **4 screen-px tolerance** at every zoom, against true geometry. The topmost element wins.
- **Top-level frame:** the label selects it; a click on the body passes through to the children; an empty frame selects by its body; ⌘ on the body selects the frame.
- **Groups:** hit only through their children; empty space between children is not hittable.
- **Selection and hover boxes** hug the geometry and are not inflated by stroke.

## Selection
- **Click:** selects the **outermost layer below the top-level frame**. Depth is sticky: clicking a sibling selects at the same depth. Double-click goes exactly one level deeper. Enter selects children, ⇧Enter the parent. ⌘-click goes to the deepest layer.
- **Hover:** previews exactly what a click would select, and with ⌘ held, the deepest layer.
- **Frame and descendant** are never selected together.
- **Multi-select:** ⇧-click toggles; ⌘A selects all top-level elements; Tab / ⇧Tab cycle siblings, wrapping around.
- **Marquee:**
  - top-level layers are selected on touch;
  - a frame with children is selected only when **fully enclosed**; otherwise its touched children are;
  - ⇧ adds;
  - ⌘-drag is the deep marquee, even when it starts on an element.
- **"Select layer" context menu:** lists every layer under the cursor.
- **Escape ladder,** one context per press: close menu → exit edit mode → back to the Move tool, keeping the selection → clear the selection.
- **Layers panel:** reveals every canvas selection (expands parents, scrolls the row into view).
- **Locked elements** are click-through.

## Move and drag
- **Drag threshold** about 3 px [I].
- **⇧-drag:** locks to the axis with the larger delta.
- **⌥-drag duplicates:** the original snaps back and the copy follows. Releasing ⌥ mid-drag cancels the copy. The whole drag is one undo step.
- **Reparenting while dragging:** leaving a frame reparents immediately; entering one waits until drop, with the target frame highlighted. Space blocks reparenting. Groups are not drop targets.
- **Nudge:** 1 px, ⇧ = 10 px. A run of nudges coalesces into one undo step after 1 s idle. In auto layout, arrows reorder instead.

## Snapping (the biggest quality lever)
- **Per-axis:** X and Y snap independently; the closest target wins on each axis.
- **Hysteresis:** once snapped, an axis stays snapped until the pointer leaves a widened release band (about 1.5× the snap distance [I]) or a strictly closer target appears.
- **Targets offered:**
  - same edge ↔ same edge (left↔left, …) and center ↔ center;
  - flush edge-to-edge only within about **2 px**;
  - **never edge ↔ center**;
  - recently touched elements rank higher.
  - Snap distance about 6 screen px [I].
- **Snap types:** alignment, equal gaps, equidistant (3 or more), size match, frame edges. ⇧ snaps rotation to 15°.
- **Scope:** only siblings in the same parent; never across frames; never off-screen elements.
- **Guide drawing:** solid for alignment, dashed with px labels for spacing. Guides show where the element *landed*.
- **Pixel snap:** on by default for move and corner resize.
- **⌘ held mid-drag** suspends all snapping, including pixel snap.

## Resize and rotate
- **Handles:** 8 px squares with a 16 px grab area. **Edges resize along their full length** (±5 px).
- **Small objects:** handles sit outside small objects, so dragging the middle moves them. The squares are hidden below 8 px on screen.
- **Modifiers:** ⇧ keeps aspect; ⌥ resizes from the center; ⌘ keeps image content fixed (crop-compensate).
- **Flip:** dragging past the anchor flips smoothly.
- **Media:** keeps aspect by default (our choice, for media nodes).
- **Rotate zone:** diagonally outside the corners (up to about 16 px), never just outside an edge. ⇧ snaps to 15°.
- **Cursors:** axis-aware resize and rotate cursors.

## Double-click and Enter
- **Containers:** go one level deeper.
- **Text:** edits with all text selected.
- **Image:** enters crop mode (30% ghost of the full image, corner brackets, pan and scale the image inside the box; Enter or Esc commits). Enter never crops a frame.
- **Frame label:** renames.

## Text
- **Exiting:** Enter, Esc or clicking outside all commit (Esc does not revert); ⇧Enter inserts a newline.
- **Empty text is deleted** on exit.
- **Layer name** follows the content until the user renames it.
- **Inside an edit:** double-click selects a word, triple-click a line.

## Media in
- **Drop / paste / import:** paste lands at the cursor, import at the viewport center. Images fill (cover) by default.
- **Paste to replace:** ⌘⇧R.
- **Copy:** a single image copies its raw bytes.
- **Slow paste:** shows a "Pasting…" progress toast.

## Clipboard and duplicate
- **Paste source:** internal if the system clipboard is unchanged since the last copy, otherwise external.
- **⌘D:** places the copy to the right with a 40 px gap, in the first free slot, and selects it.
- **Internal paste:** goes at the cursor. If frames are selected, pastes into each of them.

## Measure
- **⌥-hover** with a selection: gap distances to the hovered element, or paddings when hovering the parent. Stays live while nudging.

## Undo
- **Scope:** per canvas, with AI operations on a separate stack.
- **Granularity:** one drag = one step; nudges coalesce.

## Frame labels
- **Interaction:** click selects, double-click renames.
- **Declutter at low zoom:** small frames drop their label; on collision the widest frame wins; hovered and selected labels are always kept.

## Shortcuts to adopt
Tools V / H / F / T; Space hand; ⌘A; Tab / ⇧Tab; Enter / ⇧Enter; Esc ladder; ⌘R rename; ⌘⇧H hide; ⌫ delete; ⌘C / X / V; ⌘⇧R paste-replace; ⌘D; ⌘Z / ⌘⇧Z; arrows / ⇧arrows; ⇧H / ⇧V flip; ⌥⇧L / R / T / B / H / V align; ⌃⌥H / V distribute; ] / [ front / back; ⌘] / ⌘[ forward / backward; ⌘G group; ⌘⇧G ungroup; ⌘⌥G frame selection (our ⌘F is find); ⌘= / ⌘-; 1–9 and 0 zoom; ⌘⌃A / F / C fit / zoom to selection / center; ⌘⇧; snap guides; ⌘⇧' pixel snap; ⇧? shortcuts; ⌘K palette; ⌘L add to chat; / prompt.

## Top 30 to match, in priority order
1. Hysteretic per-axis snapping.
2. Selective snap targets (no edge↔center; flush only within 2 px; recent targets first).
3. ⌘ suspends snapping mid-drag.
4. Guides show the landed position: solid for alignment, dashed with labels for spacing.
5. Full-length edge resize, 16 px corner grab, handles outside small objects.
6. Rotate zone diagonally outside the corners, with proper cursors.
7. Outermost-first click, sticky depth, ⌘ deep, double-click one level.
8. Hover previews the click target.
9. 4 px true-geometry hit testing; group gaps not hittable.
10. Frame and descendant never selected together.
11. Marquee rules.
12. Zoom around the cursor with eased, retargetable animation, 2%–25600%.
13. Momentum pan (trackpad and flick), none for the wheel; ⇧wheel pans horizontally.
14. Space and middle-mouse temporary hand.
15. Fit, zoom and center respect the visible area; digit zoom keys.
16. Per-canvas camera memory and auto-fit on first open.
17. ⌥-drag duplicate semantics.
18. ⇧-drag axis lock.
19. Drag reparenting rules.
20. Nudge with coalesced undo.
21. Resize modifiers and flip through zero.
22. Media as cover fill; paste at the cursor.
23. Double-click an image to crop.
24. Escape ladder.
25. Enter / ⇧Enter / Tab hierarchy navigation.
26. Tools revert to Move after creating.
27. ⌘D to the right with a 40 px gap; paste at the cursor.
28. Pixel snap by default.
29. ⌥-hover measurement.
30. Frame labels: select, rename, declutter.
