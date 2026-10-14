//! The user's own agent as director (Claude Code, Codex, … over MCP; see ARCHITECTURE.md
//! "Director agents"):
//!
//! - **Binding**: `session` lands on the right project without being told (explicit project →
//!   the agent's folder → the project open in fal OS → the most recent one); see `mcp.rs`.
//! - **Context**: [`overview`] is the compact picture `session` / `get_context` return
//!   (brief, references, selection, comments, chats, what's generating).
//! - **Inbox** (user → agent): messages typed into the agent's chat tab in fal OS, comments,
//!   new references and selection changes queue per session. `inbox` drains them (and can
//!   long-poll); messages and comments are also appended to every tool result, and pushed to
//!   Claude Code as `notifications/claude/channel` when the session enabled channels.
//! - **Report** (agent → user): `report` posts to the agent's chat tab.
//! - **Arrange**: `arrange` lays nodes out as a board (frame) / grid / row / column.
//! - **Pointers**: every project folder gets an `AGENTS.md` + `CLAUDE.md` block saying what
//!   the folder is (between marker lines; the rest of the file is the user's).
//! - **Continue in my agent**: the `cd <project> && claude "…"` command (copied natively).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use falos_doc::{ActorKind, Command, NodeKind, Props, ReadOpts};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, CustomNotification, ServerNotification};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::context::untrusted;
use crate::host::{Event, Host, now_ms};
use crate::mcp::CanvasMcp;

// ---- inbox --------------------------------------------------------------------------------

/// Something the humans sent an external agent.
#[derive(Clone, Debug)]
pub struct InboxItem {
    pub id: String,
    /// `message` | `stop` | `comment` | `reference` | `selection`.
    pub kind: &'static str,
    pub text: String,
    /// Human actor id (`lukas`).
    pub author: String,
    pub nodes: Vec<String>,
    pub chips: Vec<Json>,
    pub annotation: Option<String>,
    pub canvas: String,
    /// The agent chat a `message` / `stop` was typed into (its `queued` event).
    pub chat: Option<String>,
    pub at: i64,
}

impl InboxItem {
    /// Selection changes don't end a wait on their own (they ride along with the next item).
    fn wakes(&self) -> bool {
        self.kind != "selection"
    }
}

fn short_id(prefix: &str) -> String {
    format!("{prefix}{}", (0..5).map(|_| format!("{:02x}", rand::random::<u8>())).collect::<String>())
}

/// External agent sessions (those with an `ext:` chat), on `canvas` when given.
pub fn ext_sessions(host: &Host, canvas: Option<&str>) -> Vec<String> {
    host.with_sessions(|ss| {
        ss.values()
            .filter(|s| s.chat.as_deref().is_some_and(|c| c.starts_with("ext:")))
            .filter(|s| canvas.is_none_or(|c| s.presence.canvas.as_deref().is_none_or(|pc| pc == c)))
            .map(|s| s.id.clone())
            .collect()
    })
}

fn is_human(host: &Host, actor: &str) -> bool {
    host.with_sessions(|ss| ss.values().any(|s| s.actor.id == actor && s.actor.kind == ActorKind::Human))
}

/// Queue `item` for `session`; wakes `inbox` waits and pushes to Claude Code channels.
pub fn push(host: &Host, session: &str, item: InboxItem) {
    let peer = host.with_sessions(|ss| {
        let s = ss.get_mut(session)?;
        if item.kind == "selection" {
            s.inbox.retain(|i| i.kind != "selection");
        }
        s.inbox.push(item.clone());
        let claude = s.client.as_deref() == Some("claude");
        (claude && item.wakes()).then(|| s.peer.clone()).flatten()
    });
    if item.wakes() {
        host.inbox_wake.notify_waiters();
    }
    channel(peer, &item);
}

/// Channels (Claude Code `--channels` / dev channels): the message becomes the session's
/// next prompt. Best effort; without channels it waits in the inbox like everywhere else.
fn channel(peer: Option<rmcp::Peer<rmcp::RoleServer>>, item: &InboxItem) {
    if let Some(peer) = peer
        && let Ok(rt) = tokio::runtime::Handle::try_current()
    {
        let content = format!("{} (from fal OS; also in your fal-os inbox)", line(item));
        let meta = json!({"source": "fal-os", "kind": item.kind, "from": item.author});
        rt.spawn(async move {
            let n = CustomNotification::new("notifications/claude/channel", Some(json!({"content": content, "meta": meta})));
            let _ = peer.send_notification(ServerNotification::CustomNotification(n)).await;
        });
    }
}

/// Run `f` on the messages waiting in an external tab (loaded from its log on first use, so
/// they survive host restarts).
fn pending<T>(host: &Host, chat: &str, f: impl FnOnce(&mut Vec<InboxItem>) -> T) -> T {
    let loaded = host.inbox_orphans.lock().unwrap().contains_key(chat);
    if !loaded {
        let canvas = host.chat.canvas_of(host, chat).unwrap_or_else(|| "main".into());
        let items: Vec<InboxItem> = host
            .chat
            .undelivered(host, chat)
            .into_iter()
            .map(|(id, text, author, chips, stop)| InboxItem {
                id,
                kind: if stop { "stop" } else { "message" },
                text,
                author,
                nodes: chips.iter().filter_map(|c| c["node"].as_str().map(String::from)).collect(),
                chips,
                annotation: None,
                canvas: canvas.clone(),
                chat: Some(chat.to_string()),
                at: now_ms(),
            })
            .collect();
        host.inbox_orphans.lock().unwrap().entry(chat.to_string()).or_insert(items);
    }
    f(host.inbox_orphans.lock().unwrap().get_mut(chat).expect("loaded"))
}

