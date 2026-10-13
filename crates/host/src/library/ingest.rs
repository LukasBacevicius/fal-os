//! Ingest: every project's canvas asset records, its `assets/` files and the nodes that
//! place them → the library index. Runs for all projects at startup, then for a project
//! whenever its change signature (canvas versions / file mtimes) moves.
//!
//! Asset records are read as raw JSON so new provenance fields (`source`, `origin`, `media`,
//! the new `lineage` shape) are picked up without depending on the doc model's struct.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::Result;
use falos_doc::Canvas;
use falos_doc::loro::LoroValue;
use serde_json::{Value as Json, json};

use super::db::Ingested;
use crate::workspace::Workspace;

pub fn loro_json(v: &LoroValue) -> Json {
    match v {
        LoroValue::Null | LoroValue::Binary(_) | LoroValue::Container(_) => Json::Null,
        LoroValue::Bool(b) => Json::Bool(*b),
        LoroValue::Double(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        LoroValue::I64(i) => Json::from(*i),
        LoroValue::String(s) => Json::String(s.to_string()),
        LoroValue::List(l) => Json::Array(l.iter().map(loro_json).collect()),
        LoroValue::Map(m) => Json::Object(m.iter().map(|(k, v)| (k.clone(), loro_json(v))).collect()),
    }
}

pub fn kind_of_mime(mime: &str) -> &'static str {
    match mime.split('/').next().unwrap_or("") {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "model" => "model3d",
        _ => "other",
    }
}

fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// One placement: a node showing an asset.
#[derive(Debug, Clone)]
pub struct Placement {
    pub canvas: String,
    pub node: String,
    pub sha: String,
    pub role: Option<String>,
    pub name: Option<String>,
}

#[derive(Default, Debug)]
pub struct Scan {
    pub assets: Vec<Ingested>,
    pub placements: Vec<Placement>,
}

fn str_at<'a>(v: &'a Json, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().filter(|s| !s.trim().is_empty())
}

/// `asset.source`, else the lineage kind, else inferred from the old lineage shape.
pub fn source_of(rec: &Json) -> String {
    const OK: [&str; 5] = ["import", "generate", "derive", "render", "capture"];
    if let Some(s) = rec["source"].as_str().filter(|s| OK.contains(s)) {
        return s.into();
    }
    let l = &rec["lineage"];
    if let Some(k) = l["kind"].as_str().filter(|s| OK.contains(s)) {
        return k.into();
    }
    let model = l["model"].as_str().or(l["op"].as_str()).unwrap_or("").to_ascii_lowercase();
    if model.is_empty() || model == "import" {
        return "import".into();
    }
    if ["remotion", "hyperframes", "render", "compose"].iter().any(|m| model.contains(m)) {
        return "render".into();
    }
    if l["fal_request_id"].is_string() || model.contains('/') || model.starts_with("fal") {
        return "generate".into();
    }
    "derive".into()
}

/// What made it: the endpoint / model / op.
pub fn model_of(rec: &Json) -> Option<String> {
    let l = &rec["lineage"];
    [str_at(l, &["recipe", "endpoint"]), str_at(l, &["recipe", "model"]), str_at(l, &["model"]), str_at(l, &["op"])]
        .into_iter()
        .flatten()
        .find(|m| *m != "import")
        .map(str::to_string)
}

pub fn prompt_of(rec: &Json) -> Option<String> {
    let l = &rec["lineage"];
    [
        str_at(l, &["params", "prompt"]),
        str_at(l, &["recipe", "prompt"]),
        str_at(l, &["recipe", "args", "prompt"]),
        str_at(l, &["recipe", "input", "prompt"]),
        str_at(l, &["params", "task"]),
        str_at(rec, &["prompt"]),
    ]
    .into_iter()
    .flatten()
    .next()
    .map(|s| s.chars().take(4000).collect())
}

fn is_fal_url(u: &str) -> bool {
    u.starts_with("https://") && (u.contains(".fal.media/") || u.contains("fal.run/") || u.contains("fal.ai/"))
}

