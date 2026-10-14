//! Workspace of projects (`serve --workspace`): the `/workspace` socket and `project.*` ops,
//! per-project sync / assets / MCP sessions / chats, runtime frames tagged with `project`,
//! the fal Agent project link, and steering (`chat.steer`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use falos_host::context::NoRenderer;
use falos_host::workspace::Workspace;
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

fn opts(home: PathBuf) -> impl Fn() -> HostOptions + Send + Sync + 'static {
    move || HostOptions {
        fal_queue_url: "http://127.0.0.1:1".into(),
        fal_key: None,
        fal_session: false,
        renderer: Box::new(NoRenderer),
        home: Some(home.clone()),
    }
}

struct Env {
    dir: tempfile::TempDir,
    ws: Arc<Workspace>,
    port: u16,
    _running: server::Running,
}

impl Env {
    fn root(&self) -> PathBuf {
        self.dir.path().join("projects")
    }
}

/// A workspace with an existing project folder `demo` (made the old way, by a single-project
/// host) plus whatever the test creates.
async fn start() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    {
        let h = Host::open(&root.join("demo"), opts(dir.path().join("home"))()).unwrap();
        let who = falos_doc::Actor { id: "human".into(), name: "Human".into(), kind: falos_doc::ActorKind::Human };
        h.mutate("main", "human", None, |c| c.apply_edit("note \"old\" p(0,0) s(100,100)", &who, None).map(|_| ()))
            .unwrap();
        h.flush().unwrap();
    }
    let ws = Workspace::open(&root, opts(dir.path().join("home"))).unwrap();
    let running = server::serve_workspace(ws.clone(), 0, TOKEN).await.unwrap();
    Env { port: running.addr.port(), dir, ws, _running: running }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(port: u16, path: &str) -> Ws {
    let sep = if path.contains('?') { '&' } else { '?' };
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}{path}{sep}token={TOKEN}&actor=lukas&name=Lukas"))
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

async fn next_event(ws: &mut Ws, chat: &str, kind: &str) -> Json {
    next_where(ws, |v| v["type"] == "chat.event" && v["chat"] == chat && v["event"]["kind"] == kind).await["event"].clone()
}

async fn send(ws: &mut Ws, v: Json) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Next `projects` frame satisfying `pred`.
async fn projects_until(ws: &mut Ws, pred: impl Fn(&[Json]) -> bool) -> Vec<Json> {
    loop {
        let f = next_of(ws, "projects").await;
        let p = f["projects"].as_array().unwrap().clone();
        if pred(&p) {
            return p;
        }
    }
}

fn project<'a>(ps: &'a [Json], id: &str) -> Option<&'a Json> {
    ps.iter().find(|p| p["id"] == id)
}

type Client = RunningService<RoleClient, ClientConfig>;

