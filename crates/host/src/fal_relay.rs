//! Minimal MCP client for fal's hosted relay (`https://mcp.fal.ai/mcp-relay`), authorized with
//! the user's "Sign in with fal" OAuth token (see `auth`). Generation runs through the relay's
//! `submit_job` / `check_job` / `get_job_result` tools, so the host never handles API keys and
//! jobs bill to the account the user selected in fal.

use anyhow::{Result, bail};
use serde_json::{Value as Json, json};
use tokio::sync::Mutex;

use crate::auth;

pub const RELAY_URL: &str = "https://mcp.fal.ai/mcp-relay";

pub struct Relay {
    http: reqwest::Client,
    url: String,
    session: Mutex<Option<String>>,
}

impl Relay {
    pub fn new(http: reqwest::Client) -> Self {
        let url = std::env::var("FALOS_FAL_RELAY_URL").unwrap_or_else(|_| RELAY_URL.into());
        Self { http, url, session: Mutex::new(None) }
    }

    /// Whether the user is signed in (a relay call may still fail if the session expired).
    pub fn available() -> bool {
        auth::load().is_some()
    }

    async fn post(&self, token: &str, session: Option<&str>, body: &Json) -> Result<(Option<String>, Json)> {
        let mut req = self
            .http
            .post(&self.url)
            .bearer_auth(token)
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(s) = session {
            req = req.header("mcp-session-id", s);
        }
        let res = req.send().await?;
        let status = res.status();
        let sid = res
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let text = res.text().await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            bail!("fal rejected the session (401): run `falos-host login`");
        }
        if !status.is_success() {
            bail!("fal relay error {status}: {}", text.chars().take(300).collect::<String>());
        }
        // Streamable HTTP answers either JSON or an SSE stream; take the JSON-RPC response.
        let msg = if text.trim_start().starts_with('{') {
            serde_json::from_str(&text)?
        } else {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Json>(d.trim()).ok())
                .find(|m| m.get("id").is_some())
                .unwrap_or(Json::Null)
        };
        Ok((sid, msg))
    }

    async fn ensure_session(&self, token: &str) -> Result<Option<String>> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let (sid, msg) = self
                .post(
                    token,
                    None,
                    &json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": {"name": "fal-os", "version": env!("CARGO_PKG_VERSION")},
                    }}),
                )
                .await?;
            if msg.get("error").is_some() {
                bail!("fal relay initialize failed: {}", msg["error"]);
            }
            self.post(token, sid.as_deref(), &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await
                .ok();
            *guard = Some(sid.unwrap_or_default());
        }
        Ok(guard.clone().filter(|s| !s.is_empty()))
    }

    /// Calls a relay tool and returns its structured result (or the JSON in its text content).
    pub async fn call(&self, tool: &str, args: Json) -> Result<Json> {
        let token = auth::access_token(&self.http).await?;
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
        let mut attempt = 0;
        let msg = loop {
            let session = self.ensure_session(&token).await?;
            let res = self.post(&token, session.as_deref(), &body).await;
            match res {
                // Session expired server-side: re-initialize once.
                Err(e) if attempt == 0 && e.to_string().contains("404") => {
                    *self.session.lock().await = None;
                    attempt += 1;
                }
                other => break other?.1,
            }
        };
        if let Some(err) = msg.get("error") {
            bail!("fal relay `{tool}` failed: {}", err["message"].as_str().unwrap_or(&err.to_string()));
        }
        let result = &msg["result"];
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result["isError"].as_bool() == Some(true) {
            bail!("fal `{tool}`: {}", text.chars().take(500).collect::<String>());
        }
        if let Some(s) = result.get("structuredContent").filter(|s| !s.is_null()) {
            return Ok(s.clone());
        }
        Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({"text": text})))
    }
}
