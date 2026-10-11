//! Host op registry (`docs/ASSETS-V1.md` §3): one implementation for the UI context menu
//! (REST `/api/*`), the in-app agent swarm and external MCP agents.
//!
//! How an op runs: placeholder `generation` node(s) beside the input(s) (`find_space`, a
//! grid for batches) → engine (ffmpeg subprocess / in-process image / fal model) → the
//! placeholder converts in place into the output node, whose asset records structured
//! lineage (`kind: derive, op, recipe{op,args,inputs}, parents`) → `job` events with
//! progress and cost on `/sync`.
//!
//! Inputs: node ids, asset ids, `"selection"`, a frame/group id (its media children), or
//! chips: a region chip is an implicit crop, a frame chip extracts that frame, a range chip
//! trims.

pub mod export;
pub mod falops;
pub mod ff;
pub mod img;
pub mod inspect;
pub mod probe;
pub mod rest;
pub mod tools;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, Command, Lineage, LineageKind, Media, NodeKind, Parent, ParentRole, Props};
use serde::Serialize;
use serde_json::{Value as Json, json};
use tokio::sync::{Semaphore, watch};

use crate::host::{Event, Host};
use crate::ingest::{ImportOpts, import_file};
use crate::mcp::PlacementArgs;

/// The system actor that fills in op outputs and progress (kept off the requester's undo
/// stack; the requester is recorded as the lineage actor).
pub fn ops_actor() -> Actor {
    Actor::agent("ops", "fal-os ops")
}

// ---- registry -------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct OpSpec {
    pub name: &'static str,
    /// `inspect` | `transform` | `compose`.
    pub category: &'static str,
    /// `ffmpeg` | `image` | `image+ffmpeg` | `fal`.
    pub engine: &'static str,
    /// `each`: one output per input (batch over a selection = a grid of outputs);
    /// `all`: every input goes into one output.
    pub inputs: &'static str,
    /// Accepted input kinds.
    pub accepts: &'static [&'static str],
    /// Output kind (`same` = like the input).
    pub output: &'static str,
    pub deterministic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_hint: Option<&'static str>,
    pub description: &'static str,
    /// JSON schema of `args`.
    pub args: Json,
}

const IMG: &[&str] = &["image"];
const AV: &[&str] = &["video", "audio"];
const VID: &[&str] = &["video"];
const VIS: &[&str] = &["image", "video"];
const ANY: &[&str] = &["image", "video", "audio"];

fn obj(props: Json) -> Json {
    json!({"type": "object", "properties": props})
}

pub fn specs() -> Vec<OpSpec> {
    let t = |name, engine, inputs, accepts, output, det, description, args| OpSpec {
        name,
        category: "transform",
        engine,
        inputs,
        accepts,
        output,
        deterministic: det,
        cost_hint: None,
        description,
        args,
    };
    let aspect = json!({"type": "string", "description": "w:h, e.g. \"9:16\", \"1:1\", \"16:9\""});
    let fmt_v = json!({"type": "string", "enum": ["mp4", "mov", "webm"], "description": "output container (default: the input's)"});
    let mut v = vec![
        OpSpec {
            name: "inspect",
            category: "inspect",
            engine: "ffmpeg+image",
            inputs: "each",
            accepts: ANY,
            output: "none",
            deterministic: true,
            cost_hint: None,
            description: "Read-only facts, cached by sha: probe (codec, fps, duration, color/HDR, alpha), contact_sheet (image), palette, loudness (EBU R128), scenes, compare (SSIM/PSNR of 2 inputs).",
            args: obj(json!({"what": {"type": "array", "items": {"enum": ["probe", "contact_sheet", "palette", "loudness", "scenes", "compare"]}}})),
        },
        t("crop", "image+ffmpeg", "each", VIS, "same", true, "Crop to a box (normalized 0..1 or px) or center-crop to an aspect. A region chip as input is an implicit crop.",
          obj(json!({"box": {"type": "array", "items": {"type": "number"}, "description": "[x,y,w,h] normalized 0..1"},
                     "px": {"type": "array", "items": {"type": "number"}, "description": "[x,y,w,h] in pixels"},
                     "aspect": aspect, "anchor": {"type": "string", "enum": ["center", "top", "bottom", "left", "right"], "default": "center"}}))),
        t("resize", "image+ffmpeg", "each", VIS, "same", true, "Lanczos resize to w and/or h, or a scale factor. Never upscales unless allow_upscale (use `upscale` for AI upscaling).",
          obj(json!({"w": {"type": "number"}, "h": {"type": "number"}, "scale": {"type": "number"},
                     "fit": {"type": "string", "enum": ["contain", "cover", "fill"], "default": "contain"},
                     "allow_upscale": {"type": "boolean", "default": false}}))),
        t("reframe", "image+ffmpeg|fal", "each", VIS, "same", true, "Change the aspect ratio: crop-center, pad-blur (default), pad-color, or outpaint (fal-ai/bria/expand, images).",
          obj(json!({"aspect": aspect, "w": {"type": "number"}, "h": {"type": "number"},
                     "mode": {"type": "string", "enum": ["pad-blur", "crop-center", "pad-color", "outpaint"], "default": "pad-blur"},
                     "color": {"type": "string", "default": "#000000"}, "prompt": {"type": "string", "description": "outpaint only"}}))),
        t("convert", "image+ffmpeg", "each", ANY, "same", true, "Change format: png jpg webp avif gif (images), mp4 mov webm (video), m4a mp3 wav flac (audio).",
          obj(json!({"format": {"type": "string"}, "quality": {"type": "number", "description": "1..100 (images)"}}))),
        t("compress", "image+ffmpeg", "each", VIS, "same", true, "Re-encode to fit a target size (images: jpg/webp quality search; video: bitrate from duration).",
          obj(json!({"target_kb": {"type": "number"}, "target_mb": {"type": "number"}, "max_w": {"type": "number"}, "format": {"type": "string"}}))),
        t("trim", "ffmpeg", "each", AV, "same", true, "Cut a time range (seconds or mm:ss). A range chip as input is an implicit trim.",
          obj(json!({"start": {"type": ["number", "string"]}, "end": {"type": ["number", "string"]}, "duration": {"type": ["number", "string"]},
                     "copy": {"type": "boolean", "default": false, "description": "stream copy (fast, keyframe-accurate only)"}, "format": fmt_v.clone()}))),
        t("concat", "ffmpeg", "all", AV, "same", true, "Join inputs in order, normalized to the first input's size/fps; optional crossfade seconds.",
          obj(json!({"xfade": {"type": "number", "default": 0}, "fps": {"type": "number"}, "format": fmt_v.clone()}))),
        t("speed", "ffmpeg", "each", AV, "same", true, "Change playback speed (setpts/atempo). factor 2 = twice as fast.",
          obj(json!({"factor": {"type": "number"}}))),
        t("extract_frames", "ffmpeg", "each", VID, "image", true, "Stills from a video: at times[], count evenly spaced, or every N seconds. A frame chip extracts that frame.",
          obj(json!({"times": {"type": "array", "items": {"type": ["number", "string"]}}, "count": {"type": "number"}, "every": {"type": "number"},
                     "format": {"type": "string", "enum": ["png", "jpg"], "default": "png"}}))),
        t("gif", "ffmpeg", "each", VID, "image", true, "Looping GIF with an optimized palette.",
          obj(json!({"fps": {"type": "number", "default": 12}, "width": {"type": "number", "default": 480}, "start": {"type": "number"}, "duration": {"type": "number"}}))),
        t("audio_normalize", "ffmpeg", "each", AV, "same", true, "Two-pass EBU R128 loudness normalization (video keeps its picture).",
          obj(json!({"lufs": {"type": "number", "default": -14}, "tp": {"type": "number", "default": -1.5}, "lra": {"type": "number", "default": 11}}))),
        t("audio_mix", "ffmpeg", "all", AV, "same", true, "Mix a bed under a primary: inputs [primary (voice or video), bed (music)]; ducks the bed under the primary.",
          obj(json!({"bed_gain_db": {"type": "number", "default": -10}, "duck": {"type": "boolean", "default": true}}))),
        t("audio_extract", "ffmpeg", "each", VID, "audio", true, "The audio track of a video as its own node.",
          obj(json!({"format": {"type": "string", "enum": ["m4a", "mp3", "wav", "flac"], "default": "m4a"}}))),
        t("strip", "ffmpeg", "each", AV, "same", true, "Remove the audio track (what: audio) or metadata (what: metadata) without re-encoding.",
          obj(json!({"what": {"type": "string", "enum": ["audio", "metadata"], "default": "audio"}}))),
        OpSpec { cost_hint: Some("~$0.001/image"), ..t("remove_bg", "fal", "each", IMG, "image", false, "Cut out the subject (transparent PNG) with fal-ai/birefnet/v2.",
          obj(json!({"model": {"type": "string", "enum": ["General Use (Light)", "General Use (Heavy)", "Portrait", "Matting"], "default": "General Use (Light)"}}))) },
        OpSpec { cost_hint: Some("$0.001/megapixel (seedvr)"), ..t("upscale", "fal", "each", VIS, "same", false, "AI upscale with fal-ai/seedvr (image and video).",
          obj(json!({"factor": {"type": "number", "default": 2}}))) },
        OpSpec { cost_hint: Some("$0.05/megapixel"), ..t("inpaint", "fal", "each", IMG, "image", false, "Repaint a masked area (fal-ai/flux-pro/v1/fill). mask: an annotation id (box/polygon) or a region chip box; prompt describes the fill.",
          obj(json!({"prompt": {"type": "string"}, "mask": {"type": "string", "description": "annotation id"}, "box": {"type": "array", "items": {"type": "number"}}}))) },
        t("ffmpeg", "ffmpeg", "all", ANY, "same", true, "Escape hatch: raw ffmpeg args with {in0},{in1},… and {out} placeholders, recorded as a re-runnable recipe.",
          obj(json!({"args": {"type": "array", "items": {"type": "string"}}, "ext": {"type": "string", "description": "output extension (default: in0's)"}}))),
    ];
    v.push(OpSpec {
        name: "grid",
        category: "compose",
        engine: "image",
        inputs: "all",
        accepts: VIS,
        output: "image",
        deterministic: true,
        cost_hint: None,
        description: "Grid / collage of the inputs (videos contribute a poster frame).",
        args: obj(json!({"cols": {"type": "number"}, "cell": {"type": "number", "default": 512}, "gap": {"type": "number", "default": 16},
                         "bg": {"type": "string", "default": "#111111"}})),
    });
    v.push(OpSpec {
        name: "contact_sheet",
        category: "compose",
        engine: "ffmpeg+image",
        inputs: "each",
        accepts: VID,
        output: "image",
        deterministic: true,
        cost_hint: None,
        description: "Contact sheet of a video: count evenly spaced frames in a grid.",
        args: obj(json!({"count": {"type": "number", "default": 12}, "cols": {"type": "number", "default": 4}, "cell": {"type": "number", "default": 480}})),
    });
    v
}

