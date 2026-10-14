//! Content-addressed project assets over HTTP: `GET|HEAD|PUT /assets/{sha256}`, and
//! `POST /import/url` (the host downloads a remote file into the assets: pages can't fetch
//! most CDNs themselves because of CORS, e.g. the signed image URLs of a Figma paste).
//!
//! Files live in `<project>/assets/<hex>.<ext>` (the extension carries the content type; the
//! fal job runner writes the same layout). `GET` supports single byte ranges so `<video>` can
//! seek; `PUT` streams the body to disk, verifies the SHA-256 and caps the size.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::workspace::Workspace;

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
    // Wider imports (docs/ASSETS-V1.md §3): served with a type the browser understands, so
    // quick look can read, play or load them.
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("exr", "image/x-exr"),
    ("bmp", "image/bmp"),
    ("mkv", "video/x-matroska"),
    ("aiff", "audio/aiff"),
    ("aif", "audio/aiff"),
    ("srt", "application/x-subrip"),
    ("vtt", "text/vtt"),
    ("cube", "application/x-cube-lut"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("otio", "application/vnd.otio+json"),
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

#[derive(serde::Deserialize)]
pub struct AssetQuery {
    /// Project id or name (default: the default project; GET also looks in the others).
    project: Option<String>,
    /// `HEAD …?probe=1`: the upload dedupe check. A missing asset answers 204 instead of 404
    /// (an expected answer, not an error in the browser console).
    probe: Option<String>,
}

/// The stored file for `hex`: in `project`, else the default project, else any project.
/// An asset file of a project: `assets/`, or an app snapshot still (`apps/.snapshots/`).
pub fn find_in_project(dir: &Path, hex: &str) -> Option<PathBuf> {
    find(&dir.join("assets"), hex).or_else(|| find(&crate::apps::snapshots_dir(dir), hex))
}

pub fn locate(space: &Workspace, project: Option<&str>, hex: &str) -> Option<PathBuf> {
    if project.is_some() {
        return find_in_project(space.host_for_frame(project).ok()?.dir(), hex);
    }
    if let Some(p) = space.default_host().ok().and_then(|h| find_in_project(h.dir(), hex)) {
        return Some(p);
    }
    match space.root() {
        Some(root) => space.ids().iter().find_map(|id| find_in_project(&root.join(id), hex)),
        None => None,
    }
}

pub async fn get(
    method: Method,
    UrlPath(id): UrlPath<String>,
    State(space): State<Arc<Workspace>>,
    Query(q): Query<AssetQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(hex) = parse_id(&id) else {
        return text(
            StatusCode::BAD_REQUEST,
            "asset id must be a sha256 hex digest",
        );
    };
    let Some(path) = locate(&space, q.project.as_deref(), &hex) else {
        if q.probe.is_some() && method == Method::HEAD {
            return StatusCode::NO_CONTENT.into_response();
        }
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
    State(space): State<Arc<Workspace>>,
    Query(q): Query<AssetQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(hex) = parse_id(&id) else {
        return text(
            StatusCode::BAD_REQUEST,
            "asset id must be a sha256 hex digest",
        );
    };
    let host = match space.host_for_frame(q.project.as_deref()) {
        Ok(h) => h,
        Err(e) => return text(StatusCode::NOT_FOUND, format!("{e:#}")),
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

/// Largest file `POST /import/url` downloads.
pub const MAX_IMPORT_BYTES: u64 = 512 * 1024 * 1024;

/// Pixel size from an image header (PNG, JPEG, GIF, WebP).
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le24 = |i: usize| Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0]));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.len() > 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let (b0, b1, b2, b3) = (b[21] as u32, b[22] as u32, b[23] as u32, b[24] as u32);
                Some((1 + (b0 | (b1 & 0x3f) << 8), 1 + (b1 >> 6 | b2 << 2 | (b3 & 0xf) << 10)))
            }
            b"VP8X" => Some((1 + le24(24)?, 1 + le24(27)?)),
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            if m == 0xd8 || m == 0x01 || (0xd0..=0xd7).contains(&m) || m == 0xff {
                i += if m == 0xff { 1 } else { 2 };
                continue;
            }
            let len = be16(i + 2)? as usize;
            if matches!(m, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + len;
        }
    }
    None
}

/// Content type by magic bytes, for servers that answer `application/octet-stream`.
fn sniff(b: &[u8]) -> Option<&'static str> {
    Some(match b {
        _ if b.starts_with(b"\x89PNG") => "image/png",
        _ if b.starts_with(&[0xff, 0xd8, 0xff]) => "image/jpeg",
        _ if b.starts_with(b"GIF8") => "image/gif",
        _ if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" => "image/webp",
        _ if b.len() > 12 && &b[4..8] == b"ftyp" && (&b[8..12] == b"avif" || &b[8..12] == b"avis") => "image/avif",
        _ if b.len() > 12 && &b[4..8] == b"ftyp" => "video/mp4",
        _ => return None,
    })
}

#[derive(serde::Deserialize)]
pub struct ImportUrl {
    pub url: String,
    /// Expected digest (`sha256:<hex>` or hex): skips the download when the asset exists,
    /// and the download must match it.
    #[serde(default)]
    pub sha256: Option<String>,
    /// SHA-1 of the content (what Figma names its images by): the download must match it, and
    /// is remembered (`assets/.sha1/<sha1>` → sha256) so the same image imports again without
    /// a download.
    #[serde(default)]
    pub sha1: Option<String>,
}

