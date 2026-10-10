# Generation (fal models)

`generate(sessionId, canvas, model, input, placement?)` returns a node id **immediately**:
a `generation` node with `status(queued)`. The fal queue job runs in the background.

- `model`: a fal endpoint id, e.g. `fal-ai/flux/dev`, `fal-ai/kling-video/v2/master/text-to-video`.
- `input`: the model's JSON input, e.g. `{"prompt":"red car at dusk","image_size":"landscape_16_9"}`.
  For image-to-X models pass a URL (from an asset's `uri`) as the model expects.
- `placement`: `{x, y, w, h, parent}`; defaults to the right of existing content.

Progress: `read(root:<id>)` shows `status(queued|running|done|failed)` and `error(...)`.
On success the **same id** becomes a media node (`image`, `video`, `audio`, `model3d`) with
an `asset(...)` whose lineage records model, params, fal request id and you as the actor.

Etiquette: generations cost the user money. Generate the number of variants asked for,
don't loop on failures (read the `error` and report it), and prefer one good prompt over
many retries. Place variants next to the source and connect them with
`connector from(src) to(new) kind(lineage)` when it helps humans follow the work.
