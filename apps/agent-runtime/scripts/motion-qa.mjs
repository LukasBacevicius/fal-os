#!/usr/bin/env node
// Motion QA for a rendered film (run by the motion / qa agents before import):
//   node motion-qa.mjs <video.mp4> [--out <dir>] [--fps 30] [--duration <s>] [--master] [--lufs -14]
// Checks: 1920×1080, fps, h264/yuv420p, duration, audio present, loudness (EBU R128 integrated
// LUFS + true peak), black / frozen stretches, cut timing vs. the music's beat grid. Writes a
// contact sheet (12 frames, 4×3) and qa.json to --out (default: <video dir>/qa), prints a
// summary and exits 1 when a hard check fails. --master normalizes the audio to -14 LUFS /
// -1.5 dBTP (two-pass loudnorm, video copied) into <name>.master.mp4 and re-checks it.
import { execFile } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { basename, dirname, extname, join, resolve } from "node:path";
import { promisify } from "node:util";

const run = promisify(execFile);
const BIG = { maxBuffer: 512 * 1024 * 1024 };

function args() {
  const a = process.argv.slice(2);
  const o = { video: "", out: "", fps: 30, duration: undefined, master: false, lufs: -14 };
  for (let i = 0; i < a.length; i++) {
    const k = a[i];
    if (k === "--out") o.out = a[++i];
    else if (k === "--fps") o.fps = Number(a[++i]);
    else if (k === "--duration") o.duration = Number(a[++i]);
    else if (k === "--lufs") o.lufs = Number(a[++i]);
    else if (k === "--master") o.master = true;
    else if (!o.video) o.video = k;
  }
  if (!o.video) {
    console.error("usage: node motion-qa.mjs <video.mp4> [--out dir] [--fps 30] [--duration s] [--master] [--lufs -14]");
    process.exit(2);
  }
  o.video = resolve(o.video);
  o.out = resolve(o.out || join(dirname(o.video), "qa"));
  return o;
}

async function probe(file) {
  const { stdout } = await run("ffprobe", ["-v", "error", "-show_streams", "-show_format", "-of", "json", file], BIG);
  const j = JSON.parse(stdout);
  const v = j.streams.find((s) => s.codec_type === "video");
  const a = j.streams.find((s) => s.codec_type === "audio");
  const [n, d] = String(v?.avg_frame_rate ?? v?.r_frame_rate ?? "0/1").split("/").map(Number);
  return {
    width: v?.width,
    height: v?.height,
    fps: d ? Math.round((n / d) * 100) / 100 : 0,
    codec: v?.codec_name,
    pix_fmt: v?.pix_fmt,
    duration: Number(j.format?.duration ?? v?.duration ?? 0),
    audio: a ? { codec: a.codec_name, channels: a.channels, sample_rate: Number(a.sample_rate) } : null,
  };
}

/** Integrated loudness (LUFS), loudness range and true peak via loudnorm's measurement pass. */
async function loudness(file, target) {
  const { stderr } = await run("ffmpeg", ["-hide_banner", "-nostats", "-i", file, "-af", `loudnorm=I=${target}:TP=-1.5:LRA=11:print_format=json`, "-f", "null", "-"], BIG);
  const m = stderr.slice(stderr.lastIndexOf("{"));
  const j = JSON.parse(m.slice(0, m.indexOf("}") + 1));
  return { i: Number(j.input_i), tp: Number(j.input_tp), lra: Number(j.input_lra), thresh: Number(j.input_thresh), offset: Number(j.target_offset), raw: j };
}

async function master(file, target, measured) {
  const out = join(dirname(file), `${basename(file, extname(file))}.master${extname(file)}`);
  const m = measured.raw;
  const af = `loudnorm=I=${target}:TP=-1.5:LRA=11:measured_I=${m.input_i}:measured_TP=${m.input_tp}:measured_LRA=${m.input_lra}:measured_thresh=${m.input_thresh}:offset=${m.target_offset}:linear=true,aresample=48000`;
  await run("ffmpeg", ["-hide_banner", "-y", "-i", file, "-map", "0:v:0", "-map", "0:a:0", "-c:v", "copy", "-af", af, "-c:a", "aac", "-b:a", "320k", "-movflags", "+faststart", out], BIG);
  return out;
}

