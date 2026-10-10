//! Chat + agent swarm (see `docs/CHAT-PROTOCOL.md`).
//!
//! The host is the hub between canvas clients (`chat.send` / `chat.stop` on `/sync`) and the
//! agent runtime (`/runtime`, see `runtime.rs`). It resolves context chips into rows,
//! lineage and local media paths, forwards runs to the runtime, numbers the runtime's
//! events, appends them to `<project>/chats/<chat>.jsonl` and broadcasts them to every
//! client on the chat's canvas as `chat.event`.
//!
//! Also home of the swarm's canvas tools: `import_file` and `get_context`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, Asset, Canvas, Command, Lineage, NodeKind, Props, ReadOpts};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::context::untrusted;
use crate::host::{Event, Host, now_ms};
use crate::mcp::{CanvasMcp, PlacementArgs};

/// Events kept per chat in `chat.history` (older ones stay in the jsonl file).
const HISTORY_EVENTS: usize = 5000;

// ---- hub ----------------------------------------------------------------------------------

#[derive(Default)]
struct ChatState {
    canvas: String,
    title: String,
    seq: u64,
    running: bool,
}

/// Chat routing state of one host.
pub struct ChatHub {
    /// Second secret (besides the per-install token) that only the supervised runtime
    /// knows: guards `/runtime` and the fal token endpoint.
    pub runtime_token: String,
    /// Outgoing frames to the connected runtime (one at a time; a new one replaces it).
    runtime: Mutex<Option<(u64, mpsc::UnboundedSender<String>)>>,
    /// Runs sent while no runtime was connected; flushed on connect.
    pending: Mutex<Vec<String>>,
    chats: Mutex<HashMap<String, ChatState>>,
    /// The host spawned a runtime (so a missing connection is transient).
    pub supervised: std::sync::atomic::AtomicBool,
}

impl Default for ChatHub {
    fn default() -> Self {
        let runtime_token = std::env::var("FALOS_RUNTIME_TOKEN")
            .ok()
            .filter(|t| t.len() >= 16)
            .unwrap_or_else(|| {
                (0..24)
                    .map(|_| format!("{:02x}", rand::random::<u8>()))
                    .collect()
            });
        Self {
            runtime_token,
            runtime: Mutex::default(),
            pending: Mutex::default(),
            chats: Mutex::default(),
            supervised: Default::default(),
        }
    }
}

fn valid_chat_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn chats_dir(host: &Host) -> PathBuf {
    host.dir().join("chats")
}

/// `{"seq":…,"ts":…,"event":{…}}` lines of a chat log.
fn read_log(path: &Path) -> Vec<Json> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .collect()
}

fn meta_path(host: &Host, chat: &str) -> PathBuf {
    chats_dir(host).join(format!("{chat}.meta.json"))
}

