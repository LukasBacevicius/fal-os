//! Chat routing (sync client ↔ host ↔ runtime), history persistence, and the swarm's
//! canvas tools `import_file` / `get_context`.

use std::sync::Arc;
use std::time::Duration;

use falos_host::context::NoRenderer;
use falos_host::{Host, HostOptions, server};
use futures::{SinkExt, StreamExt};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value as Json, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const TOKEN: &str = "test-token-0123456789abcdef0123456789abcdef";

/// A valid 1x1 RGBA PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

struct Env {
    dir: tempfile::TempDir,
    host: Arc<Host>,
    port: u16,
    _running: server::Running,
}

async fn start() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(
        &dir.path().join("proj"),
        HostOptions {
            fal_queue_url: "http://127.0.0.1:1".into(),
            fal_key: None,
            fal_session: false,
            renderer: Box::new(NoRenderer),
        },
    )
    .unwrap();
    let running = server::serve(host.clone(), 0, TOKEN).await.unwrap();
    Env {
        dir,
        host,
        port: running.addr.port(),
        _running: running,
    }
}

type Client = RunningService<RoleClient, ()>;

async fn client(port: u16) -> Client {
    let t = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(server::mcp_url(port)).auth_header(TOKEN),
    );
    ().serve(t).await.unwrap()
}

