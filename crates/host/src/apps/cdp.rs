//! Headless check of an app: load it in chrome-headless-shell over the DevTools protocol,
//! collect console errors / uncaught exceptions / failed loads, take a screenshot. Audio is
//! muted (`--mute-audio`, and the guard script suspends audio).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

/// How long the app runs after `load` before the screenshot (animations start, errors land).
const SETTLE: Duration = Duration::from_millis(1500);
const LOAD_TIMEOUT: Duration = Duration::from_secs(10);

pub struct CheckOut {
    /// (level, text): `error` | `exception` | `warning` | `log`.
    pub entries: Vec<(String, String)>,
    pub png: Option<Vec<u8>>,
    /// Something about the check itself (load timeout), not the app.
    pub note: Option<String>,
}

/// `$FALOS_CHROME`, Remotion's chrome-headless-shell next to the agent runtime, or Chrome.
pub fn chrome_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FALOS_CHROME").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let plat = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "mac-arm64",
        ("macos", _) => "mac-x64",
        ("linux", "aarch64") => "linux-arm64",
        _ => "linux64",
    };
    if let Some(rt) = crate::runtime::detect() {
        let p = rt
            .join("motion-template/node_modules/.remotion/chrome-headless-shell")
            .join(plat)
            .join(format!("chrome-headless-shell-{plat}"))
            .join("chrome-headless-shell");
        if p.is_file() {
            return Some(p);
        }
    }
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/chromium",
        "/usr/bin/google-chrome",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Cdp {
    ws: Ws,
    next: u64,
    session: Option<String>,
    events: Vec<Json>,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Json) -> Result<Json> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = &self.session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::Text(msg.to_string().into())).await?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let m = tokio::time::timeout(left, self.ws.next())
                .await
                .map_err(|_| anyhow!("devtools: {method} timed out"))?
                .ok_or_else(|| anyhow!("devtools closed"))??;
            let Message::Text(t) = m else { continue };
            let v: Json = serde_json::from_str(&t)?;
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    bail!("devtools {method}: {e}");
                }
                return Ok(v["result"].clone());
            }
            if v.get("method").is_some() {
                self.events.push(v);
            }
        }
    }

    /// Collect events for `d` (or until `stop` matches one).
    async fn pump(&mut self, d: Duration, stop: impl Fn(&Json) -> bool) -> bool {
        let deadline = Instant::now() + d;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match tokio::time::timeout(left, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if let Ok(v) = serde_json::from_str::<Json>(&t)
                        && v.get("method").is_some()
                    {
                        let hit = stop(&v);
                        self.events.push(v);
                        if hit {
                            return true;
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
                _ => return false,
            }
        }
    }
}

fn remote_text(v: &Json) -> String {
    if let Some(s) = v["value"].as_str() {
        return s.to_string();
    }
    if let Some(d) = v["description"].as_str() {
        return d.to_string();
    }
    if !v["value"].is_null() {
        return v["value"].to_string();
    }
    v["type"].as_str().unwrap_or("").to_string()
}

/// Turn DevTools events into log entries.
fn entries(events: &[Json]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for e in events {
        let p = &e["params"];
        let item = match e["method"].as_str().unwrap_or("") {
            "Runtime.consoleAPICalled" => {
                let level = match p["type"].as_str().unwrap_or("log") {
                    "error" | "assert" => "error",
                    "warning" => "warning",
                    _ => "log",
                };
                let text: Vec<String> = p["args"].as_array().into_iter().flatten().map(remote_text).collect();
                Some((level, text.join(" ")))
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let desc = d["exception"]["description"].as_str().map(str::to_string).unwrap_or_else(|| d["text"].as_str().unwrap_or("exception").to_string());
                let at = match (d["url"].as_str(), d["lineNumber"].as_u64()) {
                    (Some(u), Some(l)) if !u.is_empty() => format!(" ({}:{})", u.rsplit('/').next().unwrap_or(u), l + 1),
                    _ => String::new(),
                };
                Some(("exception", format!("{desc}{at}")))
            }
            "Log.entryAdded" => {
                let en = &p["entry"];
                let level = match en["level"].as_str().unwrap_or("info") {
                    "error" => "error",
                    "warning" => "warning",
                    _ => "log",
                };
                let url = en["url"].as_str().filter(|u| !u.is_empty()).map(|u| format!(" {u}")).unwrap_or_default();
                Some((level, format!("{}{url}", en["text"].as_str().unwrap_or(""))))
            }
            _ => None,
        };
        if let Some((level, text)) = item {
            let text: String = text.chars().take(2000).collect();
            // Favicon misses are browser noise, not the app's.
            if text.contains("favicon.ico") || out.iter().any(|(l, t)| l == level && *t == text) {
                continue;
            }
            out.push((level.to_string(), text));
        }
    }
    out.truncate(80);
    out
}

