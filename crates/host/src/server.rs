//! The local HTTP server: MCP at `/mcp`, canvas sync at `/sync/{canvas}`, both behind a
//! per-install bearer token and loopback-only Host/Origin checks.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::host::Host;
use crate::mcp::CanvasMcp;
use crate::workspace::Workspace;

pub const DEFAULT_PORT: u16 = 7317;

/// `~/.fal-os` (override with `FALOS_HOME`).
pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("FALOS_HOME") {
        return h.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| ".".into());
    home.join(".fal-os")
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    o.mode(0o600);
    let mut f = o.open(&tmp)?;
    #[cfg(unix)]
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The per-install token at `~/.fal-os/token` (0600), created on first use. Creation is
/// exclusive so concurrent processes agree on one token.
pub fn load_or_create_token() -> Result<String> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = home_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("token");
    for _ in 0..50 {
        if let Ok(t) = std::fs::read_to_string(&path) {
            let t = t.trim().to_string();
            if t.len() >= 32 {
                return Ok(t);
            }
        }
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        o.mode(0o600);
        match o.open(&path) {
            Ok(mut f) => {
                let t: String = (0..32)
                    .map(|_| format!("{:02x}", rand::random::<u8>()))
                    .collect();
                f.write_all(t.as_bytes())?;
                return Ok(t);
            }
            // Another process is writing it right now.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(e) => return Err(e).context("creating ~/.fal-os/token"),
        }
    }
    anyhow::bail!(
        "{} is empty or corrupt; delete it to regenerate",
        path.display()
    )
}

/// Discovery file at `~/.fal-os/host.json` (0600).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostFile {
    pub port: u16,
    pub url: String,
    pub token: String,
    pub pid: u32,
    /// The default project's folder (single-project mode: the project).
    pub project: String,
    /// The workspace folder (`--workspace`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl HostFile {
    pub fn load() -> Option<Self> {
        serde_json::from_slice(&std::fs::read(home_dir().join("host.json")).ok()?).ok()
    }
    pub fn save(&self) -> Result<()> {
        write_private(
            &home_dir().join("host.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

pub fn mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

fn is_loopback_host(host: &str) -> bool {
    // Strip the port; IPv6 literals are bracketed.
    let h = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(h, p)| {
            if p.chars().all(|c| c.is_ascii_digit()) {
                h
            } else {
                host
            }
        })
    };
    matches!(
        h.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn is_allowed_origin(origin: &str) -> bool {
    if origin == "tauri://localhost" {
        return true;
    }
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https")
        && (is_loopback_host(rest) || rest.split(':').next() == Some("tauri.localhost"))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone)]
struct Guard {
    token: Arc<str>,
}

/// CORS for the canvas UI (a loopback dev server or the Tauri webview fetching `/assets`).
/// Only origins that already pass `is_allowed_origin` get these headers.
fn add_cors(resp: &mut Response, origin: &header::HeaderValue) {
    let h = resp.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    h.insert(header::VARY, header::HeaderValue::from_static("Origin"));
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("content-range, content-length, accept-ranges, etag"),
    );
}

async fn guard(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).cloned();
    let mut resp = guard_inner(g, req, next).await;
    if let Some(o) = origin.filter(|o| o.to_str().is_ok_and(is_allowed_origin)) {
        add_cors(&mut resp, &o);
    }
    resp
}