fn sha1_hex(s: &str) -> Option<String> {
    let h = s.strip_prefix("sha1:").unwrap_or(s).to_ascii_lowercase();
    (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// `POST /import/url?project=<id>` `{url, sha256?}` → `{id, mime, size, width, height, file,
/// existing}`: downloads an http(s) image/video/audio file into the project's assets.
pub async fn import_url(
    State(space): State<Arc<Workspace>>,
    Query(q): Query<AssetQuery>,
    axum::Json(req): axum::Json<ImportUrl>,
) -> Response {
    let host = match space.host_for_frame(q.project.as_deref()) {
        Ok(h) => h,
        Err(e) => return text(StatusCode::NOT_FOUND, format!("{e:#}")),
    };
    let dir = host.dir().join("assets");
    let found = |path: &Path, existing: bool, size: u64, bytes: Option<&[u8]>| {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let hex = name.split('.').next().unwrap_or("").to_string();
        let mime = mime_of_ext(path.extension().and_then(|e| e.to_str()).unwrap_or(""));
        let dims = match bytes {
            Some(b) => image_size(b),
            None => std::fs::File::open(path).ok().and_then(|mut f| {
                use std::io::Read;
                let mut head = vec![0u8; 256 * 1024];
                let n = f.read(&mut head).ok()?;
                image_size(&head[..n])
            }),
        };
        let status = if existing { StatusCode::OK } else { StatusCode::CREATED };
        (
            status,
            axum::Json(serde_json::json!({
                "id": format!("sha256:{hex}"), "mime": mime, "size": size, "file": name, "existing": existing,
                "width": dims.map(|d| d.0), "height": dims.map(|d| d.1),
            })),
        )
            .into_response()
    };
    let sha1 = match req.sha1.as_deref().map(sha1_hex) {
        Some(None) => return text(StatusCode::BAD_REQUEST, "sha1 must be a sha1 hex digest"),
        Some(Some(h)) => Some(h),
        None => None,
    };
    let index = sha1.as_ref().map(|h| dir.join(".sha1").join(h));
    let mut want = match req.sha256.as_deref().map(parse_id) {
        Some(None) => return text(StatusCode::BAD_REQUEST, "sha256 must be a sha256 hex digest"),
        Some(Some(h)) => Some(h),
        None => None,
    };
    if want.is_none()
        && let Some(i) = &index
    {
        want = std::fs::read_to_string(i).ok().and_then(|v| parse_id(v.trim()));
    }
    if let Some(h) = &want
        && let Some(p) = find(&dir, h)
    {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        return found(&p, true, size, None);
    }
    let scheme = req.url.split(':').next().unwrap_or("").to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return text(StatusCode::BAD_REQUEST, "url must be http(s)");
    }
    let resp = match host.http.get(&req.url).timeout(std::time::Duration::from_secs(120)).send().await {
        Ok(r) => r,
        Err(e) => return text(StatusCode::BAD_GATEWAY, format!("download failed: {e}")),
    };
    if !resp.status().is_success() {
        return text(StatusCode::BAD_GATEWAY, format!("download failed: upstream {}", resp.status()));
    }
    if resp.content_length().is_some_and(|n| n > MAX_IMPORT_BYTES) {
        return text(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds 512 MiB");
    }
    let header_mime = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut resp = resp;
    let mut bytes = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                bytes.extend_from_slice(&c);
                if bytes.len() as u64 > MAX_IMPORT_BYTES {
                    return text(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds 512 MiB");
                }
            }
            Ok(None) => break,
            Err(e) => return text(StatusCode::BAD_GATEWAY, format!("download failed: {e}")),
        }
    }
    let mime = match sniff(&bytes) {
        Some(m) => m.to_string(),
        None => header_mime,
    };
    if !(mime.starts_with("image/") || mime.starts_with("video/") || mime.starts_with("audio/")) {
        return text(StatusCode::UNSUPPORTED_MEDIA_TYPE, format!("not media: {}", if mime.is_empty() { "unknown type" } else { &mime }));
    }
    let hex: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if want.as_ref().is_some_and(|w| *w != hex) && req.sha256.is_some() {
        return text(StatusCode::BAD_REQUEST, format!("sha256 mismatch: download hashes to {hex}"));
    }
    if let (Some(h1), Some(i)) = (&sha1, &index) {
        use sha1::Digest as _;
        let got: String = sha1::Sha1::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if got != *h1 {
            return text(StatusCode::BAD_REQUEST, format!("sha1 mismatch: download hashes to {got}"));
        }
        if let Some(parent) = i.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(i, &hex);
    }
    if let Some(p) = find(&dir, &hex) {
        return found(&p, true, bytes.len() as u64, Some(&bytes));
    }
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let file = dir.join(format!("{hex}.{}", ext_of_mime(&mime)));
    let tmp = dir.join(format!("{hex}.part{}", rand::random::<u32>()));
    let written = async {
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &file).await
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&tmp).await;
        return text(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    found(&file, false, bytes.len() as u64, Some(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sizes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((640, 480)));
        let gif = b"GIF89a\x20\x03\x58\x02".to_vec();
        assert_eq!(image_size(&gif), Some((800, 600)));
        // JPEG: SOI, an APP0 segment, then SOF0 with height 300 and width 400.
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0];
        jpg.extend_from_slice(&[0xff, 0xc0, 0, 11, 8, 0x01, 0x2c, 0x01, 0x90, 3, 0, 0, 0, 0]);
        assert_eq!(image_size(&jpg), Some((400, 300)));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend_from_slice(&[0; 8]);
        webp.extend_from_slice(&[0xff, 0x03, 0x00, 0xff, 0x01, 0x00, 0, 0, 0]);
        assert_eq!(image_size(&webp), Some((1024, 512)));
        assert_eq!(image_size(b"nope"), None);
        assert_eq!(sniff(&jpg), Some("image/jpeg"));
    }

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
