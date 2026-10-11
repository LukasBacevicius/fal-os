//! ffmpeg engine: video/audio ops as subprocesses with `-progress` reporting.
//!
//! Output defaults: even dimensions, yuv420p (alpha kept in mov/webm), `+faststart`, bt709
//! tags, 48 kHz AAC, `comment=falos:<input sha>`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value as Json;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::probe::ffmpeg_bin;
use super::{Prepared, num, str_arg};

/// `ffmpeg 8.0` (for `engine_version`).
pub fn version() -> String {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        std::process::Command::new(ffmpeg_bin())
            .arg("-version")
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(2).map(|v| format!("ffmpeg {v}")))
            })
            .unwrap_or_else(|| "ffmpeg".into())
    })
    .clone()
}

/// Run ffmpeg with `args` (no `-y`/progress flags needed); `duration` (s) turns
/// `out_time` into a 0..1 progress for `on_progress`.
pub async fn run(args: &[String], duration: Option<f64>, on_progress: &(dyn Fn(f64) + Send + Sync)) -> Result<()> {
    let mut cmd = tokio::process::Command::new(ffmpeg_bin());
    cmd.args(["-y", "-hide_banner", "-nostdin", "-v", "error", "-progress", "pipe:1", "-nostats"])
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("running ffmpeg (install ffmpeg)")?;
    let stdout = child.stdout.take().context("ffmpeg stdout")?;
    let stderr = child.stderr.take().context("ffmpeg stderr")?;
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            s.push_str(&l);
            s.push('\n');
            if s.len() > 8000 {
                s.drain(..s.len() - 4000);
            }
        }
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        if let (Some(v), Some(d)) = (l.strip_prefix("out_time_us="), duration)
            && let Ok(us) = v.trim().parse::<f64>()
            && d > 0.0
        {
            on_progress((us / 1e6 / d).clamp(0.0, 1.0));
        }
    }
    let status = child.wait().await?;
    let err = err_task.await.unwrap_or_default();
    if !status.success() {
        let tail: String = err.trim().chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("ffmpeg failed: {tail}");
    }
    Ok(())
}

pub fn s(v: impl ToString) -> String {
    v.to_string()
}