/// The external tab a session works for.
fn ext_chat_of(host: &Host, session: &str) -> Option<String> {
    host.with_sessions(|ss| ss.get(session).and_then(|s| s.chat.clone())).filter(|c| c.starts_with("ext:"))
}

/// After a doc change: a human's new comments and imported media go to the external agents
/// on that canvas.
pub fn on_change(
    host: &Host,
    canvas: &str,
    actor: &str,
    created: &[String],
    comments: Vec<(String, String, String, Option<String>)>,
) {
    if (comments.is_empty() && created.is_empty()) || !is_human(host, actor) {
        return;
    }
    let targets = ext_sessions(host, Some(canvas));
    if targets.is_empty() {
        return;
    }
    let mut items = vec![];
    for (ann, node, text, assignee) in comments {
        let to = assignee.map(|a| format!(" (assigned to @{a})")).unwrap_or_default();
        items.push(InboxItem {
            id: short_id("i_"),
            kind: "comment",
            text: format!("comment on {node}{to}: {text}"),
            author: actor.into(),
            nodes: vec![node],
            chips: vec![],
            annotation: Some(ann),
            canvas: canvas.into(),
            chat: None,
            at: now_ms(),
        });
    }
    let media: Vec<String> = host
        .read(canvas, |c| {
            created
                .iter()
                .filter(|id| c.node(id).is_some_and(|n| is_media(n.kind) || matches!(n.kind, NodeKind::Note | NodeKind::Text)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if !media.is_empty() {
        items.push(InboxItem {
            id: short_id("i_"),
            kind: "reference",
            text: format!("added {}", plural(media.len(), "node")),
            author: actor.into(),
            nodes: media,
            chips: vec![],
            annotation: None,
            canvas: canvas.into(),
            chat: None,
            at: now_ms(),
        });
    }
    for s in &targets {
        for i in &items {
            push(host, s, i.clone());
        }
    }
}

/// A human changed their selection (replaces the previous selection item).
pub fn on_selection(host: &Host, human_session: &str, nodes: &[String]) {
    let Some((actor, canvas)) = host.with_sessions(|ss| {
        let s = ss.get(human_session)?;
        Some((s.actor.id.clone(), s.presence.canvas.clone()?))
    }) else {
        return;
    };
    for s in ext_sessions(host, Some(&canvas)) {
        push(
            host,
            &s,
            InboxItem {
                id: short_id("i_"),
                kind: "selection",
                text: String::new(),
                author: actor.clone(),
                nodes: nodes.to_vec(),
                chips: vec![],
                annotation: None,
                canvas: canvas.clone(),
                chat: None,
                at: now_ms(),
            },
        );
    }
}

/// The user typed into an external agent's chat tab (`chat.send` / `chat.steer`), or pressed
/// stop (`kind` = `stop`). Shown as `queued` until the agent picks it up: the attached session
/// at its next tool call (or `inbox`), else the agent's next session ("waits for its next
/// session" in the tab).
pub fn user_message(host: &Host, chat: &str, author: &str, text: &str, chips: Vec<Json>, kind: &'static str) -> Result<()> {
    let text = match kind {
        "stop" => "stop: finish the step you are in, don't start new work, `report` what you did and wait for the user".to_string(),
        _ => text.trim().to_string(),
    };
    if text.is_empty() {
        bail!("empty message");
    }
    let canvas = host.chat.canvas_of(host, chat).ok_or_else(|| anyhow!("unknown chat `{chat}`"))?;
    let detached = host.chat.state_of(host, chat).as_deref() == Some("detached");
    if kind == "stop" && detached {
        bail!("the agent isn't attached: nothing to stop");
    }
    let item = InboxItem {
        id: short_id("m_"),
        kind,
        text: text.clone(),
        author: author.into(),
        nodes: chips.iter().filter_map(|c| c["node"].as_str().map(String::from)).collect(),
        chips: chips.clone(),
        annotation: None,
        canvas,
        chat: Some(chat.into()),
        at: now_ms(),
    };
    pending(host, chat, |v| v.push(item.clone()));
    let mut ev = json!({"kind": "queued", "id": item.id, "text": text, "author": author, "context": chips});
    if kind == "stop" {
        ev["stop"] = json!(true);
    }
    host.chat.append(host, chat, None, ev)?;
    let peers: Vec<rmcp::Peer<rmcp::RoleServer>> = host.with_sessions(|ss| {
        ss.values()
            .filter(|s| s.chat.as_deref() == Some(chat) && s.client.as_deref() == Some("claude"))
            .filter_map(|s| s.peer.clone())
            .collect()
    });
    host.inbox_wake.notify_waiters();
    if !detached {
        for p in peers {
            channel(Some(p), &item);
        }
    }
    Ok(())
}

/// `chat.unqueue` on an agent chat: take a message back before the agent read it.
pub fn unqueue(host: &Host, chat: &str, id: &str) -> bool {
    let mut found = pending(host, chat, |v| {
        let before = v.len();
        v.retain(|i| i.id != id);
        v.len() != before
    });
    found |= host.with_sessions(|ss| {
        let mut f = false;
        for s in ss.values_mut() {
            let before = s.inbox.len();
            s.inbox.retain(|i| i.id != id);
            f |= s.inbox.len() != before;
        }
        f
    });
    if found {
        let _ = host.chat.append(host, chat, None, json!({"kind": "dequeued", "id": id, "reason": "cancelled"}));
    }
    found
}

/// A new session attached to its agent's tab: a `stop` left for an earlier session has
/// nothing left to stop (dropped); messages wait for this session's first result.
pub fn adopt_orphans(host: &Host, session: &str) {
    let Some(chat) = ext_chat_of(host, session) else { return };
    let stale: Vec<String> = pending(host, &chat, |v| {
        let ids = v.iter().filter(|i| i.kind == "stop").map(|i| i.id.clone()).collect();
        v.retain(|i| i.kind != "stop");
        ids
    });
    for id in stale {
        let _ = host.chat.append(host, &chat, None, json!({"kind": "dequeued", "id": id, "reason": "expired"}));
    }
}

/// Take the session's items (`all`: also the selection). Messages are marked delivered in
/// their chat (the `queued` line becomes a user message).
pub fn drain(host: &Host, session: &str, all: bool) -> Vec<InboxItem> {
    // The tab's messages first (any session of that agent may take them), then this
    // session's own notices.
    let mut items = match ext_chat_of(host, session) {
        Some(chat) => pending(host, &chat, std::mem::take),
        None => vec![],
    };
    items.extend(
        host.with_sessions(|ss| {
            let s = ss.get_mut(session)?;
            if s.inbox.is_empty() {
                return None;
            }
            let (take, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut s.inbox).into_iter().partition(|i| all || i.wakes());
            s.inbox = keep;
            Some(take)
        })
        .unwrap_or_default(),
    );
    for i in &items {
        if let Some(chat) = &i.chat {
            let _ = host.chat.append(host, chat, None, json!({"kind": "dequeued", "id": i.id, "reason": "injected"}));
            let _ = host.chat.append(
                host,
                chat,
                None,
                json!({"kind": "user", "text": i.text, "context": i.chips, "author": i.author, "queued": i.id}),
            );
        }
    }
    items
}

fn line(i: &InboxItem) -> String {
    match i.kind {
        "message" => format!("{} says: {}", i.author, i.text),
        "stop" => format!("{} pressed stop — {}", i.author, i.text),
        "comment" => format!("@{} {}", i.author, i.text),
        "reference" => format!("@{} {}: {}", i.author, i.text, i.nodes.join(", ")),
        _ => String::new(),
    }
}

/// Items as lines for the agent (node rows for references and the selection).
pub fn render(host: &Host, items: &[InboxItem]) -> String {
    let mut out = String::new();
    for i in items {
        match i.kind {
            "message" | "stop" => {
                let _ = writeln!(out, "- {}", line(i));
                if !i.nodes.is_empty() {
                    let _ = writeln!(out, "  attached: {}", rows(host, &i.canvas, &i.nodes, 8).trim_end().replace('\n', "\n  "));
                }
            }
            "comment" => {
                let _ = writeln!(out, "- comment by @{} (annotation {}): {}", i.author, i.annotation.as_deref().unwrap_or("?"), untrusted(&i.text));
            }
            "reference" => {
                let _ = writeln!(out, "- @{} {}:\n  {}", i.author, i.text, rows(host, &i.canvas, &i.nodes, 8).trim_end().replace('\n', "\n  "));
            }
            "selection" => {
                if i.nodes.is_empty() {
                    let _ = writeln!(out, "- @{} cleared the selection", i.author);
                } else {
                    let _ = writeln!(out, "- @{} now has selected:\n  {}", i.author, rows(host, &i.canvas, &i.nodes, 12).trim_end().replace('\n', "\n  "));
                }
            }
            _ => {}
        }
    }
    out
}

/// For every tool result: messages and comments that arrived meanwhile (drained).
pub fn pending_for_result(host: &Host, session: &str) -> Option<String> {
    let items = drain(host, session, false);
    (!items.is_empty()).then(|| format!("\n\nnotices from the user in fal OS (act on this before you continue):\n{}", render(host, &items).trim_end()))
}

/// Wait up to `secs` for something that wakes (a message, comment, reference), then drain.
pub async fn wait(host: &Host, session: &str, secs: u64) -> Result<Vec<InboxItem>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let notified = host.inbox_wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let ready = host.with_session(session, |s| s.inbox.iter().any(InboxItem::wakes))?
            || ext_chat_of(host, session).is_some_and(|c| pending(host, &c, |v| !v.is_empty()));
        if ready || Instant::now() >= deadline {
            return Ok(drain(host, session, true));
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep_until(deadline.into()) => {}
        }
    }
}

// ---- context overview ------------------------------------------------------------------------

fn is_media(k: NodeKind) -> bool {
    matches!(k, NodeKind::Image | NodeKind::Video | NodeKind::Audio | NodeKind::Model3d | NodeKind::Vector | NodeKind::Document)
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

fn ago(ms: i64) -> String {
    let s = ((now_ms() - ms).max(0) / 1000) as u64;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// One row per node (text format, one line, with the local file for media), max `max` rows.
fn rows(host: &Host, canvas: &str, ids: &[String], max: usize) -> String {
    host.read(canvas, |c| {
        let mut out = String::new();
        for id in ids.iter().take(max) {
            let Some(n) = c.node(id) else { continue };
            let row = c
                .read(&ReadOpts { root: Some(id.clone()), depth: Some(0), annotations: false, ..Default::default() })
                .unwrap_or_default();
            let mut row: String = row.lines().next().unwrap_or_default().trim().to_string();
            // The local file says more than the content hash.
            if let Some(a) = row.find(" asset(sha256:")
                && let Some(e) = row[a + 1..].find(')')
            {
                row.replace_range(a..a + 1 + e + 1, "");
            }
            let mut row: String = row.chars().take(320).collect();
            if let Some(f) = n.asset_id().and_then(|a| crate::chat::asset_file(host, a)) {
                let _ = write!(row, " file({})", f.display());
            }
            let _ = writeln!(out, "{row}");
        }
        if ids.len() > max {
            let _ = writeln!(out, "… {} more", ids.len() - max);
        }
        out.trim_end().to_string()
    })
    .unwrap_or_default()
}

/// What the project is for: `project.json` `brief`, a note named "brief", else the first
/// message of the oldest chat.
pub fn brief(host: &Host, canvas: &str) -> Option<(String, String)> {
    if let Some(b) = host.project().extra.get("brief").and_then(Json::as_str).filter(|b| !b.trim().is_empty()) {
        return Some(("project brief".into(), b.to_string()));
    }
    let note = host
        .read(canvas, |c| {
            c.nodes().into_iter().find(|n| {
                matches!(n.kind, NodeKind::Note | NodeKind::Text)
                    && (n.name.to_lowercase().contains("brief") || n.text.as_deref().is_some_and(|t| t.trim_start().to_lowercase().starts_with("brief")))
            })
        })
        .ok()
        .flatten();
    if let Some(n) = note {
        return Some((format!("note {}", n.id), n.text.unwrap_or(n.name)));
    }
    host.chat.first_user_message(host).map(|(chat, t)| (format!("first message of chat {chat}"), t))
}

/// Media the human brought in (role input, or protected without a role) and human notes.
pub fn references(host: &Host, canvas: &str) -> (Vec<String>, Vec<String>) {
    host.read(canvas, |c| {
        let mut media = vec![];
        let mut notes = vec![];
        for n in c.nodes() {
            if is_media(n.kind) && (n.role() == Some("input") || (n.role().is_none() && n.is_protected())) {
                media.push(n.id);
            } else if matches!(n.kind, NodeKind::Note | NodeKind::Text) && n.role() != Some("final") && !is_agent_actor(&n.created_by) {
                notes.push(n.id);
            }
        }
        (media, notes)
    })
    .unwrap_or_default()
}

/// Agent actors are slugs of agent names; humans are `human` / the user's id. A heuristic for
/// offline nodes: anything not created by a known human-ish id counts as an agent's.
fn is_agent_actor(a: &str) -> bool {
    !(a.is_empty() || a == "human" || a == "user" || a.starts_with("human-"))
        && ["claude", "codex", "opus", "fal", "director", "generator", "gpt", "cursor", "gemini", "agent", "motion", "coder", "editor", "writer"]
            .iter()
            .any(|p| a.starts_with(p))
}

/// The project as an external agent should see it first: who/where, brief, selection,
/// references, deliverables, comments, chats, other agents, jobs. `full` adds the canvas
/// outline and every reference row.
pub fn overview(host: &Host, session: Option<&str>, canvas: &str, full: bool) -> Result<String> {
    let canvas = host.resolve_canvas(canvas)?;
    let p = host.project();
    let mut out = String::new();
    let count = host.read(&canvas, |c| c.nodes().len())?;
    let _ = writeln!(out, "fal OS project \"{}\" (id `{}`) · canvas `{canvas}` · {} · folder {}", p.name, host.id, plural(count, "node"), host.dir().display());
    let bound = session.and_then(|s| host.with_sessions(|ss| ss.get(s).and_then(|x| x.bound_by.clone())));
    let open = host.with_sessions(|ss| ss.values().any(|s| s.actor.kind == ActorKind::Human));
    let seen = host.human_seen.load(std::sync::atomic::Ordering::Relaxed);
    let _ = writeln!(
        out,
        "{}{}",
        bound.map(|b| format!("bound by: {b} · ")).unwrap_or_default(),
        if open { format!("open in fal OS (user active {})", ago(seen)) } else { "not open in fal OS right now".into() }
    );
    // Everything below comes from the canvas and chats: one data block.
    let split = out.len();
    let brief = brief(host, &canvas);
    match brief.clone() {
        Some((src, b)) => {
            let b: String = b.chars().take(if full { 2000 } else { 600 }).collect();
            let _ = writeln!(out, "\nbrief ({src}):\n{}", b);
        }
        None => {
            let _ = writeln!(out, "\nbrief: none written down (ask the user, or infer it from the references)");
        }
    }
    let selected = host.selected_nodes(&canvas);
    if selected.is_empty() {
        let _ = writeln!(out, "\nselected by the user: nothing");
    } else {
        let _ = writeln!(out, "\nselected by the user ({}) — \"this\" / \"these\" means these:\n{}", selected.len(), rows(host, &canvas, &selected, 12));
    }
    let (media, mut notes) = references(host, &canvas);
    if let Some((src, _)) = &brief {
        notes.retain(|n| !src.ends_with(n.as_str()));
    }
    if media.is_empty() {
        let _ = writeln!(out, "\nreferences: none yet");
    } else {
        let _ = writeln!(out, "\nreferences ({}, protected inputs — derive, never modify):\n{}", media.len(), rows(host, &canvas, &media, if full { 60 } else { 16 }));
    }
    if !notes.is_empty() {
        let _ = writeln!(out, "notes ({}):\n{}", notes.len(), rows(host, &canvas, &notes, if full { 20 } else { 6 }));
    }
    let (finals, candidates, rejected) = host.read(&canvas, |c| {
        let ns = c.nodes();
        (
            ns.iter().filter(|n| n.role() == Some("final")).map(|n| n.id.clone()).collect::<Vec<_>>(),
            ns.iter().filter(|n| n.role() == Some("candidate")).count(),
            ns.iter().filter(|n| n.role() == Some("rejected")).count(),
        )
    })?;
    if finals.is_empty() {
        let _ = writeln!(out, "\ndeliverables (role final): none yet · {candidates} candidates · {rejected} rejected");
    } else {
        let _ = writeln!(out, "\ndeliverables (role final, {}) · {candidates} candidates · {rejected} rejected:\n{}", finals.len(), rows(host, &canvas, &finals, 12));
    }
    let me = session.and_then(|s| host.with_sessions(|ss| ss.get(s).map(|x| x.actor.id.clone())));
    let anns: Vec<falos_doc::Annotation> = host.read(&canvas, |c| c.annotations())?.into_iter().filter(|a| a.status != falos_doc::Status::Resolved).collect();
    if !anns.is_empty() {
        let mine = anns.iter().filter(|a| me.is_some() && a.assignee == me).count();
        let lines: Vec<String> = anns
            .iter()
            .take(if full { 30 } else { 6 })
            .map(|a| {
                let last = a.thread.last().map(|m| format!("@{}: {}", m.author, m.text.chars().take(160).collect::<String>())).unwrap_or_default();
                format!("! {} {} on({}){} {last}", a.id, a.status.as_str(), a.node_id, a.assignee.as_ref().map(|x| format!(" to(@{x})")).unwrap_or_default())
            })
            .collect();
        let _ = writeln!(out, "\nopen comments ({}, {mine} assigned to you):\n{}", anns.len(), lines.join("\n"));
    }
    let chats: Vec<Json> = host.chat.list(host, None).as_array().cloned().unwrap_or_default();
    let inapp: Vec<String> = chats
        .iter()
        .filter(|r| r["external"] != true)
        .take(if full { 12 } else { 5 })
        .map(|r| {
            let step = r["step"].as_str().filter(|s| !s.is_empty() && r["state"] == "running").map(|s| format!(" · now: {s}")).unwrap_or_default();
            format!("{} \"{}\" {} ({}){step}", r["chat"].as_str().unwrap_or_default(), r["title"].as_str().unwrap_or_default(), r["state"].as_str().unwrap_or("idle"), r["driver"].as_str().unwrap_or("opus"))
        })
        .collect();
    if !inapp.is_empty() {
        let _ = writeln!(out, "\nin-app chats (Opus swarm; chat_read / chat_send to steer):\n{}", inapp.join("\n"));
    }
    let others: Vec<String> = host
        .sessions()
        .into_iter()
        .filter(|s| s.kind == ActorKind::Agent && Some(s.id.as_str()) != session && s.chat.as_deref().is_some_and(|c| c.starts_with("ext:")))
        .map(|s| format!("@{} ({}){}", s.actor, s.agent_name, if s.presence.status.is_empty() { String::new() } else { format!(" status \"{}\"", s.presence.status) }))
        .collect();
    if !others.is_empty() {
        let _ = writeln!(out, "\nother external agents attached: {}", others.join(" · "));
    }
    let (running, queued) = host.batches.in_flight();
    let singles = host.jobs.lock().unwrap().len();
    if running + queued + singles > 0 {
        let _ = writeln!(out, "\ngenerating now: {} running, {} queued", running + singles, queued);
    }
    if full {
        let outline = host.read(&canvas, |c| {
            c.read(&ReadOpts { depth: Some(1), annotations: false, attribution: false, ..Default::default() }).unwrap_or_default()
        })?;
        let lines: Vec<&str> = outline.lines().take(80).collect();
        let more = outline.lines().count().saturating_sub(lines.len());
        let _ = writeln!(out, "\ncanvas outline (depth 1{}):\n{}", if more > 0 { format!(", {more} more rows") } else { String::new() }, lines.join("\n"));
    }
    let body = out.split_off(split);
    out.push_str(&untrusted(&body));
    let _ = write!(
        out,
        "\n\nnext: look at the references (`get_context` returns them as images) · ask only if the request is ambiguous · ONE `generate_batch` · `arrange` the picks into a board · `inbox` between steps · `report` when done"
    );
    Ok(out)
}

/// Small JPEG previews of image nodes (`(caption, base64)`), at most `max`.
pub async fn previews(host: &Host, canvas: &str, ids: &[String], max: usize) -> Vec<(String, String)> {
    let files: Vec<(String, String, PathBuf)> = host
        .read(canvas, |c| {
            ids.iter()
                .filter_map(|id| {
                    let n = c.node(id)?;
                    (n.kind == NodeKind::Image).then_some(())?;
                    let f = crate::chat::asset_file(host, n.asset_id()?)?;
                    Some((n.id.clone(), n.name.clone(), f))
                })
                .take(max)
                .collect()
        })
        .unwrap_or_default();
    let mut out = vec![];
    for (id, name, f) in files {
        let Ok(img) = crate::ops::img::load(&f).await else { continue };
        let (w, h) = (img.width(), img.height());
        let Ok(jpg) = tokio::task::spawn_blocking(move || crate::ops::img::preview_jpeg(&img, 512)).await else { continue };
        let Ok(jpg) = jpg else { continue };
        out.push((format!("{id} \"{name}\" ({w}x{h})"), base64::engine::general_purpose::STANDARD.encode(jpg)));
    }
    out
}

// ---- project folder pointers ------------------------------------------------------------------

const BEGIN: &str = "<!-- fal-os:begin (generated by fal OS; this block is rewritten, the rest of the file is yours) -->";
const END: &str = "<!-- fal-os:end -->";
const BEGIN_PREFIX: &str = "<!-- fal-os:begin";

fn pointer_block(host: &Host) -> String {
    let p = host.project();
    format!(
        "{BEGIN}\n# fal OS project: {name}\n\nThis folder is the fal OS project \"{name}\" (id `{id}`): its canvas, references, generations and chats live in fal OS.\n\n- Use the `fal-os` MCP tools. Start with `session` (agentName = your name, cwd = this folder): it binds to this project and returns the brief, the references, what the user has selected and what is running.\n- Playbook (fal-os skill / `get_knowledge [\"director\"]`): look at the references (`get_context`), ask only if ambiguous, ONE `generate_batch`, `arrange` the picks into a board, `inbox` between steps, `report` when done.\n- The files here (project.json, canvases/, assets/, chats/) are fal OS's: change the project through the tools, not by editing them.\n{END}\n",
        name = p.name,
        id = host.id
    )
}

/// Put `block` into `path` between the marker lines (appended when the file has none; a file
/// containing `fal-os:off` is left alone). Returns whether the file changed.
fn upsert_block(path: &Path, block: &str) -> Result<bool> {
    let cur = std::fs::read_to_string(path).unwrap_or_default();
    if cur.contains("fal-os:off") {
        return Ok(false);
    }
    let next = match (cur.find(BEGIN_PREFIX), cur.find(END)) {
        (Some(b), Some(e)) if e > b => format!("{}{}{}", &cur[..b], block.trim_end(), &cur[e + END.len()..]),
        _ if cur.trim().is_empty() => block.to_string(),
        _ => format!("{}\n\n{block}", cur.trim_end()),
    };
    if next == cur {
        return Ok(false);
    }
    let tmp = path.with_extension(format!("md.tmp{}", std::process::id()));
    std::fs::write(&tmp, &next)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// (Re)write the `AGENTS.md` (Codex, most agents) and `CLAUDE.md` (Claude Code) pointers.
pub fn write_pointers(host: &Host) {
    if host.closed.is_cancelled() {
        return;
    }
    let block = pointer_block(host);
    for f in ["AGENTS.md", "CLAUDE.md"] {
        if let Err(e) = upsert_block(&host.dir().join(f), &block) {
            tracing::warn!("writing {f} in {}: {e:#}", host.dir().display());
        }
    }
}

// ---- continue in my agent ------------------------------------------------------------------------

/// Shell-safe `cd` target, `~/…` when under $HOME.
fn shell_path(dir: &Path) -> String {
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,@".contains(c));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rel) = dir.strip_prefix(&home)
    {
        let rel = rel.to_string_lossy();
        if safe(&rel) {
            return format!("~/{rel}");
        }
        return format!("\"$HOME/{}\"", rel.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$").replace('`', "\\`"));
    }
    let s = dir.to_string_lossy();
    if safe(&s) { s.into_owned() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

/// `cd ~/fal-os-projects/x && claude "continue this fal OS project"` (or `codex`).
pub fn continue_command(dir: &Path, agent: &str) -> Result<String> {
    let bin = match agent {
        "claude" | "claude-code" => "claude",
        "codex" => "codex",
        _ => bail!("unknown agent `{agent}` (claude, codex)"),
    };
    Ok(format!("cd {} && {bin} \"continue this fal OS project\"", shell_path(dir)))
}

fn pbcopy(text: &str) -> Result<()> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn().context("pbcopy")?;
    child.stdin.take().context("pbcopy stdin")?.write_all(text.as_bytes())?;
    if !child.wait()?.success() {
        bail!("pbcopy failed");
    }
    Ok(())
}

/// Open Terminal running `command` (a `.command` file opened by Terminal: no Automation
/// permission needed).
fn open_terminal(host: &Host, agent: &str, command: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::server::home_dir().join("continue");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}-{agent}.command", host.id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_")));
    std::fs::write(&path, format!("#!/bin/zsh -il\n{command}\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    let ok = std::process::Command::new("open").args(["-a", "Terminal"]).arg(&path).status()?.success();
    if !ok {
        bail!("couldn't open Terminal");
    }
    Ok(())
}

/// `{"type":"agent.continue","agent":"claude"|"codex","open"?:bool}` from the UI: copy (and
/// optionally run in Terminal) the command that continues this project in the user's agent.
pub fn continue_frame(host: &Host, msg: &Json) -> Json {
    let agent = msg["agent"].as_str().unwrap_or("claude").to_string();
    let r = (|| -> Result<(String, bool, bool)> {
        let cmd = continue_command(host.dir(), &agent)?;
        let copied = pbcopy(&cmd).is_ok();
        let opened = if msg["open"] == true {
            open_terminal(host, &agent, &cmd)?;
            true
        } else {
            false
        };
        Ok((cmd, copied, opened))
    })();
    match r {
        Ok((command, copied, opened)) => json!({"type": "agent.continue", "agent": agent, "command": command, "copied": copied, "opened": opened}),
        Err(e) => json!({"type": "agent.continue", "agent": agent, "error": format!("{e:#}")}),
    }
}

// ---- MCP tools -------------------------------------------------------------------------------------

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InboxArgs {
    pub session_id: String,
    /// Seconds to wait for the user when nothing is waiting (default 0 = just check; max 300).
    /// Use ~30–50 in a loop when you are waiting for an answer.
    pub wait_secs: Option<u64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportArgs {
    pub session_id: String,
    /// Short message for the user's fal OS chat tab (what you did / found / need).
    pub text: String,
    /// Node ids to point at (shown as links).
    pub nodes: Option<Vec<String>>,
    /// End of a piece of work (default true); false for a progress note.
    pub done: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArrangeArgs {
    pub session_id: String,
    pub canvas: String,
    /// Node ids in order (best first). Protected inputs are placed as copies (same asset).
    pub nodes: Vec<String>,
    /// "board" (default: a titled frame), "grid" (no frame), "row", "column".
    pub layout: Option<String>,
    /// Board title (default "Board").
    pub title: Option<String>,
    /// Columns (default: a row up to 3, else about square).
    pub columns: Option<usize>,
    /// Long side of each cell in canvas units (default: the nodes' typical size, 200–512).
    pub cell: Option<f64>,
    /// Gap between cells (default 24).
    pub gap: Option<f64>,
    /// Put the board next to these node ids (default: next to the arranged nodes).
    pub near: Option<Vec<String>>,
    /// Re-lay out inside this existing frame instead of making a new one.
    pub into: Option<String>,
}

const PAD: f64 = 40.0;
const TOP: f64 = 72.0;

#[tool_router(router = director_tools, vis = "pub(crate)")]
impl CanvasMcp {
    #[tool(
        description = "What the user sent you from fal OS since you last looked: messages typed into your chat tab, stop, comments, new references, selection changes. Call it between steps and before you finish; with waitSecs it waits for the user (long-poll) when nothing is there yet. Messages and comments also show up at the end of any tool result.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn inbox(&self, Parameters(a): Parameters<InboxArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = async {
            let secs = a.wait_secs.unwrap_or(0).min(300);
            let prev = host.with_session(&a.session_id, |s| s.presence.status.clone())?;
            if secs > 0 {
                let _ = host.set_presence(&a.session_id, |p| p.status = "waiting for you · message me here".into());
            }
            let items = wait(&host, &a.session_id, secs).await;
            if secs > 0 {
                let _ = host.set_presence(&a.session_id, |p| p.status = prev);
            }
            let items = items?;
            if items.is_empty() {
                return Ok(if secs > 0 { format!("nothing from the user in {secs}s") } else { "nothing new from the user".into() });
            }
            Ok(format!("{}\n{}", plural(items.len(), "item"), render(&host, &items).trim_end()))
        }
        .await;
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Tell the user something in your fal OS chat tab (they see your tool calls there already): what you made and where, what you need, a question. Keep it short; pass the node ids you mean. done:false for a progress note.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn report(&self, Parameters(a): Parameters<ReportArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = (|| {
            let (chat, agent) = host.with_session(&a.session_id, |s| (s.chat.clone(), s.agent_label()))?;
            let chat = chat.filter(|c| c.starts_with("ext:")).ok_or_else(|| anyhow!("report is for external agents (in-app agents talk in their chat)"))?;
            let text = a.text.trim();
            if text.is_empty() {
                bail!("text is empty");
            }
            let text: String = text.chars().take(4000).collect();
            host.chat.append(&host, &chat, None, json!({"kind": "text", "agent": agent, "delta": text}))?;
            let nodes = a.nodes.unwrap_or_default();
            if a.done != Some(false) {
                host.chat.append(&host, &chat, None, json!({"kind": "done", "summary": "", "nodes": nodes}))?;
                let _ = host.set_presence(&a.session_id, |p| p.status.clear());
            }
            Ok("shown to the user".to_string())
        })();
        self.finish(Some(&a.session_id), r)
    }

    #[tool(
        description = "Lay nodes out tidily in one step: layout \"board\" (default) makes a titled frame with the nodes in a grid, in free space next to them (\"pick the best 4, lay them out as a board\" = arrange(nodes:[best first], title)). \"grid\" / \"row\" / \"column\" arrange without a frame; `into` re-lays an existing frame. Nodes keep their aspect, scaled to a common cell; protected inputs are placed as copies. One undo step.",
        annotations(read_only_hint = false, destructive_hint = false, idempotent_hint = false, open_world_hint = false)
    )]
    async fn arrange(&self, Parameters(a): Parameters<ArrangeArgs>) -> CallToolResult {
        let host = self.host().clone();
        let r = (|| {
            let actor = host.actor(&a.session_id)?;
            let canvas = host.resolve_canvas(&a.canvas)?;
            let layout = a.layout.as_deref().unwrap_or("board");
            if !["board", "grid", "row", "column"].contains(&layout) {
                bail!("layout must be board, grid, row or column");
            }
            let mut ids: Vec<String> = vec![];
            for id in &a.nodes {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
            if ids.is_empty() {
                bail!("nodes is empty");
            }
            let nodes: Vec<falos_doc::Node> = host.read(&canvas, |c| {
                ids.iter().map(|id| c.node(id).ok_or_else(|| anyhow!("unknown node `{id}`"))).collect::<Result<Vec<_>>>()
            })??;
            if let Some(f) = &a.into {
                let k = host.read(&canvas, |c| c.node(f).map(|n| n.kind))?;
                if k != Some(NodeKind::Frame) {
                    bail!("`into` must be a frame id");
                }
            }
            // Common cell: every node fits an L×L box, keeping its aspect.
            let mut longs: Vec<f64> = nodes.iter().map(|n| n.w.max(n.h).max(1.0)).collect();
            longs.sort_by(f64::total_cmp);
            let l = a.cell.unwrap_or(longs[longs.len() / 2]).clamp(64.0, 2048.0);
            let l = if a.cell.is_some() { l } else { l.clamp(200.0, 512.0) };
            let sized: Vec<(f64, f64)> = nodes.iter().map(|n| {
                let s = l / n.w.max(n.h).max(1.0);
                ((n.w * s).round(), (n.h * s).round())
            }).collect();
            let cw = sized.iter().map(|s| s.0).fold(0.0, f64::max);
            let ch = sized.iter().map(|s| s.1).fold(0.0, f64::max);
            let n = nodes.len();
            let cols = match layout {
                "row" => n,
                "column" => 1,
                _ => a.columns.unwrap_or_else(|| if n <= 3 { n } else { ((n as f64) * ch / cw).sqrt().ceil() as usize }),
            }
            .clamp(1, n);
            let rows = n.div_ceil(cols);
            let gap = a.gap.unwrap_or(24.0).clamp(0.0, 400.0);
            let gw = cols as f64 * cw + (cols - 1) as f64 * gap;
            let gh = rows as f64 * ch + (rows - 1) as f64 * gap;
            let framed = layout == "board" || a.into.is_some();
            let (bw, bh) = if framed { (gw + 2.0 * PAD, gh + TOP + PAD) } else { (gw, gh) };
            let near = a.near.clone().unwrap_or_else(|| ids.clone());
            let mut cmds = vec![];
            let frame = match &a.into {
                Some(f) => {
                    let mut p = json!({"w": bw, "h": bh});
                    if let Some(t) = &a.title {
                        p["name"] = json!(t.chars().take(80).collect::<String>());
                    }
                    cmds.push(Command::UpdateNode { id: f.clone(), props: props(p)? });
                    Some(f.clone())
                }
                None if framed => {
                    let (x, y) = host.find_space(&canvas, None, &near, bw, bh)?;
                    let id = short_id("f");
                    cmds.push(Command::CreateNode {
                        id: Some(id.clone()),
                        kind: NodeKind::Frame,
                        parent: None,
                        index: None,
                        name: a.title.clone().unwrap_or_else(|| "Board".into()).chars().take(80).collect(),
                        props: props(json!({"x": x, "y": y, "w": bw, "h": bh}))?,
                        text: None,
                    });
                    Some(id)
                }
                None => None,
            };
            let (ox, oy) = match &frame {
                Some(_) => (PAD, TOP),
                None => host.find_space(&canvas, None, &near, bw, bh)?,
            };
            let mut placed = vec![];
            let mut copies = vec![];
            for (i, (node, (w, h))) in nodes.iter().zip(&sized).enumerate() {
                let (col, row) = (i % cols, i / cols);
                // Centered in its cell.
                let x = (ox + col as f64 * (cw + gap) + (cw - w) / 2.0).round();
                let y = (oy + row as f64 * (ch + gap) + (ch - h) / 2.0).round();
                let geo = json!({"x": x, "y": y, "w": w, "h": h});
                let reparent = node.parent != frame;
                if node.is_protected() && reparent {
                    let Some(asset) = node.asset_id() else {
                        bail!("`{}` is a protected input without an asset; leave it out", node.id);
                    };
                    let id = short_id("n");
                    let mut p = geo.clone();
                    p["asset"] = json!(asset);
                    cmds.push(Command::CreateNode { id: Some(id.clone()), kind: node.kind, parent: frame.clone(), index: None, name: node.name.clone(), props: props(p)?, text: None });
                    copies.push(json!({"from": node.id, "node": id}));
                    placed.push(id);
                } else {
                    if reparent {
                        cmds.push(Command::MoveNode { id: node.id.clone(), parent: frame.clone(), index: None });
                    }
                    cmds.push(Command::UpdateNode { id: node.id.clone(), props: props(geo)? });
                    placed.push(node.id.clone());
                }
            }
            let res = host.mutate(&canvas, &actor.id, None, |c| c.apply(&actor, None, &cmds))?;
            let mut shown = placed.clone();
            shown.extend(frame.clone());
            host.activity(&a.session_id, &canvas, "edit", &shown);
            host.touch_nodes(&a.session_id, &canvas, std::slice::from_ref(frame.as_ref().unwrap_or(&placed[0])));
            host.with_session(&a.session_id, |s| {
                s.read_versions.insert(canvas.clone(), res.version.clone());
            })?;
            Ok(serde_json::to_string_pretty(&json!({
                "frame": frame, "layout": layout, "nodes": placed, "copies": copies,
                "grid": [cols, rows], "cell": [cw, ch], "size": [bw, bh], "version": res.version.to_string(),
            }))?)
        })();
        self.finish(Some(&a.session_id), r)
    }
}

fn props(v: Json) -> Result<Props> {
    Ok(serde_json::from_value(v)?)
}

/// The session's own image previews, as MCP blocks.
pub fn image_blocks(previews: Vec<(String, String)>) -> Vec<ContentBlock> {
    let mut out = vec![];
    for (caption, b64) in previews {
        out.push(ContentBlock::text(caption));
        out.push(ContentBlock::image(b64, "image/jpeg"));
    }
    out
}

/// Emit a presence update after binding fields changed (keeps the UI's attach line fresh).
pub fn touch_presence(host: &Host, session: &str) {
    if let Ok(info) = host.with_session(session, |s| s.info()) {
        host.emit(Event::Presence { session: info });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_blocks_keep_user_text() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("AGENTS.md");
        let block = format!("{BEGIN}\nv1\n{END}\n");
        assert!(upsert_block(&p, &block).unwrap());
        assert!(!upsert_block(&p, &block).unwrap());
        std::fs::write(&p, format!("# mine\n\n{block}\nmore of mine\n")).unwrap();
        let v2 = format!("{BEGIN}\nv2\n{END}\n");
        assert!(upsert_block(&p, &v2).unwrap());
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.starts_with("# mine") && s.contains("v2") && !s.contains("v1") && s.contains("more of mine"));
        std::fs::write(&p, "fal-os:off\n").unwrap();
        assert!(!upsert_block(&p, &v2).unwrap());
    }

    #[test]
    fn continue_commands() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(continue_command(&home.join("fal-os-projects/x"), "codex").unwrap(), "cd ~/fal-os-projects/x && codex \"continue this fal OS project\"");
        assert_eq!(continue_command(&home.join("fal-os-projects/My Film"), "claude").unwrap(), "cd \"$HOME/fal-os-projects/My Film\" && claude \"continue this fal OS project\"");
        assert_eq!(continue_command(Path::new("/tmp/a b"), "claude").unwrap(), "cd '/tmp/a b' && claude \"continue this fal OS project\"");
        assert!(continue_command(Path::new("/x"), "vim").is_err());
    }
}