/// Load `url` at w×h, let it settle, return what it logged plus a PNG screenshot.
pub async fn check(chrome: &Path, url: &str, w: u32, h: u32) -> Result<CheckOut> {
    let profile = std::env::temp_dir().join(format!("falos-check-{}", rand::random::<u64>()));
    let mut child = tokio::process::Command::new(chrome)
        .args([
            "--headless",
            "--remote-debugging-port=0",
            "--mute-audio",
            "--no-first-run",
            "--no-default-browser-check",
            "--hide-scrollbars",
            "--autoplay-policy=user-gesture-required",
            &format!("--user-data-dir={}", profile.display()),
            &format!("--window-size={w},{h}"),
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", chrome.display()))?;
    let r = async {
        let stderr = child.stderr.take().context("chrome stderr")?;
        let mut lines = BufReader::new(stderr).lines();
        let ws_url = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(l) = lines.next_line().await? {
                if let Some(u) = l.split("DevTools listening on ").nth(1) {
                    return Ok::<_, anyhow::Error>(u.trim().to_string());
                }
            }
            bail!("chrome exited before devtools came up")
        })
        .await
        .map_err(|_| anyhow!("chrome didn't start in time"))??;
        // Keep draining stderr so chrome never blocks on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let (ws, _) = tokio_tungstenite::connect_async(ws_url.as_str()).await.context("devtools connect")?;
        let mut cdp = Cdp { ws, next: 0, session: None, events: vec![] };
        let target = cdp.call("Target.createTarget", json!({"url": "about:blank"})).await?;
        let tid = target["targetId"].as_str().context("no target")?.to_string();
        let att = cdp.call("Target.attachToTarget", json!({"targetId": tid, "flatten": true})).await?;
        cdp.session = Some(att["sessionId"].as_str().context("no session")?.to_string());
        cdp.call("Runtime.enable", json!({})).await?;
        cdp.call("Log.enable", json!({})).await?;
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Emulation.setDeviceMetricsOverride", json!({"width": w, "height": h, "deviceScaleFactor": 1, "mobile": false})).await?;
        let nav = cdp.call("Page.navigate", json!({"url": url})).await?;
        if let Some(e) = nav["errorText"].as_str().filter(|e| !e.is_empty()) {
            bail!("loading the app failed: {e}");
        }
        let loaded = cdp.events.iter().any(|e| e["method"] == "Page.loadEventFired")
            || cdp.pump(LOAD_TIMEOUT, |e| e["method"] == "Page.loadEventFired").await;
        cdp.pump(SETTLE, |_| false).await;
        let shot = cdp.call("Page.captureScreenshot", json!({"format": "png"})).await.ok();
        let png = shot
            .and_then(|s| s["data"].as_str().map(str::to_string))
            .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok());
        let _ = cdp.call("Browser.close", json!({})).await;
        Ok(CheckOut {
            entries: entries(&cdp.events),
            png,
            note: (!loaded).then(|| format!("the page didn't finish loading in {} s (a slow CDN import?)", LOAD_TIMEOUT.as_secs())),
        })
    }
    .await;
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&profile);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devtools_events_become_entries() {
        let ev = vec![
            json!({"method": "Runtime.consoleAPICalled", "params": {"type": "error", "args": [{"type": "string", "value": "boom"}, {"type": "number", "value": 3}]}}),
            json!({"method": "Runtime.exceptionThrown", "params": {"exceptionDetails": {"text": "Uncaught", "url": "http://x/a/b/game.js", "lineNumber": 9, "exception": {"description": "ReferenceError: paddle is not defined"}}}}),
            json!({"method": "Log.entryAdded", "params": {"entry": {"level": "error", "text": "Failed to load resource: 404", "url": "http://x/a/b/missing.js"}}}),
            json!({"method": "Log.entryAdded", "params": {"entry": {"level": "error", "text": "Failed to load resource: 404", "url": "http://x/favicon.ico"}}}),
        ];
        let out = entries(&ev);
        assert_eq!(out[0], ("error".into(), "boom 3".into()));
        assert_eq!(out[1], ("exception".into(), "ReferenceError: paddle is not defined (game.js:10)".into()));
        assert!(out[2].1.ends_with("missing.js"));
        assert_eq!(out.len(), 3);
    }
}
