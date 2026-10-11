# Results: Assets v1 ("bring it in, generate, take it out")

Status as of 2026-10-11. Everything is merged to `main` and pushed, and the live host and web app run the final build. The definition of done is ASSETS-V1.md §8.

## Definition of done: checklist
| # | Item | Status |
|---|---|---|
| 1 | Provenance: imports are `src(import) role(input) protected`; agents can't change them; derived assets carry lineage and a recipe; `rerun` works | ✅ Host and doc tests; verified live in the end-to-end run |
| 2 | Ops from the UI menu and from Opus (remove background, upscale, reframe, trim, extract frames, GIF, convert, …) | ✅ Host ops tested on real media. The UI right-click menu and ⌘K call `/api/transform`. Missing: `burn_captions` (Homebrew ffmpeg lacks libass/drawtext) |
| 3 | Export presets with verified files, a manifest, and file cards in chat | ✅ 16 presets, sha256-verified, `exports/manifest.jsonl`. **Fixed after the end-to-end run:** deliverables now pair each preset with the final of matching aspect (no letterboxed extras), and export uses two-pass loudnorm |
| 4 | Desktop: drag a node to Finder, copy a file, Open in…, Reveal | ✅ Commands built and tested (clipboard file URLs; Open-in list: Figma, Keynote, Preview, QuickTime…). ⚠️ **A real mouse drag into Finder or Premiere still needs a person to try** |
| 5 | Library ⌘⇧L: all workspace assets, search, filters, drag onto the canvas; fal pulled read-only, writes if the key allows | ✅ Running live on your real assets. 7.7k items open in ~25 ms and scroll at 120 fps. **fal library writes work with your AGENT key** (tested and cleaned up) |
| 6 | Opus end to end: "make a 10s teaser from my clip and photos, deliver 16:9 and 9:16" | ✅ **9/9 checks** on the final merged build (see below) |
| 7 | Merged, tests green, live restarted, this doc | ✅ |

## The end-to-end run (real Opus 5.5, fal, Remotion)
Inputs: two of your photos (the screen corridor and the woman in the red coat) and a 5 s bottle clip, imported as user inputs into an isolated test workspace.

Result after 25 min (contact sheets: `scratchpad/results/teaser-16x9-sheet.jpg` and `teaser-9x16-sheet.jpg`):
- **The edit:** "OUT OF THE NOISE" over the corridor, then "INTO STILLNESS" over the red coat, then a flash cut to the bottle on the music hit, "FEEL THE QUIET", and an end card reading "A NEW FRAGRANCE · COMING SOON".
- **Music:** a new Stable Audio track, shifted so its hit lands on the bottle reveal.
- **9:16 cut:** its own layout, not a crop, with text kept clear of the Reels/TikTok interface.
- **QA:** an independent pass found 2 text-placement problems and 1 stutter; all were fixed in v2. Loudness is −14.2 LUFS on both deliverables.
- **Your inputs:** byte-identical and still protected. All 6 new media nodes carry lineage, for example "clip 30fps (minterpolate) ← clip.mp4".
- **Deliverables:** `…Teaser-16x9-v2_youtube-1080_1920x1080_v1.mp4` and `…Teaser-9x16-v2_reel_1080x1920_v1.mp4`, posted as file cards in chat. The v1 cuts are kept and marked rejected.
- **Extras:** an OTIO timeline (`teaser.otio`) for re-cutting in an editor, and the Remotion source.
- **Uploads:** only the music prompt went to fal; your photos and clip were processed locally.
- Copies of the two deliverables are at `scratchpad/results/teaser-16x9.mp4` and `teaser-9x16.mp4`.

## What shipped (all on `main`)
- **Provenance:** `source` (import/generate/derive/render) and `role` (input/candidate/wip/final/rejected), `protected` enforced inside the doc model, and lineage with recipe, parent roles and costs. Old projects migrate automatically.
- **Ops** (host op registry; UI, Opus and external MCP agents share it):
  - inspect: probe, contact_sheet, palette, loudness, scenes, compare;
  - transform: crop, resize, reframe (pad-blur, crop, outpaint via `fal-ai/bria/expand`), convert, compress, trim, concat, speed, extract_frames, gif, audio normalize/mix/extract/strip, remove_bg (`fal-ai/birefnet/v2`), upscale (`fal-ai/seedvr`), inpaint (`fal-ai/flux-pro/v1/fill`), and the ffmpeg escape hatch;
  - compose: grid, contact_sheet;
  - rerun with overrides, or applying a recipe to other nodes.
- **Export and package:** 16 presets (reel/tiktok/short/square/ig-feed/youtube/x/linkedin/prores/alpha/gif/web-image/wav/mp3/original) and Paper's scale grammar. Verified files, a manifest, and project and deliverables `.zip` packages with a lineage manifest.
- **Generation with references:** `mediaInputs` take nodes or chips; a region crops, a frame extracts a still, a range trims.
- **Wider import:** svg, pdf, glb/usdz, heic/tiff/exr (shown as proxies, originals kept), srt/vtt, LUTs, fonts, md/txt.
- **Library:** a SQLite index across the workspace; smart thumbnails (video posters by best frame, hover strips, audio waveforms); fal sync (pull, plus push with an Idempotency-Key, per-project collections attached to fal Agent projects); search by text, `#hex` color, "similar to" and fal semantic search; a minimal mono grid; and agent tools `search_assets`, `get_asset`, `place_asset`, `tag_assets`, `collect_assets`, `show_assets`.
- **Out:**
  - drag-out of nodes, the `[⇲]` grip and chat file cards;
  - copy file or image;
  - Open in… and Reveal;
  - an 8-item right-click menu (everything else in ⌘K) and an export panel (⌘⇧E);
  - human-named APFS clones in `.out/` (never hardlinks; `project.duplicate` was fixed too).
- **Opus swarm:**
  - asset rules: protect inputs, derive with lineage, library first, candidates vs rejected;
  - a deliverables mindset with sensible default platforms;
  - new `editor` and `finisher` roles;
  - `deliver` and `share_files` tools;
  - the motion agent writes OTIO;
  - it adapts to the tools the host actually offers.
- **Sharing:** `dist/fal-os-0.1.0-arm64.dmg` (63 MB, ad-hoc signed; git-ignored). See [PACKAGING.md](PACKAGING.md) for colleague install steps: `xattr -dr com.apple.quarantine /Applications/fal-os.app`, then paste a fal key on the first-run screen, with Claude Code on their own login.

## Known gaps and next steps
- **Drag-out with a real mouse** into Finder or Premiere is untested. If WKWebView drops pointer events outside the window, the `[⇲]` grip and file cards still work.
- **burn_captions, transcript, describe and storyboard PDF** aren't built. Captions need an ffmpeg with libass; bundling one is worth doing.
- **Editor and finisher roles** are defined and tested but weren't called in the small runs; the director did that work itself.
- **OTIO** is hand-written JSON and hasn't been opened in Premiere or Resolve yet.
- **Revert/redo** only covers turns since the host started.
- **Library:** no collection browser UI yet; uploads over 90 MB need multipart.
- **Packaging:** no auto-update; colleagues repeat the `xattr` step until there's a fal Developer ID and notarization. Remotion needs a company licence.
- **Disk:** the Mac is at 99% (about 5 GB free). Builds share one build folder, and I removed about 40 GB of rebuildable caches during the session. Freeing space elsewhere is recommended.
