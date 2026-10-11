//! Export (nodes → files) and packages (`.zip` with a manifest). Transform/compose make
//! nodes; export makes files:
//!
//! - presets are data (`presets/export.json`, versioned); per-node `export` props are saved
//!   settings; target `"deliverables"` = nodes with `role: final` or inside a frame named
//!   "Deliverables";
//! - an aspect mismatch reframes first (pad-blur) into a `role: final` node;
//! - names `{project}_{name}_{preset}_{w}x{h}_v{n}.{ext}` with Downloads-style collisions
//!   (`name (1).ext`), into `<project>/exports/<date>/` (deliverables: `<project>/deliverables/`);
//! - every file is verified (re-read + sha256, refuse blank) and logged in
//!   `<project>/exports/manifest.jsonl`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, NodeKind};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use super::{Request, ff, img, num, parse_aspect, prepare, probe, resolve_inputs, verify_output};
use crate::host::{Host, now_ms};

pub const PRESETS: &str = include_str!("../../presets/export.json");

pub fn presets() -> Json {
    serde_json::from_str(PRESETS).expect("presets/export.json is valid JSON")
}

pub fn preset(id: &str) -> Option<Json> {
    presets()["presets"].as_array()?.iter().find(|p| p["id"] == id).cloned()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(48).collect();
    if out.is_empty() { "untitled".into() } else { out }
}