/// An `Ingested` from a canvas asset record (raw JSON).
pub fn from_record(raw: &Json) -> Option<Ingested> {
    // The doc model's reading (migrates old records: `model` → `op`, `params` → `recipe`,
    // source inferred), falling back to the raw JSON for anything it doesn't know.
    let mut canon = raw.clone();
    if let Ok(a) = serde_json::from_value::<falos_doc::Asset>(raw.clone()) {
        canon["source"] = json!(a.source.as_str());
        if let Some(l) = a.lineage.as_ref().and_then(|l| serde_json::to_value(l).ok()) {
            canon["lineage"] = l;
        }
    }
    let rec = &canon;
    let sha = crate::assets::parse_id(rec["id"].as_str()?)?;
    let mime = rec["mime"].as_str().map(str::to_string);
    let media = rec.get("media").filter(|m| m.is_object()).cloned();
    let origin = rec.get("origin").filter(|m| m.is_object()).cloned();
    let num = |k: &str| media.as_ref().and_then(|m| m[k].as_f64());
    let uri = rec["uri"].as_str().filter(|u| is_fal_url(u)).map(str::to_string);
    let created = origin.as_ref().and_then(|o| o["at"].as_i64()).or_else(|| rec["created"].as_i64()).or_else(|| rec["lineage"]["at"].as_i64());
    Some(Ingested {
        kind: mime.as_deref().map(kind_of_mime).unwrap_or("other").into(),
        mime,
        w: num("w").map(|v| v as i64),
        h: num("h").map(|v| v as i64),
        duration: num("duration"),
        source: Some(source_of(rec)),
        lineage: rec.get("lineage").filter(|l| l.is_object()).cloned(),
        prompt: prompt_of(rec),
        caption: str_at(rec, &["caption"]).map(str::to_string),
        model: model_of(rec),
        name: origin.as_ref().and_then(|o| o["filename"].as_str()).map(str::to_string),
        fal_request_id: str_at(&rec["lineage"], &["fal_request_id"]).map(str::to_string),
        fal_url: uri,
        created,
        origin,
        media,
        record: Some(raw.clone()),
        sha,
        ..Default::default()
    })
}

/// An `Ingested` from a file `<hex>.<ext>` in a project's `assets/`.
pub fn from_file(path: &Path) -> Option<Ingested> {
    let name = path.file_name()?.to_str()?;
    if name.contains(".part") || name.starts_with('.') {
        return None;
    }
    let (hex, ext) = name.split_once('.')?;
    let sha = crate::assets::parse_id(hex)?;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mime = crate::assets::mime_of_ext(ext);
    let kind = kind_of_mime(mime);
    let (w, h) = if kind == "image" {
        use std::io::Read;
        let mut head = vec![0u8; 256 * 1024];
        let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
        crate::assets::image_size(&head[..n]).map_or((None, None), |(w, h)| (Some(w as i64), Some(h as i64)))
    } else {
        (None, None)
    };
    Some(Ingested {
        sha,
        kind: kind.into(),
        mime: (mime != "application/octet-stream").then(|| mime.to_string()),
        w,
        h,
        bytes: Some(meta.len() as i64),
        created: Some(mtime_ms(path)),
        path: Some(path.display().to_string()),
        ..Default::default()
    })
}

/// Asset records and placements of one canvas.
pub fn scan_canvas(c: &Canvas, canvas_id: &str, out: &mut Scan) {
    if let LoroValue::Map(m) = c.doc().get_map("assets").get_deep_value() {
        for (_, v) in m.iter() {
            if let Some(a) = from_record(&loro_json(v)) {
                out.assets.push(a);
            }
        }
    }
    for n in c.nodes() {
        // An app's `asset` is its snapshot still, not library media.
        if n.kind == falos_doc::NodeKind::App {
            continue;
        }
        let Some(sha) = n.props.get("asset").and_then(Json::as_str).and_then(crate::assets::parse_id) else { continue };
        out.placements.push(Placement {
            canvas: canvas_id.to_string(),
            node: n.id.clone(),
            sha,
            role: n.props.get("role").and_then(Json::as_str).map(str::to_string),
            name: Some(n.name.clone()).filter(|s| !s.is_empty()),
        });
    }
}

/// The folder of project `id` (`None` if it doesn't exist).
pub fn project_dir(ws: &Workspace, id: &str) -> Option<PathBuf> {
    if let Some(h) = ws.loaded().into_iter().find(|h| h.id == id) {
        return Some(h.dir().to_path_buf());
    }
    let d = ws.root()?.join(id);
    d.join("project.json").is_file().then_some(d)
}