async function detect(file, filter, re) {
  const { stderr } = await run("ffmpeg", ["-hide_banner", "-nostats", "-i", file, "-an", "-vf", filter, "-f", "null", "-"], BIG);
  return [...stderr.matchAll(re)];
}

async function cuts(file) {
  const m = await detect(file, "select='gt(scene,0.30)',showinfo", /pts_time:([\d.]+)/g);
  return m.map((x) => Number(x[1]));
}

async function blacks(file) {
  const m = await detect(file, "blackdetect=d=0.25:pix_th=0.08", /black_start:([\d.]+) black_end:([\d.]+)/g);
  return m.map((x) => [Number(x[1]), Number(x[2])]);
}

async function freezes(file) {
  const { stderr } = await run("ffmpeg", ["-hide_banner", "-nostats", "-i", file, "-an", "-vf", "freezedetect=n=0.002:d=1.2", "-f", "null", "-"], BIG);
  const starts = [...stderr.matchAll(/freeze_start: ([\d.]+)/g)].map((x) => Number(x[1]));
  const ends = [...stderr.matchAll(/freeze_end: ([\d.]+)/g)].map((x) => Number(x[1]));
  return starts.map((s, i) => [s, ends[i] ?? null]);
}

/** Onsets + tempo from the audio: spectral-ish energy flux on 512-sample hops at 22.05 kHz. */
async function beats(file) {
  const { stdout } = await run("ffmpeg", ["-hide_banner", "-v", "error", "-i", file, "-vn", "-ac", "1", "-ar", "22050", "-f", "f32le", "-"], { ...BIG, encoding: "buffer" });
  const pcm = new Float32Array(stdout.buffer, stdout.byteOffset, Math.floor(stdout.byteLength / 4));
  const hop = 512;
  const rate = 22050 / hop;
  const env = [];
  for (let i = 0; i + hop <= pcm.length; i += hop) {
    let e = 0;
    for (let j = i; j < i + hop; j++) e += pcm[j] * pcm[j];
    env.push(Math.log1p(1000 * Math.sqrt(e / hop)));
  }
  const flux = env.map((v, i) => Math.max(0, v - (env[i - 1] ?? v)));
  if (!flux.length || Math.max(...flux) === 0) return { bpm: null, onsets: [], grid: [] };
  // Adaptive threshold peak picking.
  const onsets = [];
  const w = Math.round(rate * 0.25);
  for (let i = 1; i < flux.length - 1; i++) {
    let s = 0;
    let c = 0;
    for (let k = Math.max(0, i - w); k < Math.min(flux.length, i + w); k++) (s += flux[k]), c++;
    const thr = (s / c) * 1.6 + 0.01;
    if (flux[i] > thr && flux[i] >= flux[i - 1] && flux[i] >= flux[i + 1] && (!onsets.length || i / rate - onsets.at(-1) > 0.1)) onsets.push(i / rate);
  }
  // Tempo: autocorrelation of the flux between 70 and 180 BPM.
  const ac = (lag) => {
    let s = 0;
    for (let i = lag; i < flux.length; i++) s += flux[i] * flux[i - lag];
    return s / (flux.length - lag);
  };
  let best = 0;
  let bestLag = 0;
  for (let lag = Math.round((60 / 180) * rate); lag <= Math.round((60 / 70) * rate); lag++) {
    const s = ac(lag);
    if (s > best) (best = s), (bestLag = lag);
  }
  if (!bestLag) return { bpm: null, onsets, grid: [] };
  // Sub-hop precision: parabolic peak interpolation, then refine with the onsets' spacing.
  const [a, b0, c] = [ac(bestLag - 1), best, ac(bestLag + 1)];
  const den = a - 2 * b0 + c;
  let period = (bestLag + (den ? (0.5 * (a - c)) / den : 0)) / rate;
  const gaps = onsets.slice(1).map((t, i) => t - onsets[i]).filter((g) => g > period * 0.5);
  const near = gaps.map((g) => g / Math.max(1, Math.round(g / period))).filter((g) => Math.abs(g - period) < period * 0.08);
  if (near.length >= 4) period = near.reduce((s, g) => s + g, 0) / near.length;
  // Phase: the offset that lands most onsets on the grid.
  let phase = 0;
  let hits = -1;
  for (let p = 0; p < period; p += 0.005) {
    const h = onsets.filter((t) => Math.abs(((t - p + period / 2) % period) - period / 2) < 0.05).length;
    if (h > hits) (hits = h), (phase = p);
  }
  const dur = pcm.length / 22050;
  const grid = [];
  for (let t = phase; t < dur; t += period) grid.push(Math.round(t * 1000) / 1000);
  return { bpm: Math.round((60 / period) * 10) / 10, onsets: onsets.map((t) => Math.round(t * 1000) / 1000), grid };
}