impl ChatHub {
    /// Load (or create) a chat's state from disk.
    fn state<'a>(
        host: &Host,
        chats: &'a mut HashMap<String, ChatState>,
        chat: &str,
        canvas: &str,
    ) -> &'a mut ChatState {
        chats.entry(chat.to_string()).or_insert_with(|| {
            let meta: Json = std::fs::read(meta_path(host, chat))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let seq = read_log(&chats_dir(host).join(format!("{chat}.jsonl")))
                .iter()
                .filter_map(|l| l["seq"].as_u64())
                .max()
                .unwrap_or(0);
            ChatState {
                canvas: meta["canvas"].as_str().unwrap_or(canvas).to_string(),
                title: meta["title"].as_str().unwrap_or_default().to_string(),
                seq,
                running: false,
            }
        })
    }

    fn save_meta(host: &Host, chat: &str, s: &ChatState) {
        let _ = std::fs::create_dir_all(chats_dir(host));
        let _ = std::fs::write(
            meta_path(host, chat),
            serde_json::to_vec_pretty(&json!({"chat": chat, "canvas": s.canvas, "title": s.title}))
                .unwrap_or_default(),
        );
    }

    /// Number an event, persist it and broadcast it as `chat.event`.
    pub fn append(
        &self,
        host: &Host,
        chat: &str,
        canvas: Option<&str>,
        event: Json,
    ) -> Result<u64> {
        if !valid_chat_id(chat) {
            bail!("bad chat id `{chat}`");
        }
        let (seq, canvas) = {
            let mut chats = self.chats.lock().unwrap();
            let s = Self::state(host, &mut chats, chat, canvas.unwrap_or("main"));
            let mut meta_dirty = false;
            match event["kind"].as_str() {
                Some("user") if s.title.is_empty() => {
                    s.title = title_from(event["text"].as_str().unwrap_or_default());
                    meta_dirty = true;
                }
                Some("title") => {
                    s.title = event["title"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(80)
                        .collect();
                    meta_dirty = true;
                }
                Some("status") => s.running = event["state"] == "running",
                Some("done") => s.running = false,
                _ => {}
            }
            if s.seq == 0 {
                meta_dirty = true;
            }
            s.seq += 1;
            if meta_dirty {
                Self::save_meta(host, chat, s);
            }
            (s.seq, s.canvas.clone())
        };
        let dir = chats_dir(host);
        std::fs::create_dir_all(&dir)?;
        let line = json!({"seq": seq, "ts": now_ms(), "event": event});
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{chat}.jsonl")))?;
        writeln!(f, "{line}")?;
        host.emit(Event::ChatEvent {
            canvas,
            chat: chat.to_string(),
            seq,
            event,
        });
        Ok(seq)
    }

    /// Send a frame to the runtime, or queue it until one connects.
    fn to_runtime(&self, frame: String) -> bool {
        if let Some((_, tx)) = &*self.runtime.lock().unwrap()
            && tx.send(frame.clone()).is_ok()
        {
            return true;
        }
        self.pending.lock().unwrap().push(frame);
        false
    }

    /// Register a runtime connection; returns its id and receiver (queued runs first).
    pub fn connect_runtime(&self) -> (u64, mpsc::UnboundedReceiver<String>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        for f in self.pending.lock().unwrap().drain(..) {
            let _ = tx.send(f);
        }
        *self.runtime.lock().unwrap() = Some((id, tx));
        (id, rx)
    }

    /// The runtime went away: running chats end with an error.
    pub fn disconnect_runtime(&self, host: &Host, id: u64) {
        {
            let mut r = self.runtime.lock().unwrap();
            if r.as_ref().is_some_and(|(i, _)| *i == id) {
                *r = None;
            } else {
                return;
            }
        }
        let running: Vec<String> = self
            .chats
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.running)
            .map(|(k, _)| k.clone())
            .collect();
        for chat in running {
            let _ = self.append(
                host,
                &chat,
                None,
                json!({"kind": "status", "state": "error", "error": "agent runtime disconnected"}),
            );
        }
    }

    pub fn runtime_connected(&self) -> bool {
        self.runtime.lock().unwrap().is_some()
    }
}

fn title_from(text: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 48 {
        format!("{}…", t.chars().take(47).collect::<String>())
    } else {
        t
    }
}

// ---- client messages ------------------------------------------------------------------------

/// A `/sync` text frame whose `type` starts with `chat.`.
pub fn chat_frame(text: &str) -> Option<Json> {
    if !text.contains("chat.") {
        return None;
    }
    serde_json::from_str::<Json>(text)
        .ok()
        .filter(|m| m["type"].as_str().is_some_and(|t| t.starts_with("chat.")))
}

