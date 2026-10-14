//! UX v1 protocol: activity, external pseudo-chats + connect flow, selection, turns with
//! revert/redo, the queue, chat delete/open/close, agent.register, batch generation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use falos_host::context::NoRenderer;
use falos_host::{Host, HostOptions, server};
use futures::{SinkExt, StreamExt};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

struct Env {
    dir: tempfile::TempDir,
    host: Arc<Host>,
    port: u16,
    _running: server::Running,
}

async fn start(fal_queue_url: &str, fal_key: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(
        &dir.path().join("proj"),
        HostOptions {
            fal_queue_url: fal_queue_url.into(),
            fal_key: fal_key.map(String::from),
            fal_session: false,
            renderer: Box::new(NoRenderer),
            home: Some(dir.path().join("home")),
        },
    )
    .unwrap();
    let running = server::serve(host.clone(), 0, TOKEN).await.unwrap();
    Env { dir, host, port: running.addr.port(), _running: running }
}

type Client = RunningService<RoleClient, ClientConfig>;

/// An MCP client that says it is `name` (clientInfo) and optionally carries `X-Falos-Chat`.
async fn client_as(port: u16, name: &str, chat: Option<&str>) -> Client {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(server::mcp_url(port)).auth_header(TOKEN);
    if let Some(c) = chat {
        cfg.custom_headers.insert("x-falos-chat".parse().unwrap(), c.parse().unwrap());
    }
    let t = StreamableHttpClientTransport::from_config(cfg);
    ClientConfig::new(ClientCapabilities::default(), Implementation::new(name, "1.0"))
        .serve(t)
        .await
        .unwrap()
}

fn text(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).filter(|t| !t.starts_with("{\"project\"")).collect::<Vec<_>>().join("\n")
}

async fn call(c: &Client, tool: &'static str, args: Json) -> Json {
    let r = c
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap();
    let t = text(&r);
    assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
    let end = t.find("\n\nnotices").unwrap_or(t.len());
    serde_json::from_str(&t[..end]).unwrap_or(Json::String(t))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Json {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("ws timeout")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Next frame satisfying `pred`.
async fn next_where(ws: &mut Ws, pred: impl Fn(&Json) -> bool) -> Json {
    loop {
        let v = next_json(ws).await;
        if pred(&v) {
            return v;
        }
    }
}

async fn next_of(ws: &mut Ws, ty: &str) -> Json {
    next_where(ws, |v| v["type"] == ty).await
}

/// Next chat event of `kind` in `chat`.
async fn next_event(ws: &mut Ws, chat: &str, kind: &str) -> Json {
    next_where(ws, |v| v["type"] == "chat.event" && v["chat"] == chat && v["event"]["kind"] == kind).await["event"].clone()
}

async fn sync_client(port: u16) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/sync/main?token={TOKEN}&actor=lukas&name=Lukas"))
        .await
        .unwrap();
    ws
}

async fn runtime_client(port: u16, runtime_token: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/runtime").into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    req.headers_mut().insert("x-falos-runtime", runtime_token.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws).unwrap()
}

async fn send(ws: &mut Ws, v: Json) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Next `chat.list` satisfying `pred`.
async fn list_until(ws: &mut Ws, pred: impl Fn(&[Json]) -> bool) -> Vec<Json> {
    loop {
        let l = next_of(ws, "chat.list").await;
        let chats = l["chats"].as_array().unwrap().clone();
        if pred(&chats) {
            return chats;
        }
    }
}

fn row<'a>(chats: &'a [Json], chat: &str) -> Option<&'a Json> {
    chats.iter().find(|c| c["chat"] == chat)
}

