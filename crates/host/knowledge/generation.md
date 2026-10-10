# Generation (fal models)

`generate(sessionId, canvas, model, input, placement?)` returns a node id **immediately**:
a `generation` node with `status(queued)`. The fal queue job runs in the background.

- `model`: a fal endpoint id, e.g. `fal-ai/flux/dev`, `fal-ai/kling-video/v2/master/text-to-video`.
- `input`: the model's JSON input, e.g. `{"prompt":"red car at dusk","image_size":"landscape_16_9"}`.
  For image-to-X models pass a URL (from an asset's `uri`) as the model expects.
- `placement`: `{x, y, w, h, parent, near}`. Without x/y the node goes into **free space next
  to** `near` (node ids), else next to what the user has selected, else right of the content.
  It never lands on top of existing nodes.

## Many outputs: `generate_batch`

For more than ~3 outputs, make **one** `generate_batch` call, never a loop of `generate`:

```
generate_batch(sessionId, canvas, model:"fal-ai/flux/schnell",
               input:{"prompt":"pizza, studio photo","image_size":"square_hd","num_images":4},
               count:50, variations:["margherita","pepperoni","truffle"])
```

- `count` requests with the shared `input`; `variations` cycle through `input.prompt`; a
  numeric `input.seed` is incremented per request. Or pass explicit `jobs:[{model?, input, name?}]`.
- `num_images` (when the model supports it) returns several outputs per request: fewer requests.
- All placeholders appear at once in a grid in free space (inside a frame "<label> ×N" for 8+).
  Every job is submitted in parallel; results land progressively.
- It returns `{batch, frame, nodes}` right away. `await_batch(batch)` waits for the outcome
  (done/failed counts, errors) when you need the results; otherwise carry on.
- For "quick"/"fast" requests use a fast model such as `fal-ai/flux/schnell`.

Progress: `read(root:<id>)` shows `status(queued|running|done|failed)` and `error(...)`.
On success the **same id** becomes a media node (`image`, `video`, `audio`, `model3d`) with
an `asset(...)` whose lineage records model, params, fal request id and you as the actor.

Etiquette: generations cost the user money. Generate the number of variants asked for,
don't loop on failures (read the `error` and report it), and prefer one good prompt over
many retries. Place variants next to the source and connect them with
`connector from(src) to(new) kind(lineage)` when it helps humans follow the work.
