//! `POST /api/materialize {node|sha, name?, canvas?}` (`?project=`) → `{path}`: a human-named
//! copy of a content-addressed asset in `<project>/.out/` (docs/ASSETS-V1.md §7), for
//! clients without the desktop shell's own `materialize` (the web build, the desktop
//! fallback when it only has a node id).
//!
//! The copy is made with `std::fs::copy`, which on macOS clones (`fclonefileat`, APFS
//! copy-on-write) and falls back to `fcopyfile`. **Never a hardlink**: other apps may edit
//! the file in place, which would corrupt the store.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::assets::{find, parse_id};
use crate::host::Host;
use crate::workspace::Workspace;

pub fn routes() -> Router<Arc<Workspace>> {
    Router::new().route("/api/materialize", axum::routing::post(materialize))
}

#[derive(Deserialize)]
pub struct ProjectQuery {
    project: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct Req {
    node: Option<String>,
    sha: Option<String>,
    name: Option<String>,
    canvas: Option<String>,
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, axum::Json(json!({"error": msg.into()}))).into_response()
}

/// Asset id and name of `node` in `canvas` (default: every canvas of the project).
fn node_asset(host: &Host, canvas: Option<&str>, node: &str) -> Option<(String, String)> {
    let canvases: Vec<String> = match canvas {
        Some(c) => vec![host.resolve_canvas(c).ok()?],
        None => host.project().canvases.iter().map(|c| c.id.clone()).collect(),
    };
    canvases.iter().find_map(|c| {
        host.read(c, |cv| {
            let n = cv.node(node)?;
            let asset = n.props.get("asset").and_then(Json::as_str)?.to_string();
            Some((asset, n.name.clone()))
        })
        .ok()
        .flatten()
    })
}

fn safe_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c == '/' || c == ':' || c == '\\' || c.is_control() { '-' } else { c })
        .collect();
    let s: String = s.trim().trim_start_matches('.').trim().chars().take(120).collect();
    s.trim().to_string()
}

fn sha256_file(p: &Path) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `<out>/<name>.<ext>`, reusing an identical earlier copy; another file with the name gets
/// a ` (1)` suffix.
pub fn materialize_file(src: &Path, hex: &str, out: &Path, name: Option<&str>) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut stem = safe_stem(name.unwrap_or(""));
    if let Some(s) = stem.strip_suffix(&format!(".{ext}")).filter(|_| !ext.is_empty()) {
        stem = s.to_string();
    }
    if stem.is_empty() {
        stem = hex[..12].to_string();
    }
    let size = std::fs::metadata(src)?.len();
    for i in 0..1000 {
        let suffix = if i == 0 { String::new() } else { format!(" ({i})") };
        let file = if ext.is_empty() { format!("{stem}{suffix}") } else { format!("{stem}{suffix}.{ext}") };
        let dst = out.join(file);
        match std::fs::symlink_metadata(&dst) {
            Ok(m) => {
                if m.is_file() && m.len() == size && sha256_file(&dst).as_deref() == Some(hex) {
                    return Ok(dst);
                }
            }
            Err(_) => {
                let tmp = out.join(format!(".{hex}.tmp{}", std::process::id()));
                let _ = std::fs::remove_file(&tmp);
                std::fs::copy(src, &tmp)?;
                std::fs::rename(&tmp, &dst)?;
                return Ok(dst);
            }
        }
    }
    Err(std::io::Error::other("too many files with that name"))
}

async fn materialize(
    State(space): State<Arc<Workspace>>,
    Query(q): Query<ProjectQuery>,
    axum::Json(req): axum::Json<Req>,
) -> Response {
    let host = match space.host_for_frame(q.project.as_deref()) {
        Ok(h) => h,
        Err(e) => return err(StatusCode::NOT_FOUND, format!("{e:#}")),
    };
    let (asset, node_name) = match (&req.sha, &req.node) {
        (Some(s), _) => (s.clone(), None),
        (None, Some(n)) => match node_asset(&host, req.canvas.as_deref(), n) {
            Some((a, name)) => (a, Some(name)),
            None => return err(StatusCode::NOT_FOUND, format!("node {n} has no asset")),
        },
        (None, None) => return err(StatusCode::BAD_REQUEST, "need node or sha"),
    };
    let Some(hex) = parse_id(&asset) else {
        return err(StatusCode::BAD_REQUEST, format!("not a stored asset: {asset}"));
    };
    let Some(src) = find(&host.dir().join("assets"), &hex) else {
        return err(StatusCode::NOT_FOUND, "asset is not in the project store");
    };
    let name = req.name.clone().or(node_name).filter(|n| !n.trim().is_empty());
    let out = host.dir().join(".out");
    match tokio::task::spawn_blocking(move || materialize_file(&src, &hex, &out, name.as_deref())).await {
        Ok(Ok(p)) => axum::Json(json!({"path": p.to_string_lossy()})).into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_reuses_and_suffixes() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src.png");
        std::fs::write(&src, b"abc").unwrap();
        let hex: String = Sha256::digest(b"abc").iter().map(|b| format!("{b:02x}")).collect();
        let out = d.path().join(".out");
        let a = materialize_file(&src, &hex, &out, Some("hero.png")).unwrap();
        assert_eq!(a, out.join("hero.png"));
        assert_eq!(materialize_file(&src, &hex, &out, Some("hero")).unwrap(), a);
        std::fs::write(&a, b"changed").unwrap();
        assert_eq!(materialize_file(&src, &hex, &out, Some("hero")).unwrap(), out.join("hero (1).png"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&src).unwrap().nlink(), 1, "never hardlinked");
        }
        assert_eq!(safe_stem("a/b:c"), "a-b-c");
    }
}
