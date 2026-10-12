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
  numeric `input.seed` is incremented per request. Or pass explicit
  `jobs:[{model?, input, name?, mediaInputs?}]` — e.g. N distinct prompts from a variation
  matrix (subject × angle × light × style), or one job per still to animate, each with its own
  `mediaInputs` (`{"image_url": "<node>"}`).
- Packing: identical requests (same model + input, seeds aside) are merged by the host into
  fewer requests via the model's `num_images` when its fal schema has one (up to its maximum).
  You can also set `num_images` yourself.
- All placeholders appear at once in a grid in free space (inside a frame "<label> ×N" for 8+).
  Every job is submitted in parallel; results land progressively.
- It returns `{batch, frame, nodes}` right away. Don't block on it: carry on with other work.
  `await_batch(batch)` waits for the outcome (done/failed counts, errors) only when you need
  the results now.
- For "quick"/"fast" requests use a fast model such as `fal-ai/flux/schnell`.

### Per-item pipelines: `then`

`then:[{model|op, args?, input_from?, name?}, …]` runs steps on **each** output the moment it
lands — no agent round-trip between steps:

```
generate_batch(sessionId, canvas, model:"fal-ai/flux/dev", jobs:[…3 prompts…],
  then:[{"model":"fal-ai/kling-video/v2.1/standard/image-to-video",
         "args":{"prompt":"slow orbit, soft light","duration":"5"}, "input_from":"image_url"},
        {"op":"upscale"}])
```

- `model` steps get the previous output as `input_from` (default by kind: `image_url` /
  `video_url` / `audio_url`; a `*_urls` param gets a one-item list), merged into `args`.
  `op` steps run that asset op (`list_ops`) with the previous output as input and `args`.
- Each step is a new node next to the previous one, named "<previous> · <model/op>", with
  lineage (parent = the previous step). A failure stops only that item's chain.
- `transform` takes the same `then` (and then returns at once unless `wait:true`).
- The batch finishes when every chain has; `await_batch` lists the chains
  (`then.chains: [{source, nodes, status, error?}]`), and the `batch` progress event carries
  `then: {running, done, failed}`.

Progress: `read(root:<id>)` shows `status(queued|running|done|failed)` and `error(...)`.
On success the **same id** becomes a media node (`image`, `video`, `audio`, `model3d`) with
an `asset(...)` whose lineage records model, params, fal request id and you as the actor.

Etiquette: generations cost the user money. Generate the number of variants asked for,
don't loop on failures (read the `error` and report it), and prefer one good prompt over
many retries. Place variants next to the source and connect them with
`connector from(src) to(new) kind(lineage)` when it helps humans follow the work.
