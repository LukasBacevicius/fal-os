//! Bringing files in: copy into `<project>/assets/<sha256>.<ext>`, record the asset with
//! provenance (`source`, `origin`, `media`, `lineage`) and create its node (or convert a
//! `generation` placeholder in place). Used by `import_file` and the op runner.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{
    Actor, Asset, Command, Lineage, LineageKind, Media, NodeKind, Origin, Parent, ParentRole,
    Props, Source,
};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::chat::Probe;
use crate::host::{Host, now_ms};
use crate::mcp::PlacementArgs;

/// Node kind + mime for an importable extension.
pub fn media_kind(ext: &str) -> Option<(NodeKind, &'static str)> {
    use falos_doc::StyleKind;
    Some(match ext {
        "png" => (NodeKind::Image, "image/png"),
        "jpg" | "jpeg" => (NodeKind::Image, "image/jpeg"),
        "webp" => (NodeKind::Image, "image/webp"),
        "gif" => (NodeKind::Image, "image/gif"),
        "avif" => (NodeKind::Image, "image/avif"),
        "heic" | "heif" => (NodeKind::Image, "image/heic"),
        "tif" | "tiff" => (NodeKind::Image, "image/tiff"),
        "exr" => (NodeKind::Image, "image/x-exr"),
        "bmp" => (NodeKind::Image, "image/bmp"),
        "svg" => (NodeKind::Vector, "image/svg+xml"),
        "mp4" | "m4v" => (NodeKind::Video, "video/mp4"),
        "mov" => (NodeKind::Video, "video/quicktime"),
        "webm" => (NodeKind::Video, "video/webm"),
        "mkv" => (NodeKind::Video, "video/x-matroska"),
        "mp3" => (NodeKind::Audio, "audio/mpeg"),
        "wav" => (NodeKind::Audio, "audio/wav"),
        "m4a" | "aac" => (NodeKind::Audio, "audio/mp4"),
        "ogg" => (NodeKind::Audio, "audio/ogg"),
        "flac" => (NodeKind::Audio, "audio/flac"),
        "aiff" | "aif" => (NodeKind::Audio, "audio/aiff"),
        "pdf" => (NodeKind::Document, "application/pdf"),
        "srt" => (NodeKind::Document, "application/x-subrip"),
        "vtt" => (NodeKind::Document, "text/vtt"),
        "glb" => (NodeKind::Model3d, "model/gltf-binary"),
        "gltf" => (NodeKind::Model3d, "model/gltf+json"),
        "usdz" => (NodeKind::Model3d, "model/vnd.usdz+zip"),
        "cube" => (NodeKind::Style(StyleKind::Lut), "application/x-cube-lut"),
        "ttf" => (NodeKind::Style(StyleKind::Font), "font/ttf"),
        "otf" => (NodeKind::Style(StyleKind::Font), "font/otf"),
        "woff" => (NodeKind::Style(StyleKind::Font), "font/woff"),
        "woff2" => (NodeKind::Style(StyleKind::Font), "font/woff2"),
        "md" | "markdown" => (NodeKind::Text, "text/markdown"),
        "txt" => (NodeKind::Text, "text/plain"),
        _ => return None,
    })
}

pub const IMPORT_TYPES: &str = "png jpg webp gif avif heic tiff exr bmp svg mp4 mov webm mkv mp3 wav m4a ogg flac aiff pdf srt vtt glb gltf usdz cube ttf otf woff woff2 md txt";

/// Formats a browser can't draw: imported with a PNG proxy (the original is kept as an asset).
fn needs_proxy(ext: &str) -> bool {
    matches!(ext, "heic" | "heif" | "tif" | "tiff" | "exr")
}

pub struct Imported {
    pub node: String,
    pub asset: String,
    pub file: PathBuf,
    pub kind: NodeKind,
    pub probe: Probe,
    pub w: f64,
    pub h: f64,
}

