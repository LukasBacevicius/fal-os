# Atlas 1.7.1 (asset library): teardown

Static analysis: strings, symbols, plists, the extension JS, a SKILL.md embedded in the binary, and debug settings text. **(I)** marks inference.

## What it is
- **App:** native macOS, SwiftUI plus AppKit and Core Animation (no Metal).
- **Content:** boards of images, video, GIFs, link cards and colors.
- **Extras:**
  - an `atlas` CLI;
  - Safari/Chrome/Firefox extension and a Share extension;
  - a local MCP server with about 80 tools;
  - a Figma plugin;
  - an iPhone companion that syncs via iCloud.

## Storage
- **One library file:** a single Codable `library.json` inside a `*.atlaslibrary` package; no database.
  - Top level: `items, collections, linkedFolders, smartFolders, manualTags, stacks…`.
  - Folders: `originals/`, `thumbnails/`.
- **Item fields:**
  - `kind`, file names, dimensions/duration, dates, `tags`, `sourceURL`, `annotation`, `author`, `liked`;
  - `folderIDs` and stacks;
  - `contentHash` (SHA-256);
  - links into watched folders;
  - trash with original locations;
  - link-card fields, color hex, carousel pages.
- **Placements are separate from items:** one item can sit on many boards. Duplicates by hash become "virtual copies" (many items, one blob). Watched folders are referenced in place.
- **Derived indexes:** versioned sidecars, never synced, rebuilt by bumping a version:
  - `colors-v5.bin`, `embeddings-v1.bin` (SigLIP), `featureprints-r2.bin` (Vision similarity), `query-cache`.
- **Sync** works by placing the library in iCloud or Dropbox, with handling for conflicted copies.
- **Safety:** debounced saves; a snapshot before an agent's first write; recoverable trash; permanent delete is human-only.

## Ingestion
- **In the app:** drop, paste (images, URLs, hex colors), menu-bar pin, Share extension, CLI `atlas add`, MCP import and save-from-URL.
- **Browser extension:**
  - fetches media bytes *inside the page*, so the user's cookies apply;
  - posts to a loopback bridge at `127.0.0.1:50473/add`, with a deep-link fallback;
  - saves page snapshots;
  - on drag, a panel of collections springs up for dropping.
- **Resolvers:** X, Instagram (carousels), Pinterest, YouTube up to 1080p. Link previews come from LinkPresentation plus WKWebView snapshots.
- **Other sources:** source URL from `kMDItemWhereFroms`; RAW files; connected folders; Eagle import.
- **Dedup:** by hash at import (skip, add anyway, or apply to the rest of the batch); "remove duplicates" by dimensions plus size.

## Performance
- **One renderer for everything:** a single custom layer-based canvas view for grid, canvas and infinite modes.
- **Band virtualization:**
  - **materialize margin:** tiles created N screens ahead;
  - **keep margin:** tiles recycled only past this, which is always larger (hysteresis);
  - **prefetch:** loading across the whole band.
  - Load passes trigger on **camera travel distance**, not every frame. Tiles loaded off-screen skip the crossfade.
- **Decoding:**
  - `CGImageSourceCreateThumbnailAtIndex` with a max pixel size, off the main thread;
  - separate caches for thumbnails, full images, a capped GIF cache and peeks, with a cost ledger and limit;
  - decoded caches dropped under memory pressure.
- **Thumbnails:**
  - pre-generated files, with video posters from mid-clip;
  - a background thumbnail-repair job;
  - the agent API serves 128–1024 px (default 512).
- **Grid zoom:** justified rows within min/max row height; the camera scales during a pinch and rows re-pack on release, optionally anchored under the pointer.
- **Motion:** spring animations, with the ripple weakening above a tile-count threshold; far tiles "fly in first".
- **Video:** hover/scrub playback only, players pooled.
- **Perf HUD:** CPU, GPU, memory, hitches, live tiles and caches, turning orange at 70% and red at 90% of a limit.

## Search and organization
- **Semantic search:** SigLIP2 ViT-B-16, 8-bit Core ML, models downloaded on demand. Embeddings keyed by content hash, cosine top-N via Accelerate, with a query cache. Bring-your-own embeddings are supported.
- **Find Similar:** a separate visual index built on Vision feature prints.
- **Color:** 8 dominant/accent colors per item; hex search in the search box or ⌘K; opening a color swatch searches by that color.
- **OCR:** feeds experimental tag suggestions only, not search.
- **Smart folders:** rules over type, format, size, dimensions, orientation, duration, date added, liked, collection, source and tags, with all/any matching. The same rules power the filter bar.
- **Structure:**
  - **System views:** Inbox (the default import target), Likes, Links, Colors, per-source views, Trash.
  - **Grouping:** collections, folders, stacks.
  - **Tags:** ordered, with rename-to-merge.

## Browsing
- **Modes:** Grid (justified), Canvas (freeform groups), Infinity.
- **Opening an item:** the tile glides to center on a spring while neighbours scatter. Arrow keys move between items; zoom options are fit, fill or actual size.
- **Video:** hover-to-play, scrub, frame step, save a frame as a new item.
- **Keyboard:** ⌘K palette, rebindable shortcuts.
- **Inspector:** EXIF, palette, tags, note, source, collections.

## Out
- **Drag out:** custom pasteboard types plus file URLs.
- **Actions:** copy, copy link, export to folder, Open With…, Reveal in Finder, set as wallpaper.
- **CLI / MCP:** `get_file`, plus `atlas://item/<id>/original|thumbnail` resources.
- **Figma plugin:** places images or fills layers, and sends exports back.
- **Publishing:** a board as a read-only web page with "Copy to Atlas".

## Inputs vs. outputs
Atlas has no generated-vs-reference model. The closest things are source URL and author, and the Inbox as a triage stage. **fal-os must go further:**
- record each asset's origin: import or generation;
- for generations, keep the model, prompt, seed, input hashes and parent job;
- add Generated and References system views;
- show a "used as input in" graph.

## Adopt for fal-os (priority)
1. A virtualized tile renderer with hysteresis bands and loads triggered by camera travel; one renderer for grid, canvas and infinite.
2. The thumbnail pipeline as the backbone: several sizes, decoding off-thread, byte-budget caches, a repair job, and never decoding originals in the grid.
3. Content-addressed identity with virtual copies, and placements kept separate from items.
4. Versioned derived indexes (embeddings, palettes, similarity) as rebuildable sidecars keyed by hash and model id.
5. Semantic plus similar-image plus color search.
6. Inbox and Trash (with origins); snapshot before bulk or agent writes.
7. Smart folders as rule JSON, shared with the filter bar and the agent API.
8. First-class agent and CLI tools: search, view, show_items, batched writes, and destructive operations that are human-only.
9. Capture everywhere: an extension using the page's cookies, a loopback bridge, resolvers, automatic source URLs.
10. Provenance beyond Atlas (origin, recipe, lineage graph).
11. A perf HUD with thresholds.