#[tokio::test]
async fn external_agent_pseudo_chat_activity_and_connect_flow() {
    let env = start("http://127.0.0.1:1", None).await;
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;

    let c = client_as(env.port, "claude-code", None).await;
    let s = call(&c, "session", json!({"agentName": "Claude Code", "canvas": "main"})).await;
    let sid = s["sessionId"].as_str().unwrap().to_string();
    assert_eq!(s["chat"], "ext:claude-1");
    let conn = next_of(&mut ui, "agent.connected").await;
    assert_eq!((conn["client"].as_str(), conn["clientName"].as_str()), (Some("claude"), Some("claude-code")));
    assert_eq!((conn["agentName"].as_str(), conn["session"].as_str()), (Some("Claude Code"), Some(sid.as_str())));
    assert_eq!(conn["transport"], "http");
    let chats = list_until(&mut ui, |c| row(c, "ext:claude-1").is_some()).await;
    let ext = row(&chats, "ext:claude-1").unwrap();
    assert_eq!((ext["title"].as_str(), ext["external"].as_bool(), ext["client"].as_str()), (Some("Claude Code"), Some(true), Some("claude")));
    let color = ext["color"].as_str().unwrap().to_string();
    assert!(falos_host::chat::CHAT_COLORS.contains(&color.as_str()));

    // A write: activity (write), first write, presence in the chat's color, transcript line.
    let e = call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": "frame \"Moodboard\" p(0,0) s(800,600)\nnote \"Idea\" p(900,0) s(300,200) text(\"neon\")"})).await;
    let created: Vec<String> = e["created"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let fw = next_of(&mut ui, "agent.first_write").await;
    assert_eq!(fw["client"], "claude");
    let mut fwn: Vec<String> = fw["nodes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    fwn.sort();
    let mut sorted = created.clone();
    sorted.sort();
    assert_eq!(fwn, sorted);
    let act = next_where(&mut ui, |v| v["type"] == "activity" && v["op"] == "write").await;
    assert_eq!((act["session"].as_str(), act["chat"].as_str(), act["color"].as_str()), (Some(sid.as_str()), Some("ext:claude-1"), Some(color.as_str())));
    assert_eq!(act["nodes"], json!(created));
    assert!(act["at"].as_i64().unwrap() > 0);
    assert_eq!(act["agent"], "claude-code");
    let pres = next_where(&mut ui, |v| v["type"] == "presence" && v["session"]["id"] == sid && v["session"]["state"] == "writing").await;
    assert_eq!((pres["session"]["chat"].as_str(), pres["session"]["color"].as_str()), (Some("ext:claude-1"), Some(color.as_str())));
    assert_eq!(pres["session"]["client"], "claude");
    let tool = next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == "ext:claude-1" && v["event"]["state"] == "done").await;
    assert_eq!(tool["event"]["name"], "edit");
    assert_eq!(tool["event"]["summary"], "created 2 nodes");
    assert_eq!(tool["event"]["agent"], "claude-code");

    // Edit (modify) + delete ops; read → read sweep.
    call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": format!("{} text(\"neon tokyo\")", created[1])})).await;
    let act = next_where(&mut ui, |v| v["type"] == "activity" && v["op"] == "edit").await;
    assert_eq!(act["nodes"], json!([created[1]]));
    call(&c, "read", json!({"sessionId": sid, "canvas": "main", "root": created[0]})).await;
    let act = next_where(&mut ui, |v| v["type"] == "activity" && v["op"] == "read").await;
    assert_eq!(act["nodes"][0], created[0]);
    call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": format!("delete({})", created[1])})).await;
    let act = next_where(&mut ui, |v| v["type"] == "activity" && v["op"] == "delete").await;
    assert_eq!(act["nodes"], json!([created[1]]));

    // finish_working clears the working set.
    call(&c, "finish_working", json!({"sessionId": sid})).await;
    let clr = next_of(&mut ui, "activity.clear").await;
    assert_eq!((clr["session"].as_str(), clr["nodes"].clone()), (Some(sid.as_str()), Json::Null));

    // The user messages the agent from its tab: queued, then delivered with its next tool call.
    send(&mut ui, json!({"type": "chat.send", "chat": "ext:claude-1", "text": "make it warmer"})).await;
    let q = next_event(&mut ui, "ext:claude-1", "queued").await;
    assert_eq!(q["text"], "make it warmer");
    let r = call(&c, "list_annotations", json!({"sessionId": sid, "canvas": "main"})).await;
    assert!(r.as_str().unwrap().contains("Lukas says: make it warmer"), "{r}");
    let u = next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == "ext:claude-1" && v["event"]["kind"] == "user").await;
    assert_eq!((u["event"]["queued"].clone(), u["event"]["author"].as_str()), (q["id"].clone(), Some("Lukas")));
    assert_eq!(call(&c, "inbox", json!({"sessionId": sid})).await, Json::String("nothing new from the user".into()));

    // inbox long-polls until the user writes; stop is a message too.
    let peer = c.peer().clone();
    let args = json!({"sessionId": sid, "waitSecs": 20});
    let waiting = tokio::spawn(async move {
        let r = peer.call_tool(CallToolRequestParams::new("inbox").with_arguments(args.as_object().unwrap().clone())).await.unwrap();
        text(&r)
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&mut ui, json!({"type": "chat.stop", "chat": "ext:claude-1"})).await;
    let got = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("inbox woke").unwrap();
    assert!(got.contains("pressed stop"), "{got}");

    // report posts to the tab; arrange lays nodes out as a board.
    call(&c, "report", json!({"sessionId": sid, "text": "made a moodboard", "nodes": [created[0]]})).await;
    let t = next_event(&mut ui, "ext:claude-1", "text").await;
    assert_eq!(t["delta"], "made a moodboard");
    let e2 = call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": "image \"a\" p(0,900) s(400,300)\nimage \"b\" p(500,900) s(300,400)\nimage \"c\" p(900,900) s(512,512)\nimage \"d\" p(1500,900) s(512,512)"})).await;
    let imgs: Vec<String> = e2["created"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let b = call(&c, "arrange", json!({"sessionId": sid, "canvas": "main", "nodes": imgs, "title": "Best 4"})).await;
    let frame = b["frame"].as_str().unwrap().to_string();
    assert_eq!(b["grid"], json!([2, 2]));
    let rows = call(&c, "read", json!({"sessionId": sid, "canvas": "main", "root": frame})).await;
    let rows = rows.as_str().unwrap();
    assert!(rows.contains("frame \"Best 4\"") && imgs.iter().all(|i| rows.contains(&format!("  {i} image"))), "{rows}");

    // A second session from the same client gets its own pseudo-chat.
    let c2 = client_as(env.port, "codex-mcp-client", None).await;
    assert_eq!(call(&c2, "session", json!({"agentName": "Codex"})).await["chat"], "ext:codex-1");
    let c3 = client_as(env.port, "claude-code", None).await;
    assert_eq!(call(&c3, "session", json!({"agentName": "Claude Code"})).await["chat"], "ext:claude-2");
}

