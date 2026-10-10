//! `falos-host mcp`: a stdio ↔ Streamable-HTTP relay to the running host, for MCP clients
//! that prefer stdio servers. Reads `~/.fal-os/host.json` (`FALOS_HOME`) for the URL and
//! token; every request carries `X-Falos-From: stdio` so the host can tell.
//!
//! Each stdin line is one JSON-RPC message, POSTed to `/mcp`; JSON or SSE responses are
//! written back to stdout one message per line. `initialize` runs first (it yields the MCP
//! session id); later messages are relayed concurrently.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::server::HostFile;

#[derive(Default)]
struct State {
    session: Option<String>,
    protocol: Option<String>,
}

struct Relay {
    http: reqwest::Client,
    url: String,
    token: String,
    state: std::sync::Mutex<State>,
    out: Mutex<tokio::io::Stdout>,
}

impl Relay {
    async fn write(&self, msg: &Json) {
        let mut line = msg.to_string();
        line.push('\n');
        let mut out = self.out.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.flush().await;
    }

    fn request(&self, method: reqwest::Method) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, &self.url)
            .bearer_auth(&self.token)
            .header("accept", "application/json, text/event-stream")
            .header("x-falos-from", "stdio");
        let st = self.state.lock().unwrap();
        if let Some(s) = &st.session {
            r = r.header("mcp-session-id", s);
        }
        if let Some(p) = &st.protocol {
            r = r.header("mcp-protocol-version", p);
        }
        r
    }

    /// POST one message and relay whatever comes back.
    async fn relay(&self, msg: Json) {
        let id = msg.get("id").cloned();
        if let Err(e) = self.relay_inner(&msg).await
            && let Some(id) = id
        {
            self.write(&json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": format!("fal-os relay: {e:#}")}}))
                .await;
        }
    }

    async fn relay_inner(&self, msg: &Json) -> Result<()> {
        let mut resp = self
            .request(reqwest::Method::POST)
            .header("content-type", "application/json")
            .body(msg.to_string())
            .send()
            .await
            .context("the fal-os host is not reachable; is `falos-host serve` running?")?;
        if let Some(s) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            self.state.lock().unwrap().session = Some(s.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("host answered {status}: {}", body.chars().take(300).collect::<String>());
        }
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.starts_with("text/event-stream"));
        if !sse {
            let body = resp.text().await?;
            if !body.trim().is_empty() {
                let v: Json = serde_json::from_str(&body)?;
                self.on_message(&v);
                self.write(&v).await;
            }
            return Ok(());
        }
        // SSE: forward each `data:` event as it arrives.
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await? {
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
                    self.write(&v).await;
                }
            }
        }
        Ok(())
    }

    /// Remember the negotiated protocol version (sent on later requests).
    fn on_message(&self, v: &Json) {
        if let Some(p) = v["result"]["protocolVersion"].as_str() {
            self.state.lock().unwrap().protocol = Some(p.to_string());
        }
    }
}

pub async fn run() -> Result<()> {
    let hf = HostFile::load().context(
        "no running fal-os host found (~/.fal-os/host.json); start it with `falos-host serve`",
    )?;
    let relay = Arc::new(Relay {
        http: reqwest::Client::new(),
        url: hf.url.clone(),
        token: hf.token.clone(),
        state: Default::default(),
        out: Mutex::new(tokio::io::stdout()),
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut tasks = tokio::task::JoinSet::new();
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
        } else {
            let r = relay.clone();
            tasks.spawn(async move { r.relay(msg).await });
        }
    }
    while tasks.join_next().await.is_some() {}
    if relay.state.lock().unwrap().session.is_some() {
        let _ = relay.request(reqwest::Method::DELETE).send().await;
    }
    Ok(())
}