pub fn spec(name: &str) -> Option<OpSpec> {
    specs().into_iter().find(|s| s.name == name)
}

/// `args` with the schema defaults filled in (the canonical recipe).
pub fn with_defaults(spec: &OpSpec, args: &Json) -> Json {
    let mut out = args.as_object().cloned().unwrap_or_default();
    if let Some(props) = spec.args["properties"].as_object() {
        for (k, p) in props {
            if let Some(d) = p.get("default")
                && !out.contains_key(k)
            {
                out.insert(k.clone(), d.clone());
            }
        }
    }
    Json::Object(out)
}

pub(crate) fn num(args: &Json, k: &str) -> Option<f64> {
    match &args[k] {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.trim_end_matches(['x', '%']).parse::<f64>().ok().map(|v| if s.ends_with('%') { v / 100.0 } else { v }),
        _ => None,
    }
}

pub(crate) fn str_arg(args: &Json, k: &str) -> Option<String> {
    args[k].as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty())
}

/// "9:16" → 0.5625.
pub fn parse_aspect(s: &str) -> Option<f64> {
    let (a, b) = s.split_once([':', 'x', '/'])?;
    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

// ---- inputs ---------------------------------------------------------------------------------

/// One resolved op input.
#[derive(Clone, Debug)]
pub struct Input {
    pub node: Option<String>,
    pub name: String,
    pub asset: String,
    pub file: PathBuf,
    pub kind: NodeKind,
    /// The chip it came from (region / frame / range / spot), applied before the op.
    pub chip: Option<Json>,
    /// The original input reference (node id, asset id or chip), for the recipe.
    pub reference: Json,
    /// Node rect (canvas coords of its parent) and parent.
    pub rect: Option<[f64; 4]>,
    pub parent: Option<String>,
}

fn media_node_input(host: &Host, c: &falos_doc::Canvas, n: &falos_doc::Node, chip: Option<Json>, reference: Json) -> Result<Input> {
    let asset = n
        .asset_id()
        .ok_or_else(|| anyhow!("node `{}` ({}) has no media asset", n.id, n.kind))?
        .to_string();
    let file = crate::chat::asset_file(host, &asset)
        .ok_or_else(|| anyhow!("the file of `{}` ({asset}) isn't in this project's assets", n.id))?;
    let _ = c;
    Ok(Input {
        node: Some(n.id.clone()),
        name: n.name.clone(),
        asset,
        file,
        kind: n.kind,
        chip,
        reference,
        rect: Some([n.x, n.y, n.w, n.h]),
        parent: n.parent.clone(),
    })
}

fn kind_of_file(file: &Path) -> NodeKind {
    let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    crate::ingest::media_kind(&ext).map_or(NodeKind::Image, |(k, _)| k)
}

/// Resolve `inputs` (a node/asset/frame id, `"selection"`, a chip, or a list of those).
pub fn resolve_inputs(host: &Host, canvas: &str, inputs: &Json) -> Result<Vec<Input>> {
    let canvas = host.resolve_canvas(canvas)?;
    let list: Vec<Json> = match inputs {
        Json::Array(a) => a.clone(),
        Json::Null => bail!("inputs required: node ids, \"selection\", a frame id or chips"),
        v => vec![v.clone()],
    };
    let selected = host.selected_nodes(&canvas);
    let mut out = vec![];
    host.read(&canvas, |c| -> Result<()> {
        for item in &list {
            match item {
                Json::String(s) if s == "selection" => {
                    if selected.is_empty() {
                        bail!("nothing is selected");
                    }
                    for id in &selected {
                        if let Some(n) = c.node(id) {
                            collect(host, c, &n, &mut out)?;
                        }
                    }
                }
                Json::String(s) if s.starts_with("sha256:") => {
                    let file = crate::chat::asset_file(host, s).ok_or_else(|| anyhow!("no file for asset `{s}`"))?;
                    out.push(Input {
                        node: None,
                        name: falos_doc::Asset::short_id(s),
                        asset: s.clone(),
                        kind: kind_of_file(&file),
                        file,
                        chip: None,
                        reference: item.clone(),
                        rect: None,
                        parent: None,
                    });
                }
                Json::String(id) => {
                    let n = c.node(id).ok_or_else(|| anyhow!("unknown node `{id}`"))?;
                    collect(host, c, &n, &mut out)?;
                }
                Json::Object(o) => {
                    let kind = o.get("kind").and_then(Json::as_str).unwrap_or("node");
                    if !matches!(kind, "node" | "region" | "frame" | "range" | "spot") {
                        bail!("unsupported chip kind `{kind}` as an op input");
                    }
                    let id = o.get("node").and_then(Json::as_str).ok_or_else(|| anyhow!("chip needs node"))?;
                    let n = c.node(id).ok_or_else(|| anyhow!("unknown node `{id}`"))?;
                    let chip = (kind != "node").then(|| item.clone());
                    out.push(media_node_input(host, c, &n, chip, item.clone())?);
                }
                v => bail!("bad input {v}"),
            }
        }
        Ok(())
    })??;
    if out.is_empty() {
        bail!("no media inputs found");
    }
    Ok(out)
}

/// A media node, or the media children of a frame/group.
fn collect(host: &Host, c: &falos_doc::Canvas, n: &falos_doc::Node, out: &mut Vec<Input>) -> Result<()> {
    if matches!(n.kind, NodeKind::Frame | NodeKind::Group) {
        let before = out.len();
        for child in &n.children {
            if let Some(ch) = c.node(child)
                && (ch.asset_id().is_some() || matches!(ch.kind, NodeKind::Frame | NodeKind::Group))
            {
                collect(host, c, &ch, out)?;
            }
        }
        if out.len() == before {
            bail!("frame `{}` has no media nodes", n.id);
        }
        return Ok(());
    }
    out.push(media_node_input(host, c, n, None, json!(n.id))?);
    Ok(())
}

/// An input after its chip was applied, probed and ready for an engine.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub input: Input,
    pub file: PathBuf,
    pub ext: String,
    pub kind: NodeKind,
    pub media: Media,
    pub report: Json,
    /// `falos:<input sha>` written into outputs.
    pub comment: String,
}