/// `name.ext`, else `name (1).ext`, `name (2).ext`, … (like ~/Downloads).
pub fn free_name(dir: &Path, file: &str) -> PathBuf {
    let p = dir.join(file);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match file.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (file.to_string(), String::new()),
    };
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// Fill naming tokens.
pub fn render_name(pattern: &str, tokens: &[(&str, String)]) -> String {
    let mut s = pattern.to_string();
    for (k, v) in tokens {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    // Drop unknown tokens, path separators and doubled separators.
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        match c {
            '{' => skip = true,
            '}' => skip = false,
            '/' | '\\' | ':' if !skip => out.push('-'),
            c if !skip => out.push(c),
            _ => {}
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches(['_', '-', ' ']).to_string()
}

/// One resolved export row.
#[derive(Clone, Debug)]
struct Row {
    preset: Json,
    format: Option<String>,
    scale: Option<String>,
    suffix: String,
    quality: Option<f32>,
}

fn rows_of(items: &[Json]) -> Result<Vec<Row>> {
    let mut out = vec![];
    for it in items {
        let (pid, o) = match it {
            Json::String(s) => (Some(s.clone()), serde_json::Map::new()),
            Json::Object(o) => (o.get("preset").and_then(Json::as_str).map(String::from), o.clone()),
            v => bail!("bad export entry {v}"),
        };
        let fmt = o.get("format").and_then(Json::as_str).map(|s| s.to_lowercase());
        let preset = match (&pid, &fmt) {
            (Some(p), _) => preset(p).ok_or_else(|| anyhow!("unknown preset `{p}` (see GET /api/presets)"))?,
            (None, Some(f)) if matches!(f.as_str(), "png" | "jpg" | "jpeg" | "webp" | "avif") => preset("web-image").unwrap(),
            (None, Some(f)) if matches!(f.as_str(), "wav" | "mp3") => preset(f).unwrap(),
            (None, Some(f)) => json!({"id": f, "kind": "video", "container": f, "vcodec": "h264", "video_bitrate": "12M", "maxrate": "16M", "audio_bitrate": "192k"}),
            (None, None) => bail!("export entry needs preset or format: {it}"),
        };
        out.push(Row {
            preset,
            format: fmt,
            scale: o.get("scale").map(|s| s.as_str().map(String::from).unwrap_or_else(|| s.to_string())),
            suffix: o.get("suffix").and_then(Json::as_str).unwrap_or("").to_string(),
            quality: o.get("quality").and_then(Json::as_f64).map(|q| q as f32),
        });
    }
    Ok(out)
}

/// Image scale grammar → target width, given the 1x size and the source size.
pub fn scale_to(scale: &str, base: (f64, f64)) -> Option<(f64, f64)> {
    let s = scale.trim().to_lowercase();
    let (bw, bh) = base;
    if let Some(v) = s.strip_suffix('x').and_then(|v| v.parse::<f64>().ok()) {
        return Some((bw * v, bh * v));
    }
    if let Some(v) = s.strip_suffix('w').and_then(|v| v.parse::<f64>().ok()) {
        return Some((v, bh * v / bw));
    }
    if let Some(v) = s.strip_suffix('h').or(s.strip_suffix('p')).and_then(|v| v.parse::<f64>().ok()) {
        return Some((bw * v / bh, v));
    }
    s.parse::<f64>().ok().map(|v| (bw * v, bh * v))
}

/// What to export.
pub enum Target {
    Nodes(Vec<String>),
    Frame(String),
    Deliverables,
}

impl Target {
    pub fn from_json(v: &Json) -> Result<Self> {
        Ok(match v {
            Json::String(s) if s == "deliverables" => Target::Deliverables,
            Json::String(s) => Target::Nodes(vec![s.clone()]),
            Json::Array(a) => Target::Nodes(a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
            _ => bail!("export target: node ids, a frame id or \"deliverables\""),
        })
    }
}

/// Nodes with `role: final`, or inside a frame named like "Deliverables".
pub fn deliverable_nodes(c: &falos_doc::Canvas) -> Vec<String> {
    let nodes = c.nodes();
    let in_deliv = |n: &falos_doc::Node| {
        let mut p = n.parent.clone();
        while let Some(id) = p {
            match c.node(&id) {
                Some(f) if f.kind == NodeKind::Frame && f.name.to_lowercase().contains("deliverable") => return true,
                Some(f) => p = f.parent,
                None => break,
            }
        }
        false
    };
    nodes
        .iter()
        .filter(|n| n.asset_id().is_some() && (n.role() == Some("final") || in_deliv(n)))
        .map(|n| n.id.clone())
        .collect()
}

pub struct ExportOpts {
    pub presets: Vec<Json>,
    pub dir: Option<String>,
    pub naming: Option<String>,
    /// Reframe mode for aspect mismatches (default pad-blur).
    pub reframe: Option<String>,
}

/// Export → `{files:[{path, name, node, preset, w, h, bytes, sha256}], dir, warnings}`.
pub async fn export(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, target: Target, o: ExportOpts) -> Result<Json> {
    let canvas = host.resolve_canvas(canvas)?;
    let deliverables = matches!(target, Target::Deliverables);
    let ids: Vec<String> = match target {
        Target::Deliverables => {
            let ids = host.read(&canvas, deliverable_nodes)?;
            if ids.is_empty() {
                bail!("no deliverables: mark nodes role(final) or put them in a frame named \"Deliverables\"");
            }
            ids
        }
        Target::Frame(f) => vec![f],
        Target::Nodes(n) => n,
    };
    if ids.is_empty() {
        bail!("nothing to export");
    }
    let inputs = resolve_inputs(host, &canvas, &json!(ids))?;
    let project = host.project();
    let dir = match &o.dir {
        Some(d) if Path::new(d).is_absolute() => PathBuf::from(d),
        Some(d) => {
            if d.contains("..") {
                bail!("dir must stay inside the project (or be absolute)");
            }
            host.dir().join(d)
        }
        None if deliverables => host.dir().join("deliverables"),
        None => host.dir().join("exports").join(chrono::Local::now().format("%Y-%m-%d").to_string()),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let pattern = o.naming.clone().unwrap_or_else(|| presets()["naming"].as_str().unwrap_or("{name}_{preset}.{ext}").to_string());
    let tmp = std::env::temp_dir().join(format!("falos-export-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&tmp)?;
    let mut files = vec![];
    let mut warnings: Vec<String> = vec![];
    let r: Result<()> = async {
        for (k, input) in inputs.iter().enumerate() {
            let node = host.read(&canvas, |c| input.node.as_ref().and_then(|n| c.node(n)))?;
            let items: Vec<Json> = if !o.presets.is_empty() {
                o.presets.clone()
            } else {
                node.as_ref().and_then(|n| n.props.get("export")).and_then(Json::as_array).cloned().unwrap_or_else(|| vec![json!("original")])
            };
            for (j, row) in rows_of(&items)?.into_iter().enumerate() {
                let sub = tmp.join(format!("{k}-{j}"));
                std::fs::create_dir_all(&sub)?;
                if let Some((file, w, h, used_node)) = export_one(host, actor, session, &canvas, input, node.as_ref(), &row, &sub, o.reframe.as_deref(), &mut warnings).await? {
                    {
                        verify_output(&file).await?;
                        let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                        let pid = row.preset["id"].as_str().unwrap_or("custom").to_string();
                        let n = 1 + manifest_count(host, used_node.as_deref().unwrap_or(&input.asset), &pid);
                        let name = render_name(&pattern, &[
                            ("project", slug(&project.name)),
                            ("name", slug(node.as_ref().map(|n| n.name.as_str()).filter(|s| !s.is_empty()).unwrap_or(&input.name))),
                            ("preset", pid.clone()),
                            ("w", w.map(|v| v.to_string()).unwrap_or_default()),
                            ("h", h.map(|v| v.to_string()).unwrap_or_default()),
                            ("n", n.to_string()),
                            ("version", n.to_string()),
                            ("index", format!("{:03}", k + 1)),
                            ("date", chrono::Local::now().format("%Y-%m-%d").to_string()),
                            ("node", input.node.clone().unwrap_or_default()),
                            ("scale", row.scale.clone().unwrap_or_default()),
                            ("suffix", row.suffix.clone()),
                            ("ext", ext.clone()),
                        ]);
                        let name = if !row.suffix.is_empty() && !pattern.contains("{suffix}") {
                            match name.rsplit_once('.') {
                                Some((s, e)) => format!("{s}{}.{e}", row.suffix),
                                None => format!("{name}{}", row.suffix),
                            }
                        } else {
                            name
                        };
                        let name = if name.ends_with(&format!(".{ext}")) { name } else { format!("{name}.{ext}") };
                        let dest = free_name(&dir, &name);
                        let want = sha256_file(&file)?;
                        std::fs::copy(&file, &dest).with_context(|| format!("writing {}", dest.display()))?;
                        let got = sha256_file(&dest)?;
                        if got != want {
                            let _ = std::fs::remove_file(&dest);
                            bail!("verification failed for {} (sha256 mismatch after write)", dest.display());
                        }
                        let bytes = std::fs::metadata(&dest)?.len();
                        let rel = dest.strip_prefix(host.dir()).map(|p| p.display().to_string()).unwrap_or_else(|_| dest.display().to_string());
                        append_manifest(host, &json!({
                            "file": rel, "sha": format!("sha256:{got}"), "asset": input.asset, "node": used_node,
                            "preset": pid, "recipe": {"preset": row.preset, "format": row.format, "scale": row.scale, "suffix": row.suffix},
                            "ts": now_ms(), "actor": actor.id,
                        }))?;
                        files.push(json!({
                            "path": dest.display().to_string(),
                            "name": dest.file_name().map(|f| f.to_string_lossy().into_owned()),
                            "node": used_node, "preset": pid, "w": w, "h": h, "bytes": bytes, "sha256": got,
                        }));
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    r?;
    if files.is_empty() {
        bail!("nothing exported: {}", warnings.join("; "));
    }
    let out = json!({"files": files, "dir": dir.display().to_string(), "warnings": warnings});
    // Agent sessions in a chat: file cards.
    if let Some(s) = session
        && let Some(chat) = host.with_sessions(|ss| ss.get(s).and_then(|x| x.chat.clone()))
        && crate::chat::valid_chat_id(&chat)
    {
        let _ = host.chat.append(host, &chat, Some(&canvas), json!({"kind": "files", "files": out["files"], "dir": out["dir"]}));
    }
    Ok(out)
}

fn manifest_path(host: &Host) -> PathBuf {
    host.dir().join("exports").join("manifest.jsonl")
}

fn append_manifest(host: &Host, line: &Json) -> Result<()> {
    let p = manifest_path(host);
    std::fs::create_dir_all(p.parent().unwrap())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Earlier exports of `node_or_asset` with `preset` (for `v{n}`).
fn manifest_count(host: &Host, key: &str, preset: &str) -> usize {
    std::fs::read_to_string(manifest_path(host))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .filter(|l| l["preset"] == preset && (l["node"] == key || l["asset"] == key))
        .count()
}

/// Render one (input × row) into `dir`. Returns (file, w, h, node actually exported).
#[allow(clippy::too_many_arguments)]
async fn export_one(
    host: &Arc<Host>,
    actor: &Actor,
    session: Option<&str>,
    canvas: &str,
    input: &super::Input,
    node: Option<&falos_doc::Node>,
    row: &Row,
    dir: &Path,
    reframe_mode: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Option<(PathBuf, Option<u32>, Option<u32>, Option<String>)>> {
    let p = prepare(input, dir).await?;
    let pr = &row.preset;
    let kind = pr["kind"].as_str().unwrap_or("any");
    let id = pr["id"].as_str().unwrap_or("custom");
    let label = input.node.clone().unwrap_or_else(|| input.asset.clone());
    let dims = |m: &falos_doc::Media| (m.w.map(|v| v as u32), m.h.map(|v| v as u32));
    if pr["copy"] == true {
        let out = dir.join(format!("orig.{}", p.ext));
        std::fs::copy(&p.file, &out)?;
        let (w, h) = dims(&p.media);
        return Ok(Some((out, w, h, input.node.clone())));
    }
    match (kind, p.kind) {
        ("image", NodeKind::Image) => {
            let im = img::load(&p.file).await?;
            let (sw, sh) = (im.width() as f64, im.height() as f64);
            let base = node.filter(|n| n.w > 0.0 && n.h > 0.0).map(|n| (n.w, n.w * sh / sw)).unwrap_or((sw, sh));
            let scale = row.scale.clone().or(pr["scale"].as_str().map(String::from)).unwrap_or_else(|| "1x".into());
            let (mut tw, mut th) = scale_to(&scale, base).ok_or_else(|| anyhow!("bad scale `{scale}` (2x, 0.5x, 1080p, 512w, 512h)"))?;
            if tw > sw + 0.5 {
                warnings.push(format!("{label}: {scale} would be {}px wide but the source is {sw}px; exported at source size (never upscaled — use the upscale op)", tw.round()));
                (tw, th) = (sw, sh);
            }
            let fmt = row.format.clone().or(pr["format"].as_str().map(String::from)).unwrap_or_else(|| "webp".into());
            let fmt = if fmt == "jpeg" { "jpg".to_string() } else { fmt };
            let (tw, th) = ((tw.round() as u32).max(1), (th.round() as u32).max(1));
            let out = dir.join(format!("export.{fmt}"));
            let q = row.quality.or(pr["quality"].as_f64().map(|q| q as f32));
            img::save(&img::resize(&im, tw, th)?, &out, q).await?;
            Ok(Some((out, Some(tw), Some(th), input.node.clone())))
        }
        ("video", NodeKind::Video) => {
            let mut p = p;
            let mut used = input.node.clone();
            if let Some(max) = pr["max_seconds"].as_f64()
                && p.media.duration.is_some_and(|d| d > max)
            {
                warnings.push(format!("{label}: {:.1}s is longer than {id}'s {max}s limit", p.media.duration.unwrap_or(0.0)));
            }
            // Aspect mismatch: reframe into a role:final node first, then export that.
            if let (Some(w), Some(h), Some(sw), Some(sh)) = (pr["w"].as_f64(), pr["h"].as_f64(), p.media.w, p.media.h)
                && pr["container"] != "gif"
                && ((w / h) / (sw / sh) - 1.0).abs() > 0.01
            {
                let mode = reframe_mode.unwrap_or("pad-blur");
                let run = super::start(host, actor, session, canvas, Request {
                    op: "reframe".into(),
                    inputs: input.reference.clone(),
                    args: json!({"aspect": format!("{}:{}", w as i64, h as i64), "mode": mode}),
                    placement: None,
                    name: Some(format!("{} {id}", input.name).trim().to_string()),
                    role: Some("final".into()),
                })?;
                let res = run.wait(Duration::from_secs(30 * 60)).await.ok_or_else(|| anyhow!("reframe timed out"))?;
                let r = res.first().cloned().unwrap_or_default();
                if r["status"] != "done" {
                    bail!("reframe for {id} failed: {}", r["error"]);
                }
                let new_node = r["node"].as_str().unwrap_or_default().to_string();
                warnings.push(format!("{label}: aspect differs from {id} ({}:{}); reframed ({mode}) into final node {new_node}", w as i64, h as i64));
                let ins = resolve_inputs(host, canvas, &json!(new_node))?;
                let sub = dir.join("reframed");
                std::fs::create_dir_all(&sub)?;
                p = prepare(&ins[0], &sub).await?;
                used = Some(new_node);
            }
            let out = encode_video(&p, pr, dir, warnings, &label).await?;
            let (m, _) = probe::probe(&out).await?;
            let (w, h) = dims(&m);
            Ok(Some((out, w, h, used)))
        }
        ("audio", NodeKind::Audio | NodeKind::Video) => {
            if !ff::has_audio(&p) {
                warnings.push(format!("{label}: no audio for {id}"));
                return Ok(None);
            }
            let ext = pr["container"].as_str().unwrap_or("wav");
            let out = ff::audio_extract(&p, &json!({"format": ext}), dir, &|_| {}).await?;
            Ok(Some((out, None, None, input.node.clone())))
        }
        (k, nk) => {
            warnings.push(format!("{label}: preset {id} is for {k}, not {nk}; skipped"));
            Ok(None)
        }
    }
}

async fn encode_video(p: &super::Prepared, pr: &Json, dir: &Path, warnings: &mut Vec<String>, label: &str) -> Result<PathBuf> {
    let container = pr["container"].as_str().unwrap_or("mp4");
    if container == "gif" {
        return ff::gif(p, &json!({"width": pr["w"], "fps": pr["fps"]}), dir, &|_| {}).await;
    }
    let out = dir.join(format!("export.{container}"));
    let (sw, sh) = (p.media.w.unwrap_or(0.0), p.media.h.unwrap_or(0.0));
    let mut vf = vec![];
    if let (Some(w), Some(h)) = (pr["w"].as_f64(), pr["h"].as_f64()) {
        if w > sw + 0.5 || h > sh + 0.5 {
            warnings.push(format!("{label}: source {sw}x{sh} is smaller than {w}x{h}; kept at source size (never upscaled — use the upscale op)"));
        } else {
            vf.push(format!("scale={w}:{h}:flags=lanczos"));
        }
    }
    if let Some(fm) = pr["fps_max"].as_f64()
        && p.media.fps.is_some_and(|f| f > fm + 0.01)
    {
        vf.push(format!("fps={fm}"));
    }
    vf.push(ff::EVEN.into());
    let mut a = vec![ff::s("-i"), p.file.display().to_string(), ff::s("-vf"), vf.join(",")];
    match pr["vcodec"].as_str().unwrap_or("h264") {
        "prores_hq" => a.extend(["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"].map(ff::s)),
        "prores_4444" => a.extend(["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"].map(ff::s)),
        _ => {
            a.extend(["-c:v", "libx264", "-preset", "medium", "-profile:v", "high", "-pix_fmt", "yuv420p"].map(ff::s));
            if let Some(b) = pr["video_bitrate"].as_str() {
                let max = pr["maxrate"].as_str().unwrap_or(b);
                a.extend([ff::s("-b:v"), b.into(), ff::s("-maxrate"), max.into(), ff::s("-bufsize"), max.into()]);
            }
        }
    }
    a.extend(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"].map(ff::s));
    if ff::has_audio(p) {
        match pr["audio"].as_str() {
            Some("pcm_s24le") => a.extend(["-c:a", "pcm_s24le", "-ar", "48000"].map(ff::s)),
            _ => {
                a.extend([ff::s("-c:a"), ff::s("aac"), ff::s("-b:a"), pr["audio_bitrate"].as_str().unwrap_or("192k").into(), ff::s("-ar"), ff::s("48000")]);
            }
        }
        if let Some(l) = num(pr, "lufs") {
            a.extend([ff::s("-af"), format!("loudnorm=I={l}:TP=-1.5:LRA=11")]);
        }
    }
    if container != "webm" {
        a.extend(["-movflags", "+faststart"].map(ff::s));
    }
    a.extend([ff::s("-metadata"), format!("comment={}", p.comment), out.display().to_string()]);
    ff::run(&a, p.media.duration, &|_| {}).await?;
    Ok(out)
}

// ---- packages -------------------------------------------------------------------------------

/// `.zip` of the project (canvases + referenced assets) or its deliverables, with
/// `manifest.json` (nodes → assets, lineage DAG, recipes, models, costs).
pub async fn package(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, kind: &str) -> Result<Json> {
    let canvas = host.resolve_canvas(canvas)?;
    host.flush()?;
    let project = host.project();
    let mut entries: Vec<(String, PathBuf)> = vec![];
    let mut canvases = vec![];
    let mut assets = serde_json::Map::new();
    let mut models = std::collections::BTreeSet::new();
    let mut cost = 0.0;
    let mut referenced = std::collections::BTreeSet::new();
    for c in &project.canvases {
        let (nodes, recs) = host.read(&c.id, |cv| {
            let nodes: Vec<Json> = cv
                .nodes()
                .iter()
                .map(|n| json!({"id": n.id, "type": n.kind.to_string(), "name": n.name, "role": n.role(), "asset": n.asset_id(), "parent": n.parent}))
                .collect();
            let recs: Vec<falos_doc::Asset> = cv.nodes().iter().filter_map(|n| n.asset_id().and_then(|a| cv.asset(a))).collect();
            (nodes, recs)
        })?;
        for n in &nodes {
            if let Some(a) = n["asset"].as_str() {
                referenced.insert(a.to_string());
            }
        }
        for a in recs {
            if let Some(l) = &a.lineage {
                models.insert(format!("{}:{}", l.kind.as_str(), l.op));
                cost += l.cost.unwrap_or(0.0);
                for p in &l.parents {
                    referenced.insert(p.asset.clone());
                }
            }
            assets.insert(a.id.clone(), serde_json::to_value(&a)?);
        }
        canvases.push(json!({"id": c.id, "name": c.name, "nodes": nodes}));
    }
    let dag: Vec<Json> = assets
        .values()
        .filter_map(|a| {
            let l = a.get("lineage")?;
            Some(json!({"asset": a["id"], "op": l["op"], "kind": l["kind"], "parents": l["parents"], "recipe": l["recipe"]}))
        })
        .collect();
    let mut exported = json!(null);
    match kind {
        "project" => {
            entries.push(("project.json".into(), host.dir().join("project.json")));
            for e in std::fs::read_dir(host.dir().join("canvases")).into_iter().flatten().flatten() {
                entries.push((format!("canvases/{}", e.file_name().to_string_lossy()), e.path()));
            }
            for a in &referenced {
                if let Some(f) = crate::chat::asset_file(host, a) {
                    entries.push((format!("assets/{}", f.file_name().unwrap().to_string_lossy()), f));
                }
            }
        }
        "deliverables" => {
            let tmp = std::env::temp_dir().join(format!("falos-pkg-{}", rand::random::<u64>()));
            let r = export(host, actor, session, &canvas, Target::Deliverables, ExportOpts {
                presets: vec![],
                dir: Some(tmp.display().to_string()),
                naming: None,
                reframe: None,
            })
            .await?;
            for f in r["files"].as_array().into_iter().flatten() {
                let p = PathBuf::from(f["path"].as_str().unwrap_or_default());
                entries.push((format!("deliverables/{}", f["name"].as_str().unwrap_or("file")), p));
            }
            exported = r["files"].clone();
        }
        k => bail!("package kind must be project|deliverables, not `{k}`"),
    }
    let manifest = json!({
        "format": "falos-package/1", "kind": kind, "project": {"id": host.id, "name": project.name},
        "created": now_ms(), "actor": actor.id, "canvases": canvases, "assets": assets, "lineage": dag,
        "models": models, "cost_usd": (cost * 10000.0f64).round() / 10000.0, "files": exported,
    });
    let dir = host.dir().join("exports").join(chrono::Local::now().format("%Y-%m-%d").to_string());
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}_{kind}_{}.zip", slug(&project.name), chrono::Local::now().format("%H%M%S"));
    let dest = free_name(&dir, &name);
    let m2 = manifest.clone();
    let d2 = dest.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let f = std::fs::File::create(&d2)?;
        let mut z = zip::ZipWriter::new(f);
        // Media is already compressed: store everything.
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).large_file(true);
        z.start_file("manifest.json", opts)?;
        z.write_all(&serde_json::to_vec_pretty(&m2)?)?;
        for (name, path) in &entries {
            z.start_file(name.as_str(), opts)?;
            let mut src = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
            std::io::copy(&mut src, &mut z)?;
        }
        z.finish()?;
        Ok(())
    })
    .await??;
    if kind == "deliverables" {
        for f in exported.as_array().into_iter().flatten() {
            if let Some(p) = f["path"].as_str()
                && let Some(parent) = Path::new(p).parent()
                && parent.starts_with(std::env::temp_dir())
            {
                let _ = std::fs::remove_dir_all(parent);
                break;
            }
        }
    }
    let sha = sha256_file(&dest)?;
    let bytes = std::fs::metadata(&dest)?.len();
    Ok(json!({"path": dest.display().to_string(), "bytes": bytes, "sha256": sha, "kind": kind,
              "assets": manifest["assets"].as_object().map_or(0, |m| m.len()), "cost_usd": manifest["cost_usd"]}))
}

/// Clonefile copy (APFS; plain copy elsewhere) of a node's / asset's file into
/// `<project>/.out/` under a human name — for drag-out and file URLs (never hardlinks).
pub fn materialize(host: &Host, canvas: &str, key: &str, name: Option<&str>) -> Result<PathBuf> {
    let (asset, nname) = if key.starts_with("sha256:") {
        (key.to_string(), falos_doc::Asset::short_id(key))
    } else {
        let canvas = host.resolve_canvas(canvas)?;
        let n = host.read(&canvas, |c| c.node(key))?.ok_or_else(|| anyhow!("unknown node `{key}`"))?;
        (n.asset_id().ok_or_else(|| anyhow!("`{key}` has no asset"))?.to_string(), if n.name.is_empty() { n.id.clone() } else { n.name.clone() })
    };
    let src = crate::chat::asset_file(host, &asset).ok_or_else(|| anyhow!("no file for {asset}"))?;
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let dir = host.dir().join(".out");
    std::fs::create_dir_all(&dir)?;
    let base = slug(name.unwrap_or(&nname));
    let file = if base.ends_with(&format!(".{ext}")) { base } else { format!("{base}.{ext}") };
    let dest = dir.join(&file);
    if dest.exists() && sha256_file(&dest).ok().as_deref() == Some(asset.trim_start_matches("sha256:")) {
        return Ok(dest);
    }
    let dest = free_name(&dir, &file);
    clone_or_copy(&src, &dest)?;
    Ok(dest)
}

/// APFS `clonefile(2)` (copy-on-write, independent inode), falling back to a byte copy.
pub fn clone_or_copy(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        unsafe extern "C" {
            fn clonefile(src: *const std::ffi::c_char, dst: *const std::ffi::c_char, flags: u32) -> i32;
        }
        let (s, d) = (CString::new(src.as_os_str().as_bytes())?, CString::new(dst.as_os_str().as_bytes())?);
        // SAFETY: both are valid NUL-terminated paths; clonefile doesn't retain them.
        if unsafe { clonefile(s.as_ptr(), d.as_ptr(), 0) } == 0 {
            return Ok(());
        }
    }
    std::fs::copy(src, dst).with_context(|| format!("copying {} → {}", src.display(), dst.display()))?;
    Ok(())
}

pub fn aspect_matches(a: &str, w: f64, h: f64) -> bool {
    parse_aspect(a).is_some_and(|r| ((w / h) / r - 1.0).abs() <= 0.01)
}
