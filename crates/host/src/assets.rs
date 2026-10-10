//! Content-addressed project assets over HTTP: `GET|HEAD|PUT /assets/{sha256}`.
//!
//! Files live in `<project>/assets/<hex>.<ext>` (the extension carries the content type; the
//! fal job runner writes the same layout). `GET` supports single byte ranges so `<video>` can
//! seek; `PUT` streams the body to disk, verifies the SHA-256 and caps the size.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::host::Host;

/// Largest accepted upload.
pub const MAX_ASSET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("avif", "image/avif"),
    ("svg", "image/svg+xml"),
    ("mp4", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("m4v", "video/x-m4v"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("glb", "model/gltf-binary"),
    ("gltf", "model/gltf+json"),
    ("usdz", "model/vnd.usdz+zip"),
    ("pdf", "application/pdf"),
    ("json", "application/json"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
];

pub fn mime_of_ext(ext: &str) -> &'static str {
    let ext = ext.to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(e, _)| *e == ext)
        .map_or("application/octet-stream", |(_, m)| m)
}

pub fn ext_of_mime(mime: &str) -> &'static str {
    let m = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(_, t)| *t == m)
        .map_or("bin", |(e, _)| e)
}

/// `sha256:<hex>` or `<hex>` → lowercase hex, if it is a SHA-256 digest.
pub fn parse_id(id: &str) -> Option<String> {
    let hex = id
        .strip_prefix("sha256:")
        .unwrap_or(id)
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The stored file for a digest, if present.
pub fn find(dir: &Path, hex: &str) -> Option<PathBuf> {
    let prefix = format!("{hex}.");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| (n == hex || n.starts_with(&prefix)) && !n.contains(".part"))
        })
}

fn text(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

/// Parses a single `bytes=` range against `len`. `Err(())` = unsatisfiable.
fn parse_range(h: &str, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        // Multipart ranges aren't supported; serving the whole file is allowed.
        return Ok(None);
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", "") => return Err(()),
        ("", n) => {
            let n: u64 = n.parse().map_err(|_| ())?;
            if n == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        (s, "") => (s.parse().map_err(|_| ())?, len.saturating_sub(1)),
        (s, e) => {
            let (s, e): (u64, u64) = (s.parse().map_err(|_| ())?, e.parse().map_err(|_| ())?);
            (s, e.min(len.saturating_sub(1)))
        }
    };
    if len == 0 || start >= len || start > end {
        return Err(());
    }
    Ok(Some((start, end)))
}

pub async fn get(
    method: Method,
    UrlPath(id): UrlPath<String>,
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
) -> Response {
    let Some(hex) = parse_id(&id) else {
        return text(
            StatusCode::BAD_REQUEST,
            "asset id must be a sha256 hex digest",
        );
    };
    let dir = host.dir().join("assets");
    let Some(path) = find(&dir, &hex) else {
        return text(StatusCode::NOT_FOUND, "no such asset");
    };
    let mime = mime_of_ext(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return text(StatusCode::NOT_FOUND, "no such asset");
    };
    let len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let range = match headers.get(header::RANGE).and_then(|r| r.to_str().ok()) {
        Some(r) => match parse_range(r, len) {
            Ok(r) => r,
            Err(()) => {
                return (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{len}"))],
                )
                    .into_response();
            }
        },
        None => None,
    };
    let (status, start, count) = match range {
        Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        None => (StatusCode::OK, 0, len),
    };
    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, count)
        .header(header::ACCEPT_RANGES, "bytes")
        // Content-addressed: never changes.
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .header(header::ETAG, format!("\"{hex}\""));
    if range.is_some() {
        resp = resp.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", start + count - 1),
        );
    }
    if method == Method::HEAD {
        return resp.body(Body::empty()).unwrap_or_default();
    }
    if start > 0
        && let Err(e) = file.seek(SeekFrom::Start(start)).await
    {
        return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let stream = tokio_util::io::ReaderStream::new(file.take(count));
    resp.body(Body::from_stream(stream)).unwrap_or_default()
}

pub async fn put(
    UrlPath(id): UrlPath<String>,
    State(host): State<Arc<Host>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hex) = parse_id(&id) else {
        return text(
            StatusCode::BAD_REQUEST,
            "asset id must be a sha256 hex digest",
        );
    };
    let dir = host.dir().join("assets");
    if let Some(p) = find(&dir, &hex) {
        return (
            StatusCode::OK,
            axum::Json(serde_json::json!({"id": format!("sha256:{hex}"), "existing": true, "file": p.file_name().map(|n| n.to_string_lossy().into_owned())})),
        )
            .into_response();
    }
    if let Some(n) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        && n > MAX_ASSET_BYTES
    {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "asset exceeds 2 GiB");
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let ext = ext_of_mime(mime);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let tmp = dir.join(format!("{hex}.part{}", rand::random::<u32>()));
    let result = async {
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            size += chunk.len() as u64;
            if size > MAX_ASSET_BYTES {
                return Ok::<_, std::io::Error>(Err(text(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "asset exceeds 2 GiB",
                )));
            }
            hasher.update(&chunk);
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        let got: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if got != hex {
            return Ok(Err(text(
                StatusCode::BAD_REQUEST,
                format!("sha256 mismatch: body hashes to {got}"),
            )));
        }
        Ok(Ok(size))
    }
    .await;
    match result {
        Ok(Ok(size)) => {
            let file = dir.join(format!("{hex}.{ext}"));
            if let Err(e) = tokio::fs::rename(&tmp, &file).await {
                let _ = tokio::fs::remove_file(&tmp).await;
                return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
            (
                StatusCode::CREATED,
                axum::Json(serde_json::json!({"id": format!("sha256:{hex}"), "size": size, "mime": mime_of_ext(ext), "file": format!("{hex}.{ext}")})),
            )
                .into_response()
        }
        Ok(Err(resp)) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            resp
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Ok(Some((0, 9))));
        assert_eq!(parse_range("bytes=90-", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=50-500", 100), Ok(Some((50, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Err(()));
        assert_eq!(parse_range("bytes=5-1", 100), Err(()));
        assert_eq!(parse_range("items=1-2", 100), Ok(None));
        assert_eq!(parse_id("SHA256:".to_string().as_str()), None);
        assert!(parse_id(&format!("sha256:{}", "a".repeat(64))).is_some());
        assert_eq!(ext_of_mime("image/png; x=1"), "png");
        assert_eq!(mime_of_ext("MP4"), "video/mp4");
    }
}