fn chip_box(chip: &Json) -> Option<[f64; 4]> {
    if chip["kind"] == "spot" {
        let (x, y) = (chip["x"].as_f64().unwrap_or(0.5), chip["y"].as_f64().unwrap_or(0.5));
        return Some([(x - 0.125).clamp(0.0, 0.75), (y - 0.125).clamp(0.0, 0.75), 0.25, 0.25]);
    }
    let a = chip["box"].as_array().filter(|a| a.len() == 4)?;
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?, a[3].as_f64()?])
}

pub async fn prepare(input: &Input, dir: &Path) -> Result<Prepared> {
    let mut file = input.file.clone();
    let mut kind = input.kind;
    if let Some(chip) = &input.chip {
        let ck = chip["kind"].as_str().unwrap_or("");
        match ck {
            "frame" => {
                let t = chip["t"].as_f64().unwrap_or(0.0);
                file = ff::still(&file, t, dir).await?;
                kind = NodeKind::Image;
            }
            "range" => {
                let (m, r) = probe::probe(&file).await?;
                let p = Prepared { input: input.clone(), ext: ext_of(&file), file: file.clone(), kind, media: m, report: r, comment: String::new() };
                let sub = dir.join("range");
                std::fs::create_dir_all(&sub)?;
                file = ff::trim(&p, &json!({"start": chip["t0"], "end": chip["t1"]}), &sub, &|_| {}).await?;
            }
            _ => {}
        }
        if let Some(b) = chip_box(chip).filter(|_| matches!(ck, "region" | "spot" | "frame")) {
            let (m, r) = probe::probe(&file).await?;
            let p = Prepared { input: input.clone(), ext: ext_of(&file), file: file.clone(), kind, media: m, report: r, comment: String::new() };
            let sub = dir.join("chipcrop");
            std::fs::create_dir_all(&sub)?;
            file = crop_px(&p, px_box(&p.media, b, false), &json!({}), &sub, &|_| {}).await?;
        }
    }
    let (media, report) = probe::probe(&file).await?;
    Ok(Prepared {
        ext: ext_of(&file),
        comment: format!("falos:{}", input.asset.trim_start_matches("sha256:")),
        input: input.clone(),
        file,
        kind,
        media,
        report,
    })
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

// ---- geometry -------------------------------------------------------------------------------

fn dims(m: &Media) -> Result<(f64, f64)> {
    match (m.w, m.h) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Ok((w, h)),
        _ => bail!("the input has no picture"),
    }
}

/// Normalized (or pixel) box → integer pixel box clamped to the media.
fn px_box(m: &Media, b: [f64; 4], pixels: bool) -> [u32; 4] {
    let (w, h) = (m.w.unwrap_or(1.0), m.h.unwrap_or(1.0));
    let (x, y, bw, bh) = if pixels { (b[0], b[1], b[2], b[3]) } else { (b[0] * w, b[1] * h, b[2] * w, b[3] * h) };
    let x = x.clamp(0.0, w - 1.0).round();
    let y = y.clamp(0.0, h - 1.0).round();
    let bw = bw.clamp(1.0, w - x).round();
    let bh = bh.clamp(1.0, h - y).round();
    [x as u32, y as u32, bw as u32, bh as u32]
}

/// Largest box of `aspect` inside w×h, placed by `anchor`.
fn aspect_box(w: f64, h: f64, aspect: f64, anchor: &str) -> [f64; 4] {
    let (bw, bh) = if w / h > aspect { (h * aspect, h) } else { (w, w / aspect) };
    let (mut x, mut y) = ((w - bw) / 2.0, (h - bh) / 2.0);
    match anchor {
        "top" => y = 0.0,
        "bottom" => y = h - bh,
        "left" => x = 0.0,
        "right" => x = w - bw,
        _ => {}
    }
    [x, y, bw, bh]
}

fn is_image(p: &Prepared) -> bool {
    p.kind == NodeKind::Image && !matches!(p.ext.as_str(), "gif")
}

/// Image output extension: keep jpg/webp/png, else png.
fn img_ext(p: &Prepared, args: &Json) -> String {
    let e = str_arg(args, "format").unwrap_or_else(|| p.ext.clone());
    match e.as_str() {
        "jpg" | "jpeg" => "jpg".into(),
        "webp" | "png" | "avif" | "gif" => e,
        _ => "png".into(),
    }
}

async fn crop_px(p: &Prepared, b: [u32; 4], args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    if is_image(p) {
        let im = img::load(&p.file).await?;
        let out = dir.join(format!("crop.{}", img_ext(p, args)));
        img::save(&img::crop(&im, b[0], b[1], b[2], b[3]), &out, None).await?;
        return Ok(out);
    }
    ff::filter_one(p, Some(format!("crop={}:{}:{}:{}", b[2], b[3], b[0], b[1])), None, &[], args, dir, progress).await
}

async fn op_crop(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let (w, h) = dims(&p.media)?;
    let nums = |k: &str| -> Option<[f64; 4]> {
        let a = args[k].as_array().filter(|a| a.len() == 4)?;
        Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?, a[3].as_f64()?])
    };
    let b = if let Some(b) = nums("px") {
        px_box(&p.media, b, true)
    } else if let Some(b) = nums("box") {
        px_box(&p.media, b, false)
    } else if let Some(a) = str_arg(args, "aspect").and_then(|a| parse_aspect(&a)) {
        let anchor = str_arg(args, "anchor").unwrap_or_else(|| "center".into());
        px_box(&p.media, aspect_box(w, h, a, &anchor), true)
    } else if p.input.chip.is_some() {
        // The chip already cropped it.
        [0, 0, w as u32, h as u32]
    } else {
        bail!("crop needs box [x,y,w,h] (0..1), px [x,y,w,h] or aspect \"w:h\"");
    };
    crop_px(p, b, args, dir, progress).await
}

/// Target size for resize (never upscales unless allowed).
fn resize_target(p: &Prepared, args: &Json) -> Result<(u32, u32)> {
    let (w, h) = dims(&p.media)?;
    let fit = str_arg(args, "fit").unwrap_or_else(|| "contain".into());
    let (tw, th) = match (num(args, "w"), num(args, "h"), num(args, "scale")) {
        (_, _, Some(s)) if s > 0.0 => (w * s, h * s),
        (Some(tw), Some(th), _) => match fit.as_str() {
            "fill" => (tw, th),
            "cover" => {
                let s = (tw / w).max(th / h);
                (w * s, h * s)
            }
            _ => {
                let s = (tw / w).min(th / h);
                (w * s, h * s)
            }
        },
        (Some(tw), None, _) => (tw, h * tw / w),
        (None, Some(th), _) => (w * th / h, th),
        _ => bail!("resize needs w, h or scale"),
    };
    if (tw > w + 0.5 || th > h + 0.5) && args["allow_upscale"].as_bool() != Some(true) {
        bail!(
            "resize would upscale {}x{} → {}x{}; pass allow_upscale:true, or use the `upscale` op for AI upscaling",
            w, h, tw.round(), th.round()
        );
    }
    Ok(((tw.round() as u32).max(1), (th.round() as u32).max(1)))
}

