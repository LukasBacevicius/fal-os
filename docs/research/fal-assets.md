# fal.ai assets: model, API and integration plan

Researched 2026-10-11 with read-only calls using the fal-os FAL_KEY (AGENT preset). Reads worked on both APIs; writes are untested with this key.

## Data model
- **Asset** (Agent SDK camelCase / Platform API snake_case):
  - `assetRecordId` is the id for all library writes (`asset_id` in the Platform API).
  - `vectorId` (search), `requestId`, `url` (fal.media, immutable, 60-day cache headers), `type` (image|video|audio|3d).
  - `title`, `endpoint`, `prompt`, `width`/`height`/`size`/`contentType`, `source` (`upload` | `response`), `isFavorited`, `collectionIds`, `tags`, `createdAt`.
  - No thumbnails or variants, no `updatedAt`, no seed or params; full inputs come from request history.
- **Uploads vs generated:**
  - Uploads: `source:"upload"`, `requestId:"upload:<hex>"`, `endpoint:null`, an automatic caption as the prompt (editable), dimensions often null.
  - Generated: `source:"response"`, a UUIDv7 `requestId`, an endpoint, the original prompt and dimensions.
- **Tag:** `{id, name (unique, lowercase), color}`.
- **Collection:**
  - Fields: `{id, type: manual|smart|<entity type>, name, description, icon, color, coverImageUrl, filters, parentCollectionId, assetCount, previewAssets}`. Collections nest.
  - Smart collections are filter trees (and/or over endpoint/status/type/source/created_at, plus semantic text/image/video match with `min_similarity`), at most 3 levels deep.
- **Entity:**
  - Fields: `{id, type: character|prop|environment|style|scene, name, handle (@-able), description, references[1–20 assets], thumbnail}`.
  - Each entity also has a gallery of linked media.
- **Lineage:** `GET /v1/assets/{id}/lineage` returns a graph.
  - Node kinds: asset, request (with endpoint and prompt), external_input.
  - Edges: `generated_by` / `input_to`, with roles such as `source`.
  - Ancestors only, at most 3 hops; deleted or expired nodes are tombstones.

## API surface
- **Agent SDK library** (`@fal-ai/client` `fal().agent.library.*`): `https://fal.ai/api/agent-v2/sdk/agent/library/{assets,collections,entities,characters,tags}` (reads take `?input=<json>`).
- **Platform API:** `https://api.fal.ai/v1/assets…`, `Authorization: Key …`.
- **List / search:**
  - Filters: `q`, `searchImageUrl`/`searchVideoUrl` (semantic), `mediaTypes`, `sources`, `section` (all-media | generated | uploads | favorites), `endpoints`, `collectionId` (+ recursive), character ids, `tagIds` + `tagMode`, `sortOrder`.
  - Paging: keyset cursor, `limit` ≤ 100.
  - There's no `requestId` filter.
- **Import a local file:**
  1. `storage.upload` (fal-cdn-v3; use `lifecycle.expiresIn:"never"` to keep it).
  2. `POST /v1/assets/uploads {url, type, prompt?, collection_id?, favorite?, tag_ids?}` with an `Idempotency-Key` header.

  fal does **not** dedupe.
- **Generated outputs:** indexed automatically as `source:"response"`, depending on the account's request-source settings.
- **Organizing:**
  - Collections: CRUD, nesting/move, add/remove assets.
  - Tags: CRUD, plus attach/detach on assets.
  - Entities: CRUD, plus gallery management.
  - Assets: favorite, and `updatePrompt` (uploads only).
- **Deleting** removes the library record but **not** the CDN file.
- **Agent projects:** `PUT /agent/projects/{id}/assets/{id}` and `…/collections/{id}`. `projects.resources(id)` returns attached and generated media, collections and characters.
- **Request history:** `GET /v1/models/requests/by-endpoint?endpoint_id=…&expand=payloads`. The index covers 90 days; payloads are kept 30 days by default.
- **Limits:** none documented beyond `429 rate_limited`.
- **Storage:** this account has `expiration_duration_seconds: null`, so files never expire. Saving to the library does not extend a file's lifetime.

## Integration plan
- **Source of truth:**
  - The **fal library** owns identity and metadata: prompt, tags, collections, favorites, entities, lineage.
  - The `fal.media` URL is the canonical remote copy.
  - Local content-addressed files are a cache, and the master only until pushed.
  - The host keeps a SQLite index: `sha256 → {assetRecordId, url, requestId, source, meta, syncState}`.
- **Projects:** each fal-os project gets a manual fal collection, nested under a "fal-os" parent and attached to the project's fal Agent project. Placing a library asset on the canvas adds it to the collection; removing it from the canvas never deletes the asset.
- **Write path:**
  - **Import:** hash, then skip if already known. Otherwise upload → register with Idempotency-Key = sha → add to the project collection.
  - **Generations:** auto-indexed. Find the `assetRecordId` by listing newest with the `endpoints` filter and matching `requestId`/`url`, retrying briefly. If nothing is found, register the URL as an upload.
- **Read path (lazy):**
  - Incremental pull newest-first, stopping at the last seen item.
  - No webhooks and no `updatedAt`, so favorites, tags, membership and deletions need a periodic reconcile.
  - Fetch metadata first; fetch the file when it's viewed or placed.
- **Thumbnails are ours:** the CDN ignores resize parameters. Generate 256/512 px webp and a video poster on first sight, cached by content hash. Keep thumbnails for everything, originals in an LRU, and always the originals of assets placed on the canvas.
- **Offline:** browse from the cached index and thumbnails. Queue writes in an outbox; uploads are safe to repeat thanks to the Idempotency-Key, while other writes need a read-back before re-sending. A 404 or tombstone means expired: fall back to the local copy.
- **Agents:** library tools go through the runtime, so FAL_KEY stays there. Use `assetRecordId` for library operations and `url` for model inputs.

## Open questions for fal
1. Does the AGENT preset allow **writes** to `/v1/assets/*` and `/agent/library/*`?
2. Do direct queue calls made with this key get indexed into the library, given the request-source settings?
3. Is there a public way to register an asset with `requestId`/`endpoint` provenance, and could list get a `requestId` filter?
4. What are the rate limits, and is there a change or deletion feed?

Sources: fal docs (Agent SDK library and projects, Platform API assets and uploads, assets retention, adding, access and history, request history, storage, media expiration); `@fal-ai/client@1.11.0-alpha.4` `src/agent/{library,projects,transport}.js`, `src/storage.js`.
