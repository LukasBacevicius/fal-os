//! Read-only transcripts for external agents (Claude Code, Codex, Cursor, … over MCP): each
//! external session has a pseudo-chat `ext:<client>-<n>`, and its tool calls appear there as
//! `tool` events (`running`, then `done` / `error` with a past-tense summary and node ids).

use rmcp::ErrorData;
use rmcp::model::CallToolResponse;
use serde_json::{Value as Json, json};

use crate::host::Host;

pub struct ExtCall {
    chat: String,
    id: String,
    agent: String,
}

/// Housekeeping calls that don't get a transcript line.
const HIDDEN: &[&str] = &["session"];

pub fn ext_start(host: &Host, name: &str, args: &Json) -> Option<ExtCall> {
    if HIDDEN.contains(&name) {
        return None;
    }
    let session = args["sessionId"].as_str()?;
    let (chat, agent) = host
        .with_sessions(|ss| {
            let s = ss.get(session)?;
            Some((s.chat.clone()?, s.agent_label()))
        })
        .filter(|(c, _)| c.starts_with("ext:"))?;
    let id = format!(
        "x_{}",
        (0..4)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect::<String>()
    );
    host.chat.ext_active(host, &chat);
    let _ = host.chat.append(
        host,
        &chat,
        None,
        json!({"kind": "tool", "agent": agent, "id": id, "name": name,
               "summary": running_summary(name, args), "state": "running"}),
    );
    Some(ExtCall { chat, id, agent })
}

pub fn ext_end(
    host: &Host,
    call: ExtCall,
    name: &str,
    args: &Json,
    res: &Result<CallToolResponse, ErrorData>,
) {
    let (text, error) = match res {
        Ok(CallToolResponse::Complete(r)) => (
            r.content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .join("\n"),
            r.is_error == Some(true),
        ),
        Ok(_) => (String::new(), false),
        Err(e) => (e.message.to_string(), true),
    };
    let body = text.split("\n\nnotices").next().unwrap_or_default();
    let j: Json = serde_json::from_str(body.trim()).unwrap_or(Json::Null);
    let nodes = nodes_of(name, args, &j);
    let summary = if error {
        body.trim().chars().take(160).collect()
    } else {
        done_summary(name, args, &j, &nodes)
    };
    let _ = host.chat.append(
        host,
        &call.chat,
        None,
        json!({"kind": "tool", "agent": call.agent, "id": call.id, "name": name,
               "summary": summary, "state": if error { "error" } else { "done" }, "nodes": nodes}),
    );
}

fn nodes_of(name: &str, args: &Json, j: &Json) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |v: &Json| {
        if let Some(s) = v.as_str()
            && !out.iter().any(|x| x == s)
        {
            out.push(s.to_string());
        }
    };
    add(&j["node"]);
    for k in ["created", "touched", "granted"] {
        for v in j[k].as_array().into_iter().flatten() {
            add(v);
        }
    }
    match name {
        "read" => add(&args["root"]),
        "get_context" => {
            for c in args["chips"].as_array().into_iter().flatten() {
                add(&c["node"]);
            }
        }
        "annotate" => add(&args["nodeId"]),
        _ => {}
    }
    out.truncate(30);
    out
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn running_summary(name: &str, args: &Json) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or_default().to_string();
    match name {
        "read" => "reading the canvas".into(),
        "edit" => "editing".into(),
        "generate" => format!("generating with {}", s("model")),
        "import_file" => format!("importing {}", s("path").rsplit('/').next().unwrap_or_default()),
        "set_status" => s("text"),
        "create_app" => format!("building {}", s("name")),
        "write_app_files" => format!("writing {}", s("app")),
        "app_logs" => format!("checking {}", s("app")),
        _ => name.replace('_', " "),
    }
}

/// `built ping-pong v3 · 2 errors` for app tools.
fn app_summary(verb: &str, j: &Json) -> String {
    let check = &j["check"];
    let errs = check["errors"].as_array().map_or(0, Vec::len);
    let tail = if check.is_null() { String::new() } else if errs == 0 { " · runs clean".into() } else { format!(" · {}", plural(errs, "error")) };
    format!("{verb} {} v{}{tail}", j["app"].as_str().unwrap_or("app"), j["version"].as_u64().unwrap_or(0))
}

/// Past-tense, human-friendly line for a finished call.
pub fn done_summary(name: &str, args: &Json, j: &Json, nodes: &[String]) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or_default().to_string();
    let len = |k: &str| j[k].as_array().map_or(0, Vec::len);
    match name {
        "read" => match args["root"].as_str() {
            Some(r) => format!("looked at {r}"),
            None => "looked at the canvas".into(),
        },
        "edit" => {
            let (c, t) = (len("created"), len("touched"));
            let edited = t.saturating_sub(c);
            match (c, edited) {
                (0, 0) => "edited nothing".into(),
                (c, 0) => format!("created {}", plural(c, "node")),
                (0, e) => format!("edited {}", plural(e, "node")),
                (c, e) => format!("created {}, edited {}", plural(c, "node"), plural(e, "other")),
            }
        }
        "generate" => format!("started generating ({})", s("model").trim_start_matches("fal-ai/")),
        "import_file" => format!("imported {}", s("path").rsplit('/').next().unwrap_or_default()),
        "get_context" => format!("looked at {}", plural(nodes.len(), "node")),
        "get_annotation" => format!("read comment {}", s("id")),
        "list_annotations" => "checked comments".into(),
        "annotate" => "left a comment".into(),
        "reply" => "replied to a comment".into(),
        "claim" => format!("claimed {}", plural(len("granted"), "node")),
        "release" => "released claims".into(),
        "undo" => "undid changes".into(),
        "revert_since" => "reverted changes".into(),
        "set_status" => s("text"),
        "finish_working" => "finished".into(),
        "get_selection" => "looked at your selection".into(),
        "get_knowledge" => "read docs".into(),
        "create_app" => app_summary("built", j),
        "write_app_files" => app_summary("updated", j),
        "read_app" => format!("read {}", j["app"].as_str().unwrap_or("the app")),
        "app_logs" => match j["errors"].as_u64() {
            Some(0) => format!("checked {} · runs clean", j["app"].as_str().unwrap_or("the app")),
            Some(n) => format!("checked {} · {}", j["app"].as_str().unwrap_or("the app"), plural(n as usize, "error")),
            None => "checked the app".into(),
        },
        _ => name.replace('_', " "),
    }
}
