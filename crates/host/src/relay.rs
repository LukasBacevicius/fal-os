//! `falos-host mcp`: a stdio ↔ Streamable-HTTP relay to the running host, for MCP clients
//! that prefer stdio servers (what `connect` registers in Claude Code, the Claude desktop app
//! and Codex). Reads `~/.fal-os/host.json` (`FALOS_HOME`) for the URL and token; every request
//! carries `X-Falos-From: stdio` so the host can tell.
//!
//! Each stdin line is one JSON-RPC message, POSTed to `/mcp`; JSON or SSE responses are
//! written back to stdout one message per line. `initialize` runs first (it yields the MCP
//! session id); later messages are relayed concurrently.
//!
//! The app may not be running yet, or restart on another port with a new session: host.json
//! is read lazily, and when the host is unreachable or no longer knows our session, the relay
//! re-reads host.json, replays `initialize` (+ `notifications/initialized`) and retries once,
//! so the client keeps working without a restart. Requests that still fail get a JSON-RPC
//! error that says what to do.
//!
//! fal OS not running: `initialize` and `tools/list` are answered from the manifest the host
//! writes on every start (`$FALOS_HOME/mcp-manifest.json`), so an agent started before the app
//! still sees every tool; a tool call then returns a tool error that says to ask the user to
//! open fal OS (and not to launch it). Once the app runs, the next request connects for real.
//!
//! Project binding: every request carries `X-Falos-Cwd` (the relay's working directory, i.e.
//! the folder the agent was started in) and, when set, `FALOS_PROJECT` as `X-Falos-Project`.
//! Push: after `initialize` the relay also holds the server's GET event stream open and
//! forwards server-initiated messages (e.g. `notifications/claude/channel`) to stdout.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::server::HostFile;

const NOT_RUNNING: &str = "fal OS isn't running: tell the user to open it, then retry; don't try to launch it";

/// The manifest the host wrote on its last start (see `mcp::write_manifest`).
fn manifest() -> Option<Json> {
    serde_json::from_slice(&std::fs::read(crate::mcp::manifest_path()).ok()?).ok()
}