async fn client(port: u16, headers: &[(&str, &str)]) -> Client {
    let mut cfg = StreamableHttpClientTransportConfig::with_uri(server::mcp_url(port)).auth_header(TOKEN);
    for (k, v) in headers {
        cfg.custom_headers.insert(k.parse().unwrap(), v.parse().unwrap());
    }
    ClientConfig::new(ClientCapabilities::default(), Implementation::new("claude-code", "1.0"))
        .serve(StreamableHttpClientTransport::from_config(cfg))
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

fn project_json(dir: &Path) -> Json {
    serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn workspace_projects_lifecycle() {
    let env = start().await;
    let mut w = connect(env.port, "/workspace").await;
    let first = next_of(&mut w, "projects").await;
    let demo = project(first["projects"].as_array().unwrap(), "demo").unwrap().clone();
    assert_eq!((demo["name"].as_str(), demo["running"].as_u64()), (Some("demo"), Some(0)));
    assert!(demo["fal_project"].is_null() && demo["updated"].as_i64().unwrap() > 0);

    // create → project.created (to the sender) and a new list; it is the most recent.
    send(&mut w, json!({"type": "project.create", "name": "Launch Film!"})).await;
    let created = next_of(&mut w, "project.created").await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(id, "launch-film");
    let ps = projects_until(&mut w, |p| project(p, &id).is_some()).await;
    assert_eq!(ps[0]["id"], id, "newest first: {ps:?}");
    assert!(env.root().join(&id).join("canvases/main.loro").exists());
    assert_eq!(project_json(&env.root().join(&id))["name"], "Launch Film!");

    // Same name again → a free id.
    send(&mut w, json!({"type": "project.create", "name": "Launch Film!"})).await;
    let id2 = next_of(&mut w, "project.created").await["id"].as_str().unwrap().to_string();
    assert_eq!(id2, "launch-film-2");

    // rename keeps the id; touch makes demo the most recent again.
    send(&mut w, json!({"type": "project.rename", "id": id, "name": "Launch film v2"})).await;
    projects_until(&mut w, |p| project(p, &id).is_some_and(|r| r["name"] == "Launch film v2")).await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    send(&mut w, json!({"type": "project.touch", "id": "demo"})).await;
    projects_until(&mut w, |p| p[0]["id"] == "demo").await;

    // duplicate copies canvas + assets (APFS clones, never hard links), not chats; the copy has no fal project.
    let demo_host = env.ws.get("demo").unwrap();
    demo_host.set_fal_project(Some("fp_demo")).unwrap();
    std::fs::create_dir_all(demo_host.dir().join("assets")).unwrap();
    let hex = "ab".repeat(32);
    std::fs::write(demo_host.dir().join(format!("assets/{hex}.png")), b"png").unwrap();
    std::fs::create_dir_all(demo_host.dir().join("chats")).unwrap();
    std::fs::write(demo_host.dir().join("chats/c_x.jsonl"), "").unwrap();
    send(&mut w, json!({"type": "project.duplicate", "id": "demo"})).await;
    let dup = next_of(&mut w, "project.created").await;
    let dup_id = dup["id"].as_str().unwrap().to_string();
    assert_eq!((dup_id.as_str(), dup["from"].as_str()), ("demo-copy", Some("demo")));
    let ps = projects_until(&mut w, |p| project(p, &dup_id).is_some()).await;
    let d = project(&ps, &dup_id).unwrap();
    assert_eq!(d["name"], "demo copy");
    assert!(d["fal_project"].is_null());
    assert_eq!(d["thumb"], format!("/assets/{hex}?project={dup_id}"));
    assert_eq!(project(&ps, "demo").unwrap()["fal_project"], "fp_demo");
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(demo_host.dir().join(format!("assets/{hex}.png"))).unwrap();
        let b = std::fs::metadata(env.ws.get(&dup_id).unwrap().dir().join(format!("assets/{hex}.png"))).unwrap();
        assert_ne!(a.ino(), b.ino(), "duplicate must not hardlink assets");
        assert_eq!(b.nlink(), 1);
    }
    let copy = env.root().join(&dup_id);
    assert!(copy.join(format!("assets/{hex}.png")).exists() && !copy.join("chats").exists());
    let dh = env.ws.get(&dup_id).unwrap();
    assert!(dh.read("main", |c| c.nodes().iter().any(|n| n.name == "old")).unwrap());

    // delete moves the folder to .trash (and disconnects its sync clients).
    let mut sync = connect(env.port, &format!("/sync/{id2}")).await;
    assert_eq!(next_of(&mut sync, "hello").await["canvas"], "main");
    send(&mut w, json!({"type": "project.delete", "id": id2})).await;
    projects_until(&mut w, |p| project(p, &id2).is_none()).await;
    assert!(!env.root().join(&id2).exists());
    let trashed: Vec<String> = std::fs::read_dir(env.root().join(".trash"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(trashed.iter().any(|t| t.starts_with(&format!("{id2}-"))), "{trashed:?}");
    let err = next_of(&mut sync, "error").await;
    assert!(err["message"].as_str().unwrap().contains("deleted"));
    assert!(env.ws.ids().iter().all(|i| i != &id2));

    // Errors come back to the sender.
    send(&mut w, json!({"type": "project.rename", "id": "nope", "name": "x"})).await;
    assert!(next_of(&mut w, "error").await["message"].as_str().unwrap().contains("unknown project"));
    send(&mut w, json!({"type": "project.create", "name": "  "})).await;
    assert!(next_of(&mut w, "error").await["message"].as_str().unwrap().contains("name"));
}

#[tokio::test]
async fn projects_are_isolated_sync_assets_and_mcp() {
    let env = start().await;
    let b = env.ws.create("Beta").unwrap();

    // Back-compat: the old project loads unchanged, via /sync/<project> and /sync/<canvas>.
    let mut a_ui = connect(env.port, "/sync/demo").await;
    let hello = next_of(&mut a_ui, "hello").await;
    assert_eq!(hello["canvas"], "main");
    let mut b_ui = connect(env.port, &format!("/sync/{b}")).await;
    next_of(&mut b_ui, "hello").await;
    assert!(env.ws.get("demo").unwrap().read("main", |c| c.nodes().iter().any(|n| n.name == "old")).unwrap());

    // MCP: session(project) picks the project; tools follow the session.
    let c = client(env.port, &[]).await;
    let sa = call(&c, "session", json!({"agentName": "Claude Code", "project": "demo", "canvas": "main"})).await;
    assert_eq!(sa["project"]["id"], "demo");
    let sb = call(&c, "session", json!({"agentName": "Claude Code", "project": "Beta", "canvas": "main"})).await;
    assert_eq!(sb["project"]["id"], b);
    let (sa, sb) = (sa["sessionId"].as_str().unwrap().to_string(), sb["sessionId"].as_str().unwrap().to_string());
    call(&c, "edit", json!({"sessionId": sb, "canvas": "main", "text": "note \"only in beta\" p(0,0) s(100,100)"})).await;
    let read_a = call(&c, "read", json!({"sessionId": sa, "canvas": "main"})).await;
    let read_b = call(&c, "read", json!({"sessionId": sb, "canvas": "main"})).await;
    let (ta, tb) = (read_a.to_string(), read_b.to_string());
    assert!(!ta.contains("only in beta") && ta.contains("old"), "{ta}");
    assert!(tb.contains("only in beta") && !tb.contains("old"), "{tb}");
    // Doc events go only to that project's clients.
    next_where(&mut b_ui, |v| v["type"] == "doc").await;
    // External agents get their pseudo-chat in the project they act on.
    let bl = env.ws.get(&b).unwrap().chat.list(&env.ws.get(&b).unwrap(), None);
    assert!(bl.as_array().unwrap().iter().any(|r| r["external"] == true));

    // list_projects / create_project; a connection's next session binds to the project it just
    // created (ahead of the one open in fal OS); an explicit project always wins.
    let ps = call(&c, "list_projects", json!({})).await;
    assert!(ps.as_array().unwrap().iter().any(|p| p["id"] == "demo"));
    let np = call(&c, "create_project", json!({"name": "Gamma"})).await;
    assert_eq!(np["id"], "gamma");
    let s = call(&c, "session", json!({"agentName": "Cursor"})).await;
    assert_eq!(s["project"]["id"], "gamma");
    let s = call(&c, "session", json!({"agentName": "Cursor", "project": "demo"})).await;
    assert_eq!(s["project"]["id"], "demo");
    // X-Falos-Project (the runtime's agents) picks the project too.
    let rc = client(env.port, &[("x-falos-project", &b)]).await;
    assert_eq!(call(&rc, "session", json!({"agentName": "Director"})).await["project"]["id"], b);
    let r = c
        .call_tool(CallToolRequestParams::new("session").with_arguments(json!({"agentName": "x", "project": "nope"}).as_object().unwrap().clone()))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));

    // Assets: ?project= picks the project; without it the others are searched too.
    let http = reqwest::Client::new();
    let body = b"hello beta".to_vec();
    let hex = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let r = http
        .put(format!("http://127.0.0.1:{}/assets/{hex}?project={b}", env.port))
        .bearer_auth(TOKEN)
        .header("content-type", "text/plain")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    assert!(env.root().join(&b).join(format!("assets/{hex}.txt")).exists());
    assert!(!env.root().join("demo").join(format!("assets/{hex}.txt")).exists());
    for q in [format!("?project={b}&token={TOKEN}"), format!("?token={TOKEN}")] {
        let r = http.get(format!("http://127.0.0.1:{}/assets/{hex}{q}", env.port)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{q}");
    }
    let r = http
        .get(format!("http://127.0.0.1:{}/assets/{hex}?project=demo&token={TOKEN}", env.port))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/sync/nope?token={TOKEN}", env.port)).await;
    assert!(r.is_err(), "unknown project");
}

#[tokio::test]
async fn runtime_frames_carry_project_and_fal_project_link() {
    let env = start().await;
    let b = env.ws.create("Beta").unwrap();
    let mut rt = runtime_client(env.port, &env.ws.link.token).await;
    let hello = next_of(&mut rt, "hello").await;
    assert!(hello["projects"].as_array().unwrap().contains(&json!("demo")));
    let mut a_ui = connect(env.port, "/sync/demo").await;
    let mut b_ui = connect(env.port, &format!("/sync/{b}")).await;
    let mut w = connect(env.port, "/workspace").await;
    next_of(&mut w, "projects").await;

    send(&mut b_ui, json!({"type": "chat.send", "text": "hi beta", "driver": "fal"})).await;
    let user = next_where(&mut b_ui, |v| v["type"] == "chat.event" && v["event"]["kind"] == "user").await;
    let chat = user["chat"].as_str().unwrap().to_string();
    let run = next_of(&mut rt, "run").await;
    assert_eq!((run["project"].as_str(), run["chat"].as_str()), (Some(b.as_str()), Some(chat.as_str())));
    assert_eq!(run["project_name"], "Beta");
    assert!(run["project_dir"].as_str().unwrap().ends_with(&format!("/{b}")));
    assert!(run["fal_project"].is_null());

    // Runtime events route by project: the running count shows in the list.
    send(&mut rt, json!({"type": "event", "project": b, "chat": chat, "event": {"kind": "status", "state": "running"}})).await;
    next_event(&mut b_ui, &chat, "status").await;
    projects_until(&mut w, |p| project(p, &b).is_some_and(|r| r["running"] == 1)).await;
    // The runtime links the fal Agent project.
    send(&mut rt, json!({"type": "project.fal", "project": b, "fal_project": "fp_123"})).await;
    projects_until(&mut w, |p| project(p, &b).is_some_and(|r| r["fal_project"] == "fp_123")).await;
    assert_eq!(project_json(&env.root().join(&b))["fal_project"], "fp_123");
    send(&mut rt, json!({"type": "event", "project": b, "chat": chat, "event": {"kind": "status", "state": "idle"}})).await;
    projects_until(&mut w, |p| project(p, &b).is_some_and(|r| r["running"] == 0)).await;

    // Same chat id in another project stays separate.
    send(&mut rt, json!({"type": "event", "project": "demo", "chat": chat, "event": {"kind": "text", "delta": "demo side"}})).await;
    let e = next_event(&mut a_ui, &chat, "text").await;
    assert_eq!(e["delta"], "demo side");
    let demo = env.ws.get("demo").unwrap();
    let beta = env.ws.get(&b).unwrap();
    assert!(std::fs::read_to_string(demo.dir().join(format!("chats/{chat}.jsonl"))).unwrap().contains("demo side"));
    assert!(!std::fs::read_to_string(beta.dir().join(format!("chats/{chat}.jsonl"))).unwrap().contains("demo side"));

    // The next run of that project carries the link.
    send(&mut b_ui, json!({"type": "chat.send", "chat": chat, "text": "again"})).await;
    let run = next_of(&mut rt, "run").await;
    assert_eq!(run["fal_project"], "fp_123");

    // Cross-chat requests are answered per project.
    send(&mut rt, json!({"type": "request", "id": 5, "project": b, "op": "chat.list"})).await;
    let reply = next_of(&mut rt, "reply").await;
    assert_eq!(reply["ok"], true);
    assert!(reply["result"].as_array().unwrap().iter().any(|r| r["chat"] == chat));
}

#[tokio::test]
async fn steering_interrupts_with_queued_or_new_messages() {
    let env = start().await;
    let mut rt = runtime_client(env.port, &env.ws.link.token).await;
    next_of(&mut rt, "hello").await;
    let mut ui = connect(env.port, "/sync/demo").await;

    // Not running: a steer is an ordinary message (starts a run).
    send(&mut ui, json!({"type": "chat.send", "text": "make an ad"})).await;
    let chat = next_where(&mut ui, |v| v["type"] == "chat.event" && v["event"]["kind"] == "user").await["chat"]
        .as_str()
        .unwrap()
        .to_string();
    next_of(&mut rt, "run").await;
    send(&mut rt, json!({"type": "event", "project": "demo", "chat": chat, "event": {"kind": "status", "state": "running"}})).await;
    next_event(&mut ui, &chat, "status").await;

    // A queued message, then steer it: dequeued(steered) + user(steer) + a steer frame.
    send(&mut ui, json!({"type": "chat.send", "chat": chat, "text": "make it darker"})).await;
    let q = next_event(&mut ui, &chat, "queued").await;
    let qid = q["id"].as_str().unwrap().to_string();
    assert_eq!(next_of(&mut rt, "queue").await["id"], qid);
    send(&mut ui, json!({"type": "chat.steer", "chat": chat, "id": qid})).await;
    let d = next_event(&mut ui, &chat, "dequeued").await;
    assert_eq!((d["id"].as_str(), d["reason"].as_str()), (Some(qid.as_str()), Some("steer")));
    let u = next_event(&mut ui, &chat, "user").await;
    assert_eq!((u["text"].as_str(), u["steer"].as_bool(), u["queued"].as_str()), (Some("make it darker"), Some(true), Some(qid.as_str())));
    let s = next_of(&mut rt, "steer").await;
    assert_eq!((s["chat"].as_str(), s["id"].as_str(), s["text"].as_str(), s["project"].as_str()), (Some(chat.as_str()), Some(qid.as_str()), Some("make it darker"), Some("demo")));
    assert!(s["agent"].is_null());
    let list = env.ws.get("demo").unwrap().chat.list(&env.ws.get("demo").unwrap(), None);
    assert!(list.as_array().unwrap().iter().find(|r| r["chat"] == chat).unwrap()["queued"].as_array().unwrap().is_empty());

    // New text, for a subagent.
    send(&mut ui, json!({"type": "chat.steer", "chat": chat, "text": "use the red logo", "agent": "motion"})).await;
    let u = next_event(&mut ui, &chat, "user").await;
    assert_eq!((u["agent"].as_str(), u["steer"].as_bool()), (Some("motion"), Some(true)));
    let s = next_of(&mut rt, "steer").await;
    assert_eq!((s["agent"].as_str(), s["text"].as_str()), (Some("motion"), Some("use the red logo")));
    let sid = s["id"].as_str().unwrap().to_string();

    // Errors: unknown queued id, nothing to send.
    send(&mut ui, json!({"type": "chat.steer", "chat": chat, "id": "q_nope"})).await;
    assert!(next_of(&mut ui, "error").await["message"].as_str().unwrap().contains("not queued"));
    send(&mut ui, json!({"type": "chat.steer", "chat": chat})).await;
    assert!(next_of(&mut ui, "error").await["message"].as_str().unwrap().contains("needs text"));

    // The run ended before the runtime got it: steer.failed starts a run with it (no second
    // user event).
    send(&mut rt, json!({"type": "event", "project": "demo", "chat": chat, "event": {"kind": "status", "state": "idle"}})).await;
    next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == chat && v["event"]["kind"] == "turn" && v["event"]["state"] == "end").await;
    send(&mut rt, json!({"type": "steer.failed", "project": "demo", "chat": chat, "id": sid, "text": "use the red logo", "author": "Lukas"})).await;
    let run = next_of(&mut rt, "run").await;
    assert_eq!((run["text"].as_str(), run["chat"].as_str()), (Some("use the red logo"), Some(chat.as_str())));
    let ev = next_where(&mut ui, |v| v["type"] == "chat.event" && v["chat"] == chat && matches!(v["event"]["kind"].as_str(), Some("turn" | "user"))).await;
    assert_eq!(ev["event"]["kind"], "turn", "no duplicate user event: {ev}");
}
