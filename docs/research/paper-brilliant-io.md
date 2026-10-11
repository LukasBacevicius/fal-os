# Paper and Brilliant: assets in and out

**[I]** marks inference.

## Export
- **Paper:**
  - **Formats:** png, jpg, webp, avif, svg (SVG nodes only), pdf (desktop), mp4/webm (webm keeps alpha).
  - **Per-node export entries:** `{format, scale, suffix, quality, resampling, duration}`.
    - Scale grammar: `0.5x | 2x | 720p | 512w | 512h`; the default is 2x, and the last format used is remembered.
  - **Batch:**
    - "Export N elements"; a hotkey with nothing selected exports every node that has export settings.
    - Agents get the same with `nodes:"nodes-with-exports-only"`.
  - **Combined PDF:** pages in reading order (rows that overlap vertically by ≥15% are grouped, then sorted left to right). Quality presets: high = jpg 0.92 @2x, medium = 0.8 @1.5x, low = 0.6 @1x.
  - **Video:** fixed at 60 fps; 720p to 2160p; 1–300 s.
  - **Where files go:** straight to ~/Downloads with no dialogs, so batch exports don't stack Save dialogs. Collisions become `name (1).ext`; the Dock bounces and a toast says "Exported N files to Downloads".
  - **Naming:** `${layer}${@2x}.${ext}`.
- **Brilliant:**
  - **Formats:** PNG/JPEG/WebP/SVG (fonts embedded)/PDF, HTML ×3, React (tokens become CSS vars), MP4 (H.264/HEVC), MOV (HEVC with alpha, or ProRes 4444), and a Replay animation.
  - **Resolution presets:** 1x–4x, 720p–8K, social sizes, devices, custom. Fit/Fill/Stretch/Repeat; background transparent or canvas color.
  - **Batch:**
    - Multiple config rows run in one click.
    - Multi-select gives "separate files / single file". Only selection roots export, with `-2` suffixes; web builds download a zip.
  - **PDF:** multi-page (one page per top-level frame), with page reordering and per-page include toggles.
  - **Video:** 15/24/30/60 fps; refuses oversized frames up front.
  - **Defaults:** Cmd+E saves a PNG through a Save dialog with a timestamped name. Agent exports land in `Exports/`; a path ending in `/` uses the element name.
  - **Integrity:** each export reports its **sha256**, re-reads the file to verify, and refuses rather than writing a blank file.

## Out to other apps
- **Neither app supports drag-out.** This is a differentiator for fal-os.
- **Copy-as in Paper:** PNG (⌘⇧C), SVG, React+Tailwind, React+CSS.
- **Copy-as in Brilliant:**
  - PNG at device pixel ratio (and @2x/@4x), WebP, SVG, HTML, React, CSS, and its native format.
  - Plain ⌘C writes a PNG, or the **raw original bytes** when the selection is one untouched image.
- **Other targets in Brilliant:**
  - Send to Figma through a paired plugin, keeping things editable.
  - Sketch.
  - A portable package (relative asset paths).
  - Share links.
  - Reveal in Finder.
- **Round-trip:** neither app re-imports an externally edited file. The closest is paste-to-replace (⌘⇧R).

## In
- **Paper:**
  - **Entry points:** drop, paste (Figma via OAuth to fetch images, its own HTML and trees), URL images, and "capture from element" (rasterizes a node to WebP).
  - **`paper-asset://` scheme:** serves images only, with path hardening.
  - **Limits:** 25/100 MB uploads; HEIC converted to JPEG.
  - **Storage:** URL images dedupe by a hash of url + file id. Assets are scoped per file. Each image has a status (uploading / processing / ready / error) and a "Missing image" placeholder.
  - **Image record:** `{name, ext, w, h, hasTransparency, descriptionShort/Long (AI captions), prompt, modelId, relatedImageIds}`.
- **Brilliant:**
  - **Entry points:** ⌘⇧O, paste, drop, Figma by URL (OAuth), Sketch. Native conversion of TIFF/HEIC/AVIF.
  - **Storage:** `Assets/` with `.thumbs/`, plus "Clean up unused assets".
  - **Fill picker:** Replace, drop, URL, and a recent-images row.
  - **Missing assets:** shown as messages, with relinking.

## Provenance
- **Paper:**
  - Stores prompt + modelId + related images on the image record. Every generated image offers "Start over from this prompt" and "Copy prompt".
  - Selecting images automatically switches to image-to-image; a variety mode runs one prompt through several models.
  - Agents are told: each variant is a new node with the original as reference; never overwrite the original.
- **Brilliant:**
  - Remove background keeps the original inside the fill, so "Restore background" works later.
- **Neither app** has an explicit AI-generated flag.

## Agent access
- **Paper:**
  - `export` returns `{nodeId, filePath}`; `get_fill_image` returns a downscaled JPEG for model input plus the original URL.
  - Generation is async: set a `paper-gen://` URL on a node, then poll its status.
- **Brilliant:**
  - `export` returns stills inline unless a path is given, and writes a file for video. It reports path, dimensions and sha256.
  - `generate_image` and `remove_background` return the element and the asset path.

## Adopt
1. **One content-addressed store with rich per-asset records:** origin, prompt/model/params, parent refs, status, dimensions, alpha. Dedupe, missing placeholders, clean-unused.
2. **Per-node export settings:** a scale grammar and `@2x` naming, multiple config rows, separate files or one, and "export all marked".
3. **Agent export returns `{nodeId, path, w, h, sha256}`:** default to `Exports/`, verify after writing, refuse blank output.
4. **Async generation into placeholder nodes;** variants are new nodes with lineage; "Start over from this prompt" and "Copy prompt".
5. **Native drag-out plus Open in… and Reveal.** The differentiator, since neither competitor has it.
6. **Copy-as:** PNG at DPR and @2x, the raw original for a single untouched image, SVG, and the native format.
7. **Downloads-style collision naming** and a toast with Reveal; no stacked dialogs.
8. **A hardened local-file scheme** for agents.
9. **Combined PDF** in reading order, with page toggles.
10. **Keep originals for non-destructive AI edits,** and re-import an edited file as a replacement that keeps the node id. Neither app has this [I].