/// Handle a `chat.*` frame from a `/sync` client. Returns an error message for the client.
pub async fn handle_client(host: &Arc<Host>, canvas: &str, author: &str, msg: &Json) -> Result<()> {
    let hub = &host.chat;
    match msg["type"].as_str() {
        Some("chat.send") => {
            let text = msg["text"].as_str().unwrap_or_default().trim().to_string();
            if text.is_empty() {
                bail!("chat.send needs text");
            }
            let chat = match msg["chat"].as_str() {
                Some(c) if valid_chat_id(c) => c.to_string(),
                Some(c) => bail!("bad chat id `{c}`"),
                None => format!(
                    "c_{}",
                    (0..5)
                        .map(|_| format!("{:02x}", rand::random::<u8>()))
                        .collect::<String>()
                ),
            };
            let chips: Vec<Json> = msg["context"].as_array().cloned().unwrap_or_default();
            let options = msg.get("options").cloned().unwrap_or(Json::Null);
            // Prior turns, before this message is appended.
            let history = condensed_history(host, &chat);
            hub.append(
                host,
                &chat,
                Some(canvas),
                json!({"kind": "user", "text": text, "context": chips, "author": author}),
            )?;
            let mut resolved = Vec::new();
            for c in &chips {
                resolved.push(match resolve_chip(host, canvas, c).await {
                    Ok(r) => r,
                    Err(e) => json!({"chip": c, "error": format!("{e:#}")}),
                });
            }
            let chat_canvas = {
                let mut chats = hub.chats.lock().unwrap();
                ChatHub::state(host, &mut chats, &chat, canvas)
                    .canvas
                    .clone()
            };
            let frame = json!({
                "type": "run",
                "chat": chat,
                "canvas": chat_canvas,
                "text": text,
                "author": author,
                "context": resolved,
                "history": history,
                "options": options,
                "project": host.dir().display().to_string(),
            });
            let delivered = hub.to_runtime(frame.to_string());
            if !delivered && !hub.supervised.load(std::sync::atomic::Ordering::Relaxed) {
                hub.append(
                    host,
                    &chat,
                    None,
                    json!({"kind": "status", "state": "error",
                           "error": "no agent runtime is connected (start the host with --runtime); the message is queued"}),
                )?;
            }
            Ok(())
        }
        Some("chat.stop") => {
            let chat = msg["chat"]
                .as_str()
                .filter(|c| valid_chat_id(c))
                .context("chat.stop needs chat")?;
            hub.to_runtime(json!({"type": "stop", "chat": chat}).to_string());
            Ok(())
        }
        other => bail!("unknown chat message {other:?}"),
    }
}

/// Prior turns of a chat as `[{role, text}]`: user messages and the director's text.
fn condensed_history(host: &Host, chat: &str) -> Vec<Json> {
    let mut out: Vec<Json> = Vec::new();
    let mut assistant = String::new();
    for l in read_log(&chats_dir(host).join(format!("{chat}.jsonl"))) {
        let e = &l["event"];
        match e["kind"].as_str() {
            Some("user") => {
                if !assistant.is_empty() {
                    out.push(json!({"role": "assistant", "text": std::mem::take(&mut assistant)}));
                }
                out.push(json!({"role": "user", "text": e["text"]}));
            }
            Some("text") if e["agent"].as_str().is_none_or(|a| a == "director") => {
                assistant.push_str(e["delta"].as_str().unwrap_or_default())
            }
            Some("done") => {
                if let Some(s) = e["summary"].as_str() {
                    let _ = write!(assistant, "\n[done: {s}]");
                }
            }
            _ => {}
        }
    }
    if !assistant.is_empty() {
        out.push(json!({"role": "assistant", "text": assistant}));
    }
    out
}

/// `chat.history` for a canvas: every chat whose canvas it is, oldest first.
pub fn history(host: &Host, canvas: &str) -> Json {
    let mut chats = Vec::new();
    if let Ok(rd) = std::fs::read_dir(chats_dir(host)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(chat) = name.strip_suffix(".meta.json") else {
                continue;
            };
            let meta: Json = std::fs::read(e.path())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            if meta["canvas"].as_str() != Some(canvas) {
                continue;
            }
            let log = read_log(&chats_dir(host).join(format!("{chat}.jsonl")));
            let first = log.first().and_then(|l| l["ts"].as_i64()).unwrap_or(0);
            let skip = log.len().saturating_sub(HISTORY_EVENTS);
            let events: Vec<Json> = log
                .into_iter()
                .skip(skip)
                .map(|mut l| {
                    let mut ev = l["event"].take();
                    ev["seq"] = l["seq"].take();
                    ev
                })
                .collect();
            chats.push((
                first,
                json!({"chat": chat, "title": meta["title"], "events": events}),
            ));
        }
    }
    chats.sort_by_key(|(t, _)| *t);
    json!({"type": "chat.history", "chats": chats.into_iter().map(|(_, c)| c).collect::<Vec<_>>()})
}

// ---- context chips ----------------------------------------------------------------------------

/// Local file of a content-addressed asset (`sha256:<hex>` → `<project>/assets/<hex>.*`).
pub fn asset_file(host: &Host, asset: &str) -> Option<PathBuf> {
    let hex = asset.strip_prefix("sha256:").unwrap_or(asset);
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    std::fs::read_dir(host.dir().join("assets"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == hex))
}