#[tokio::test]
async fn selection_is_visible_to_agents_and_placement_avoids_overlap() {
    let env = start("http://127.0.0.1:1", None).await;
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;
    let c = client_as(env.port, "cursor-vscode", None).await;
    let sid = call(&c, "session", json!({"agentName": "Cursor", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string();
    let e = call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": "image \"a\" p(0,0) s(400,300)\nimage \"b\" p(440,0) s(400,300)"})).await;
    let a = e["created"][0].as_str().unwrap().to_string();

    send(&mut ui, json!({"type": "selection", "nodes": [a]})).await;
    let mut sel = Json::Null;
    for _ in 0..50 {
        sel = call(&c, "get_selection", json!({"sessionId": sid})).await;
        if sel["selections"][0]["nodes"].as_array().is_some_and(|n| !n.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let me = &sel["selections"][0];
    assert_eq!((me["actor"].as_str(), me["canvas"].as_str()), (Some("@lukas"), Some("main")));
    assert_eq!((me["nodes"][0]["id"].as_str(), me["nodes"][0]["type"].as_str()), (Some(a.as_str()), Some("image")));
    assert!(me["nodes"][0]["row"].as_str().unwrap().contains("untrusted-canvas-data"));

    // Default anchor = the selection; right of `a` is taken by `b`, so it goes below `a`.
    let p = call(&c, "find_space", json!({"sessionId": sid, "canvas": "main", "w": 400, "h": 300})).await;
    assert_eq!((p["x"].as_f64(), p["y"].as_f64()), (Some(0.0), Some(340.0)));
}

#[tokio::test]
async fn turns_revert_redo_queue_and_chat_lifecycle() {
    let env = start("http://127.0.0.1:1", None).await;
    let mut rt = runtime_client(env.port, &env.host.chat.runtime_token).await;
    assert_eq!(next_json(&mut rt).await["type"], "hello");
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;

    send(&mut ui, json!({"type": "chat.send", "text": "make two notes"})).await;
    let user = next_where(&mut ui, |v| v["type"] == "chat.event" && v["event"]["kind"] == "user").await;
    let chat = user["chat"].as_str().unwrap().to_string();
    let turn = next_event(&mut ui, &chat, "turn").await;
    assert_eq!(turn["state"], "start");
    let turn_id = turn["turn"].as_str().unwrap().to_string();
    let run = next_of(&mut rt, "run").await;
    assert_eq!(run["turn"], turn_id);
    assert!(run["outline"].as_str().unwrap().contains("empty"));
    send(&mut rt, json!({"type": "event", "chat": chat, "event": {"kind": "status", "state": "running"}})).await;

    // The runtime's agent connection carries X-Falos-Chat: the session joins the chat.
    let c = client_as(env.port, "claude-code", Some(&chat)).await;
    let s = call(&c, "session", json!({"agentName": "Generator", "canvas": "main"})).await;
    assert_eq!(s["chat"], chat);
    let sid = s["sessionId"].as_str().unwrap().to_string();
    send(&mut rt, json!({"type": "session.agent", "chat": chat, "session": sid, "agent": "generator-2", "parent": "director"})).await;
    let pres = next_where(&mut ui, |v| v["type"] == "presence" && v["session"]["id"] == sid && v["session"]["agent"] == "generator-2").await;
    assert_eq!((pres["session"]["chat"].as_str(), pres["session"]["parent"].as_str()), (Some(chat.as_str()), Some("director")));
    let e = call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": "note \"one\" p(0,0) s(100,100)\nnote \"two\" p(200,0) s(100,100)"})).await;
    let made: Vec<String> = e["created"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let act = next_where(&mut ui, |v| v["type"] == "activity" && v["session"] == sid).await;
    assert_eq!((act["chat"].as_str(), act["agent"].as_str()), (Some(chat.as_str()), Some("generator-2")));

    // A message while running is queued; unqueue cancels it.
    send(&mut ui, json!({"type": "chat.send", "chat": chat, "text": "also a third"})).await;
    let q = next_event(&mut ui, &chat, "queued").await;
    assert_eq!(q["text"], "also a third");
    let qid = q["id"].as_str().unwrap().to_string();
    let qf = next_of(&mut rt, "queue").await;
    assert_eq!(qf["id"], qid);
    send(&mut ui, json!({"type": "chat.unqueue", "chat": chat, "id": qid})).await;
    let d = next_event(&mut ui, &chat, "dequeued").await;
    assert_eq!((d["id"].as_str(), d["reason"].as_str()), (Some(qid.as_str()), Some("cancelled")));
    assert_eq!(next_of(&mut rt, "unqueue").await["id"], qid);

    // Reverting a running turn is refused; once it ends, it reverts in one step.
    send(&mut ui, json!({"type": "chat.revert", "chat": chat, "turn": turn_id})).await;
    assert!(next_of(&mut ui, "error").await["message"].as_str().unwrap().contains("still running"));
    send(&mut rt, json!({"type": "event", "chat": chat, "event": {"kind": "status", "state": "idle"}})).await;
    let end = next_event(&mut ui, &chat, "turn").await;
    assert_eq!((end["state"].as_str(), end["turn"].as_str()), (Some("end"), Some(turn_id.as_str())));
    for n in &made {
        assert!(end["nodes"].as_array().unwrap().contains(&json!(n)), "{end}");
    }
    send(&mut ui, json!({"type": "chat.revert", "chat": chat, "turn": turn_id})).await;
    let rv = next_event(&mut ui, &chat, "reverted").await;
    assert_eq!(rv["turn"], turn_id);
    assert!(rv["count"].as_u64().unwrap() >= 1);
    assert!(env.host.read("main", |c| made.iter().all(|n| c.node(n).is_none())).unwrap());
    send(&mut ui, json!({"type": "chat.redo", "chat": chat, "turn": turn_id})).await;
    next_event(&mut ui, &chat, "redone").await;
    assert!(env.host.read("main", |c| made.iter().all(|n| c.node(n).is_some())).unwrap());

    // Tabs: close keeps the chat in the list (open: false); open restores; delete removes it.
    let chats = list_until(&mut ui, |c| row(c, &chat).is_some_and(|r| r["open"] == true)).await;
    assert!(row(&chats, &chat).unwrap()["color"].as_str().unwrap().starts_with('#'));
    send(&mut ui, json!({"type": "chat.close", "chat": chat})).await;
    list_until(&mut ui, |c| row(c, &chat).is_some_and(|r| r["open"] == false)).await;
    send(&mut ui, json!({"type": "chat.open", "chat": chat})).await;
    list_until(&mut ui, |c| row(c, &chat).is_some_and(|r| r["open"] == true)).await;
    send(&mut ui, json!({"type": "chat.delete", "chat": chat})).await;
    assert_eq!(next_of(&mut ui, "chat.deleted").await["chat"], chat);
    assert_eq!(next_of(&mut rt, "stop").await["chat"], chat);
    list_until(&mut ui, |c| row(c, &chat).is_none()).await;
    assert!(!env.host.dir().join(format!("chats/{chat}.jsonl")).exists());
    let index: Json = serde_json::from_slice(&std::fs::read(env.host.dir().join("chats/index.json")).unwrap()).unwrap();
    assert!(index["chats"].as_array().unwrap().iter().all(|r| r["chat"] != chat));
}