async function contactSheet(file, duration, out) {
  const n = 12;
  const times = Array.from({ length: n }, (_, i) => Math.round(((i + 0.5) * duration * 1000) / n) / 1000);
  const sheet = join(out, "contact.jpg");
  const fps = n / Math.max(duration, 0.1);
  await run("ffmpeg", ["-hide_banner", "-v", "error", "-y", "-i", file, "-vf", `fps=${fps},scale=480:270:force_original_aspect_ratio=decrease,pad=480:270:(ow-iw)/2:(oh-ih)/2,tile=4x3:padding=6:color=0x202020`, "-frames:v", "1", "-q:v", "3", sheet], BIG);
  return { sheet, times };
}

const TYPOGRAPHY = [
  "Body copy ≥ 40 px and headlines ≥ 96 px at 1080p; nothing under 28 px.",
  "All text inside the title-safe area (5% margins: x 96–1824, y 54–1026); nothing touches an edge.",
  "At most 2 typefaces; consistent weights; tight tracking on big display type, wider on small caps.",
  "Contrast ≥ 4.5:1 against what's behind it (add a scrim/shadow over busy footage).",
  "≤ 6 words per on-screen line, ≤ 2 lines at once; each line stays up ≥ 1.2 s + 0.25 s per word.",
  "No orphans/widows, no overlapping text, no text cut off mid-animation.",
];