fn lineage_lines(c: &Canvas, asset: Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut next = asset;
    while let Some(id) = next.take()
        && out.len() < 5
    {
        let Some(rec) = c.asset(&id) else { break };
        let mut s = format!("asset {id}");
        if let Some(l) = &rec.lineage {
            let _ = write!(s, " ← {} by @{} params {}", l.model, l.actor, l.params);
            if let Some(rid) = &l.fal_request_id {
                let _ = write!(s, " request {rid}");
            }
            next = l.parents.first().cloned();
        }
        out.push(s);
    }
    out
}

fn num(v: &Json) -> Option<f64> {
    v.as_f64()
}

fn quote_path(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', "'\\''"))
}

/// Resolve one chip to rows, lineage, local file and crop/frame hints.
pub async fn resolve_chip(host: &Host, canvas: &str, chip: &Json) -> Result<Json> {
    let kind = chip["kind"].as_str().unwrap_or("node");
    if !matches!(kind, "node" | "spot" | "region" | "frame" | "range") {
        bail!("unknown chip kind `{kind}`");
    }
    let node_id = chip["node"]
        .as_str()
        .context("chip needs node")?
        .to_string();
    let canvas = host.resolve_canvas(canvas)?;
    let (node, row, asset, lineage) = host.read(&canvas, |c| -> Result<_> {
        let n = c
            .node(&node_id)
            .ok_or_else(|| anyhow!("unknown node `{node_id}`"))?;
        let row = c.read(&ReadOpts {
            root: Some(n.id.clone()),
            depth: Some(0),
            annotations: true,
            ..Default::default()
        })?;
        let asset_id = n
            .props
            .get("asset")
            .and_then(Json::as_str)
            .map(String::from);
        let asset = asset_id.as_deref().and_then(|a| c.asset(a));
        let lineage = lineage_lines(c, asset_id);
        Ok((n, row, asset, lineage))
    })??;
    let mut out = json!({
        "chip": chip,
        "node": node.id,
        "type": node.kind.to_string(),
        "name": node.name,
        "row": untrusted(&row),
        "lineage": lineage,
    });
    if node.kind.has_text() || node.text.is_some() {
        out["text"] = Json::from(untrusted(node.text.as_deref().unwrap_or_default()));
    }
    let Some(asset) = asset else { return Ok(out) };
    out["asset"] = json!(asset.id);
    out["mime"] = json!(asset.mime);
    out["uri"] = json!(asset.uri);
    let Some(file) = asset_file(host, &asset.id) else {
        return Ok(out);
    };
    out["file"] = json!(file.display().to_string());
    let probe = probe(&file).await.unwrap_or_default();
    out["media"] =
        json!({"width": probe.width, "height": probe.height, "duration": probe.duration});
    let (pw, ph) = (probe.width.unwrap_or(0.0), probe.height.unwrap_or(0.0));
    let f = quote_path(&file);
    let px = |b: [f64; 4]| -> Json {
        let (x, y) = ((b[0] * pw).round(), (b[1] * ph).round());
        let (w, h) = ((b[2] * pw).round().max(1.0), (b[3] * ph).round().max(1.0));
        json!({"x": x, "y": y, "w": w, "h": h})
    };
    let boxed = |v: &Json| -> Option<[f64; 4]> {
        let a = v.as_array().filter(|a| a.len() == 4)?;
        Some([num(&a[0])?, num(&a[1])?, num(&a[2])?, num(&a[3])?])
    };
    match kind {
        "spot" => {
            let (x, y) = (
                num(&chip["x"]).unwrap_or(0.5),
                num(&chip["y"]).unwrap_or(0.5),
            );
            out["point"] = json!({"x": (x * pw).round(), "y": (y * ph).round()});
            // A quarter-size box around the spot, clamped to the image.
            let b = [
                (x - 0.125).clamp(0.0, 0.75),
                (y - 0.125).clamp(0.0, 0.75),
                0.25,
                0.25,
            ];
            out["crop"] = px(b);
        }
        "region" => {
            if let Some(b) = boxed(&chip["box"]) {
                out["crop"] = px(b);
            }
        }
        "frame" => {
            let t = num(&chip["t"]).unwrap_or(0.0);
            out["frame"] = json!({"t": t});
            let mut hint = format!("ffmpeg -y -ss {t} -i {f} -frames:v 1");
            if let Some(b) = boxed(&chip["box"]) {
                let c = px(b);
                out["crop"] = c.clone();
                let _ = write!(
                    hint,
                    " -vf crop={}:{}:{}:{}",
                    c["w"], c["h"], c["x"], c["y"]
                );
            }
            hint.push_str(" frame.png");
            out["hint"] = json!(hint);
        }
        "range" => {
            let (t0, t1) = (
                num(&chip["t0"]).unwrap_or(0.0),
                num(&chip["t1"]).unwrap_or(0.0),
            );
            out["range"] = json!({"t0": t0, "t1": t1});
            out["hint"] = json!(format!(
                "ffmpeg -y -ss {t0} -to {t1} -i {f} clip.{}",
                file.extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "mp4".into())
            ));
        }
        _ => {}
    }
    if let Some(c) = out.get("crop").cloned()
        && kind != "frame"
    {
        out["hint"] = json!(format!(
            "ffmpeg -y -i {f} -vf crop={}:{}:{}:{} crop.png",
            c["w"], c["h"], c["x"], c["y"]
        ));
    }
    Ok(out)
}