/// Provenance for an imported file. Without it, the file is a user input (`source: import`,
/// `role: input`, protected). With it, it's a derivative / render / generation.
#[derive(Deserialize, schemars::JsonSchema, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LineageArgs {
    /// "derive" (made from other assets, e.g. a Bash/ffmpeg result), "render" (Remotion,
    /// HyperFrames) or "generate" (a fal model / fal Agent artifact). Default: "render" for
    /// model remotion/hyperframes, else "generate".
    pub kind: Option<String>,
    /// The op or tool, e.g. "ffmpeg", "remotion", "fal-ai/flux/dev".
    pub op: Option<String>,
    /// Same as `op` (older name), e.g. "fal-agent".
    pub model: Option<String>,
    /// Canonical args, enough to re-run (JSON object).
    pub recipe: Option<Json>,
    /// Same as `recipe` (older name): task text, prompt, composition props, …
    pub params: Option<Json>,
    /// What it was made from: asset ids (sha256:…), node ids, or {"asset"|"node", "role"}
    /// (role: input|reference|mask|style|audio|start_frame|end_frame).
    pub parents: Option<Vec<Json>>,
    pub seed: Option<i64>,
    /// fal request / Agent response id.
    pub fal_request_id: Option<String>,
    /// USD.
    pub cost: Option<f64>,
    /// Remote URL of the original (e.g. the fal CDN url), kept as the asset uri.
    pub uri: Option<String>,
    /// Re-running gives identical bytes.
    pub deterministic: Option<bool>,
}

#[derive(Default)]
pub struct ImportOpts {
    pub place: Option<PlacementArgs>,
    pub name: Option<String>,
    pub lineage: Option<LineageArgs>,
    /// Exact lineage (op runner); wins over `lineage`.
    pub full_lineage: Option<Lineage>,
    /// A `generation` placeholder node to convert in place (same id, parent and z-order).
    pub replace: Option<String>,
    /// Node role (default: `input` for imports, `candidate` for generations, none for
    /// derivatives).
    pub role: Option<String>,
    /// Default: true for imports.
    pub protected: Option<bool>,
    /// App the file came from (`finder`, `premiere`, …), recorded in `origin`.
    pub app: Option<String>,
    /// Extra node props (e.g. a video's `muted`).
    pub props: Props,
}

/// Write bytes into the project's content-addressed store; returns (asset id, file).
pub async fn store(host: &Host, bytes: &[u8], ext: &str) -> Result<(String, PathBuf)> {
    let hex: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let dir = host.dir().join("assets");
    tokio::fs::create_dir_all(&dir).await?;
    if let Some(existing) = crate::assets::find(&dir, &hex) {
        return Ok((format!("sha256:{hex}"), existing));
    }
    let file = dir.join(format!("{hex}.{ext}"));
    let tmp = dir.join(format!("{hex}.{ext}.part{}", rand::random::<u32>()));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, &file).await?;
    Ok((format!("sha256:{hex}"), file))
}

/// Resolve lineage parents: asset ids stay, node ids become their node's asset.
fn parents_of(c: &falos_doc::Canvas, raw: &[Json]) -> Vec<Parent> {
    raw.iter()
        .filter_map(|p| {
            let (id, role) = match p {
                Json::String(s) => (s.clone(), ParentRole::Input),
                Json::Object(o) => (
                    o.get("asset").or(o.get("node")).and_then(Json::as_str)?.to_string(),
                    o.get("role")
                        .and_then(Json::as_str)
                        .and_then(ParentRole::parse)
                        .unwrap_or_default(),
                ),
                _ => return None,
            };
            if id.starts_with("sha256:") {
                return Some(Parent::new(&id, None, role));
            }
            let n = c.node(&id)?;
            let asset = n.asset_id()?.to_string();
            Some(Parent::new(&asset, Some(&n.id), role))
        })
        .collect()
}

fn lineage_from_args(c: &falos_doc::Canvas, l: LineageArgs, actor: &str) -> Lineage {
    let op = l.op.or(l.model).unwrap_or_else(|| "import".into());
    let kind = l
        .kind
        .as_deref()
        .and_then(LineageKind::parse)
        .unwrap_or(match op.as_str() {
            "remotion" | "hyperframes" | "render" => LineageKind::Render,
            "ffmpeg" | "sips" | "imagemagick" => LineageKind::Derive,
            _ => LineageKind::Generate,
        });
    Lineage {
        kind,
        deterministic: l.deterministic.unwrap_or(kind == LineageKind::Derive),
        op,
        recipe: l.recipe.or(l.params).unwrap_or_else(|| json!({})),
        parents: parents_of(c, l.parents.as_deref().unwrap_or_default()),
        seed: l.seed,
        fal_request_id: l.fal_request_id,
        cost: l.cost,
        actor: actor.into(),
        engine_version: None,
    }
}