/// A cheap value that changes whenever the project's assets or canvases change.
pub fn signature(ws: &Workspace, id: &str) -> Option<String> {
    let dir = project_dir(ws, id)?;
    let mut sig = format!("{}", mtime_ms(&dir.join("assets")));
    if let Some(h) = ws.loaded().into_iter().find(|h| h.id == id) {
        for c in h.project().canvases {
            if let Ok(v) = h.read(&c.id, |c| c.version().to_string()) {
                sig.push('|');
                sig.push_str(&v);
            }
        }
    } else {
        for e in std::fs::read_dir(dir.join("canvases")).into_iter().flatten().flatten() {
            sig.push_str(&format!("|{}", mtime_ms(&e.path())));
        }
    }
    Some(sig)
}

/// Everything ingest needs from one project (loaded projects are read from memory).
pub fn scan_project(ws: &Workspace, id: &str) -> Result<Scan> {
    let mut out = Scan::default();
    let dir = project_dir(ws, id).ok_or_else(|| anyhow::anyhow!("unknown project `{id}`"))?;
    if let Some(h) = ws.loaded().into_iter().find(|h| h.id == id) {
        for c in h.project().canvases {
            let _ = h.read(&c.id, |canvas| scan_canvas(canvas, &c.id, &mut out));
        }
    } else {
        let p = falos_doc::Project::load(&dir)?;
        for c in &p.canvases {
            match p.open_canvas(&dir, &c.id) {
                Ok(canvas) => scan_canvas(&canvas, &c.id, &mut out),
                Err(e) => tracing::warn!("library: reading {id}/{}: {e:#}", c.id),
            }
        }
    }
    // Files: fill path/size/dims, and files nobody placed yet.
    let mut files: std::collections::HashMap<String, Ingested> = std::collections::HashMap::new();
    for e in std::fs::read_dir(dir.join("assets")).into_iter().flatten().flatten() {
        if let Some(f) = from_file(&e.path()) {
            files.insert(f.sha.clone(), f);
        }
    }
    for a in &mut out.assets {
        if let Some(f) = files.remove(&a.sha) {
            a.path = f.path;
            a.bytes = f.bytes;
            a.w = a.w.or(f.w);
            a.h = a.h.or(f.h);
            a.created = a.created.or(f.created);
            if a.mime.is_none() {
                a.mime = f.mime;
                a.kind = f.kind;
            }
        }
    }
    out.assets.extend(files.into_values());
    Ok(out)
}

/// Default role of a placement when the node has none (spec §1).
pub fn default_role(source: &str) -> &'static str {
    match source {
        "import" => "input",
        _ => "candidate",
    }
}

pub fn placement_json(p: &Placement) -> Json {
    json!({"canvas": p.canvas, "node": p.node, "role": p.role, "name": p.name})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_inference() {
        let old_gen = json!({"id": format!("sha256:{}", "b".repeat(64)), "mime": "image/png", "lineage": {"model": "fal-ai/flux/dev", "params": {"prompt": "fox"}, "actor": "x", "fal_request_id": "r1"}});
        let a = from_record(&old_gen).unwrap();
        assert_eq!(a.source.as_deref(), Some("generate"));
        assert_eq!(a.model.as_deref(), Some("fal-ai/flux/dev"));
        assert_eq!(a.prompt.as_deref(), Some("fox"));
        assert_eq!(a.kind, "image");
        let new_derive = json!({"id": format!("sha256:{}", "c".repeat(64)), "mime": "video/mp4", "source": "derive", "media": {"w": 1080, "h": 1920, "duration": 3.5}, "lineage": {"kind": "derive", "op": "reframe", "recipe": {"aspect": "9:16"}}});
        let b = from_record(&new_derive).unwrap();
        assert_eq!((b.source.as_deref(), b.model.as_deref(), b.w, b.duration), (Some("derive"), Some("reframe"), Some(1080), Some(3.5)));
        let imp = json!({"id": format!("sha256:{}", "d".repeat(64)), "lineage": {"model": "import", "actor": "human"}});
        assert_eq!(from_record(&imp).unwrap().source.as_deref(), Some("import"));
        assert_eq!(source_of(&json!({"lineage": {"model": "remotion"}})), "render");
    }
}