// ---- media probing + import ----------------------------------------------------------------

#[derive(Default, Debug, Clone, Copy)]
pub struct Probe {
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub duration: Option<f64>,
}

/// Width/height/duration via ffprobe (when installed), with a PNG header fallback.
pub async fn probe(path: &Path) -> Result<Probe> {
    let ffprobe = std::env::var("FALOS_FFPROBE").unwrap_or_else(|_| "ffprobe".into());
    let out = tokio::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(path)
        .output()
        .await;
    let mut p = Probe::default();
    if let Ok(o) = out
        && o.status.success()
        && let Ok(v) = serde_json::from_slice::<Json>(&o.stdout)
    {
        for s in v["streams"].as_array().into_iter().flatten() {
            if s["codec_type"] == "video" && p.width.is_none() {
                p.width = s["width"].as_f64();
                p.height = s["height"].as_f64();
            }
        }
        p.duration = v["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| *d > 0.0);
    } else if let Ok(bytes) = tokio::fs::read(path).await
        && bytes.len() > 24
        && bytes.starts_with(b"\x89PNG")
    {
        let be =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        p.width = Some(be(16) as f64);
        p.height = Some(be(20) as f64);
    }
    Ok(p)
}

fn media_kind(ext: &str) -> Option<(NodeKind, &'static str)> {
    Some(match ext {
        "png" => (NodeKind::Image, "image/png"),
        "jpg" | "jpeg" => (NodeKind::Image, "image/jpeg"),
        "webp" => (NodeKind::Image, "image/webp"),
        "gif" => (NodeKind::Image, "image/gif"),
        "mp4" | "m4v" => (NodeKind::Video, "video/mp4"),
        "mov" => (NodeKind::Video, "video/quicktime"),
        "webm" => (NodeKind::Video, "video/webm"),
        "mp3" => (NodeKind::Audio, "audio/mpeg"),
        "wav" => (NodeKind::Audio, "audio/wav"),
        "m4a" | "aac" => (NodeKind::Audio, "audio/mp4"),
        "ogg" => (NodeKind::Audio, "audio/ogg"),
        "flac" => (NodeKind::Audio, "audio/flac"),
        _ => return None,
    })
}

pub struct Imported {
    pub node: String,
    pub asset: String,
    pub file: PathBuf,
    pub kind: NodeKind,
    pub probe: Probe,
    pub w: f64,
    pub h: f64,
}

/// Provenance for an imported file (e.g. a fal Agent artifact or a Remotion render).
#[derive(Deserialize, schemars::JsonSchema, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LineageArgs {
    /// What produced it, e.g. "fal-agent", "fal-agent:fal-ai/flux/dev", "remotion".
    pub model: Option<String>,
    /// Inputs worth keeping: task text, prompt, composition props, ... (JSON object).
    pub params: Option<Json>,
    /// fal request / Agent response id.
    pub fal_request_id: Option<String>,
    /// USD.
    pub cost: Option<f64>,
    /// Asset ids (sha256:...) or node ids this was made from.
    pub parents: Option<Vec<String>>,
    /// Remote URL of the original (e.g. the fal CDN url), kept as the asset uri.
    pub uri: Option<String>,
}

