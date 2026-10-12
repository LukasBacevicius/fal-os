//! `GET /thumbs/<key>-<256|1024|strip>.webp` (token auth; `?token=` works for `<img>`):
//! served from `$FALOS_HOME/thumbs`, generated on demand when missing.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::thumbs;
use crate::workspace::Workspace;

pub async fn get(UrlPath(file): UrlPath<String>, State(space): State<Arc<Workspace>>) -> Response {
    let Some(lib) = space.library.get().cloned() else {
        return (StatusCode::NOT_FOUND, "library is not enabled").into_response();
    };
    let Some((key, size)) = thumbs::parse_name(&file) else {
        return (StatusCode::BAD_REQUEST, "expected /thumbs/<sha>-<256|1024|strip>.webp").into_response();
    };
    let mut path = thumbs::path_of(&lib.thumbs_dir, &key, size);
    if !path.is_file() {
        // Generation keeps going in the background; a slow one (remote video) answers 503 so
        // the browser's few connections per host aren't held by pending thumbnails.
        let job = tokio::spawn({
            let lib = lib.clone();
            let key = key.clone();
            async move { lib.ensure_thumb(&key).await }
        });
        match tokio::time::timeout(std::time::Duration::from_millis(1500), job).await {
            Ok(Ok(Ok(k))) => path = thumbs::path_of(&lib.thumbs_dir, &k, size),
            Ok(Ok(Err(e))) => return (StatusCode::NOT_FOUND, format!("no thumbnail: {e:#}")).into_response(),
            Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, [(header::RETRY_AFTER, "2")], "thumbnail still generating").into_response(),
        }
    }
    match tokio::fs::read(&path).await {
        Ok(b) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/webp")
            .header(header::CONTENT_LENGTH, b.len())
            .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable")
            .body(Body::from(b))
            .unwrap_or_default(),
        Err(_) => (StatusCode::NOT_FOUND, "no thumbnail").into_response(),
    }
}

/// Where rendered previews (PDF pages) are cached: the library's thumbnail folder, else a
/// temp folder.
fn preview_dir(space: &Workspace) -> std::path::PathBuf {
    space.library.get().map(|l| l.thumbs_dir.clone()).unwrap_or_else(|| std::env::temp_dir().join("falos-previews"))
}

fn pdf_of(space: &Workspace, id: &str) -> Result<(String, std::path::PathBuf), Response> {
    let hex = crate::assets::parse_id(id).ok_or_else(|| (StatusCode::BAD_REQUEST, "expected a sha256 asset id").into_response())?;
    let path = crate::assets::locate(space, None, &hex).ok_or_else(|| (StatusCode::NOT_FOUND, "no such asset").into_response())?;
    Ok((hex, path))
}

/// `GET /thumbs/pdf/<sha>` → `{pages}` (pdfinfo, else counted from the file).
pub async fn pdf_info(UrlPath(id): UrlPath<String>, State(space): State<Arc<Workspace>>) -> Response {
    let (_, path) = match pdf_of(&space, &id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || thumbs::pdf_pages(&path)).await {
        Ok(Ok(pages)) => axum::Json(serde_json::json!({ "pages": pages })).into_response(),
        Ok(Err(e)) => (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct PageQuery {
    /// 256 (page strip) or 1600 (reading); anything else snaps to one of them.
    w: Option<u32>,
}

/// `GET /thumbs/pdf/<sha>/<page>?w=256|1600` → that page as WebP (1-based; pdftoppm, cached).
/// 501 when poppler isn't installed: the UI falls back to the browser's PDF viewer.
pub async fn pdf_page(
    UrlPath((id, page)): UrlPath<(String, u32)>,
    axum::extract::Query(q): axum::extract::Query<PageQuery>,
    State(space): State<Arc<Workspace>>,
) -> Response {
    let (hex, path) = match pdf_of(&space, &id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let w = if q.w.unwrap_or(1600) <= 400 { 256 } else { 1600 };
    let page = page.max(1);
    let file = preview_dir(&space).join(format!("{hex}-p{page}-{w}.webp"));
    if !file.is_file() {
        let out = file.clone();
        let made = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let img = thumbs::pdf_page(&path, page, w)?.to_rgba8();
            if let Some(d) = out.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = out.with_extension(format!("tmp{}", rand::random::<u32>()));
            std::fs::write(&tmp, thumbs::encode_webp(&img, if w > 400 { 88.0 } else { 75.0 })?)?;
            std::fs::rename(&tmp, &out)?;
            Ok(())
        })
        .await;
        match made {
            Ok(Ok(())) => {}
            Ok(Err(e)) if format!("{e:#}").contains("running pdftoppm") => return (StatusCode::NOT_IMPLEMENTED, format!("{e:#}")).into_response(),
            Ok(Err(e)) => return (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response(),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    match tokio::fs::read(&file).await {
        Ok(b) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/webp")
            .header(header::CONTENT_LENGTH, b.len())
            .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable")
            .body(Body::from(b))
            .unwrap_or_default(),
        Err(_) => (StatusCode::NOT_FOUND, "no page").into_response(),
    }
}