async fn op_resize(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let (tw, th) = resize_target(p, args)?;
    if is_image(p) {
        let im = img::load(&p.file).await?;
        let out = dir.join(format!("resize.{}", img_ext(p, args)));
        img::save(&img::resize(&im, tw, th)?, &out, None).await?;
        return Ok(out);
    }
    ff::filter_one(p, Some(format!("scale={tw}:{th}:flags=lanczos")), None, &[], args, dir, progress).await
}

/// Target aspect of a reframe: `aspect`, else `w`/`h`.
fn reframe_aspect(args: &Json) -> Result<f64> {
    if let Some(a) = str_arg(args, "aspect").and_then(|a| parse_aspect(&a)) {
        return Ok(a);
    }
    match (num(args, "w"), num(args, "h")) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Ok(w / h),
        _ => bail!("reframe needs aspect \"w:h\" (or w and h)"),
    }
}

async fn op_reframe(ctx: &RunCtx, p: &Prepared, args: &Json, dir: &Path, out: &mut Output) -> Result<PathBuf> {
    let (w, h) = dims(&p.media)?;
    let a = reframe_aspect(args)?;
    let mode = str_arg(args, "mode").unwrap_or_else(|| "pad-blur".into());
    // Output canvas: crop = largest box inside; pad = smallest box around (even sizes).
    let even = |v: f64| ((v / 2.0).round() * 2.0).max(2.0) as u32;
    let (cw, ch) = if mode == "crop-center" {
        let b = aspect_box(w, h, a, "center");
        (even(b[2]), even(b[3]))
    } else if w / h > a {
        (even(w), even(w / a))
    } else {
        (even(h * a), even(h))
    };
    // Optional exact final size (downscale only).
    let fin = match (num(args, "w"), num(args, "h")) {
        (Some(fw), Some(fh)) if fw <= cw as f64 + 0.5 && fh <= ch as f64 + 0.5 => Some((fw as u32, fh as u32)),
        _ => None,
    };
    let progress = &ctx.progress;
    match mode.as_str() {
        "crop-center" => {
            let b = aspect_box(w, h, a, "center");
            let pb = px_box(&p.media, b, true);
            let path = crop_px(p, pb, args, dir, progress).await?;
            finish_size(p, path, fin, args, dir).await
        }
        "pad-color" | "pad-blur" => {
            let blur = mode == "pad-blur";
            if is_image(p) {
                let im = img::load(&p.file).await?;
                let r = img::pad(&im, cw, ch, blur, img::color(args["color"].as_str()))?;
                let r = match fin {
                    Some((fw, fh)) => img::resize(&r, fw, fh)?,
                    None => r,
                };
                let o = dir.join(format!("reframe.{}", img_ext(p, args)));
                img::save(&r, &o, None).await?;
                return Ok(o);
            }
            let (ow, oh) = fin.unwrap_or((cw, ch));
            let vf = if blur {
                format!(
                    "split[a][b];[a]scale={ow}:{oh}:force_original_aspect_ratio=increase,crop={ow}:{oh},boxblur=24:2,eq=brightness=-0.08[bg];[b]scale={ow}:{oh}:force_original_aspect_ratio=decrease[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2"
                )
            } else {
                let c = args["color"].as_str().unwrap_or("#000000").trim_start_matches('#').to_string();
                format!("scale={ow}:{oh}:force_original_aspect_ratio=decrease,pad={ow}:{oh}:(ow-iw)/2:(oh-ih)/2:color=0x{c}")
            };
            ff::filter_one(p, Some(vf), None, &[], args, dir, progress).await
        }
        "outpaint" => {
            if !is_image(p) {
                bail!("outpaint works on images; use pad-blur for video");
            }
            let url = falops::public_url(&ctx.host, &ctx.actor, &ctx.canvas, &p.input.asset, &p.file).await?;
            let (iw, ih) = (w as u32, h as u32);
            let input = json!({
                "image_url": url, "canvas_size": [cw, ch], "original_image_size": [iw, ih],
                "original_image_location": [(cw - iw) / 2, (ch - ih) / 2],
                "prompt": args["prompt"].as_str().unwrap_or(""),
            });
            let r = falops::run_model(&ctx.host, &ctx.canvas, &ctx.node, &ctx.actor.id, falops::EXPAND, input, progress).await?;
            out.fal_request_id = Some(r.request_id.clone());
            out.cost = falops::estimate(falops::EXPAND, (cw * ch) as f64 / 1e6, r.seconds);
            out.engine = falops::EXPAND.into();
            let f = falops::download_first(&ctx.host, &r.result, dir).await?;
            finish_size(p, f, fin, args, dir).await
        }
        m => bail!("reframe mode must be pad-blur|crop-center|pad-color|outpaint, not `{m}`"),
    }
}

async fn finish_size(p: &Prepared, path: PathBuf, fin: Option<(u32, u32)>, args: &Json, dir: &Path) -> Result<PathBuf> {
    let Some((fw, fh)) = fin else { return Ok(path) };
    let (m, r) = probe::probe(&path).await?;
    let q = Prepared { file: path.clone(), ext: ext_of(&path), media: m, report: r, ..p.clone() };
    let sub = dir.join("final");
    std::fs::create_dir_all(&sub)?;
    op_resize(&q, &json!({"w": fw, "h": fh, "fit": "fill", "allow_upscale": true, "format": args["format"]}), &sub, &|_| {}).await
}

async fn op_convert(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let fmt = str_arg(args, "format").ok_or_else(|| anyhow!("convert needs format"))?;
    let fmt = if fmt == "jpeg" { "jpg".to_string() } else { fmt };
    let quality = num(args, "quality").map(|q| q as f32);
    if p.kind == NodeKind::Image && matches!(fmt.as_str(), "png" | "jpg" | "webp" | "avif" | "gif" | "bmp") {
        let im = img::load(&p.file).await?;
        let out = dir.join(format!("convert.{fmt}"));
        img::save(&im, &out, quality).await?;
        return Ok(out);
    }
    match fmt.as_str() {
        "mp4" | "mov" | "webm" | "m4v" if ff::has_video(p) => ff::filter_one(p, None, None, &[], args, dir, progress).await,
        "m4a" | "mp3" | "wav" | "flac" | "ogg" | "aac" => ff::audio_extract(p, &json!({"format": fmt}), dir, progress).await,
        "gif" if ff::has_video(p) => ff::gif(p, args, dir, progress).await,
        f => bail!("can't convert {} to {f}", p.kind),
    }
}