#[derive(Default)]
pub struct ImportOpts {
    pub place: Option<PlacementArgs>,
    pub name: Option<String>,
    pub lineage: Option<LineageArgs>,
    /// A `generation` placeholder node to convert in place (same id, parent and z-order).
    pub replace: Option<String>,
}

/// Copy a local media file into the project's assets (by sha256) and create its node
/// (or convert a placeholder `generation` node into it).
pub async fn import_file(
    host: &Host,
    actor: &Actor,
    canvas: &str,
    path: &Path,
    opts: ImportOpts,
) -> Result<Imported> {
    let canvas = host.resolve_canvas(canvas)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (kind, mime) = media_kind(&ext).ok_or_else(|| {
        anyhow!("import_file takes image/video/audio files (png jpg webp gif mp4 mov webm mp3 wav m4a ogg flac), not `.{ext}`")
    })?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let hex: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = host.dir().join("assets");
    tokio::fs::create_dir_all(&dir).await?;
    let file = dir.join(format!("{hex}.{ext}"));
    if !file.exists() {
        tokio::fs::write(&file, &bytes).await?;
    }
    let probe = probe(&file).await.unwrap_or_default();
    let placeholder = match &opts.replace {
        Some(id) => Some(
            host.read(&canvas, |c| c.node(id))?
                .filter(|n| n.kind == NodeKind::Generation)
                .ok_or_else(|| anyhow!("`{id}` is not a generation node on this canvas"))?,
        ),
        None => None,
    };
    let mut p = opts.place.unwrap_or(PlacementArgs {
        x: None,
        y: None,
        w: None,
        h: None,
        parent: None,
    });
    if let Some(n) = &placeholder {
        // Keep the placeholder's spot and width; the height follows the media.
        p = PlacementArgs {
            x: Some(n.x),
            y: Some(n.y),
            w: Some(n.w),
            h: None,
            parent: n.parent.clone(),
        };
    }
    let (w, h) = match (kind, probe.width, probe.height) {
        (NodeKind::Audio, _, _) => (p.w.unwrap_or(480.0), p.h.unwrap_or(96.0)),
        (_, Some(iw), Some(ih)) if iw > 0.0 && ih > 0.0 => match (p.w, p.h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * ih / iw),
            (None, Some(h)) => (h * iw / ih, h),
            (None, None) => {
                let w = iw.min(960.0);
                (w, w * ih / iw)
            }
        },
        _ => (p.w.unwrap_or(512.0), p.h.unwrap_or(512.0)),
    };
    let (x, y) = match (p.x, p.y) {
        (Some(x), Some(y)) => (x, y),
        _ => host.read(&canvas, |c| {
            let right = c
                .nodes()
                .iter()
                .filter(|n| n.parent.is_none())
                .map(|n| n.x + n.w)
                .fold(f64::NEG_INFINITY, f64::max);
            (if right.is_finite() { right + 40.0 } else { 0.0 }, 0.0)
        })?,
    };
    let name = opts
        .name
        .or_else(|| {
            placeholder
                .as_ref()
                .map(|n| n.name.clone())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    let l = opts.lineage.unwrap_or_default();
    let mut params = l.params.unwrap_or_else(|| json!({}));
    if let Some(o) = params.as_object_mut() {
        o.entry("source")
            .or_insert(json!(path.display().to_string()));
        if let Some(d) = probe.duration {
            o.entry("duration").or_insert(json!(d));
        }
    }
    let asset = Asset {
        id: format!("sha256:{hex}"),
        mime: Some(mime.into()),
        uri: l.uri,
        lineage: Some(Lineage {
            model: l.model.unwrap_or_else(|| "import".into()),
            params,
            seed: None,
            parents: l.parents.unwrap_or_default(),
            fal_request_id: l.fal_request_id,
            cost: l.cost,
            actor: actor.id.clone(),
        }),
    };
    let props: Props =
        serde_json::from_value(json!({"x": x, "y": y, "w": w, "h": h, "asset": asset.id}))?;
    let replace = placeholder.as_ref().map(|n| n.id.clone());
    let res = host.mutate(&canvas, &actor.id, None, |c| {
        let mut cmds = vec![Command::PutAsset(asset.clone())];
        let mut index = None;
        if let Some(id) = &replace {
            let siblings: Vec<String> = match c.node(id).and_then(|n| n.parent) {
                Some(par) => c.node(&par).map(|n| n.children).unwrap_or_default(),
                None => c
                    .nodes()
                    .into_iter()
                    .filter(|x| x.parent.is_none())
                    .map(|x| x.id)
                    .collect(),
            };
            index = siblings.iter().position(|s| s == id);
            cmds.push(Command::DeleteNode { id: id.clone() });
        }
        cmds.push(Command::CreateNode {
            id: replace.clone(),
            kind,
            parent: p.parent.clone(),
            index,
            name: name.chars().take(80).collect(),
            props,
            text: None,
        });
        c.apply(actor, None, &cmds)
    })?;
    let node = match replace {
        Some(id) => id,
        None => res
            .created
            .iter()
            .find(|id| {
                host.read(&canvas, |c| c.node(id).is_some())
                    .unwrap_or(false)
            })
            .cloned()
            .context("import created no node")?,
    };
    Ok(Imported {
        node,
        asset: asset.id,
        file,
        kind,
        probe,
        w,
        h,
    })
}

// ---- MCP tools -----------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileArgs {
    pub session_id: String,
    pub canvas: String,
    /// Absolute path of a local image/video/audio file (e.g. a render).
    pub path: String,
    pub placement: Option<PlacementArgs>,
    /// Node name (default: the placeholder's or the file name).
    pub name: Option<String>,
    /// Provenance recorded on the asset (model, params, falRequestId, cost, parents, uri).
    pub lineage: Option<LineageArgs>,
    /// Id of a `generation` placeholder node to turn into this media node (same id/spot).
    pub replace: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetContextArgs {
    pub session_id: String,
    pub canvas: String,
    /// Context chips, e.g. [{"kind":"node","node":"a1b2c3"}], [{"kind":"region","node":"a1b2c3","box":[0.1,0.2,0.5,0.4]}],
    /// [{"kind":"frame","node":"v9","t":3.2}], [{"kind":"range","node":"au1","t0":2,"t1":5.5}].
    pub chips: Vec<Json>,
}

#[tool_router(router = chat_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "Import a local image/video/audio file (e.g. a Remotion render or a downloaded fal Agent artifact) into the project: copies it into assets by sha256 and creates the media node sized from the file (or converts the `replace` generation placeholder in place), with optional lineage. Returns the node id.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn import_file(&self, Parameters(a): Parameters<ImportFileArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let actor = host.actor(&a.session_id)?;
            let path = PathBuf::from(&a.path);
            if !path.is_absolute() {
                bail!("path must be absolute");
            }
            let opts = ImportOpts { place: a.placement, name: a.name, lineage: a.lineage, replace: a.replace };
            let i = import_file(&host, &actor, &a.canvas, &path, opts).await?;
            Ok(serde_json::to_string_pretty(&json!({
                "node": i.node, "type": i.kind.to_string(), "asset": i.asset,
                "file": i.file.display().to_string(),
                "size": [i.w, i.h],
                "media": {"width": i.probe.width, "height": i.probe.height, "duration": i.probe.duration},
            }))?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Resolve context chips (node, spot, region, frame, range) to the node's row, lineage, asset uri, local media file path and pixel crop / ffmpeg frame hints.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_context(&self, Parameters(a): Parameters<GetContextArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            host.actor(&a.session_id)?;
            let mut out = Vec::new();
            for c in &a.chips {
                out.push(match resolve_chip(&host, &a.canvas, c).await {
                    Ok(v) => v,
                    Err(e) => json!({"chip": c, "error": format!("{e:#}")}),
                });
            }
            Ok(serde_json::to_string_pretty(&out)?)
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ids_and_titles() {
        assert!(valid_chat_id("c_12ab"));
        assert!(!valid_chat_id("../x"));
        assert!(!valid_chat_id(""));
        assert_eq!(
            title_from("  OPUS   please\n direct "),
            "OPUS please direct"
        );
        assert!(title_from(&"x".repeat(100)).ends_with('…'));
    }
}
