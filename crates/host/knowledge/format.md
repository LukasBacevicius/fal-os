# Canvas text format

`read` returns it and `edit` accepts it. One row per node; children are indented 2 spaces;
sibling order is z-order (first row = bottom).

```
jbuzws frame "Moodboard" s(2000,1200) by(@lukas)
  881s9r image "red car v3" p(40,80) s(1024,1024) asset(sha:9f86) by(@claude)
    ! yflw1y open box(10,20,200,100) to(@codex) by(@lukas)
      > @lukas "make the sky stormier"
  b59dlc generation "variation" status(running) model(fal-ai/flux/dev) by(@claude)
```

Node row: `<id> <type> "<name>" <props…>`. Types: `image video audio text model3d vector
document embed timeline stream generation frame group note connector style:<palette|lut|font|character|style_ref|lora>`.
Common props: `p(x,y) s(w,h) r(deg) locked`. Text/notes: `text("…")`; notes also `color(#hex)`.
Connectors: `from(id) to(id) kind(lineage|reference|flow) label("…")`.
`by()`, `upd()`, `more(n)` are read-only.

## Editing rules
- **No id → create.** Parent = nearest less-indented row above (can be an existing id row).
  `note "todo" p(0,0) s(200,120) text("hi")`
- **Known id → change only the props you name.** `881s9r p(60,80)` moves it. Echoing an
  unchanged row is a no-op. Type can't change.
- `key(null)` removes a prop. `881s9r parent(jbuzws,0)` reparents/reorders.
- `delete(id,id)` on its own row deletes nodes (with subtrees) or annotations.
- One `edit` call = one transaction = one undo step. If any row fails nothing is applied and
  the error names the line.

Values: numbers, `true/false/null`, bare strings `[A-Za-z0-9_.:/@-]`, quoted strings with
`\" \\ \n` escapes, lists `crop(0,0,512,512)`, JSON for objects `params({"seed":7})`.

Use `read` with `detail:"blurry"` for a cheap overview, then `root:<id>` for detail.