async fn op_compress(p: &Prepared, args: &Json, dir: &Path, progress: &(dyn Fn(f64) + Send + Sync)) -> Result<PathBuf> {
    let target = num(args, "target_kb").map(|k| k * 1024.0).or(num(args, "target_mb").map(|m| m * 1024.0 * 1024.0));
    let target = target.ok_or_else(|| anyhow!("compress needs target_kb or target_mb"))?;
    if is_image(p) {
        let mut im = img::load(&p.file).await?;
        if let Some(mw) = num(args, "max_w").filter(|m| *m < im.width() as f64) {
            let h = (im.height() as f64 * mw / im.width() as f64) as u32;
            im = img::resize(&im, mw as u32, h)?;
        }
        let fmt = match str_arg(args, "format").as_deref() {
            Some("webp") => "webp",
            Some("jpg" | "jpeg") => "jpg",
            _ if im.color().has_alpha() => "webp",
            _ => "jpg",
        };
        let out = dir.join(format!("compress.{fmt}"));
        // Binary search on quality.
        let (mut lo, mut hi, mut best) = (10.0f32, 95.0f32, None);
        for _ in 0..7 {
            let q = (lo + hi) / 2.0;
            img::save(&im, &out, Some(q)).await?;
            let size = std::fs::metadata(&out)?.len() as f64;
            if size <= target {
                best = Some(q);
                lo = q;
            } else {
                hi = q;
            }
        }
        let q = best.unwrap_or(10.0);
        img::save(&im, &out, Some(q)).await?;
        if std::fs::metadata(&out)?.len() as f64 > target * 1.05 {
            bail!("can't fit {} KB even at quality 10; pass max_w to shrink it", (target / 1024.0) as u64);
        }
        return Ok(out);
    }
    let dur = p.media.duration.ok_or_else(|| anyhow!("compress: unknown duration"))?;
    let audio_kbps = if ff::has_audio(p) { 128.0 } else { 0.0 };
    let video_kbps = (target * 8.0 / 1024.0 / dur * 0.95 - audio_kbps).floor();
    if video_kbps < 100.0 {
        bail!("{} KB is too small for {dur:.1}s of video (needs ≥ ~{} KB)", (target / 1024.0) as u64, ((100.0 + audio_kbps) * dur / 8.0 / 0.95) as u64);
    }
    let out = dir.join("compress.mp4");
    let mut vf = ff::EVEN.to_string();
    if let Some(mw) = num(args, "max_w") {
        vf = format!("scale='min({mw},iw)':-2,{vf}");
    }
    let mut a = vec![ff::s("-i"), p.file.display().to_string(), ff::s("-vf"), vf];
    a.extend(["-c:v", "libx264", "-preset", "medium", "-pix_fmt", "yuv420p"].map(ff::s));
    a.extend([ff::s("-b:v"), format!("{video_kbps}k"), ff::s("-maxrate"), format!("{}k", video_kbps * 1.2), ff::s("-bufsize"), format!("{}k", video_kbps * 2.0)]);
    a.extend(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-movflags", "+faststart"].map(ff::s));
    if audio_kbps > 0.0 {
        a.extend(["-c:a", "aac", "-b:a", "128k", "-ar", "48000"].map(ff::s));
    }
    a.extend([ff::s("-metadata"), format!("comment={}", p.comment), out.display().to_string()]);
    ff::run(&a, Some(dur), progress).await?;
    Ok(out)
}

/// Rasterize an annotation (box / polygon, node-normalized) into a white-on-black mask.
fn mask_png(anchor: &falos_doc::Anchor, w: u32, h: u32, path: &Path) -> Result<()> {
    let region = anchor.region().ok_or_else(|| anyhow!("the annotation has no 2D region"))?;
    let poly: Vec<[f64; 2]> = match region {
        falos_doc::Region::Box { x, y, w, h } => vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
        falos_doc::Region::Polygon { points } => points,
        falos_doc::Region::Point { x, y } => vec![[x - 0.1, y - 0.1], [x + 0.1, y - 0.1], [x + 0.1, y + 0.1], [x - 0.1, y + 0.1]],
        falos_doc::Region::Mask { .. } => bail!("brush masks aren't supported yet; use a box or polygon"),
    };
    let mut m = image::GrayImage::new(w, h);
    for py in 0..h {
        let fy = (py as f64 + 0.5) / h as f64;
        for px in 0..w {
            let fx = (px as f64 + 0.5) / w as f64;
            // Even-odd point in polygon.
            let mut inside = false;
            let mut j = poly.len() - 1;
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[j]);
                if (a[1] > fy) != (b[1] > fy) && fx < (b[0] - a[0]) * (fy - a[1]) / (b[1] - a[1]) + a[0] {
                    inside = !inside;
                }
                j = i;
            }
            if inside {
                m.put_pixel(px, py, image::Luma([255]));
            }
        }
    }
    m.save(path)?;
    Ok(())
}

// ---- execution ------------------------------------------------------------------------------

/// What an engine produced.
#[derive(Default)]
pub struct Output {
    pub files: Vec<PathBuf>,
    pub engine: String,
    pub fal_request_id: Option<String>,
    pub cost: Option<f64>,
    pub seed: Option<i64>,
    /// Inputs besides the op inputs (e.g. an inpaint mask).
    pub extra_parents: Vec<Parent>,
}

pub struct RunCtx {
    pub host: Arc<Host>,
    pub canvas: String,
    /// The placeholder node.
    pub node: String,
    pub actor: Actor,
    pub progress: Box<dyn Fn(f64) + Send + Sync>,
}

pub async fn execute(ctx: &RunCtx, op: &str, ps: &[Prepared], args: &Json, dir: &Path) -> Result<Output> {
    let mut out = Output { engine: ff::version(), ..Default::default() };
    let p = &ps[0];
    let progress: &(dyn Fn(f64) + Send + Sync) = &*ctx.progress;
    let img_engine = |p: &Prepared| if is_image(p) { img::ENGINE.to_string() } else { ff::version() };
    let file = match op {
        "crop" => {
            out.engine = img_engine(p);
            op_crop(p, args, dir, progress).await?
        }
        "resize" => {
            out.engine = img_engine(p);
            op_resize(p, args, dir, progress).await?
        }
        "reframe" => {
            out.engine = img_engine(p);
            op_reframe(ctx, p, args, dir, &mut out).await?
        }
        "convert" => {
            out.engine = img_engine(p);
            op_convert(p, args, dir, progress).await?
        }
        "compress" => {
            out.engine = img_engine(p);
            op_compress(p, args, dir, progress).await?
        }
        "trim" => ff::trim(p, args, dir, progress).await?,
        "concat" => ff::concat(ps, args, dir, progress).await?,
        "speed" => ff::speed(p, args, dir, progress).await?,
        "extract_frames" => {
            out.files = ff::extract_frames(p, args, dir).await?;
            return Ok(out);
        }
        "gif" => ff::gif(p, args, dir, progress).await?,
        "audio_normalize" => ff::audio_normalize(p, args, dir, progress).await?,
        "audio_mix" => ff::audio_mix(ps, args, dir, progress).await?,
        "audio_extract" => ff::audio_extract(p, args, dir, progress).await?,
        "strip" => ff::strip(p, args, dir).await?,
        "ffmpeg" => ff::raw(ps, args, dir, progress).await?,
        "grid" => {
            out.engine = img::ENGINE.into();
            let mut imgs = vec![];
            for q in ps {
                let f = if q.kind == NodeKind::Video { ff::still(&q.file, q.media.duration.unwrap_or(0.0) * 0.3, dir).await? } else { q.file.clone() };
                imgs.push(img::load(&f).await?);
            }
            let cols = num(args, "cols").map(|c| c as usize).unwrap_or_else(|| (imgs.len() as f64).sqrt().ceil() as usize);
            let cell = num(args, "cell").unwrap_or(512.0).clamp(32.0, 2048.0) as u32;
            let gap = num(args, "gap").unwrap_or(16.0).clamp(0.0, 256.0) as u32;
            let g = img::grid(&imgs, cols, cell, gap, img::color(args["bg"].as_str().or(Some("#111111"))))?;
            let o = dir.join("grid.png");
            img::save(&g, &o, None).await?;
            o
        }
        "contact_sheet" => {
            out.engine = format!("{} + {}", ff::version(), img::ENGINE);
            let count = num(args, "count").unwrap_or(12.0).clamp(1.0, 64.0);
            let frames = ff::extract_frames(p, &json!({"count": count, "format": "jpg"}), dir).await?;
            let mut imgs = vec![];
            for f in &frames {
                imgs.push(img::load(f).await?);
            }
            let cols = num(args, "cols").unwrap_or(4.0) as usize;
            let cell = num(args, "cell").unwrap_or(480.0).clamp(64.0, 1920.0) as u32;
            let g = img::grid(&imgs, cols, cell, 8, img::color(Some("#111111")))?;
            let o = dir.join("contact.jpg");
            img::save(&g, &o, Some(88.0)).await?;
            o
        }
        "remove_bg" => {
            let url = falops::public_url(&ctx.host, &ctx.actor, &ctx.canvas, &p.input.asset, &p.file).await?;
            let model = args["model"].as_str().unwrap_or("General Use (Light)");
            let r = falops::run_model(&ctx.host, &ctx.canvas, &ctx.node, &ctx.actor.id, falops::REMOVE_BG,
                json!({"image_url": url, "model": model, "output_format": "png"}), progress).await?;
            out.engine = falops::REMOVE_BG.into();
            out.fal_request_id = Some(r.request_id.clone());
            out.cost = falops::estimate(falops::REMOVE_BG, 0.0, r.seconds);
            falops::download_first(&ctx.host, &r.result, dir).await?
        }
        "upscale" => {
            let factor = num(args, "factor").unwrap_or(2.0).clamp(1.0, 10.0);
            let url = falops::public_url(&ctx.host, &ctx.actor, &ctx.canvas, &p.input.asset, &p.file).await?;
            let (model, input) = if p.kind == NodeKind::Video {
                (falops::UPSCALE_VIDEO, json!({"video_url": url, "upscale_mode": "factor", "upscale_factor": factor}))
            } else {
                (falops::UPSCALE_IMAGE, json!({"image_url": url, "upscale_mode": "factor", "upscale_factor": factor, "output_format": "png"}))
            };
            let r = falops::run_model(&ctx.host, &ctx.canvas, &ctx.node, &ctx.actor.id, model, input, progress).await?;
            let (w, h) = (p.media.w.unwrap_or(1024.0) * factor, p.media.h.unwrap_or(1024.0) * factor);
            let frames = p.media.duration.unwrap_or(0.0) * p.media.fps.unwrap_or(0.0);
            out.engine = model.into();
            out.fal_request_id = Some(r.request_id.clone());
            out.seed = r.result["seed"].as_i64();
            out.cost = falops::estimate(model, w * h / 1e6 * frames.max(1.0), r.seconds);
            falops::download_first(&ctx.host, &r.result, dir).await?
        }
        "inpaint" => {
            let prompt = args["prompt"].as_str().ok_or_else(|| anyhow!("inpaint needs a prompt"))?;
            let (w, h) = dims(&p.media)?;
            let mask = dir.join("mask.png");
            if let Some(ann) = args["mask"].as_str() {
                let a = ctx.host.read(&ctx.canvas, |c| c.annotation(ann))?.ok_or_else(|| anyhow!("unknown annotation `{ann}`"))?;
                mask_png(&a.anchor, w as u32, h as u32, &mask)?;
            } else if let Some(b) = args["box"].as_array().filter(|a| a.len() == 4) {
                let v: Vec<f64> = b.iter().filter_map(Json::as_f64).collect();
                mask_png(&falos_doc::Anchor::Box { x: v[0], y: v[1], w: v[2], h: v[3] }, w as u32, h as u32, &mask)?;
            } else {
                bail!("inpaint needs mask (an annotation id) or box [x,y,w,h]");
            }
            let url = falops::public_url(&ctx.host, &ctx.actor, &ctx.canvas, &p.input.asset, &p.file).await?;
            let mask_url = falops::upload(&ctx.host, &mask).await?;
            let r = falops::run_model(&ctx.host, &ctx.canvas, &ctx.node, &ctx.actor.id, falops::FILL,
                json!({"image_url": url, "mask_url": mask_url, "prompt": prompt, "output_format": "png"}), progress).await?;
            out.engine = falops::FILL.into();
            out.fal_request_id = Some(r.request_id.clone());
            out.seed = r.result["seed"].as_i64();
            out.cost = falops::estimate(falops::FILL, w * h / 1e6, r.seconds);
            falops::download_first(&ctx.host, &r.result, dir).await?
        }
        o => bail!("unknown op `{o}` (see list_ops)"),
    };
    out.files.push(file);
    Ok(out)
}

