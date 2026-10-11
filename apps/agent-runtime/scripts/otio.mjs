#!/usr/bin/env node
// Writes an OpenTimelineIO timeline (.otio JSON) for a render, so "Open in Premiere / Resolve"
// gives an editable cut instead of a flat MP4 (docs/ASSETS-V1.md §4 Editors). The motion agent
// writes a small edit list of what is on screen when, then:
//   node otio.mjs <edit.json> [--out <file.otio>]        (default: next to edit.json)
//
// edit.json:
//   { "name": "Headphones ad", "fps": 30, "width": 1920, "height": 1080, "render": "out/ad.master.mp4",
//     "tracks": [
//       { "kind": "Video", "name": "V1", "clips": [
//         { "name": "hero", "file": "/abs/public/hero.png", "start": 0, "duration": 2.5, "in": 0, "node": "a1b2c3" } ] },
//       { "kind": "Audio", "name": "A1", "clips": [ { "name": "music", "file": "/abs/public/music.mp3", "start": 0, "duration": 15 } ] } ] }
// `start` = timeline position (s), `in` = source in-point (s), `duration` (s). Gaps are filled.
// Text, graphics and effects from the composition aren't representable in OTIO: they stay baked
// into the render, which is referenced in the timeline metadata.
import { readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, extname, isAbsolute, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const rt = (seconds, rate) => ({ OTIO_SCHEMA: "RationalTime.1", rate, value: Math.round(seconds * rate) });
const range = (start, duration, rate) => ({ OTIO_SCHEMA: "TimeRange.1", start_time: rt(start, rate), duration: rt(duration, rate) });

/** The OTIO document (plain JSON) for an edit list. */
export function toOtio(edit, baseDir = ".") {
  const rate = Number(edit.fps) || 30;
  const abs = (f) => (isAbsolute(f) ? f : resolve(baseDir, f));
  const tracks = (edit.tracks ?? []).map((t, ti) => {
    const kind = /audio/i.test(t.kind) ? "Audio" : "Video";
    const clips = [...(t.clips ?? [])].sort((a, b) => (a.start ?? 0) - (b.start ?? 0));
    const children = [];
    let cursor = 0;
    for (const c of clips) {
      const start = Math.max(Number(c.start ?? cursor), cursor);
      const dur = Number(c.duration);
      if (!(dur > 0)) throw new Error(`clip "${c.name ?? c.file}" needs a positive duration`);
      if (start - cursor >= 0.5 / rate) children.push({ OTIO_SCHEMA: "Gap.1", name: "", source_range: range(0, start - cursor, rate), effects: [], markers: [], enabled: true, metadata: {} });
      children.push({
        OTIO_SCHEMA: "Clip.2",
        name: c.name ?? basename(c.file, extname(c.file)),
        source_range: range(Number(c.in ?? 0), dur, rate),
        media_references: {
          DEFAULT_MEDIA: {
            OTIO_SCHEMA: "ExternalReference.1",
            name: basename(c.file),
            target_url: pathToFileURL(abs(c.file)).href,
            available_range: c.media_duration ? range(0, Number(c.media_duration), rate) : null,
            available_image_bounds: null,
            metadata: {},
          },
        },
        active_media_reference_key: "DEFAULT_MEDIA",
        effects: [],
        markers: [],
        enabled: true,
        metadata: c.node ? { falos: { node: c.node } } : {},
      });
      cursor = start + dur;
    }
    return { OTIO_SCHEMA: "Track.1", name: t.name ?? `${kind[0]}${ti + 1}`, kind, children, source_range: null, effects: [], markers: [], enabled: true, metadata: {} };
  });
  return {
    OTIO_SCHEMA: "Timeline.1",
    name: edit.name ?? "fal-os edit",
    global_start_time: rt(0, rate),
    metadata: { falos: { render: edit.render ? abs(edit.render) : null, width: edit.width ?? null, height: edit.height ?? null, fps: rate } },
    tracks: { OTIO_SCHEMA: "Stack.1", name: "tracks", children: tracks, source_range: null, effects: [], markers: [], enabled: true, metadata: {} },
  };
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const a = process.argv.slice(2);
  const file = a.find((x) => !x.startsWith("--"));
  const oi = a.indexOf("--out");
  if (!file) {
    console.error("usage: node otio.mjs <edit.json> [--out file.otio]");
    process.exit(2);
  }
  const edit = JSON.parse(readFileSync(file, "utf8"));
  const out = oi >= 0 ? a[oi + 1] : join(dirname(file), `${basename(file, extname(file))}.otio`);
  const doc = toOtio(edit, dirname(resolve(file)));
  writeFileSync(out, JSON.stringify(doc, null, 2));
  const n = doc.tracks.children.reduce((s, t) => s + t.children.filter((c) => c.OTIO_SCHEMA.startsWith("Clip")).length, 0);
  console.log(`wrote ${out} (${doc.tracks.children.length} tracks, ${n} clips)`);
}