/// What to answer while the host is unreachable, from the manifest: `initialize`,
/// `tools/list`, `ping`, and a tool error for `tools/call`. None: no manifest or another method.
fn offline_answer(msg: &Json) -> Option<Json> {
    let id = msg.get("id")?.clone();
    let result = match msg["method"].as_str()? {
        "ping" => json!({}),
        "tools/call" => json!({"content": [{"type": "text", "text": NOT_RUNNING}], "isError": true}),
        "initialize" => {
            let mut init = manifest()?["initialize"].clone();
            if let Some(p) = msg["params"]["protocolVersion"].as_str() {
                init["protocolVersion"] = json!(p);
            }
            init
        }
        "tools/list" => json!({"tools": manifest()?["tools"].clone()}),
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[derive(Default)]
struct State {
    endpoint: Option<(String, String)>,
    session: Option<String>,
    protocol: Option<String>,
    /// The client's `initialize` request, replayed after a host restart.
    init: Option<Json>,
}

struct Relay {
    http: reqwest::Client,
    state: std::sync::Mutex<State>,
    out: Mutex<tokio::io::Stdout>,
    /// Serializes re-initialization.
    reinit: Mutex<()>,
}

enum SendErr {
    /// Not reachable (not running, or moved to another port).
    Unreachable(String),
    /// Unknown session / token (the host restarted).
    Stale(String),
    Other(anyhow::Error),
}

impl Relay {
    async fn write(&self, msg: &Json) {
        let mut line = msg.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }

    /// The host's (url, token), reading host.json when unknown.
    fn endpoint(&self, reload: bool) -> Option<(String, String)> {
        let mut st = self.state.lock().unwrap();
        if reload || st.endpoint.is_none() {
            let fresh = HostFile::load().map(|h| (h.url, h.token));
            if fresh != st.endpoint {
                st.session = None;
            }
            st.endpoint = fresh;
        }
        st.endpoint.clone()
    }

    fn request(&self, method: reqwest::Method, with_session: bool) -> Result<reqwest::RequestBuilder, SendErr> {
        let (url, token) = self.endpoint(false).ok_or_else(|| SendErr::Unreachable(NOT_RUNNING.into()))?;
        let mut r = self
            .http
            .request(method, &url)
            .bearer_auth(&token)
            .header("accept", "application/json, text/event-stream")
            .header("x-falos-from", "stdio");
        if let Ok(cwd) = std::env::current_dir() {
            r = r.header("x-falos-cwd", cwd.to_string_lossy().as_ref());
        }
        if let Some(p) = std::env::var("FALOS_PROJECT").ok().filter(|p| !p.trim().is_empty()) {
            r = r.header("x-falos-project", p.trim());
        }
        let st = self.state.lock().unwrap();
        if with_session && let Some(s) = &st.session {
            r = r.header("mcp-session-id", s);
        }
        if let Some(p) = &st.protocol {
            r = r.header("mcp-protocol-version", p);
        }
        Ok(r)
    }

    /// POST one message and relay whatever comes back.
    async fn relay(&self, msg: Json) {
        let id = msg.get("id").cloned();
        if msg["method"] == "initialize" {
            self.state.lock().unwrap().init = Some(msg.clone());
        }
        if let Err(e) = self.relay_retrying(&msg).await
            && let Some(id) = id
        {
            let down = format!("{e:#}").starts_with(NOT_RUNNING);
            if down && let Some(answer) = offline_answer(&msg) {
                self.write(&answer).await;
                return;
            }
            self.write(&json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": format!("fal-os relay: {e:#}")}}))
                .await;
        }
    }

    async fn relay_retrying(&self, msg: &Json) -> Result<()> {
        let is_init = msg["method"] == "initialize";
        match self.send(msg, true, !is_init).await {
            Ok(()) => Ok(()),
            Err(SendErr::Other(e)) => Err(e),
            Err(SendErr::Unreachable(e) | SendErr::Stale(e)) => {
                // The host restarted (maybe on another port): new host.json, new session.
                let before = self.endpoint(false);
                if self.endpoint(true).is_none() {
                    return Err(anyhow!(NOT_RUNNING));
                }
                if !is_init {
                    self.reinitialize(before).await.map_err(|_| anyhow!("{e}"))?;
                }
                match self.send(msg, true, !is_init).await {
                    Ok(()) => Ok(()),
                    Err(SendErr::Unreachable(_)) => Err(anyhow!(NOT_RUNNING)),
                    Err(SendErr::Stale(e)) => Err(anyhow!(e)),
                    Err(SendErr::Other(e)) => Err(e),
                }
            }
        }
    }

    /// Replay the client's `initialize` (once per restart, even with concurrent requests).
    async fn reinitialize(&self, stale: Option<(String, String)>) -> Result<()> {
        let _g = self.reinit.lock().await;
        {
            let st = self.state.lock().unwrap();
            // Another request already re-initialized against the new host.
            if st.session.is_some() && st.endpoint != stale {
                return Ok(());
            }
        }
        let init = self.state.lock().unwrap().init.clone().context("no initialize to replay")?;
        self.state.lock().unwrap().session = None;
        self.send(&init, false, false).await.map_err(|e| match e {
            SendErr::Unreachable(m) | SendErr::Stale(m) => anyhow!(m),
            SendErr::Other(e) => e,
        })?;
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.send(&note, false, true).await.map_err(|_| anyhow!("re-initializing failed"))?;
        tracing::info!("fal-os relay: reconnected to the host");
        Ok(())
    }

    /// One POST. `forward`: write responses to stdout (false while replaying `initialize`).
    async fn send(&self, msg: &Json, forward: bool, with_session: bool) -> Result<(), SendErr> {
        let mut resp = match self
            .request(reqwest::Method::POST, with_session)?
            .header("content-type", "application/json")
            .body(msg.to_string())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => return Err(SendErr::Unreachable(format!("{NOT_RUNNING} ({e})"))),
            Err(e) => return Err(SendErr::Other(e.into())),
        };
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            self.state.lock().unwrap().session = Some(s.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let m = format!("host answered {status}: {}", body.chars().take(300).collect::<String>());
            // 401: the token changed (new host.json); 404 / "session": the host restarted.
            let stale = status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::NOT_FOUND
                || body.to_lowercase().contains("session");
            return Err(if stale { SendErr::Stale(m) } else { SendErr::Other(anyhow!(m)) });
        }
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/event-stream"));
        if !sse {
            let body = resp.text().await.map_err(|e| SendErr::Other(e.into()))?;
            if !body.trim().is_empty() {
                let v: Json = serde_json::from_str(&body).map_err(|e| SendErr::Other(e.into()))?;
                self.on_message(&v);
                if forward {
                    self.write(&v).await;
                }
            }
            return Ok(());
        }
        // SSE: forward each `data:` event as it arrives.
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| SendErr::Other(e.into()))? {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = buf.find("\n\n") {
                let event: String = buf.drain(..end + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(v) = serde_json::from_str::<Json>(&data) {
                    self.on_message(&v);
                    if forward {
                        self.write(&v).await;
                    }
                }
            }
        }
        Ok(())
    }

    /// The server's standalone event stream (GET): server-initiated notifications go to
    /// stdout. Reconnects with backoff; stops when the server has no such stream (405).
    async fn listen(self: Arc<Self>) {
        let mut backoff = 1u64;
        loop {
            let has_session = self.state.lock().unwrap().session.is_some();
            if has_session && let Ok(req) = self.request(reqwest::Method::GET, true) {
                match req.header("accept", "text/event-stream").send().await {
                    Ok(r) if r.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED => return,
                    Ok(mut r) if r.status().is_success() => {
                        backoff = 1;
                        let mut buf = String::new();
                        while let Ok(Some(chunk)) = r.chunk().await {
                            buf.push_str(&String::from_utf8_lossy(&chunk));
                            while let Some(end) = buf.find("\n\n") {
                                let event: String = buf.drain(..end + 2).collect();
                                let data: String = event.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
                                if let Ok(v) = serde_json::from_str::<Json>(&data) {
                                    self.write(&v).await;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    }

    /// Remember the negotiated protocol version (sent on later requests).
    fn on_message(&self, v: &Json) {
        if let Some(p) = v["result"]["protocolVersion"].as_str() {
            self.state.lock().unwrap().protocol = Some(p.to_string());
        }
    }
}

pub async fn run() -> Result<()> {
    let relay = Arc::new(Relay {
        // A host that accepts connections but never answers (stopped, wedged) must not hang
        // the agent on connect; calls themselves can legitimately take long.
        http: reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(5)).build()?,
        state: Default::default(),
        out: Mutex::new(tokio::io::stdout()),
        reinit: Mutex::new(()),
    });
    if relay.endpoint(false).is_none() {
        tracing::warn!("{NOT_RUNNING} (no {})", crate::server::home_dir().join("host.json").display());
    }
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut tasks = tokio::task::JoinSet::new();
    let mut listening = false;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Json = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                relay
                    .write(&json!({"jsonrpc": "2.0", "id": null,
                        "error": {"code": -32700, "message": format!("parse error: {e}")}}))
                    .await;
                continue;
            }
        };
        // The session id comes from `initialize`: everything else waits for it.
        if msg["method"] == "initialize" {
            relay.relay(msg).await;
            if !listening {
                listening = true;
                tokio::spawn(relay.clone().listen());
            }
        } else {
            let r = relay.clone();
            tasks.spawn(async move { r.relay(msg).await });
        }
    }
    while tasks.join_next().await.is_some() {}
    if relay.state.lock().unwrap().session.is_some()
        && let Ok(r) = relay.request(reqwest::Method::DELETE, true)
    {
        let _ = r.send().await;
    }
    Ok(())
}
