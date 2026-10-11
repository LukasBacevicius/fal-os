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
        let made = tokio::time::timeout(std::time::Duration::from_secs(60), lib.ensure_thumb(&key)).await;
        match made {
            Ok(Ok(k)) => path = thumbs::path_of(&lib.thumbs_dir, &k, size),
            Ok(Err(e)) => return (StatusCode::NOT_FOUND, format!("no thumbnail: {e:#}")).into_response(),
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