async fn guard_inner(g: Guard, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(is_loopback_host);
    if !host_ok {
        return (StatusCode::FORBIDDEN, "fal-os host: loopback Host required").into_response();
    }
    if let Some(o) = headers.get(header::ORIGIN)
        && !o.to_str().is_ok_and(is_allowed_origin)
    {
        return (StatusCode::FORBIDDEN, "fal-os host: origin not allowed").into_response();
    }
    // CORS preflight (no credentials on it by spec) for the asset routes.
    if req.method() == axum::http::Method::OPTIONS && req.uri().path().starts_with("/assets/") {
        return (
            StatusCode::NO_CONTENT,
            [
                (
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    "GET, HEAD, PUT, OPTIONS",
                ),
                (
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    "authorization, content-type, range",
                ),
                (header::ACCESS_CONTROL_MAX_AGE, "600"),
            ],
        )
            .into_response();
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim);
    // Browsers can't set headers on WebSocket upgrades or `<video src>`, so /sync, /workspace
    // and asset reads also take ?token=.
    let path = req.uri().path();
    let query_ok = path.starts_with("/sync/")
        || path == "/workspace"
        || (path.starts_with("/assets/")
            && matches!(
                *req.method(),
                axum::http::Method::GET | axum::http::Method::HEAD
            ));
    let query = query_ok
        .then(|| req.uri().query())
        .flatten()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
    let ok = bearer
        .or(query)
        .is_some_and(|t| ct_eq(t.as_bytes(), g.token.as_bytes()));
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "fal-os host: missing or bad bearer token (see ~/.fal-os/host.json)",
        )
            .into_response();
    }
    next.run(req).await
}

/// Idle MCP sessions are dropped after this long. rmcp's default (5 min) is shorter than a
/// long render; an expired session answers `404 Session not found`.
pub const MCP_SESSION_IDLE: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

pub fn router(space: Arc<Workspace>, token: &str, ct: CancellationToken) -> Router {
    let mcp_space = space.clone();
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(MCP_SESSION_IDLE);
    let mcp: StreamableHttpService<CanvasMcp, LocalSessionManager> = StreamableHttpService::new(
        move || CanvasMcp::new(mcp_space.clone()).map_err(|e| std::io::Error::other(format!("{e:#}"))),
        Arc::new(sessions),
        StreamableHttpServerConfig::default().with_cancellation_token(ct),
    );
    Router::new()
        .route("/sync/{canvas}", get(crate::sync::handler))
        .route("/workspace", get(crate::workspace::ws_handler))
        .route(
            "/assets/{id}",
            get(crate::assets::get)
                .head(crate::assets::get)
                .put(crate::assets::put),
        )
        .merge(crate::runtime::routes())
        .with_state(space)
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(
            Guard {
                token: token.into(),
            },
            guard,
        ))
}

pub struct Running {
    pub addr: SocketAddr,
    pub cancel: CancellationToken,
    pub task: tokio::task::JoinHandle<()>,
    pub workspace: Arc<Workspace>,
}

/// Serve one project (single-project mode).
pub async fn serve(host: Arc<Host>, port: u16, token: &str) -> Result<Running> {
    serve_workspace(Workspace::single(host), port, token).await
}

/// Bind 127.0.0.1 on `port` (0 = ephemeral), falling back to the next 10 ports, then to
/// an ephemeral one. Starts the projects' background tasks and serves until `cancel`.
pub async fn serve_workspace(space: Arc<Workspace>, port: u16, token: &str) -> Result<Running> {
    let mut listener = None;
    if port != 0 {
        for p in port..port.saturating_add(10) {
            if let Ok(l) = TcpListener::bind(("127.0.0.1", p)).await {
                listener = Some(l);
                break;
            }
        }
    }
    let listener = match listener {
        Some(l) => l,
        None => TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding 127.0.0.1")?,
    };
    let addr = listener.local_addr()?;
    space.set_endpoint(mcp_url(addr.port()), token.to_string());
    space.start();
    let cancel = CancellationToken::new();
    let app = router(space.clone(), token, cancel.clone());
    let ct = cancel.clone();
    let ws = space.clone();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { ct.cancelled_owned().await })
            .await;
        ws.flush_all();
    });
    Ok(Running { addr, cancel, task, workspace: space })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_checks() {
        assert!(is_loopback_host("127.0.0.1:7317"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]:80"));
        assert!(!is_loopback_host("evil.com:7317"));
        assert!(!is_loopback_host("127.0.0.1.evil.com"));
        assert!(is_allowed_origin("http://localhost:5173"));
        assert!(is_allowed_origin("tauri://localhost"));
        assert!(!is_allowed_origin("https://evil.com"));
        assert!(!is_allowed_origin("null"));
    }
}