#[tokio::test]
async fn agent_register_writes_into_the_configured_home_only() {
    let env = start("http://127.0.0.1:1", None).await;
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;
    send(&mut ui, json!({"type": "agent.register", "client": "claude"})).await;
    let r = next_of(&mut ui, "agent.registered").await;
    assert_eq!((r["client"].as_str(), r["ok"].as_bool(), r["changed"].as_bool()), (Some("claude"), Some(true), Some(true)));
    let path = r["path"].as_str().unwrap();
    assert!(path.starts_with(env.dir.path().join("home").to_str().unwrap()), "{path}");
    let cfg: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(cfg["mcpServers"]["fal-os"]["url"], server::mcp_url(env.port));
    send(&mut ui, json!({"type": "agent.register", "client": "claude"})).await;
    assert_eq!(next_of(&mut ui, "agent.registered").await["changed"], false);
    send(&mut ui, json!({"type": "agent.register", "client": "opencode"})).await;
    let r = next_of(&mut ui, "agent.registered").await;
    assert!(r["path"].as_str().unwrap().ends_with(".config/opencode/opencode.json"));
    send(&mut ui, json!({"type": "agent.register", "client": "vim"})).await;
    let r = next_of(&mut ui, "agent.registered").await;
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("unknown client"));
}

/// Mock fal queue where every job takes `latency` from submit to completion.
async fn mock_fal(latency: Duration) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use axum::extract::Path;
    use axum::{Json as AJson, Router, routing::{get, post}};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let started: Arc<Mutex<HashMap<String, (Instant, u64)>>> = Arc::default();
    let status_calls = Arc::new(AtomicUsize::new(0));
    let next = Arc::new(AtomicUsize::new(0));
    let (b, s1, s2, s3, sc) = (base.clone(), started.clone(), started.clone(), started.clone(), status_calls.clone());
    let app = Router::new()
        .route("/fal-ai/mock", post(move |AJson(input): AJson<Json>| {
            let (b, s1, next) = (b.clone(), s1.clone(), next.clone());
            async move {
                let id = format!("req-{}", next.fetch_add(1, Ordering::SeqCst));
                let n = input["num_images"].as_u64().unwrap_or(1);
                s1.lock().unwrap().insert(id.clone(), (Instant::now(), n));
                AJson(json!({"request_id": id, "status_url": format!("{b}/status/{id}"), "response_url": format!("{b}/result/{id}")}))
            }
        }))
        .route("/status/{id}", get(move |Path(id): Path<String>| {
            let (s2, sc) = (s2.clone(), sc.clone());
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
                let done = s2.lock().unwrap().get(&id).is_some_and(|(t, _)| t.elapsed() >= latency);
                AJson(json!({"status": if done { "COMPLETED" } else { "IN_PROGRESS" }}))
            }
        }))
        .route("/result/{id}", get({
            let b = base.clone();
            move |Path(id): Path<String>| {
                let (b, s3) = (b.clone(), s3.clone());
                async move {
                    let n = s3.lock().unwrap().get(&id).map_or(1, |x| x.1);
                    let images: Vec<Json> = (0..n).map(|k| json!({"url": format!("{b}/out/{id}-{k}.png"), "width": 64, "height": 64, "content_type": "image/png"})).collect();
                    AJson(json!({"seed": 1, "images": images}))
                }
            }
        }))
        .route("/out/{f}", get(|Path(f): Path<String>| async move { f.into_bytes() }));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, status_calls)
}

