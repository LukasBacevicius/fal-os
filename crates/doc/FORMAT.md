# fal-os canvas text format

The same format is used to **read** a canvas (`Canvas::read`) and to **edit** it
(`Canvas::apply_edit`). Each row is one node, annotation, message or directive. Indent
children by 2 spaces. Sibling order is z-order: the first row is at the bottom.

```
jbuzws frame "Moodboard" s(2000,1200) by(@lukas)
  881s9r image "red car v3" p(40,80) s(1024,1024) asset(sha:9f86) by(@claude)
    ! yflw1y open box(0.1,0.2,0.4,0.3) to(@codex) by(@lukas)
      > @lukas "make the sky stormier"
  g466bb video "drive" p(1100,80) asset(sha:aa) in(1.5) out(4) loop muted by(@lukas)
    ! 5zjgm0 open frame(4.2) box(0,0,0.5,0.25) by(@lukas)
  b59dlc generation "variation" status(running) model(fal-ai/flux/dev) progress(0.4) params({"prompt":"red car"}) by(@claude)
```

## Node rows
`<id> <type> "<name>" <props…>`

- **id**: 6 chars `[a-z0-9]`, stable. Leave it out to create a node.
- **type**: `image video audio text model3d vector document embed timeline stream
  generation frame group note connector`, and `style:<palette|lut|font|character|style_ref|lora>`.
- **name**: quoted. Leave it out when the name is empty.
- **common props**: `p(x,y)` position, relative to the parent node (top-level nodes use canvas coordinates; moving a frame moves its children), `s(w,h)` size, `r(deg)` rotation, `locked`.
  A prop that is 0 or false is left out.
- **type props** (printed in this order):

  | type | props |
  |---|---|
  | image | `asset crop(x,y,w,h)` |
  | video | `asset crop in out loop muted` (in/out are seconds) |
  | audio | `asset in out loop muted` |
  | model3d, vector | `asset` |
  | document | `asset page` |
  | embed | `url asset` |
  | timeline | `duration` |
  | stream | `url model muted` |
  | generation | `status(queued\|running\|done\|failed\|canceled) model job progress params({json}) asset cost error` |
  | style:* | `asset data({json})` |
  | text | `text("…")` |
  | note | `color text("…")` |
  | connector | `from(id) to(id) kind(lineage\|reference\|flow) label` |

  Unknown props are kept, and the edit result warns about them.
- **read-only**: `by(@actor)` (creator), `upd(@actor)` (last editor, shown when it differs
  from the creator), and `more(n)` (descendants hidden by the depth limit). Edits ignore
  all three: attribution always comes from the actor who makes the edit.

### Values
`key(value)`:
- number: `12`, `0.4`
- `true` / `false` / `null`
- a bare string, using the characters `[A-Za-z0-9_.:/@-]`: `sha:9f`, `fal-ai/flux/dev`
- a quoted string with `\" \\ \n \t` escapes: `"a b"`, `"123"`
- a list with two or more scalars, separated by commas: `crop(0,0,512,512)`
- JSON for objects and one-element lists: `params({"seed":7})`, `out(["a1b2c3"])`

A bare word is a `true` flag, e.g. `loop`. Writing `key(null)` removes the prop.

## Annotation rows
An annotation row sits one level below its node, before the node's children. The thread
follows as `>` rows one level deeper.

`! <id> <status> <anchor> [to(@actor)] [out(id,…)] [by(@author)]`

- **status**: `open`, `in_progress` or `resolved`.
- **anchor**: 2D coordinates are node-local and normalized (0..1 of the node's width and
  height, so they survive resizing; multiply by the asset's pixel size for media pixels); times
  are seconds:

  | anchor | syntax |
  |---|---|
  | point | `pt(x,y)` |
  | box | `box(x,y,w,h)` |
  | polygon | `poly(x1,y1,x2,y2,x3,y3,…)` |
  | brush mask | `mask(<asset id>)` |
  | time | `t(s)` |
  | time range | `range(t0,t1)` |
  | frame + region | `frame(t) [pt\|box\|poly\|mask]` |
  | text range | `chars(start,end)` |
  | page + region | `page(n) [pt\|box\|poly\|mask]` |
  | 3D point | `pt3(x,y,z) cam(px,py,pz,tx,ty,tz,fov)` |

- `to(@actor)` assigns the annotation, and `to(null)` unassigns it. `out(…)` lists the node
  ids produced in response.
- Messages: `> @author "text"`.

## Editing
`apply_edit(text, actor, expected_version)` runs as **one transaction and one undo step**.
If any row fails, nothing is applied, and the error names the line.

- **A row with a known id changes only the props it names.** `881s9r p(60,80)` moves the
  image and leaves everything else as it was. Echoing an unchanged row back does nothing.
  The type cannot change.
- **A row without an id creates a node.** Its parent is the nearest less-indented row
  above it, which can be an existing id row: `881s9r` followed by `  note "hi"` adds a
  child note. The new node goes on top of its siblings.
- **A row with an unknown id and a type creates that node with that id.** This is why
  read → apply into an empty canvas → read gives the same text.
- `parent(<id>|root[,index])` on an id row moves (reparents or reorders) the node.
- `delete(<id>,<id>…)` on its own row deletes nodes (with their subtrees) or annotations.
- **Annotations:**
  - `! box(…) "text"` under a node creates an annotation whose first message is the
    quoted text.
  - `! <id> resolved out(<node>)` updates an existing annotation, and needs no indentation.
  - `on(<node id>)` sets the node explicitly.
- **Thread rows:**
  - `> "text"` under an annotation adds a reply.
  - `> @author "…"` rows are echoes of an existing thread and are ignored, so sending a read
    back never duplicates messages.
- Lines starting with `#` are comments.

The edit result has the new `version`, the `created` ids, the `touched` ids, any
`warnings` (unknown props, `no changes`), and `stale`. If `expected_version` was passed and
other actors have changed the canvas since then, `stale` lists who changed which ids
("changed since your read: 881s9r by @lukas") and flags the ids that this edit also
touched. The edit is applied anyway.

## Guarantees
- **Deterministic output.** Rows follow tree order, annotations follow creation order, props
  follow the table order and then unknown props alphabetically. Numbers use their shortest
  form.
- **Round-trip.** read → apply on an empty canvas → read is byte-identical when the edit is
  applied by the same actor. With `ReadOpts { attribution: false }`, it is byte-identical
  for any actor.