fn text(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn call(c: &Client, tool: &'static str, args: Json) -> Json {
    let r = c
        .call_tool(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let t = text(&r);
    assert_ne!(r.is_error, Some(true), "{tool} failed: {t}");
    let end = t.find("\n\nnotices").unwrap_or(t.len());
    serde_json::from_str(&t[..end]).unwrap_or(Json::String(t))
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Json {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("ws timeout")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Next text frame of `ty`, skipping others (presence, doc, …).
async fn next_of(ws: &mut Ws, ty: &str) -> Json {
    loop {
        let v = next_json(ws).await;
        if v["type"] == ty {
            return v;
        }
    }
}

async fn sync_client(port: u16) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{port}/sync/main?token={TOKEN}&actor=lukas&name=Lukas"
    ))
    .await
    .unwrap();
    ws
}

async fn runtime_client(
    port: u16,
    runtime_token: &str,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://127.0.0.1:{port}/runtime")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    req.headers_mut()
        .insert("x-falos-runtime", runtime_token.parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

#[tokio::test]
async fn import_file_and_get_context() {
    let env = start().await;
    let c = client(env.port).await;
    let tools = c.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(
        names.contains(&"import_file") && names.contains(&"get_context") && names.contains(&"edit")
    );

    let s = call(&c, "session", json!({"agentName": "Motion"})).await;
    let sid = s["sessionId"].as_str().unwrap().to_string();
    let png = env.dir.path().join("shot.png");
    std::fs::write(&png, PNG).unwrap();

    // Relative paths and non-media files are refused.
    let r = c
        .call_tool(
            CallToolRequestParams::new("import_file").with_arguments(
                json!({"sessionId": sid, "canvas": "main", "path": "shot.png"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    let txt = env.dir.path().join("notes.txt");
    std::fs::write(&txt, "x").unwrap();
    let r = c
        .call_tool(
            CallToolRequestParams::new("import_file").with_arguments(
                json!({"sessionId": sid, "canvas": "main", "path": txt})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true), "{}", text(&r));

    let i = call(
        &c,
        "import_file",
        json!({"sessionId": sid, "canvas": "main", "path": png, "name": "hero",
               "placement": {"x": 10, "y": 20, "w": 200},
               "lineage": {"model": "fal-agent", "falRequestId": "resp_1", "cost": 0.03,
                           "params": {"task": "hero shot"}, "uri": "https://cdn/x.png"}}),
    )
    .await;
    let node = i["node"].as_str().unwrap().to_string();
    assert_eq!(i["type"], "image");
    assert_eq!(i["size"], json!([200.0, 200.0]));
    let file = i["file"].as_str().unwrap();
    assert!(file.starts_with(env.host.dir().join("assets").to_str().unwrap()));
    assert_eq!(std::fs::read(file).unwrap(), PNG);
    let (n, asset) = env
        .host
        .read("main", |cv| {
            let n = cv.node(&node).unwrap();
            let a = cv.asset(n.props["asset"].as_str().unwrap()).unwrap();
            (n, a)
        })
        .unwrap();
    assert_eq!(
        (n.name.as_str(), n.x, n.y, n.created_by.as_str()),
        ("hero", 10.0, 20.0, "motion")
    );
    let l = asset.lineage.unwrap();
    assert_eq!(
        (l.model.as_str(), l.fal_request_id.as_deref(), l.cost),
        ("fal-agent", Some("resp_1"), Some(0.03))
    );
    assert_eq!(l.params["task"], "hero shot");
    assert_eq!(asset.uri.as_deref(), Some("https://cdn/x.png"));

    // A generation placeholder converts in place: same id, its spot and width.
    let e = call(
        &c,
        "edit",
        json!({"sessionId": sid, "canvas": "main",
               "text": "generation \"clip 1\" p(500,40) s(300,120) status(running) model(fal-agent)"}),
    )
    .await;
    let ph = e["created"][0].as_str().unwrap().to_string();
    let i2 = call(
        &c,
        "import_file",
        json!({"sessionId": sid, "canvas": "main", "path": png, "replace": ph}),
    )
    .await;
    assert_eq!(i2["node"], ph);
    let n2 = env.host.read("main", |cv| cv.node(&ph).unwrap()).unwrap();
    assert_eq!(
        (
            n2.kind.to_string(),
            n2.name.as_str(),
            n2.x,
            n2.y,
            n2.w,
            n2.h
        ),
        ("image".into(), "clip 1", 500.0, 40.0, 300.0, 300.0)
    );
    // Replacing something that isn't a placeholder fails.
    let r = c
        .call_tool(
            CallToolRequestParams::new("import_file").with_arguments(
                json!({"sessionId": sid, "canvas": "main", "path": png, "replace": node})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));

    // A fal storage upload is cached on the asset (https only).
    let asset_id = i["asset"].as_str().unwrap().to_string();
    let bad = c
        .call_tool(
            CallToolRequestParams::new("set_asset_uri").with_arguments(
                json!({"sessionId": sid, "canvas": "main", "asset": asset_id, "uri": "file:///etc/passwd"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(bad.is_error, Some(true));
    // An existing uri is kept (asset records are first-write-wins).
    call(
        &c,
        "set_asset_uri",
        json!({"sessionId": sid, "canvas": "main", "asset": asset_id, "uri": "https://v3.fal.media/files/x.png"}),
    )
    .await;
    let rec = env
        .host
        .read("main", |cv| cv.asset(&asset_id).unwrap())
        .unwrap();
    assert_eq!(rec.uri.as_deref(), Some("https://cdn/x.png"));
    // A user upload (file in assets/, node with `asset`, no record yet) resolves to its
    // file, and its fal storage URL is recorded once.
    let mut user_png = PNG.to_vec();
    user_png.extend_from_slice(b"user");
    let hex: String = {
        use sha2::Digest;
        sha2::Sha256::digest(&user_png)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    std::fs::write(env.host.dir().join(format!("assets/{hex}.png")), &user_png).unwrap();
    let e = call(
        &c,
        "edit",
        json!({"sessionId": sid, "canvas": "main",
               "text": format!("image \"my headphones\" p(0,600) s(400,400) asset(sha256:{hex})")}),
    )
    .await;
    let user_node = e["created"][0].as_str().unwrap().to_string();
    let ctx = call(
        &c,
        "get_context",
        json!({"sessionId": sid, "canvas": "main", "chips": [{"kind": "node", "node": user_node}]}),
    )
    .await;
    assert!(
        ctx[0]["file"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{hex}.png"))
    );
    assert_eq!(ctx[0]["mime"], "image/png");
    assert!(ctx[0]["uri"].is_null());
    call(
        &c,
        "set_asset_uri",
        json!({"sessionId": sid, "canvas": "main", "asset": format!("sha256:{hex}"), "uri": "https://v3.fal.media/files/u.png"}),
    )
    .await;
    let rec = env
        .host
        .read("main", |cv| cv.asset(&format!("sha256:{hex}")).unwrap())
        .unwrap();
    assert_eq!(
        (rec.uri.as_deref(), rec.mime.as_deref()),
        (Some("https://v3.fal.media/files/u.png"), Some("image/png"))
    );

    // get_context resolves rows, lineage, files and pixel crops.
    let ctx = call(
        &c,
        "get_context",
        json!({"sessionId": sid, "canvas": "main", "chips": [
            {"kind": "node", "node": node},
            {"kind": "region", "node": node, "box": [0.0, 0.0, 1.0, 1.0]},
            {"kind": "node", "node": "nope00"},
        ]}),
    )
    .await;
    assert_eq!(ctx[0]["node"], node);
    assert_eq!(ctx[0]["file"], file);
    assert_eq!(ctx[0]["uri"], "https://cdn/x.png");
    assert!(
        ctx[0]["row"]
            .as_str()
            .unwrap()
            .contains("untrusted-canvas-data")
    );
    assert!(ctx[0]["lineage"][0].as_str().unwrap().contains("fal-agent"));
    assert_eq!(
        ctx[1]["crop"],
        json!({"x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0})
    );
    assert!(ctx[1]["hint"].as_str().unwrap().contains("crop="));
    assert!(ctx[2]["error"].as_str().unwrap().contains("unknown node"));
}

#[tokio::test]
async fn chat_routing_and_history() {
    let env = start().await;
    let c = client(env.port).await;
    let s = call(&c, "session", json!({"agentName": "Setup"})).await;
    let sid = s["sessionId"].as_str().unwrap().to_string();
    let png = env.dir.path().join("headphones.png");
    std::fs::write(&png, PNG).unwrap();
    let img = call(
        &c,
        "import_file",
        json!({"sessionId": sid, "canvas": "main", "path": png}),
    )
    .await;
    let img = img["node"].as_str().unwrap().to_string();

    // /runtime needs the runtime token on top of the bearer token.
    assert!(
        runtime_client(env.port, "wrong-token-wrong-token")
            .await
            .is_err()
    );
    let mut rt = runtime_client(env.port, &env.host.chat.runtime_token)
        .await
        .unwrap();
    assert_eq!(next_json(&mut rt).await["type"], "hello");

    let mut ui = sync_client(env.port).await;
    assert_eq!(next_of(&mut ui, "chat.history").await["chats"], json!([]));

    ui.send(Message::Text(
        json!({"type": "chat.send", "text": "OPUS please direct me an ad for the headphones",
               "context": [{"kind": "region", "node": img, "box": [0.0, 0.0, 1.0, 1.0]}],
               "options": {"demo": true}})
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let ev = next_of(&mut ui, "chat.event").await;
    assert_eq!(ev["seq"], 1);
    assert_eq!(ev["event"]["kind"], "user");
    assert_eq!(ev["event"]["author"], "Lukas");
    let chat = ev["chat"].as_str().unwrap().to_string();

    let run = next_json(&mut rt).await;
    assert_eq!(run["type"], "run");
    assert_eq!(run["chat"], chat);
    assert_eq!(run["canvas"], "main");
    assert_eq!(run["options"]["demo"], true);
    assert_eq!(run["history"], json!([]));
    assert_eq!(run["context"][0]["node"], img);
    assert!(
        run["context"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with(".png")
    );
    assert!(run["context"][0]["crop"].is_object());

    for e in [
        json!({"kind": "status", "state": "running"}),
        json!({"kind": "title", "title": "Headphones ad"}),
        json!({"kind": "text", "agent": "director", "delta": "On it."}),
        json!({"kind": "agent", "agent": "generator-1", "role": "fal generator", "parent": "director", "state": "started"}),
    ] {
        rt.send(Message::Text(
            json!({"type": "event", "chat": chat, "event": e})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    for seq in 2..=5 {
        let ev = next_of(&mut ui, "chat.event").await;
        assert_eq!(
            (ev["chat"].as_str(), ev["seq"].as_u64()),
            (Some(chat.as_str()), Some(seq))
        );
    }

    ui.send(Message::Text(
        json!({"type": "chat.stop", "chat": chat})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let stop = next_json(&mut rt).await;
    assert_eq!(
        (stop["type"].as_str(), stop["chat"].as_str()),
        (Some("stop"), Some(chat.as_str()))
    );

    // The runtime dies mid-run: the running chat ends with an error.
    drop(rt);
    let ev = next_of(&mut ui, "chat.event").await;
    assert_eq!(ev["event"]["state"], "error");
    assert_eq!(ev["seq"], 6);

    // Persisted: one event per line, and a new client gets it as history.
    let log = std::fs::read_to_string(env.host.dir().join(format!("chats/{chat}.jsonl"))).unwrap();
    assert_eq!(log.lines().count(), 6);
    let first: Json = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(first["event"]["kind"], "user");
    let mut ui2 = sync_client(env.port).await;
    let h = next_of(&mut ui2, "chat.history").await;
    assert_eq!(h["chats"][0]["chat"], chat);
    assert_eq!(h["chats"][0]["title"], "Headphones ad");
    assert_eq!(h["chats"][0]["events"].as_array().unwrap().len(), 6);
    assert_eq!(h["chats"][0]["events"][3]["delta"], "On it.");

    // A follow-up while no runtime is connected is queued and delivered on connect, with
    // the earlier turns as history.
    ui.send(Message::Text(
        json!({"type": "chat.send", "chat": chat, "text": "make it punchier"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let ev = next_of(&mut ui, "chat.event").await;
    assert_eq!(ev["event"]["text"], "make it punchier");
    let mut rt2 = runtime_client(env.port, &env.host.chat.runtime_token)
        .await
        .unwrap();
    let mut run2 = next_json(&mut rt2).await;
    if run2["type"] == "hello" {
        run2 = next_json(&mut rt2).await;
    }
    assert_eq!(run2["type"], "run");
    assert_eq!(run2["text"], "make it punchier");
    assert_eq!(run2["history"][0]["role"], "user");
    assert_eq!(
        run2["history"][1],
        json!({"role": "assistant", "text": "On it."})
    );

    // Chats on another canvas aren't in this canvas's history; bad chat ids are refused.
    ui.send(Message::Text(
        json!({"type": "chat.send", "chat": "../evil", "text": "x"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    assert_eq!(next_of(&mut ui, "error").await["type"], "error");
}

async fn send(ws: &mut Ws, v: Json) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Next `chat.list` satisfying `pred` (lists are coalesced, so intermediate states may be skipped).
async fn list_until(ws: &mut Ws, pred: impl Fn(&[Json]) -> bool) -> Vec<Json> {
    loop {
        let l = next_of(ws, "chat.list").await;
        let chats = l["chats"].as_array().unwrap().clone();
        if pred(&chats) {
            return chats;
        }
    }
}

thread_local! {
    /// Run frames that arrived while `request` waited for its reply (one test per thread).
    static RUNS: std::cell::RefCell<std::collections::VecDeque<Json>> = Default::default();
}

async fn next_run(rt: &mut Ws) -> Json {
    if let Some(r) = RUNS.with(|q| q.borrow_mut().pop_front()) {
        return r;
    }
    next_of(rt, "run").await
}

async fn request(rt: &mut Ws, id: u64, req: Json) -> Json {
    let mut req = req;
    req["type"] = json!("request");
    req["id"] = json!(id);
    send(rt, req).await;
    loop {
        let r = next_json(rt).await;
        match r["type"].as_str() {
            Some("reply") if r["id"] == id => return r,
            Some("run") => RUNS.with(|q| q.borrow_mut().push_back(r)),
            _ => {}
        }
    }
}

fn row<'a>(chats: &'a [Json], chat: &str) -> Option<&'a Json> {
    chats.iter().find(|c| c["chat"] == chat)
}

#[tokio::test]
async fn multi_chat_and_cross_chat() {
    let env = start().await;
    let mut rt = runtime_client(env.port, &env.host.chat.runtime_token)
        .await
        .unwrap();
    let mut ui = sync_client(env.port).await;
    next_of(&mut ui, "chat.history").await;
    assert_eq!(next_of(&mut ui, "chat.list").await["chats"], json!([]));

    // chat.new → chat.created (to the requester) + chat.list (to everyone); rename.
    send(&mut ui, json!({"type": "chat.new"})).await;
    let a = next_of(&mut ui, "chat.created").await["chat"]
        .as_str()
        .unwrap()
        .to_string();
    let chats = list_until(&mut ui, |c| row(c, &a).is_some()).await;
    assert_eq!(row(&chats, &a).unwrap()["title"], "New chat");
    assert_eq!(row(&chats, &a).unwrap()["state"], "idle");
    send(
        &mut ui,
        json!({"type": "chat.rename", "chat": a, "title": "Logo"}),
    )
    .await;
    list_until(&mut ui, |c| {
        row(c, &a).is_some_and(|r| r["title"] == "Logo")
    })
    .await;

    // Two chats run concurrently: both runs reach the runtime, both show running.
    send(
        &mut ui,
        json!({"type": "chat.send", "chat": a, "text": "make a logo"}),
    )
    .await;
    send(
        &mut ui,
        json!({"type": "chat.send", "text": "OPUS please direct me an ad for the headphones"}),
    )
    .await;
    let r1 = next_run(&mut rt).await;
    let r2 = next_run(&mut rt).await;
    assert_eq!(r1["chat"], a);
    assert_eq!(r1["title"], "Logo"); // a renamed chat keeps its title
    let b = r2["chat"].as_str().unwrap().to_string();
    for (chat, agent) in [(&a, "generator"), (&b, "motion")] {
        for e in [
            json!({"kind": "status", "state": "running"}),
            json!({"kind": "agent", "agent": agent, "state": "working", "status": "busy"}),
        ] {
            send(&mut rt, json!({"type": "event", "chat": chat, "event": e})).await;
        }
    }
    let chats = list_until(&mut ui, |c| {
        c.iter()
            .filter(|r| {
                r["state"] == "running" && r["agents"].as_array().is_some_and(|a| a.len() == 1)
            })
            .count()
            == 2
    })
    .await;
    assert_eq!(
        row(&chats, &b).unwrap()["title"],
        "OPUS please direct me an ad for the headphones"
    );
    assert_eq!(
        row(&chats, &b).unwrap()["agents"][0],
        json!({"agent": "motion", "status": "busy"})
    );

    // Cost: per-turn values from the runtime become cumulative per chat.
    send(
        &mut rt,
        json!({"type": "event", "chat": a, "event": {"kind": "cost", "usd": 0.5}}),
    )
    .await;
    let ev = loop {
        let e = next_of(&mut ui, "chat.event").await;
        if e["event"]["kind"] == "cost" {
            break e;
        }
    };
    assert_eq!(
        (
            ev["event"]["usd"].as_f64(),
            ev["event"]["turn_usd"].as_f64()
        ),
        (Some(0.5), Some(0.5))
    );

    // Runtime tools: chat.list, chat.send (delivered as a user event + a run), loop guard, chat.read.
    let l = request(&mut rt, 1, json!({"op": "chat.list"})).await;
    assert_eq!(l["ok"], true);
    assert_eq!(l["result"].as_array().unwrap().len(), 2);
    let r = request(
        &mut rt,
        2,
        json!({"op": "chat.send", "from": b, "chat": a, "text": "need a logo for the end card"}),
    )
    .await;
    assert_eq!(r["ok"], true, "{r}");
    let ev = loop {
        let e = next_of(&mut ui, "chat.event").await;
        if e["chat"] == a
            && e["event"]["kind"] == "user"
            && e["event"]["author"] == format!("chat:{b}")
        {
            break e;
        }
    };
    assert_eq!(
        ev["event"]["from_title"],
        "OPUS please direct me an ad for the headphones"
    );
    assert_eq!(ev["event"]["text"], "need a logo for the end card");
    let run = next_run(&mut rt).await;
    assert_eq!(
        (run["chat"].as_str(), run["from"]["chat"].as_str()),
        (Some(a.as_str()), Some(b.as_str()))
    );
    assert_eq!(run["author"], format!("chat:{b}"));
    assert!(
        run["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"] == "make a logo")
    );
    for i in 0..4 {
        let r = request(
            &mut rt,
            10 + i,
            json!({"op": "chat.send", "from": b, "chat": a, "text": format!("ping {i}")}),
        )
        .await;
        assert_eq!(r["ok"], true, "{r}");
    }
    let r = request(
        &mut rt,
        20,
        json!({"op": "chat.send", "from": b, "chat": a, "text": "one too many"}),
    )
    .await;
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("loop guard"));
    // The reverse direction has its own budget; self-sends are refused.
    assert_eq!(
        request(
            &mut rt,
            21,
            json!({"op": "chat.send", "from": a, "chat": b, "text": "logo done: n1"})
        )
        .await["ok"],
        true
    );
    assert_eq!(
        request(
            &mut rt,
            22,
            json!({"op": "chat.send", "from": a, "chat": a, "text": "me"})
        )
        .await["ok"],
        false
    );
    let rd = request(
        &mut rt,
        23,
        json!({"op": "chat.read", "chat": a, "last_n": 50}),
    )
    .await;
    let digest = rd["result"]["digest"].as_str().unwrap();
    assert!(
        digest.contains("need a logo for the end card")
            && digest.contains(&format!("user (chat:{b})")),
        "{digest}"
    );

    // A new turn banks the previous turn's spend.
    send(
        &mut rt,
        json!({"type": "event", "chat": a, "event": {"kind": "cost", "usd": 0.25}}),
    )
    .await;
    let ev = loop {
        let e = next_of(&mut ui, "chat.event").await;
        if e["event"]["kind"] == "cost" {
            break e;
        }
    };
    assert_eq!(
        (
            ev["event"]["usd"].as_f64(),
            ev["event"]["turn_usd"].as_f64()
        ),
        (Some(0.75), Some(0.25))
    );

    // A chat chip resolves to the chat's title and a digest of its recent events.
    send(&mut ui, json!({"type": "chat.send", "text": "use what #Logo made", "context": [{"kind": "chat", "chat": a}]})).await;
    let run = loop {
        let r = next_run(&mut rt).await;
        if r["text"] == "use what #Logo made" {
            break r;
        }
    };
    assert_eq!(run["context"][0]["title"], "Logo");
    assert!(
        run["context"][0]["digest"]
            .as_str()
            .unwrap()
            .contains("make a logo")
    );

    // Archive hides the chat; everything is persisted in chats/index.json.
    send(&mut ui, json!({"type": "chat.archive", "chat": b})).await;
    list_until(&mut ui, |c| row(c, &b).is_none() && row(c, &a).is_some()).await;
    let index: Json =
        serde_json::from_slice(&std::fs::read(env.host.dir().join("chats/index.json")).unwrap())
            .unwrap();
    let rows = index["chats"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let ra = rows.iter().find(|r| r["chat"] == a).unwrap();
    assert_eq!(
        (ra["title"].as_str(), ra["state"].as_str()),
        (Some("Logo"), Some("running"))
    );
    assert!((ra["cost"].as_f64().unwrap() - 0.5).abs() < 1e-9); // banked; the open turn adds 0.25
    assert_eq!(
        rows.iter().find(|r| r["chat"] == b).unwrap()["archived"],
        true
    );
    assert!(env.host.dir().join(format!("chats/{a}.jsonl")).exists());
}