#[tokio::test]
async fn batch_of_200_completes_in_about_one_job_latency() {
    let latency = Duration::from_secs(2);
    let (base, status_calls) = mock_fal(latency).await;
    let env = start(&base, Some("k")).await;
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;
    let c = client_as(env.port, "claude-code", None).await;
    let sid = call(&c, "session", json!({"agentName": "Claude Code", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string();
    // Something already on the canvas: the batch must not land on it.
    call(&c, "edit", json!({"sessionId": sid, "canvas": "main", "text": "frame \"Existing\" p(0,0) s(1000,800)"})).await;

    let t0 = Instant::now();
    let b = call(&c, "generate_batch", json!({
        "sessionId": sid, "canvas": "main", "model": "fal-ai/mock",
        "input": {"prompt": "pizza", "image_size": "square_hd"}, "count": 200,
    }))
    .await;
    let returned = t0.elapsed();
    assert!(returned < Duration::from_secs(2), "generate_batch blocked for {returned:?}");
    let nodes: Vec<String> = b["nodes"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(nodes.len(), 200);
    let frame = b["frame"].as_str().unwrap().to_string();
    // All placeholders exist at once, in a frame that doesn't overlap the existing one.
    let (fr, kinds) = env.host.read("main", |cv| {
        (cv.node(&frame).unwrap(), nodes.iter().filter(|n| cv.node(n).is_some_and(|x| x.kind == falos_doc::NodeKind::Generation)).count())
    }).unwrap();
    assert_eq!(kinds, 200);
    assert!(fr.name.contains("×200"), "{}", fr.name);
    assert!(fr.x >= 1000.0 || fr.y >= 800.0 || fr.x + fr.w <= 0.0 || fr.y + fr.h <= 0.0, "frame overlaps: {fr:?}");

    // Live progress events.
    let ev = next_where(&mut ui, |v| v["type"] == "batch").await;
    assert_eq!((ev["batch"]["kind"].as_str(), ev["batch"]["total"].as_u64()), (Some("batch"), Some(200)));

    let done = call(&c, "await_batch", json!({"sessionId": sid, "batch": b["batch"], "timeoutSecs": 60})).await;
    let elapsed = t0.elapsed();
    assert_eq!((done["done"].as_u64(), done["failed"].as_u64(), done["finished"].as_bool()), (Some(200), Some(0), Some(true)), "{done}");
    eprintln!(
        "batch: 200 jobs × {latency:?} latency finished in {elapsed:?} ({} status checks)",
        status_calls.load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(elapsed < latency * 3, "200 jobs took {elapsed:?}, expected ~{latency:?}");
    let images = env.host.read("main", |cv| nodes.iter().filter(|n| cv.node(n).is_some_and(|x| x.kind == falos_doc::NodeKind::Image && x.parent.as_deref() == Some(frame.as_str()))).count()).unwrap();
    assert_eq!(images, 200);
    let fin = next_where(&mut ui, |v| v["type"] == "batch" && v["batch"]["finished"] == true).await;
    assert_eq!(fin["batch"]["done"], 200);
}

#[tokio::test]
async fn batch_num_images_and_concurrent_edits_stress() {
    let (base, _) = mock_fal(Duration::from_millis(300)).await;
    let env = start(&base, Some("k")).await;
    let c = client_as(env.port, "claude-code", None).await;
    let sid = call(&c, "session", json!({"agentName": "A", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string();
    let other = client_as(env.port, "codex-mcp-client", None).await;
    let osid = call(&other, "session", json!({"agentName": "B", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string();
    // 25 requests × 4 images = 100 conversions, while another actor keeps creating/moving.
    let b = call(&c, "generate_batch", json!({
        "sessionId": sid, "canvas": "main", "model": "fal-ai/mock",
        "input": {"prompt": "p", "num_images": 4}, "count": 25,
    }))
    .await;
    assert_eq!(b["total"], 100);
    let editor = tokio::spawn(async move {
        for i in 0..40 {
            let e = call(&other, "edit", json!({"sessionId": osid, "canvas": "main", "text": format!("note \"n{i}\" p({},5000) s(50,50)", i * 60)})).await;
            let id = e["created"][0].as_str().unwrap().to_string();
            call(&other, "edit", json!({"sessionId": osid, "canvas": "main", "text": format!("{id} p({},5100)", i * 60)})).await;
        }
    });
    let done = call(&c, "await_batch", json!({"sessionId": sid, "batch": b["batch"], "timeoutSecs": 60})).await;
    editor.await.unwrap();
    assert_eq!((done["done"].as_u64(), done["failed"].as_u64()), (Some(100), Some(0)), "{done}");
    let n = env.host.read("main", |cv| cv.nodes().iter().filter(|x| x.kind == falos_doc::NodeKind::Image).count()).unwrap();
    assert_eq!(n, 100);
    // Distinct outputs got distinct assets.
    let assets: std::collections::HashSet<String> = env
        .host
        .read("main", |cv| cv.nodes().into_iter().filter_map(|x| x.props.get("asset").and_then(|a| a.as_str()).map(String::from)).collect())
        .unwrap();
    assert_eq!(assets.len(), 100);
}

#[tokio::test]
async fn external_agents_drive_in_app_chats_and_drivers() {
    let env = start("http://127.0.0.1:1", None).await;
    let mut rt = runtime_client(env.port, &env.host.chat.runtime_token).await;
    assert_eq!(next_json(&mut rt).await["type"], "hello");
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.list").await;
    let c = client_as(env.port, "claude-code", None).await;
    let sid = call(&c, "session", json!({"agentName": "Claude Code", "canvas": "main"})).await["sessionId"].as_str().unwrap().to_string();

    // chat_new: a new in-app chat, the message authored by the external client.
    let n = call(&c, "chat_new", json!({"sessionId": sid, "text": "make a moodboard", "driver": "fal"})).await;
    let chat = n["chat"].as_str().unwrap().to_string();
    let user = next_event(&mut ui, &chat, "user").await;
    assert_eq!((user["author"].as_str(), user["text"].as_str()), (Some("ext:claude"), Some("make a moodboard")));
    let run = next_of(&mut rt, "run").await;
    assert_eq!((run["chat"].as_str(), run["driver"].as_str()), (Some(chat.as_str()), Some("fal")));
    assert_eq!(run["previous_driver"], Json::Null);
    let rows = call(&c, "chat_list", json!({"sessionId": sid})).await;
    let r = rows.as_array().unwrap().iter().find(|r| r["chat"] == chat).unwrap().clone();
    assert_eq!(r["driver"], "fal");
    assert!(rows.as_array().unwrap().iter().all(|r| !r["chat"].as_str().unwrap().starts_with("ext:")));

    send(&mut rt, json!({"type": "event", "chat": chat, "event": {"kind": "text", "delta": "Board is up."}})).await;
    // While running, chat_send queues.
    let s = call(&c, "chat_send", json!({"sessionId": sid, "chat": chat, "text": "more neon"})).await;
    assert_eq!(s["queued"], true);
    next_event(&mut ui, &chat, "queued").await;
    let rd = call(&c, "chat_read", json!({"sessionId": sid, "chat": chat})).await;
    assert!(rd["transcript"].as_str().unwrap().contains("Board is up."), "{rd}");
    call(&c, "chat_stop", json!({"sessionId": sid, "chat": chat})).await;
    assert_eq!(next_of(&mut rt, "stop").await["chat"], chat);
    send(&mut rt, json!({"type": "event", "chat": chat, "event": {"kind": "status", "state": "idle", "error": "stopped"}})).await;
    next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == chat && v["event"]["kind"] == "turn" && v["event"]["state"] == "end").await;

    // chat.continue resumes without a new user message; switching the driver is reported.
    send(&mut ui, json!({"type": "chat.continue", "chat": chat})).await;
    let run = next_of(&mut rt, "run").await;
    assert_eq!((run["continue"].as_bool(), run["driver"].as_str()), (Some(true), Some("fal")));
    send(&mut rt, json!({"type": "event", "chat": chat, "event": {"kind": "status", "state": "idle"}})).await;
    next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == chat && v["event"]["kind"] == "turn" && v["event"]["state"] == "end").await;
    send(&mut ui, json!({"type": "chat.send", "chat": chat, "text": "now polish it", "driver": "opus"})).await;
    let run = next_of(&mut rt, "run").await;
    assert_eq!((run["driver"].as_str(), run["previous_driver"].as_str()), (Some("opus"), Some("fal")));
    assert!(run["history"].as_array().unwrap().iter().any(|h| h["text"] == "make a moodboard"));
    send(&mut ui, json!({"type": "chat.send", "chat": chat, "text": "x", "driver": "gpt"})).await;
    assert!(next_of(&mut ui, "error").await["message"].as_str().unwrap().contains("unknown driver"));
}

#[tokio::test]
async fn stdio_relay_reaches_the_host() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let env = start("http://127.0.0.1:1", None).await;
    let home = env.dir.path().join("falos-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("host.json"),
        json!({"port": env.port, "url": server::mcp_url(env.port), "token": TOKEN, "pid": 1, "project": "x"}).to_string(),
    )
    .unwrap();
    let mut events = env.host.subscribe();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_falos-host"))
        .arg("mcp")
        .env("FALOS_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = |v: Json| format!("{v}\n");
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "gemini-cli", "version": "1"}}})).as_bytes()).await.unwrap();
    let init: Json = serde_json::from_str(&tokio::time::timeout(Duration::from_secs(10), out.next_line()).await.unwrap().unwrap().unwrap()).unwrap();
    assert_eq!(init["id"], 1);
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    stdin.write_all(line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes()).await.unwrap();
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "session", "arguments": {"agentName": "Gemini"}}})).as_bytes()).await.unwrap();
    let res: Json = serde_json::from_str(&tokio::time::timeout(Duration::from_secs(10), out.next_line()).await.unwrap().unwrap().unwrap()).unwrap();
    assert_eq!(res["id"], 2);
    // First block: the `{project, canvas, version}` header; then the result.
    let header: Json = serde_json::from_str(res["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!((header["project"].as_str(), header["canvas"].as_str()), (Some("proj"), Some("main")), "{res}");
    assert!(header["version"].is_string(), "{res}");
    assert!(res["result"]["content"][1]["text"].as_str().unwrap().contains("ext:gemini-1"), "{res}");
    let conn = loop {
        let e = tokio::time::timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap();
        if let falos_host::Event::AgentConnected { client, transport, .. } = e {
            break (client, transport);
        }
    };
    assert_eq!(conn, ("gemini".to_string(), "stdio".to_string()));
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

/// fal OS not running: the relay answers `initialize` and `tools/list` from the manifest the
/// host wrote on its last start, and a tool call says to ask the user to open fal OS.
#[tokio::test]
async fn stdio_relay_lists_tools_while_fal_os_is_down() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let env = start("http://127.0.0.1:1", None).await;
    let home = env.dir.path().join("falos-home");
    std::fs::create_dir_all(&home).unwrap();
    let ws = falos_host::workspace::Workspace::single(env.host.clone());
    falos_host::mcp::write_manifest(&ws, &home.join("mcp-manifest.json")).unwrap();
    // host.json points at a port nobody listens on (the app quit or crashed).
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    std::fs::write(
        home.join("host.json"),
        json!({"port": dead, "url": server::mcp_url(dead), "token": TOKEN, "pid": 1, "project": "x"}).to_string(),
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_falos-host"))
        .arg("mcp")
        .env("FALOS_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = |v: Json| format!("{v}\n");
    let mut next = async || -> Json {
        serde_json::from_str(&tokio::time::timeout(Duration::from_secs(15), out.next_line()).await.unwrap().unwrap().unwrap()).unwrap()
    };
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "1"}}})).as_bytes()).await.unwrap();
    let init = next().await;
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
    assert!(init["result"]["capabilities"]["tools"].is_object(), "{init}");
    stdin.write_all(line(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes()).await.unwrap();
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).as_bytes()).await.unwrap();
    let list = next().await;
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"session") && names.contains(&"edit") && names.len() > 20, "{names:?}");
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "session", "arguments": {"agentName": "Claude"}}})).as_bytes()).await.unwrap();
    let call = next().await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("isn't running") && text.contains("tell the user to open it") && text.contains("don't try to launch it"), "{text}");

    // fal OS starts (host.json now points at the live host): the next call goes through.
    std::fs::write(
        home.join("host.json"),
        json!({"port": env.port, "url": server::mcp_url(env.port), "token": TOKEN, "pid": 1, "project": "x"}).to_string(),
    )
    .unwrap();
    stdin.write_all(line(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "session", "arguments": {"agentName": "Claude"}}})).as_bytes()).await.unwrap();
    let live = next().await;
    assert_eq!(live["result"]["isError"], false, "{live}");
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