async function main() {
  const o = args();
  mkdirSync(o.out, { recursive: true });
  let file = o.video;
  const fails = [];
  const warns = [];
  let p = await probe(file);
  let loud = p.audio ? await loudness(file, o.lufs) : null;
  let mastered = null;
  if (o.master && p.width && p.audio && loud && (Math.abs(loud.i - o.lufs) > 0.5 || loud.tp > -1)) {
    mastered = await master(file, o.lufs, loud);
    file = mastered;
    p = await probe(file);
    loud = await loudness(file, o.lufs);
  }
  if (!p.width) {
    // Audio only (e.g. the music bed): tempo, beat grid and loudness for planning cuts.
    const b = p.audio ? await beats(file) : { bpm: null, onsets: [], grid: [] };
    const report = { file, audio_only: true, duration: p.duration, loudness: loud && { integrated_lufs: loud.i, true_peak_dbtp: loud.tp }, bpm: b.bpm, beat_period_s: b.bpm ? Math.round((60 / b.bpm) * 1000) / 1000 : null, grid: b.grid, onsets: b.onsets };
    writeFileSync(join(o.out, "qa.json"), JSON.stringify(report, null, 2));
    console.log(`audio ${p.duration.toFixed(2)} s · ${b.bpm ?? "?"} BPM (beat ${report.beat_period_s ?? "?"} s) · ${loud ? `${loud.i} LUFS` : "silent"}`);
    console.log(`  beat grid (s): ${b.grid.slice(0, 64).join(", ")}${b.grid.length > 64 ? " …" : ""}`);
    console.log(`  report ${join(o.out, "qa.json")}`);
    return;
  }
  if (p.width !== 1920 || p.height !== 1080) fails.push(`resolution ${p.width}×${p.height}, expected 1920×1080`);
  if (Math.abs(p.fps - o.fps) > 0.05) fails.push(`fps ${p.fps}, expected ${o.fps}`);
  if (p.codec !== "h264") warns.push(`video codec ${p.codec} (h264 plays everywhere)`);
  if (p.pix_fmt && p.pix_fmt !== "yuv420p") warns.push(`pixel format ${p.pix_fmt} (use yuv420p)`);
  if (o.duration && Math.abs(p.duration - o.duration) > 0.5) fails.push(`duration ${p.duration.toFixed(2)} s, expected ${o.duration} s`);
  if (!p.audio) warns.push("no audio track");
  if (loud) {
    if (Math.abs(loud.i - o.lufs) > 1) (o.master ? fails : warns).push(`loudness ${loud.i} LUFS, target ${o.lufs} (run with --master)`);
    if (loud.tp > -1) warns.push(`true peak ${loud.tp} dBTP (> -1 dBTP may clip after encoding)`);
  }
  const [cutTimes, black, frozen, sheet] = await Promise.all([cuts(file), blacks(file), freezes(file), contactSheet(file, p.duration, o.out)]);
  for (const [s, e] of black) if (s > 0.15 && e < p.duration - 0.3) fails.push(`black frames ${s.toFixed(2)}–${e.toFixed(2)} s`);
  for (const [s, e] of frozen) warns.push(`frozen picture ${s.toFixed(2)}–${e == null ? "end" : e.toFixed(2)} s (ok for holds; not for clips)`);
  let timing = null;
  if (p.audio) {
    const b = await beats(file);
    const ref = b.grid.length ? b.grid : b.onsets;
    const offsets = cutTimes.map((t) => {
      let best = Infinity;
      for (const g of ref) best = Math.min(best, Math.abs(g - t));
      return Math.round(best * 1000);
    });
    const onBeat = offsets.filter((ms) => ms <= 80).length;
    timing = { bpm: b.bpm, beat_period_s: b.bpm ? Math.round((60 / b.bpm) * 1000) / 1000 : null, cuts: cutTimes.map((t, i) => ({ t: Math.round(t * 1000) / 1000, off_ms: offsets[i] })), on_beat: `${onBeat}/${cutTimes.length}`, onsets: b.onsets.slice(0, 200), grid: b.grid.slice(0, 200) };
    if (cutTimes.length >= 3 && onBeat / cutTimes.length < 0.5) warns.push(`only ${onBeat}/${cutTimes.length} cuts land within 80 ms of a beat (${b.bpm ?? "?"} BPM) — retime cuts to the grid`);
  }
  if (cutTimes.length === 0 && p.duration > 8) warns.push("no cuts detected in a long film (pacing may feel static)");
  const report = {
    file,
    mastered,
    pass: fails.length === 0,
    fails,
    warns,
    probe: p,
    loudness: loud && { integrated_lufs: loud.i, true_peak_dbtp: loud.tp, lra: loud.lra, target: o.lufs },
    timing,
    black,
    frozen,
    contact_sheet: sheet.sheet,
    contact_times: sheet.times,
    typography_checklist: TYPOGRAPHY,
  };
  writeFileSync(join(o.out, "qa.json"), JSON.stringify(report, null, 2));
  console.log(`${report.pass ? "PASS" : "FAIL"} ${file}`);
  console.log(`  ${p.width}×${p.height} @ ${p.fps} fps · ${p.codec}/${p.pix_fmt} · ${p.duration.toFixed(2)} s · audio ${p.audio ? `${p.audio.codec} ${p.audio.channels}ch` : "none"}`);
  if (loud) console.log(`  loudness ${loud.i} LUFS (target ${o.lufs}) · true peak ${loud.tp} dBTP${mastered ? ` · mastered → ${mastered}` : ""}`);
  if (timing) console.log(`  ${timing.bpm ?? "?"} BPM · cuts on beat ${timing.on_beat}`);
  for (const f of fails) console.log(`  ✗ ${f}`);
  for (const w of warns) console.log(`  ! ${w}`);
  console.log(`  contact sheet ${sheet.sheet} (frames at ${sheet.times.join(", ")} s) — Read it and check typography:`);
  for (const t of TYPOGRAPHY) console.log(`    - ${t}`);
  console.log(`  report ${join(o.out, "qa.json")}`);
  process.exit(report.pass ? 0 : 1);
}

main().catch((e) => {
  console.error(`motion-qa failed: ${e?.stderr?.toString?.().slice(-500) || e?.message || e}`);
  process.exit(2);
});