/// Convert HEIC/TIFF/EXR to a PNG proxy with `sips` (macOS) or ffmpeg.
async fn proxy_png(src: &Path) -> Result<Vec<u8>> {
    let tmp = std::env::temp_dir().join(format!("falos-proxy-{}.png", rand::random::<u64>()));
    let ok = tokio::process::Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(src)
        .arg("--out")
        .arg(&tmp)
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    if !ok {
        let o = tokio::process::Command::new(crate::ops::probe::ffmpeg_bin())
            .args(["-y", "-v", "error", "-i"])
            .arg(src)
            .args(["-frames:v", "1"])
            .arg(&tmp)
            .output()
            .await?;
        if !o.status.success() {
            bail!("can't convert {} to PNG: {}", src.display(), String::from_utf8_lossy(&o.stderr).trim());
        }
    }
    let bytes = tokio::fs::read(&tmp).await?;
    let _ = tokio::fs::remove_file(&tmp).await;
    Ok(bytes)
}

/// Copy a local file into the project's assets (by sha256) and create its node (or convert
/// a placeholder `generation` node into it). `actor` writes the node; the lineage actor is
/// `full_lineage.actor` or `actor`.
pub async fn import_file(
    host: &Host,
    actor: &Actor,
    canvas: &str,
    path: &Path,
    opts: ImportOpts,
) -> Result<Imported> {
    let canvas = host.resolve_canvas(canvas)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (mut kind, mut mime) = media_kind(&ext)
        .ok_or_else(|| anyhow!("import_file takes {IMPORT_TYPES} files, not `.{ext}`"))?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.is_empty() {
        bail!("{} is empty", path.display());
    }
    // The original always goes into the store; browsers get a PNG proxy for HEIC/TIFF/EXR.
    let (orig_id, orig_file) = store(host, &bytes, &ext).await?;
    let (asset_id, file, original) = if needs_proxy(&ext) {
        let png = proxy_png(&orig_file).await?;
        let (id, f) = store(host, &png, "png").await?;
        mime = "image/png";
        (id, f, Some(orig_id))
    } else {
        (orig_id, orig_file, None)
    };
    let media = crate::ops::probe::media_of(&file).await;
    let probe = Probe {
        width: media.w,
        height: media.h,
        duration: media.duration,
    };
    // Text files become text nodes with their content.
    let text = if kind == NodeKind::Text {
        Some(String::from_utf8_lossy(&bytes).chars().take(20_000).collect::<String>())
    } else {
        None
    };
    if kind == NodeKind::Image && media.w.is_none() && original.is_none() && ext != "avif" {
        // Not decodable as an image after all.
        kind = NodeKind::Document;
    }
    let placeholder = match &opts.replace {
        Some(id) => Some(
            host.read(&canvas, |c| c.node(id))?
                .filter(|n| n.kind == NodeKind::Generation)
                .ok_or_else(|| anyhow!("`{id}` is not a generation node on this canvas"))?,
        ),
        None => None,
    };
    let uri = lineage_uri(&opts.lineage);
    let full = match (opts.full_lineage, opts.lineage) {
        (Some(l), _) => Some(l),
        (None, Some(l)) => {
            let a = actor.id.clone();
            Some(host.read(&canvas, |c| lineage_from_args(c, l.clone(), &a))?)
        }
        (None, None) => None,
    };
    let source = full.as_ref().map_or(Source::Import, |l| l.kind.source());
    let mut p = opts.place.unwrap_or_default();
    let mut near = p.near.clone().unwrap_or_default();
    if near.is_empty()
        && let Some(l) = &full
    {
        near = l.parents.iter().filter_map(|p| p.node.clone()).collect();
    }
    if let Some(n) = &placeholder {
        p = PlacementArgs {
            x: Some(n.x),
            y: Some(n.y),
            w: Some(n.w),
            h: None,
            parent: n.parent.clone(),
            near: None,
        };
    }
    let (w, h) = match (kind, media.w, media.h) {
        (NodeKind::Audio, _, _) => (p.w.unwrap_or(480.0), p.h.unwrap_or(96.0)),
        (_, Some(iw), Some(ih)) if iw > 0.0 && ih > 0.0 => match (p.w, p.h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * ih / iw),
            (None, Some(h)) => (h * iw / ih, h),
            (None, None) => {
                let w = iw.min(960.0);
                (w, w * ih / iw)
            }
        },
        (NodeKind::Text, _, _) => (p.w.unwrap_or(400.0), p.h.unwrap_or(300.0)),
        (NodeKind::Style(_), _, _) => (p.w.unwrap_or(240.0), p.h.unwrap_or(120.0)),
        _ => (p.w.unwrap_or(512.0), p.h.unwrap_or(512.0)),
    };
    let (x, y) = match (p.x, p.y) {
        (Some(x), Some(y)) => (x, y),
        _ => host.find_space(&canvas, p.parent.as_deref(), &near, w, h)?,
    };
    let name = opts
        .name
        .or_else(|| placeholder.as_ref().map(|n| n.name.clone()).filter(|n| !n.is_empty()))
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let asset = Asset {
        id: asset_id.clone(),
        mime: Some(mime.into()),
        uri,
        source,
        origin: (source == Source::Import).then(|| Origin {
            path: Some(path.display().to_string()),
            url: None,
            filename: path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            app: opts.app.clone(),
            imported_by: actor.id.clone(),
            at: now_ms(),
        }),
        media: (media != Media::default()).then_some(media),
        lineage: full.clone(),
    };
    let mut props: Props = opts.props.clone();
    for (k, v) in [("x", json!(x)), ("y", json!(y)), ("w", json!(w)), ("h", json!(h))] {
        props.insert(k.into(), v);
    }
    if kind != NodeKind::Text {
        props.insert("asset".into(), json!(asset.id));
    }
    let role = opts.role.clone().or(match source {
        Source::Import => Some("input".into()),
        Source::Generate => Some("candidate".into()),
        _ => None,
    });
    if let Some(r) = role {
        props.insert("role".into(), json!(r));
    }
    if opts.protected.unwrap_or(source == Source::Import) {
        props.insert("protected".into(), json!(true));
    }
    if let Some(o) = &original {
        props.insert("original".into(), json!(o));
    }
    let replace = placeholder.as_ref().map(|n| n.id.clone());
    let mut records = vec![Command::PutAsset(asset.clone())];
    if let Some(o) = &original {
        records.push(Command::PutAsset(Asset {
            id: o.clone(),
            mime: media_kind(&ext).map(|(_, m)| m.to_string()),
            source: Source::Import,
            origin: asset.origin.clone(),
            ..Default::default()
        }));
    }
    let res = host.mutate(&canvas, &actor.id, None, |c| {
        let mut cmds = records.clone();
        let mut index = None;
        if let Some(id) = &replace {
            let siblings: Vec<String> = match c.node(id).and_then(|n| n.parent) {
                Some(par) => c.node(&par).map(|n| n.children).unwrap_or_default(),
                None => c
                    .nodes()
                    .into_iter()
                    .filter(|x| x.parent.is_none())
                    .map(|x| x.id)
                    .collect(),
            };
            index = siblings.iter().position(|s| s == id);
            cmds.push(Command::DeleteNode { id: id.clone() });
        }
        cmds.push(Command::CreateNode {
            id: replace.clone(),
            kind,
            parent: p.parent.clone(),
            index,
            name: name.chars().take(80).collect(),
            props: props.clone(),
            text: text.clone(),
        });
        c.apply(actor, None, &cmds)
    })?;
    let node = match replace {
        Some(id) => id,
        None => res
            .created
            .iter()
            .find(|id| host.read(&canvas, |c| c.node(id).is_some()).unwrap_or(false))
            .cloned()
            .context("import created no node")?,
    };
    Ok(Imported { node, asset: asset.id, file, kind, probe, w, h })
}

/// Set the remote `uri` from `LineageArgs.uri` (kept separate so a later record can fill it).
pub fn lineage_uri(l: &Option<LineageArgs>) -> Option<String> {
    l.as_ref().and_then(|l| l.uri.clone()).filter(|u| u.starts_with("http"))
}