/// A canvas that failed to load: the project still serves, writes are refused with "retry
/// shortly", reads say what's wrong, and every result starts with the header line.
#[tokio::test]
async fn writes_to_an_unloaded_canvas_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let proj = dir.path().join("proj");
    {
        let h = Host::open(&proj, HostOptions { fal_session: false, home: Some(dir.path().join("home")), ..Default::default() }).unwrap();
        h.flush().unwrap();
    }
    std::fs::write(proj.join("canvases/main.loro"), b"garbage").unwrap();
    let host = Host::open(&proj, HostOptions { fal_session: false, home: Some(dir.path().join("home")), ..Default::default() }).unwrap();
    let running = server::serve(host.clone(), 0, TOKEN).await.unwrap();
    let c = client_as(running.addr.port(), "claude-code", None).await;
    let raw = |tool: &'static str, args: Json| {
        let c = &c;
        async move { c.call_tool(CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone())).await.unwrap() }
    };
    let s = raw("session", json!({"agentName": "Claude"})).await;
    let sid_text = s.content[1].as_text().unwrap().text.clone();
    let sid = serde_json::from_str::<Json>(&sid_text).map(|v| v["sessionId"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
    let e = raw("edit", json!({"sessionId": sid, "canvas": "main", "text": "note \"x\" text(\"y\")"})).await;
    assert_eq!(e.is_error, Some(true));
    let header = e.content[0].as_text().unwrap().text.clone();
    assert!(header.starts_with("{\"project\":\"proj\",\"canvas\":\"main\""), "{header}");
    let msg = e.content[1].as_text().unwrap().text.clone();
    assert!(msg.contains("isn't loaded") && msg.contains("retry shortly"), "{msg}");
}