// ---- runner ---------------------------------------------------------------------------------

pub struct Request {
    pub op: String,
    pub inputs: Json,
    pub args: Json,
    pub placement: Option<PlacementArgs>,
    pub name: Option<String>,
    /// Role of the output node(s) (e.g. `final` for an export reframe).
    pub role: Option<String>,
}

/// A started op: placeholder ids at once; `wait` for the outcome.
pub struct Run {
    pub id: String,
    pub op: String,
    pub nodes: Vec<String>,
    pub frame: Option<String>,
    done: watch::Receiver<Option<Vec<Json>>>,
}

impl Run {
    /// Per-job results (`{node, status, asset?, nodes?, error?, cost?}`), or `None` on timeout.
    pub async fn wait(&self, timeout: Duration) -> Option<Vec<Json>> {
        let mut rx = self.done.clone();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|d| d.is_some())).await;
        rx.borrow().clone()
    }

    pub fn summary(&self, results: Option<&[Json]>) -> Json {
        json!({
            "run": self.id, "op": self.op, "nodes": self.nodes, "frame": self.frame,
            "status": if results.is_some() { "done" } else { "running" },
            "results": results,
        })
    }
}

#[derive(Default)]
pub struct Runs(Mutex<HashMap<String, watch::Receiver<Option<Vec<Json>>>>>);

const GAP: f64 = 24.0;
const MAX_JOBS: usize = 200;

fn short_id(prefix: &str) -> String {
    format!("{prefix}{}", (0..5).map(|_| format!("{:02x}", rand::random::<u8>())).collect::<String>())
}

/// Start an op: resolve inputs, place placeholders, run in the background.
pub fn start(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, req: Request) -> Result<Run> {
    let spec = spec(&req.op).ok_or_else(|| anyhow!("unknown op `{}` (see list_ops)", req.op))?;
    if spec.category == "inspect" {
        bail!("`inspect` is read-only: call the inspect tool");
    }
    let canvas = host.resolve_canvas(canvas)?;
    let inputs = resolve_inputs(host, &canvas, &req.inputs)?;
    for i in &inputs {
        let k = i.kind.to_string();
        let ok = spec.accepts.contains(&k.as_str()) || (i.chip.as_ref().is_some_and(|c| c["kind"] == "frame") && spec.accepts.contains(&"image"));
        if !ok {
            bail!("`{}` takes {} inputs, not {k} (`{}`)", spec.name, spec.accepts.join("/"), i.node.as_deref().unwrap_or(&i.asset));
        }
    }
    let jobs: Vec<Vec<Input>> = if spec.inputs == "each" { inputs.into_iter().map(|i| vec![i]).collect() } else { vec![inputs] };
    if jobs.len() > MAX_JOBS {
        bail!("at most {MAX_JOBS} inputs per op run");
    }
    let args = with_defaults(&spec, &req.args);
    // Placeholder size: the input node's size (audio outputs are strips).
    let out_audio = spec.output == "audio" || (spec.output == "same" && jobs[0][0].kind == NodeKind::Audio);
    let cell = |inp: &Input| -> (f64, f64) {
        if out_audio {
            return (480.0, 96.0);
        }
        match inp.rect {
            Some(r) if r[2] > 0.0 && r[3] > 0.0 => {
                let s = (480.0 / r[2].max(r[3])).min(1.0);
                (r[2] * s, r[3] * s)
            }
            _ => (320.0, 320.0),
        }
    };
    let p = req.placement.unwrap_or_default();
    let near: Vec<String> = p.near.clone().unwrap_or_else(|| jobs.iter().flatten().filter_map(|i| i.node.clone()).collect());
    let parent = p.parent.clone().or_else(|| if jobs.len() == 1 { jobs[0][0].parent.clone() } else { None });
    let mut cmds = vec![];
    let mut nodes = vec![];
    let mut frame = None;
    if jobs.len() == 1 {
        let (w, h) = (p.w.unwrap_or(cell(&jobs[0][0]).0), p.h.unwrap_or(cell(&jobs[0][0]).1));
        let (x, y) = match (p.x, p.y) {
            (Some(x), Some(y)) => (x, y),
            _ => host.find_space(&canvas, parent.as_deref(), &near, w, h)?,
        };
        let id = short_id("o");
        cmds.push(placeholder(&id, &spec, &args, &jobs[0], req.name.as_deref(), parent.clone(), x, y, w, h)?);
        nodes.push(id);
    } else {
        let (cw, ch) = cell(&jobs[0][0]);
        let cols = crate::batch::columns(jobs.len(), cw, ch);
        let rows = jobs.len().div_ceil(cols);
        let gw = cols as f64 * (cw + GAP) - GAP;
        let gh = rows as f64 * (ch + GAP) - GAP;
        let framed = jobs.len() >= crate::batch::FRAME_AT;
        let (bw, bh) = if framed { (gw + 64.0, gh + 104.0) } else { (gw, gh) };
        let (bx, by) = host.find_space(&canvas, p.parent.as_deref(), &near, bw, bh)?;
        let (ox, oy, par) = if framed {
            let fid = short_id("f");
            cmds.push(Command::CreateNode {
                id: Some(fid.clone()),
                kind: NodeKind::Frame,
                parent: p.parent.clone(),
                index: None,
                name: format!("{} ×{}", spec.name, jobs.len()),
                props: serde_json::from_value(json!({"x": bx, "y": by, "w": bw, "h": bh}))?,
                text: None,
            });
            frame = Some(fid.clone());
            (32.0, 72.0, Some(fid))
        } else {
            (bx, by, p.parent.clone())
        };
        for (k, job) in jobs.iter().enumerate() {
            let id = short_id("o");
            let (c, r) = (k % cols, k / cols);
            cmds.push(placeholder(&id, &spec, &args, job, None, par.clone(), ox + c as f64 * (cw + GAP), oy + r as f64 * (ch + GAP), cw, ch)?);
            nodes.push(id);
        }
    }
    host.mutate(&canvas, &actor.id, None, |c| c.apply(actor, None, &cmds))?;
    if let Some(s) = session {
        let mut shown = nodes.clone();
        shown.extend(frame.clone());
        host.activity(s, &canvas, "write", &shown);
    }
    let (tx, rx) = watch::channel(None);
    let run = Run { id: short_id("run_"), op: spec.name.to_string(), nodes: nodes.clone(), frame, done: rx.clone() };
    host.ops_runs.0.lock().unwrap().insert(run.id.clone(), rx);
    let host2 = host.clone();
    let actor2 = actor.clone();
    let role = req.role.clone();
    tokio::spawn(async move {
        let sem = Arc::new(Semaphore::new(3));
        let mut handles = vec![];
        for (job, node) in jobs.into_iter().zip(nodes) {
            let (host, actor, canvas, spec, args, role, sem) = (host2.clone(), actor2.clone(), canvas.clone(), spec.clone(), args.clone(), role.clone(), sem.clone());
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await;
                run_job(&host, &actor, &canvas, &spec, &args, job, &node, role).await
            }));
        }
        let mut results = vec![];
        for h in handles {
            results.push(h.await.unwrap_or_else(|e| json!({"status": "failed", "error": e.to_string()})));
        }
        let _ = tx.send(Some(results));
    });
    Ok(run)
}