/// Encoder flags for an output extension. `alpha`: the source has alpha worth keeping.
pub fn encode_args(ext: &str, alpha: bool, has_video: bool, has_audio: bool, comment: &str) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    match ext {
        "mp4" | "m4v" | "mov" if has_video => {
            if ext == "mov" && alpha {
                a.extend(["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"].map(s));
            } else {
                a.extend(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p"].map(s));
            }
            a.extend(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"].map(s));
            a.extend(["-movflags", "+faststart"].map(s));
        }
        "webm" if has_video => {
            a.extend(["-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "32", "-row-mt", "1"].map(s));
            a.extend(["-pix_fmt", if alpha { "yuva420p" } else { "yuv420p" }].map(s));
        }
        _ => {}
    }
    if has_audio {
        match ext {
            "mp4" | "m4v" | "mov" | "m4a" | "aac" => a.extend(["-c:a", "aac", "-b:a", "192k", "-ar", "48000"].map(s)),
            "webm" | "ogg" => a.extend(["-c:a", "libopus", "-b:a", "160k", "-ar", "48000"].map(s)),
            "mp3" => a.extend(["-c:a", "libmp3lame", "-b:a", "320k", "-ar", "48000"].map(s)),
            "wav" => a.extend(["-c:a", "pcm_s24le", "-ar", "48000"].map(s)),
            "flac" => a.extend(["-c:a", "flac", "-ar", "48000"].map(s)),
            _ => {}
        }
    }
    if !comment.is_empty() && !matches!(ext, "gif" | "png" | "jpg" | "webp") {
        a.extend(["-metadata".into(), format!("comment={comment}")]);
    }
    a
}

/// `scale` chain suffix that keeps dimensions even (needed by yuv420p).
pub const EVEN: &str = "scale=trunc(iw/2)*2:trunc(ih/2)*2";

pub fn has_audio(p: &Prepared) -> bool {
    p.report["streams"]
        .as_array()
        .is_some_and(|ss| ss.iter().any(|s| s["type"] == "audio"))
        || matches!(p.kind, falos_doc::NodeKind::Audio)
}

pub fn has_video(p: &Prepared) -> bool {
    p.media.w.is_some() && p.kind != falos_doc::NodeKind::Audio
}

fn out_ext_video(p: &Prepared, args: &Json) -> String {
    let e = str_arg(args, "format").unwrap_or_else(|| p.ext.clone());
    match e.as_str() {
        "mp4" | "mov" | "webm" | "m4v" => e,
        _ => "mp4".into(),
    }
}

fn out_ext_audio(p: &Prepared, args: &Json) -> String {
    let e = str_arg(args, "format").unwrap_or_else(|| p.ext.clone());
    match e.as_str() {
        "m4a" | "mp3" | "wav" | "flac" | "ogg" | "aac" => e,
        _ => "m4a".into(),
    }
}

/// Run a single-input video/audio filter op into `dir`. Returns the output file.
pub async fn filter_one(
    p: &Prepared,
    vf: Option<String>,
    af: Option<String>,
    extra_in: &[String],
    args: &Json,
    dir: &Path,
    progress: &(dyn Fn(f64) + Send + Sync),
) -> Result<PathBuf> {
    let video = has_video(p);
    let audio = has_audio(p);
    let ext = if video { out_ext_video(p, args) } else { out_ext_audio(p, args) };
    let out = dir.join(format!("out.{ext}"));
    let mut a: Vec<String> = extra_in.to_vec();
    a.extend(["-i".into(), p.file.display().to_string()]);
    if video {
        let chain = match vf {
            Some(v) if !v.is_empty() => format!("{v},{EVEN}"),
            _ => EVEN.to_string(),
        };
        a.extend(["-vf".into(), chain]);
    } else {
        a.push("-vn".into());
    }
    if let Some(f) = af.filter(|f| !f.is_empty()) {
        a.extend(["-af".into(), f]);
    }
    a.extend(encode_args(&ext, p.media.alpha == Some(true), video, audio, &p.comment));
    a.push(out.display().to_string());
    run(&a, p.media.duration, progress).await?;
    Ok(out)
}

/// Seconds from a number or "mm:ss(.ms)" / "hh:mm:ss".
pub fn secs(v: &Json) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let s = v.as_str()?;
    let mut t = 0.0;
    for part in s.split(':') {
        t = t * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(t)
}

pub async fn trim(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let start = args.get("start").and_then(secs).unwrap_or(0.0).max(0.0);
    let end = match (args.get("end").and_then(secs), args.get("duration").and_then(secs)) {
        (Some(e), _) => e,
        (None, Some(d)) => start + d,
        _ => p.media.duration.unwrap_or(f64::MAX),
    };
    if end <= start {
        bail!("trim: end ({end}) must be after start ({start})");
    }
    let video = has_video(p);
    let ext = if video { out_ext_video(p, args) } else { out_ext_audio(p, args) };
    let out = dir.join(format!("out.{ext}"));
    let mut a = vec![s("-ss"), format!("{start}"), s("-to"), format!("{end}"), s("-i"), p.file.display().to_string()];
    if args["copy"].as_bool() == Some(true) {
        a.extend(["-c", "copy"].map(s));
        if video {
            a.extend(["-movflags", "+faststart"].map(s));
        }
    } else {
        if video {
            a.extend(["-vf".into(), EVEN.into()]);
        }
        a.extend(encode_args(&ext, p.media.alpha == Some(true), video, has_audio(p), &p.comment));
    }
    a.push(out.display().to_string());
    run(&a, Some(end - start), progress).await?;
    Ok(out)
}

/// Concatenate inputs (normalized to the first input's size/fps; optional crossfade).
pub async fn concat(ps: &[Prepared], args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    if ps.len() < 2 {
        bail!("concat needs at least 2 inputs");
    }
    let video = ps.iter().all(has_video);
    let audio = ps.iter().all(has_audio);
    let first = &ps[0];
    let xfade = num(args, "xfade").unwrap_or(0.0).max(0.0);
    let ext = if video { out_ext_video(first, args) } else { out_ext_audio(first, args) };
    let out = dir.join(format!("out.{ext}"));
    let (w, h) = (
        (first.media.w.unwrap_or(1920.0) as i64) / 2 * 2,
        (first.media.h.unwrap_or(1080.0) as i64) / 2 * 2,
    );
    let fps = num(args, "fps").or(first.media.fps).unwrap_or(30.0);
    let mut a = vec![];
    for p in ps {
        a.extend(["-i".into(), p.file.display().to_string()]);
    }
    let mut fc = String::new();
    for (i, _) in ps.iter().enumerate() {
        if video {
            fc.push_str(&format!(
                "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p[v{i}];"
            ));
        }
        if audio {
            fc.push_str(&format!("[{i}:a]aresample=48000,aformat=channel_layouts=stereo[a{i}];"));
        }
    }
    let n = ps.len();
    let total: f64 = ps.iter().filter_map(|p| p.media.duration).sum();
    if xfade > 0.0 && video {
        // Chain xfade / acrossfade pairwise.
        let mut offset = 0.0;
        let mut last_v = "v0".to_string();
        let mut last_a = "a0".to_string();
        for i in 1..n {
            offset += ps[i - 1].media.duration.unwrap_or(0.0) - xfade;
            let ov = format!("xv{i}");
            fc.push_str(&format!("[{last_v}][v{i}]xfade=transition=fade:duration={xfade}:offset={offset}[{ov}];"));
            last_v = ov;
            if audio {
                let oa = format!("xa{i}");
                fc.push_str(&format!("[{last_a}][a{i}]acrossfade=d={xfade}[{oa}];"));
                last_a = oa;
            }
        }
        fc.pop();
        a.extend(["-filter_complex".into(), fc, "-map".into(), format!("[{last_v}]")]);
        if audio {
            a.extend(["-map".into(), format!("[{last_a}]")]);
        }
    } else {
        for i in 0..n {
            if video {
                fc.push_str(&format!("[v{i}]"));
            }
            if audio {
                fc.push_str(&format!("[a{i}]"));
            }
        }
        fc.push_str(&format!("concat=n={n}:v={}:a={}", video as u8, audio as u8));
        if video {
            fc.push_str("[v]");
        }
        if audio {
            fc.push_str("[a]");
        }
        a.extend(["-filter_complex".into(), fc]);
        if video {
            a.extend(["-map", "[v]"].map(s));
        }
        if audio {
            a.extend(["-map", "[a]"].map(s));
        }
    }
    a.extend(encode_args(&ext, false, video, audio, &first.comment));
    a.push(out.display().to_string());
    run(&a, Some(total), progress).await?;
    Ok(out)
}

/// `atempo` accepts 0.5..2: chain it for other factors.
fn atempo(f: f64) -> String {
    let mut parts = vec![];
    let mut f = f;
    while f > 2.0 {
        parts.push("atempo=2.0".to_string());
        f /= 2.0;
    }
    while f < 0.5 {
        parts.push("atempo=0.5".to_string());
        f /= 0.5;
    }
    parts.push(format!("atempo={f}"));
    parts.join(",")
}

pub async fn speed(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let f = num(args, "factor").ok_or_else(|| anyhow!("speed needs factor (e.g. 2 = twice as fast)"))?;
    if !(0.05..=20.0).contains(&f) {
        bail!("speed factor must be 0.05..20");
    }
    let vf = has_video(p).then(|| format!("setpts=PTS/{f}"));
    let af = has_audio(p).then(|| atempo(f));
    filter_one(p, vf, af, &[], args, dir, progress).await
}

/// Frames at `times`, or `count` evenly spaced, or one `every` N seconds.
pub async fn extract_frames(p: &Prepared, args: &Json, dir: &Path) -> Result<Vec<PathBuf>> {
    let dur = p.media.duration.unwrap_or(0.0);
    let fmt = match str_arg(args, "format").as_deref() {
        Some("jpg" | "jpeg") => "jpg",
        _ => "png",
    };
    let times: Vec<f64> = if let Some(ts) = args["times"].as_array() {
        ts.iter().filter_map(secs).collect()
    } else if let Some(every) = num(args, "every").filter(|e| *e > 0.0) {
        (0..).map(|i| i as f64 * every).take_while(|t| *t < dur).take(200).collect()
    } else {
        let n = num(args, "count").unwrap_or(1.0).clamp(1.0, 200.0) as usize;
        if n == 1 {
            vec![args.get("t").and_then(secs).unwrap_or(0.0)]
        } else {
            (0..n).map(|i| dur * (i as f64 + 0.5) / n as f64).collect()
        }
    };
    if times.is_empty() {
        bail!("extract_frames: no frames selected");
    }
    let mut outs = vec![];
    for (i, t) in times.iter().enumerate() {
        let out = dir.join(format!("frame_{i:03}.{fmt}"));
        let t = if dur > 0.0 { t.min((dur - 0.05).max(0.0)) } else { *t };
        let a = vec![s("-ss"), format!("{t}"), s("-i"), p.file.display().to_string(), s("-frames:v"), s("1"), out.display().to_string()];
        run(&a, None, &|_| {}).await?;
        outs.push(out);
    }
    Ok(outs)
}

pub async fn gif(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let fps = num(args, "fps").unwrap_or(12.0).clamp(1.0, 50.0);
    let width = num(args, "width").unwrap_or(480.0).clamp(16.0, 2048.0) as i64;
    let width = width.min(p.media.w.unwrap_or(width as f64) as i64);
    let out = dir.join("out.gif");
    let mut a = vec![];
    if let Some(st) = args.get("start").and_then(secs) {
        a.extend([s("-ss"), format!("{st}")]);
    }
    if let Some(d) = args.get("duration").and_then(secs) {
        a.extend([s("-t"), format!("{d}")]);
    }
    a.extend([s("-i"), p.file.display().to_string()]);
    a.extend([
        s("-filter_complex"),
        format!("fps={fps},scale={width}:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a"),
        s("-loop"),
        s(if args["loop"].as_bool() == Some(false) { "-1" } else { "0" }),
        out.display().to_string(),
    ]);
    run(&a, p.media.duration, progress).await?;
    Ok(out)
}

/// Two-pass EBU R128 loudness normalization to `lufs` (default -14), true peak `tp`.
pub async fn audio_normalize(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    if !has_audio(p) {
        bail!("audio_normalize: the input has no audio");
    }
    let lufs = num(args, "lufs").unwrap_or(-14.0).clamp(-40.0, -5.0);
    let tp = num(args, "tp").unwrap_or(-1.5).clamp(-9.0, 0.0);
    let lra = num(args, "lra").unwrap_or(11.0).clamp(1.0, 20.0);
    let m = loudness_measure(&p.file, Some(format!("loudnorm=I={lufs}:TP={tp}:LRA={lra}:print_format=json"))).await?;
    let g = |k: &str| m[k].as_str().unwrap_or("0").to_string();
    let af = format!(
        "loudnorm=I={lufs}:TP={tp}:LRA={lra}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true,aresample=48000",
        g("input_i"), g("input_tp"), g("input_lra"), g("input_thresh"), g("target_offset")
    );
    // Video: copy the picture, re-encode audio only.
    if has_video(p) {
        let ext = out_ext_video(p, args);
        let out = dir.join(format!("out.{ext}"));
        let mut a = vec![s("-i"), p.file.display().to_string(), s("-c:v"), s("copy"), s("-af"), af];
        a.extend(encode_args(&ext, false, false, true, &p.comment));
        a.extend(["-movflags", "+faststart"].map(s));
        a.push(out.display().to_string());
        run(&a, p.media.duration, progress).await?;
        return Ok(out);
    }
    filter_one(p, None, Some(af), &[], args, dir, progress).await
}

/// Loudnorm's first-pass JSON (from stderr).
pub async fn loudness_measure(file: &Path, filter: Option<String>) -> Result<Json> {
    let f = filter.unwrap_or_else(|| "loudnorm=I=-14:TP=-1.5:LRA=11:print_format=json".into());
    let o = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(file)
        .args(["-vn", "-af", &f, "-f", "null", "-"])
        .output()
        .await?;
    let err = String::from_utf8_lossy(&o.stderr);
    let start = err.rfind('{').ok_or_else(|| anyhow!("loudness: no measurement (does it have audio?)"))?;
    let end = err[start..].find('}').map(|e| start + e + 1).unwrap_or(err.len());
    Ok(serde_json::from_str(&err[start..end])?)
}

/// Mix a bed (inputs[1]) under the primary (inputs[0]); ducking compresses the bed while
/// the primary is loud.
pub async fn audio_mix(ps: &[Prepared], args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    if ps.len() < 2 {
        bail!("audio_mix needs 2 inputs: [primary (voice or video), bed (music)]");
    }
    let (main, bed) = (&ps[0], &ps[1]);
    if !has_audio(bed) {
        bail!("audio_mix: the bed has no audio");
    }
    let bed_db = num(args, "bed_gain_db").unwrap_or(-10.0);
    let duck = args["duck"].as_bool().unwrap_or(true);
    let video = has_video(main);
    let ext = if video { out_ext_video(main, args) } else { out_ext_audio(main, args) };
    let out = dir.join(format!("out.{ext}"));
    let mut a = vec![s("-i"), main.file.display().to_string(), s("-stream_loop"), s("-1"), s("-i"), bed.file.display().to_string()];
    let main_a = if has_audio(main) { "[0:a]aresample=48000[ma];" } else { "anullsrc=r=48000:cl=stereo[ma];" };
    let fc = if duck && has_audio(main) {
        format!(
            "{main_a}[1:a]aresample=48000,volume={bed_db}dB[bd];[ma]asplit=2[mx][sc];[bd][sc]sidechaincompress=threshold=0.03:ratio=8:attack=20:release=400[dk];[mx][dk]amix=inputs=2:duration=first:normalize=0[a]"
        )
    } else {
        format!("{main_a}[1:a]aresample=48000,volume={bed_db}dB[bd];[ma][bd]amix=inputs=2:duration=first:normalize=0[a]")
    };
    a.extend(["-filter_complex".into(), fc]);
    if video {
        a.extend(["-map", "0:v", "-c:v", "copy", "-map", "[a]"].map(s));
        a.extend(encode_args(&ext, false, false, true, &main.comment));
        a.extend(["-movflags", "+faststart"].map(s));
    } else {
        a.extend(["-map", "[a]"].map(s));
        a.extend(encode_args(&ext, false, false, true, &main.comment));
    }
    a.extend(["-t".into(), format!("{}", main.media.duration.unwrap_or(30.0))]);
    a.push(out.display().to_string());
    run(&a, main.media.duration, progress).await?;
    Ok(out)
}

pub async fn audio_extract(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    if !has_audio(p) {
        bail!("audio_extract: the input has no audio");
    }
    let ext = match str_arg(args, "format").as_deref() {
        Some(e @ ("mp3" | "wav" | "flac" | "m4a")) => e.to_string(),
        _ => "m4a".into(),
    };
    let out = dir.join(format!("out.{ext}"));
    let mut a = vec![s("-i"), p.file.display().to_string(), s("-vn")];
    a.extend(encode_args(&ext, false, false, true, &p.comment));
    a.push(out.display().to_string());
    run(&a, p.media.duration, progress).await?;
    Ok(out)
}

/// Remove audio (`what: audio`, default) or metadata (`what: metadata`) without re-encoding.
pub async fn strip(p: &Prepared, args: &Json, dir: &Path) -> Result<PathBuf> {
    let what = str_arg(args, "what").unwrap_or_else(|| "audio".into());
    let out = dir.join(format!("out.{}", p.ext));
    let mut a = vec![s("-i"), p.file.display().to_string(), s("-c"), s("copy")];
    match what.as_str() {
        "audio" => a.push(s("-an")),
        "metadata" => a.extend(["-map_metadata", "-1", "-map_chapters", "-1"].map(s)),
        o => bail!("strip: what must be audio|metadata, not `{o}`"),
    }
    if matches!(p.ext.as_str(), "mp4" | "mov" | "m4v") {
        a.extend(["-movflags", "+faststart"].map(s));
    }
    a.push(out.display().to_string());
    run(&a, None, &|_| {}).await?;
    Ok(out)
}

/// Escape hatch: raw args with `{in0}`, `{in1}`, … and `{out}` placeholders.
pub async fn raw(ps: &[Prepared], args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let list: Vec<String> = match &args["args"] {
        Json::Array(a) => a.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect(),
        Json::String(s) => s.split_whitespace().map(String::from).collect(),
        _ => bail!("ffmpeg op needs args: [\"-i\", \"{{in0}}\", …, \"{{out}}\"]"),
    };
    let ext = str_arg(args, "ext").unwrap_or_else(|| ps.first().map(|p| p.ext.clone()).unwrap_or_else(|| "mp4".into()));
    if !ext.chars().all(|c| c.is_ascii_alphanumeric()) || ext.len() > 5 {
        bail!("bad ext `{ext}`");
    }
    let out = dir.join(format!("out.{ext}"));
    if !list.iter().any(|a| a.contains("{out}")) {
        bail!("ffmpeg args must contain {{out}}");
    }
    let mut a = vec![];
    for arg in list {
        let mut v = arg.replace("{out}", &out.display().to_string());
        for (i, p) in ps.iter().enumerate() {
            v = v.replace(&format!("{{in{i}}}"), &p.file.display().to_string());
        }
        if v.contains("{in") {
            bail!("ffmpeg args reference an input that wasn't given: {v}");
        }
        a.push(v);
    }
    run(&a, ps.first().and_then(|p| p.media.duration), progress).await?;
    if !out.exists() {
        bail!("ffmpeg wrote no {{out}}");
    }
    Ok(out)
}

/// A still at `t` (PNG), for chips and video-in-image ops.
pub async fn still(file: &Path, t: f64, dir: &Path) -> Result<PathBuf> {
    let out = dir.join(format!("still_{}.png", (t * 1000.0) as i64));
    run(&[s("-ss"), format!("{}", t.max(0.0)), s("-i"), file.display().to_string(), s("-frames:v"), s("1"), out.display().to_string()], None, &|_| {}).await?;
    Ok(out)
}

/// Scene changes (`scdet`) as timestamps.
pub async fn scenes(file: &Path, threshold: f64) -> Result<Vec<f64>> {
    let o = tokio::process::Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(file)
        .args(["-an", "-vf", &format!("scdet=threshold={threshold}"), "-f", "null", "-"])
        .output()
        .await?;
    let err = String::from_utf8_lossy(&o.stderr);
    let mut out = vec![];
    for l in err.lines() {
        if let Some(i) = l.find("lavfi.scd.time:")
            && let Some(t) = l[i + 15..].split_whitespace().next().and_then(|t| t.parse::<f64>().ok())
        {
            out.push((t * 1000.0).round() / 1000.0);
        }
    }
    Ok(out)
}
