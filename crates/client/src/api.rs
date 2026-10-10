//! JSON shapes exchanged with the UI: node/annotation views and the command format accepted
//! by `CanvasApp.apply`.

use falos_doc::{Anchor, Annotation, Canvas, Command, Node, NodeKind, Props, Status};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// A doc command as JSON (`{"op": "...", ...}`), mirroring [`falos_doc::Command`].
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JsCommand {
    Create {
        #[serde(default)]
        id: Option<String>,
        kind: String,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        name: String,
        #[serde(default)]
        props: Props,
        #[serde(default)]
        text: Option<String>,
    },
    Update {
        id: String,
        props: Props,
    },
    Text {
        id: String,
        text: String,
    },
    Move {
        id: String,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        index: Option<usize>,
    },
    Delete {
        id: String,
    },
    Annotate {
        node: String,
        anchor: Anchor,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        assignee: Option<String>,
    },
    Annotation {
        id: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        anchor: Option<Anchor>,
        /// `null` unassigns; absent leaves it unchanged.
        #[serde(default, deserialize_with = "double_option")]
        assignee: Option<Option<String>>,
    },
    Reply {
        id: String,
        text: String,
    },
    DeleteAnnotation {
        id: String,
    },
}

fn double_option<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

impl JsCommand {
    pub fn into_command(self) -> Result<Command, String> {
        Ok(match self {
            JsCommand::Create { id, kind, parent, index, name, props, text } => Command::CreateNode {
                id,
                kind: NodeKind::parse(&kind).ok_or_else(|| format!("unknown node type `{kind}`"))?,
                parent,
                index,
                name,
                props,
                text,
            },
            JsCommand::Update { id, props } => Command::UpdateNode { id, props },
            JsCommand::Text { id, text } => Command::SetText { id, text },
            JsCommand::Move { id, parent, index } => Command::MoveNode { id, parent, index },
            JsCommand::Delete { id } => Command::DeleteNode { id },
            JsCommand::Annotate { node, anchor, text, assignee } => {
                Command::AddAnnotation { id: None, node_id: node, anchor, text, assignee }
            }
            JsCommand::Annotation { id, status, anchor, assignee } => Command::UpdateAnnotation {
                id,
                status: match status {
                    Some(s) => Some(Status::parse(&s).ok_or_else(|| format!("unknown status `{s}`"))?),
                    None => None,
                },
                anchor,
                assignee,
                outcome: None,
            },
            JsCommand::Reply { id, text } => Command::Reply { id, text },
            JsCommand::DeleteAnnotation { id } => Command::DeleteAnnotation { id },
        })
    }
}

pub fn parse_commands(json: &str) -> Result<Vec<Command>, String> {
    let v: Json = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = match v {
        Json::Array(a) => a,
        one => vec![one],
    };
    list.into_iter()
        .map(|c| serde_json::from_value::<JsCommand>(c).map_err(|e| e.to_string())?.into_command())
        .collect()
}

pub fn node_json(n: &Node, depth: usize) -> Json {
    json!({
        "id": n.id,
        "kind": n.kind.to_string(),
        "name": n.name,
        "x": n.x, "y": n.y, "w": n.w, "h": n.h,
        "parent": n.parent,
        "depth": depth,
        "children": n.children,
        "createdBy": n.created_by,
        "updatedBy": n.updated_by,
        "text": n.text,
        "props": n.props,
    })
}

pub fn annotation_json(a: &Annotation) -> Json {
    json!({
        "id": a.id,
        "nodeId": a.node_id,
        "author": a.author,
        "status": a.status.as_str(),
        "anchor": a.anchor,
        "thread": a.thread,
        "assignee": a.assignee,
        "outcome": a.outcome,
        "createdAt": a.created_at,
    })
}

/// Node with its asset record (lineage) and annotations, for the inspector.
pub fn node_detail_json(c: &Canvas, id: &str) -> Json {
    let Some(n) = c.node(id) else { return Json::Null };
    let asset = n.props.get("asset").and_then(Json::as_str).and_then(|a| c.asset(a));
    let mut v = node_json(&n, 0);
    v["asset"] = asset.map_or(Json::Null, |a| serde_json::to_value(a).unwrap_or(Json::Null));
    v["createdByName"] = c.actor_name(&n.created_by).map_or(Json::Null, Json::String);
    v["annotations"] = c.annotations().iter().filter(|a| a.node_id == id).map(annotation_json).collect();
    v
}

/// All nodes depth-first in z-order (bottom first), with nesting depth.
pub fn nodes_json(c: &Canvas) -> Json {
    let mut depth: std::collections::HashMap<String, usize> = Default::default();
    c.nodes()
        .iter()
        .map(|n| {
            let d = n.parent.as_ref().and_then(|p| depth.get(p)).map_or(0, |d| d + 1);
            depth.insert(n.id.clone(), d);
            node_json(n, d)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let cmds = parse_commands(
            r#"[
            {"op":"create","kind":"note","name":"n","props":{"x":1,"y":2,"w":3,"h":4},"text":"hi"},
            {"op":"annotation","id":"abc","status":"resolved","assignee":null},
            {"op":"annotation","id":"abc","status":"open"},
            {"op":"annotate","node":"n1","anchor":{"kind":"box","x":0.1,"y":0.2,"w":0.3,"h":0.4},"text":"sky"}
        ]"#,
        )
        .unwrap();
        assert_eq!(cmds.len(), 4);
        assert!(matches!(&cmds[1], Command::UpdateAnnotation { assignee: Some(None), status: Some(Status::Resolved), .. }));
        assert!(matches!(&cmds[2], Command::UpdateAnnotation { assignee: None, .. }));
        assert!(matches!(&cmds[3], Command::AddAnnotation { anchor: Anchor::Box { .. }, .. }));
        assert!(parse_commands(r#"{"op":"create","kind":"nope"}"#).is_err());
    }
}