#[allow(clippy::too_many_arguments)]
fn placeholder(id: &str, spec: &OpSpec, args: &Json, job: &[Input], name: Option<&str>, parent: Option<String>, x: f64, y: f64, w: f64, h: f64) -> Result<Command> {
    let name = name.map(String::from).unwrap_or_else(|| {
        let base = if job.len() == 1 { job[0].name.clone() } else { format!("{} inputs", job.len()) };
        format!("{base} · {}", spec.name).trim_start_matches(" · ").to_string()
    });
    Ok(Command::CreateNode {
        id: Some(id.into()),
        kind: NodeKind::Generation,
        parent,
        index: None,
        name: name.chars().take(80).collect(),
        props: serde_json::from_value(json!({
            "x": x, "y": y, "w": w, "h": h, "status": "queued", "model": format!("op:{}", spec.name),
            "params": {"op": spec.name, "args": args},
        }))?,
        text: None,
    })
}

fn emit_job(host: &Host, canvas: &str, node: &str, op: &str, status: &str, error: Option<String>, cost: Option<f64>) {
    host.emit(Event::Job {
        canvas: canvas.into(),
        node: node.into(),
        model: format!("op:{op}"),
        status: status.into(),
        queue_position: None,
        error,
        cost,
    });
}

fn update_placeholder(host: &Host, canvas: &str, node: &str, props: Json) -> bool {
    let actor = ops_actor();
    let props: Props = serde_json::from_value(props).unwrap_or_default();
    host.mutate(canvas, &actor.id, None, |c| {
        if c.node(node).is_none_or(|n| n.kind != NodeKind::Generation) {
            return Ok(false);
        }
        c.apply(&actor, None, &[Command::UpdateNode { id: node.into(), props }])?;
        Ok(true)
    })
    .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
async fn run_job(host: &Arc<Host>, actor: &Actor, canvas: &str, spec: &OpSpec, args: &Json, job: Vec<Input>, node: &str, role: Option<String>) -> Json {
    let dir = std::env::temp_dir().join(format!("falos-op-{}", short_id("")));
    let r = run_job_inner(host, actor, canvas, spec, args, &job, node, role, &dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::warn!("op {} on {node} failed: {msg}", spec.name);
            update_placeholder(host, canvas, node, json!({"status": "failed", "error": msg.chars().take(400).collect::<String>()}));
            emit_job(host, canvas, node, spec.name, "failed", Some(msg.clone()), None);
            json!({"node": node, "status": "failed", "error": msg})
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_job_inner(host: &Arc<Host>, actor: &Actor, canvas: &str, spec: &OpSpec, args: &Json, job: &[Input], node: &str, role: Option<String>, dir: &Path) -> Result<Json> {
    std::fs::create_dir_all(dir)?;
    if !update_placeholder(host, canvas, node, json!({"status": "running", "progress": 0.02})) {
        return Ok(json!({"node": node, "status": "canceled"}));
    }
    emit_job(host, canvas, node, spec.name, "running", None, None);
    let mut prepared = vec![];
    for (i, inp) in job.iter().enumerate() {
        let sub = dir.join(format!("in{i}"));
        std::fs::create_dir_all(&sub)?;
        prepared.push(prepare(inp, &sub).await?);
    }
    let last = Arc::new(Mutex::new((Instant::now(), 0.0f64)));
    let (h2, c2, n2) = (host.clone(), canvas.to_string(), node.to_string());
    let ctx = RunCtx {
        host: host.clone(),
        canvas: canvas.into(),
        node: node.into(),
        actor: actor.clone(),
        progress: Box::new(move |p: f64| {
            let mut l = last.lock().unwrap();
            if l.0.elapsed() >= Duration::from_millis(400) && (p - l.1).abs() >= 0.03 {
                *l = (Instant::now(), p);
                drop(l);
                update_placeholder(&h2, &c2, &n2, json!({"progress": (p * 0.95 * 100.0).round() / 100.0}));
            }
        }),
    };
    let outdir = dir.join("out");
    std::fs::create_dir_all(&outdir)?;
    let out = execute(&ctx, spec.name, &prepared, args, &outdir).await?;
    if out.files.is_empty() {
        bail!("{} produced no output", spec.name);
    }
    for f in &out.files {
        verify_output(f).await?;
    }
    let mut parents: Vec<Parent> = job.iter().map(|i| Parent::input(&i.asset, i.node.as_deref())).collect();
    parents.extend(out.extra_parents.clone());
    let mut recipe = json!({"op": spec.name, "args": args, "inputs": job.iter().map(|i| i.reference.clone()).collect::<Vec<_>>()});
    if let Some(chip) = job.iter().find_map(|i| i.chip.clone()) {
        recipe["chip"] = chip;
    }
    let lineage = Lineage {
        kind: LineageKind::Derive,
        op: spec.name.into(),
        recipe,
        parents,
        seed: out.seed,
        fal_request_id: out.fal_request_id.clone(),
        cost: out.cost,
        actor: actor.id.clone(),
        deterministic: spec.deterministic,
        engine_version: Some(out.engine.clone()),
    };
    let writer = ops_actor();
    let mut created = vec![];
    let mut first_asset = None;
    for (i, f) in out.files.iter().enumerate() {
        let opts = ImportOpts {
            full_lineage: Some(lineage.clone()),
            replace: (i == 0).then(|| node.to_string()),
            place: (i > 0).then(|| PlacementArgs { near: Some(vec![created.last().cloned().unwrap_or_else(|| node.to_string())]), ..Default::default() }),
            name: (i > 0).then(|| format!("{} {}", host.read(canvas, |c| c.node(node).map(|n| n.name)).ok().flatten().unwrap_or_default(), i + 1)),
            role: role.clone(),
            protected: Some(false),
            ..Default::default()
        };
        match import_file(host, &writer, canvas, f, opts).await {
            Ok(im) => {
                first_asset.get_or_insert(im.asset.clone());
                created.push(im.node);
            }
            // The placeholder was deleted meanwhile: the op was canceled.
            Err(e) if i == 0 && e.to_string().contains("is not a generation node") => {
                return Ok(json!({"node": node, "status": "canceled"}));
            }
            Err(e) => return Err(e),
        }
    }
    emit_job(host, canvas, node, spec.name, "done", None, out.cost);
    Ok(json!({"node": node, "status": "done", "asset": first_asset, "nodes": created, "cost": out.cost}))
}

/// Refuse empty / undecodable / fully transparent outputs.
pub async fn verify_output(f: &Path) -> Result<()> {
    let len = std::fs::metadata(f).with_context(|| format!("missing output {}", f.display()))?.len();
    if len == 0 {
        bail!("refusing an empty output ({})", f.display());
    }
    let ext = ext_of(f);
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp") {
        let im = img::load(f).await.context("output isn't a decodable image")?;
        if img::is_blank(&im) {
            bail!("refusing a blank (fully transparent) output");
        }
    } else if !matches!(ext.as_str(), "gif" | "avif") {
        let (m, _) = probe::probe(f).await.context("output isn't readable media")?;
        if m.w.is_none() && m.duration.is_none() {
            bail!("output has no picture or audio");
        }
    }
    Ok(())
}

// ---- mediaInputs ----------------------------------------------------------------------------

/// Lineage role of a model input field (`mask_url` → mask, `end_image_url` → end_frame, …).
pub fn role_for_param(param: &str) -> ParentRole {
    let p = param.to_lowercase();
    if p.contains("mask") {
        ParentRole::Mask
    } else if p.contains("end") || p.contains("last") || p.contains("tail") {
        ParentRole::EndFrame
    } else if p.contains("start") || p.contains("first") {
        ParentRole::StartFrame
    } else if p.contains("audio") || p.contains("voice") || p.contains("music") {
        ParentRole::Audio
    } else if p.contains("style") {
        ParentRole::Style
    } else if p.contains("ref") {
        ParentRole::Reference
    } else {
        ParentRole::Input
    }
}

/// Resolve `mediaInputs: {param: node | chip | [..]}` to public URLs (fal storage upload,
/// cached in the asset's `uri`). Chips are applied first (a region becomes a cropped asset,
/// a frame a still) and stored as derived assets so they upload once too. Returns the
/// fields to merge into the model input and the lineage parents.
pub async fn media_inputs(host: &Arc<Host>, actor: &Actor, canvas: &str, map: &serde_json::Map<String, Json>) -> Result<(serde_json::Map<String, Json>, Vec<Parent>)> {
    let canvas = host.resolve_canvas(canvas)?;
    let mut fields = serde_json::Map::new();
    let mut parents = vec![];
    for (param, refs) in map {
        let many = param.ends_with("_urls") || refs.is_array();
        let inputs = resolve_inputs(host, &canvas, refs)?;
        let mut urls = vec![];
        for inp in inputs {
            let (asset, file) = if inp.chip.is_some() {
                let dir = std::env::temp_dir().join(format!("falos-mi-{}", short_id("")));
                std::fs::create_dir_all(&dir)?;
                let p = prepare(&inp, &dir).await?;
                let bytes = std::fs::read(&p.file)?;
                let (id, file) = crate::ingest::store(host, &bytes, &p.ext).await?;
                let _ = std::fs::remove_dir_all(&dir);
                let rec = falos_doc::Asset {
                    id: id.clone(),
                    mime: Some(crate::assets::mime_of_ext(&p.ext).to_string()),
                    source: falos_doc::Source::Derive,
                    media: Some(p.media.clone()),
                    lineage: Some(Lineage {
                        kind: LineageKind::Derive,
                        op: "chip".into(),
                        recipe: json!({"chip": inp.chip}),
                        parents: vec![Parent::input(&inp.asset, inp.node.as_deref())],
                        actor: actor.id.clone(),
                        deterministic: true,
                        engine_version: Some(ff::version()),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                host.mutate(&canvas, &actor.id, None, |c| c.apply(actor, None, &[Command::PutAsset(rec)]))?;
                (id, file)
            } else {
                (inp.asset.clone(), inp.file.clone())
            };
            urls.push(falops::public_url(host, actor, &canvas, &asset, &file).await?);
            parents.push(Parent::new(&asset, inp.node.as_deref(), role_for_param(param)));
        }
        fields.insert(param.clone(), if many { json!(urls) } else { json!(urls.first()) });
    }
    Ok((fields, parents))
}

// ---- rerun ----------------------------------------------------------------------------------

/// Re-run a node's recipe with `overrides` (merged into its args / model input), or apply it
/// to other nodes (`on`).
pub async fn rerun(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, node: &str, overrides: Option<Json>, on: Option<Vec<String>>) -> Result<Json> {
    let canvas = host.resolve_canvas(canvas)?;
    let (n, asset) = host.read(&canvas, |c| {
        let n = c.node(node)?;
        let a = n.asset_id().and_then(|id| c.asset(id));
        Some((n, a))
    })?
    .ok_or_else(|| anyhow!("unknown node `{node}`"))?;
    let l = asset
        .and_then(|a| a.lineage)
        .ok_or_else(|| anyhow!("`{node}` has no recipe (it's an import); nothing to re-run"))?;
    let mut args = l.recipe.get("args").cloned().unwrap_or_else(|| l.recipe.clone());
    if let (Some(base), Some(Json::Object(o))) = (args.as_object_mut(), overrides.clone()) {
        base.extend(o);
    }
    match l.kind {
        LineageKind::Derive if spec(&l.op).is_some() => {
            let inputs = match on {
                Some(ids) if !ids.is_empty() => json!(ids),
                _ => {
                    // The original references when their nodes still exist, else the parent assets.
                    let refs = l.recipe["inputs"].as_array().cloned().unwrap_or_default();
                    let alive = !refs.is_empty()
                        && host.read(&canvas, |c| {
                            refs.iter().all(|r| {
                                let id = r.as_str().or(r["node"].as_str()).unwrap_or("");
                                id.starts_with("sha256:") || c.node(id).is_some()
                            })
                        })?;
                    if alive {
                        json!(refs)
                    } else {
                        json!(l.parents.iter().filter(|p| p.role == ParentRole::Input).map(|p| p.asset.clone()).collect::<Vec<_>>())
                    }
                }
            };
            let run = start(host, actor, session, &canvas, Request {
                op: l.op.clone(),
                inputs,
                args,
                placement: Some(PlacementArgs { near: Some(vec![node.to_string()]), ..Default::default() }),
                name: None,
                role: None,
            })?;
            Ok(run.summary(None))
        }
        LineageKind::Generate => {
            if on.is_some() {
                bail!("`on` applies a derive recipe to other nodes; `{node}` is a generation ({}) — use generate with mediaInputs", l.op);
            }
            let (x, y) = host.find_space(&canvas, n.parent.as_deref(), &[node.to_string()], n.w, n.h)?;
            let id = crate::fal::start(host, actor, &canvas, &l.op, args, crate::fal::Placement { x: Some(x), y: Some(y), w: Some(n.w), h: Some(n.h), parent: n.parent.clone() }, l.parents.clone())?;
            Ok(json!({"nodes": [id], "op": l.op, "status": "queued"}))
        }
        _ => bail!("`{node}`'s recipe ({} {}) can't be re-run by the host", l.kind.as_str(), l.op),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspects_and_boxes() {
        assert_eq!(parse_aspect("9:16"), Some(9.0 / 16.0));
        assert_eq!(parse_aspect("16x9"), Some(16.0 / 9.0));
        let b = aspect_box(1920.0, 1080.0, 9.0 / 16.0, "center");
        assert_eq!((b[2].round(), b[3]), (608.0, 1080.0));
        let m = Media { w: Some(100.0), h: Some(50.0), ..Default::default() };
        assert_eq!(px_box(&m, [0.5, 0.0, 0.9, 1.0], false), [50, 0, 50, 50]);
        let s = spec("reframe").unwrap();
        assert_eq!(with_defaults(&s, &json!({"aspect": "1:1"}))["mode"], "pad-blur");
        assert!(specs().iter().all(|s| s.args["type"] == "object"));
    }
}
