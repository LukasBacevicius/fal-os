//! The canvas document: one `LoroDoc` per canvas, mutated only through actor-tagged
//! transactions.
//!
//! # Actors, replicas and undo
//!
//! The canvas owns an authoritative `LoroDoc` (the "host" replica). Every actor that
//! mutates the canvas gets its own *session replica*: a separate `LoroDoc` with its own
//! random peer id plus a Loro `UndoManager`. A transaction runs on the actor's replica:
//!
//! 1. pull: import whatever the host has that the replica lacks (remote/other-actor ops;
//!    the `UndoManager` transforms its stacks over them),
//! 2. run the mutations, commit with `origin = "actor:<id>"` and `message = <id>`
//!    (the message is persisted in the oplog; origin is local-only),
//! 3. push: import the new ops into the host.
//!
//! Because each actor writes under its own peer, "undo agent X's last change" is just
//! `UndoManager::undo` on X's replica — Loro's per-peer undo, which only reverts X's ops
//! and leaves concurrent edits by others intact. "Revert everything X did since version
//! V" pops X's undo stack while the top item's starting op id is not contained in V
//! (each undo item records its starting counter via `on_push`). One `apply` call = one
//! commit = one undo step. A `peers` map (peer id → actor id) persisted in the doc
//! attributes every op to an actor, including ops made by undo itself.
//!
//! Undo history lives in memory with the session; after a reload an actor starts with an
//! empty undo stack (their past ops remain attributed).
//!
//! # Staleness
//!
//! `expected_version` never rejects a write (the CRDT merges it). Instead the result
//! carries a [`StaleReport`]: which nodes/annotations other actors changed since that
//! version, and which of those this transaction also touched (`conflicts`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use loro::{
    CommitOptions, Container, ContainerID, ContainerTrait, ExportMode, Frontiers, ID, Index,
    LoroDoc, LoroList, LoroMap, LoroTree, LoroValue, TreeID, TreeParentId, UndoItemMeta,
    UndoManager, UpdateOptions, ValueOrContainer, VersionVector,
};
use serde_json::Value as Json;

use crate::model::{Actor, Anchor, Annotation, Asset, Message, Node, NodeKind, Region, Status};
use crate::{Error, Result};

const META: &str = "meta";
const NODES: &str = "nodes";
const ANNS: &str = "annotations";
const ASSETS: &str = "assets";
const ACTORS: &str = "actors";
const PEERS: &str = "peers";

/// Props passed to create/update. `null` deletes a prop.
pub type Props = BTreeMap<String, Json>;

/// An opaque document version (Loro frontiers), printable as `counter@peer,...`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version(pub Frontiers);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<ID> = self.0.iter().collect();
        ids.sort();
        let s: Vec<String> = ids
            .iter()
            .map(|id| format!("{}@{}", id.counter, id.peer))
            .collect();
        f.write_str(&s.join(","))
    }
}

impl FromStr for Version {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let bad = || Error::UnknownVersion(s.to_string());
        let ids = s
            .split(',')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (c, peer) = p.split_once('@').ok_or_else(bad)?;
                Ok(ID::new(
                    peer.parse().map_err(|_| bad())?,
                    c.parse().map_err(|_| bad())?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Version(Frontiers::from(ids)))
    }
}

/// One mutation. A slice of commands passed to [`Canvas::apply`] is one transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    CreateNode {
        /// Explicit id (must be unused); generated when `None`.
        id: Option<String>,
        kind: NodeKind,
        /// `None` = top level.
        parent: Option<String>,
        /// Sibling index (z-order, 0 = bottom); `None` = on top.
        index: Option<usize>,
        name: String,
        props: Props,
        text: Option<String>,
    },
    /// Set props (incl. `name`, `x`, `y`, `w`, `h`, `rot`, `locked`). Only named props change.
    UpdateNode {
        id: String,
        props: Props,
    },
    SetText {
        id: String,
        text: String,
    },
    MoveNode {
        id: String,
        parent: Option<String>,
        index: Option<usize>,
    },
    DeleteNode {
        id: String,
    },
    PutAsset(Asset),
    AddAnnotation {
        id: Option<String>,
        node_id: String,
        anchor: Anchor,
        text: Option<String>,
        assignee: Option<String>,
    },
    UpdateAnnotation {
        id: String,
        status: Option<Status>,
        anchor: Option<Anchor>,
        /// `Some(None)` unassigns.
        assignee: Option<Option<String>>,
        outcome: Option<Vec<String>>,
    },
    Reply {
        id: String,
        text: String,
    },
    DeleteAnnotation {
        id: String,
    },
}

/// Result of a transaction (also returned by `apply_edit`).
#[derive(Clone, Debug, PartialEq)]
pub struct TxResult {
    /// Canvas version after the transaction; pass back as `expected_version` next time.
    pub version: Version,
    /// Ids of nodes/annotations created, in order.
    pub created: Vec<String>,
    /// Ids of nodes/annotations this transaction touched.
    pub touched: Vec<String>,
    pub warnings: Vec<String>,
    /// Present when `expected_version` was given and others changed things since.
    pub stale: Option<StaleReport>,
}

pub type EditResult = TxResult;

#[derive(Clone, Debug, PartialEq)]
pub struct StaleReport {
    pub since: Version,
    pub changes: Vec<ActorChanges>,
    /// Ids changed by others since `since` that this transaction also touched.
    pub conflicts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActorChanges {
    pub actor: String,
    pub nodes: Vec<String>,
    pub annotations: Vec<String>,
}

impl fmt::Display for StaleReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "changed since your read:")?;
        for c in &self.changes {
            let ids: Vec<&str> = c
                .nodes
                .iter()
                .chain(&c.annotations)
                .map(String::as_str)
                .collect();
            write!(f, " {} by @{};", ids.join(" "), c.actor)?;
        }
        if !self.conflicts.is_empty() {
            write!(
                f,
                " you also edited {} — re-read them",
                self.conflicts.join(" ")
            )?;
        }
        Ok(())
    }
}

struct Session {
    doc: LoroDoc,
    undo: UndoManager,
}

pub struct Canvas {
    doc: LoroDoc,
    sessions: HashMap<String, Session>,
}

fn configure(doc: &LoroDoc) {
    doc.set_record_timestamp(true);
    doc.get_tree(NODES).enable_fractional_index(0);
}

fn pull(host: &LoroDoc, s: &Session) -> Result<()> {
    s.doc
        .import(&host.export(ExportMode::updates(&s.doc.oplog_vv()))?)?;
    Ok(())
}

fn push(host: &LoroDoc, s: &Session) -> Result<()> {
    host.import(&s.doc.export(ExportMode::updates(&host.oplog_vv()))?)?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Canvas {
    pub fn new(id: &str, name: &str) -> Self {
        let doc = LoroDoc::new();
        configure(&doc);
        let meta = doc.get_map(META);
        meta.insert("id", id).unwrap();
        meta.insert("name", name).unwrap();
        doc.get_map(PEERS)
            .insert(&doc.peer_id().to_string(), "system")
            .unwrap();
        doc.commit_with(CommitOptions::new().commit_msg("system"));
        Self {
            doc,
            sessions: HashMap::new(),
        }
    }

    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::from_snapshot(bytes)?;
        configure(&doc);
        Ok(Self {
            doc,
            sessions: HashMap::new(),
        })
    }

    pub fn id(&self) -> String {
        get_str(&self.doc.get_map(META), "id").unwrap_or_default()
    }

    pub fn name(&self) -> String {
        get_str(&self.doc.get_map(META), "name").unwrap_or_default()
    }

    /// The authoritative replica, for subscriptions and rendering.
    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    pub fn version(&self) -> Version {
        Version(self.doc.oplog_frontiers())
    }

    pub fn vv(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::Snapshot)?)
    }

    /// Updates since `from` (another replica's [`Canvas::vv`]) for sync.
    pub fn export_updates(&self, from: &VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::updates(from))?)
    }

    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        Ok(())
    }

    // ---- transactions ----------------------------------------------------------------

    /// Apply commands as one transaction (one undo step) by `actor`.
    pub fn apply(
        &mut self,
        actor: &Actor,
        expected: Option<&Version>,
        cmds: &[Command],
    ) -> Result<TxResult> {
        self.transact(actor, expected, |tx| {
            cmds.iter().try_for_each(|c| tx.command(c))
        })
    }

    /// Run `f` as one transaction. `f` runs twice: first as a dry run that only validates
    /// (Loro can't roll back applied ops), then for real.
    pub(crate) fn transact(
        &mut self,
        actor: &Actor,
        expected: Option<&Version>,
        f: impl Fn(&mut Tx) -> Result<()>,
    ) -> Result<TxResult> {
        let changes = match expected {
            Some(v) => self.changes_by_others(v, &actor.id)?,
            None => vec![],
        };
        self.ensure_session(actor)?;
        let s = &self.sessions[&actor.id];
        f(&mut Tx::new(&s.doc, actor, true))?;
        let mut tx = Tx::new(&s.doc, actor, false);
        let res = f(&mut tx);
        let Tx {
            created,
            touched,
            mut warnings,
            ..
        } = tx;
        if res.is_ok() && touched.is_empty() {
            warnings.push("no changes".into());
        }
        if let Err(e) = res {
            // Validated but failed mid-way: the replica holds uncommitted partial ops.
            // Drop it so they never reach the canvas.
            self.sessions.remove(&actor.id);
            return Err(e);
        }
        let origin = format!("actor:{}", actor.id);
        s.doc
            .commit_with(CommitOptions::new().origin(&origin).commit_msg(&actor.id));
        push(&self.doc, s)?;
        let stale = expected.filter(|_| !changes.is_empty()).map(|since| {
            let conflicts = changes
                .iter()
                .flat_map(|c| c.nodes.iter().chain(&c.annotations))
                .filter(|id| touched.contains(*id))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            StaleReport {
                since: since.clone(),
                changes,
                conflicts,
            }
        });
        Ok(TxResult {
            version: self.version(),
            created,
            touched: touched.into_iter().collect(),
            warnings,
            stale,
        })
    }

    fn ensure_session(&mut self, actor: &Actor) -> Result<()> {
        if let Some(s) = self.sessions.get(&actor.id) {
            return pull(&self.doc, s);
        }
        let ok = !actor.id.is_empty()
            && actor
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
        if !ok {
            return Err(Error::Invalid(format!("bad actor id `{}`", actor.id)));
        }
        let doc = LoroDoc::new();
        configure(&doc);
        doc.import(&self.doc.export(ExportMode::Snapshot)?)?;
        let mut undo = UndoManager::new(&doc);
        undo.set_max_undo_steps(1000);
        undo.add_exclude_origin_prefix("sys:");
        undo.set_on_push(Some(Box::new(|_, span, _| UndoItemMeta {
            value: LoroValue::I64(span.start as i64),
            cursors: vec![],
        })));
        let info = serde_json::json!({ "name": actor.name, "kind": actor.kind });
        doc.get_map(ACTORS).insert(&actor.id, to_loro(&info))?;
        doc.get_map(PEERS)
            .insert(&doc.peer_id().to_string(), actor.id.as_str())?;
        doc.commit_with(
            CommitOptions::new()
                .origin("sys:actor")
                .commit_msg(&actor.id),
        );
        let s = Session { doc, undo };
        push(&self.doc, &s)?;
        self.sessions.insert(actor.id.clone(), s);
        Ok(())
    }

    // ---- per-actor undo --------------------------------------------------------------

    /// Undo `actor_id`'s most recent transaction (in this process). Returns false if
    /// there is nothing to undo.
    pub fn undo(&mut self, actor_id: &str) -> Result<bool> {
        self.undo_redo(actor_id, true)
    }

    pub fn redo(&mut self, actor_id: &str) -> Result<bool> {
        self.undo_redo(actor_id, false)
    }

    pub fn can_undo(&self, actor_id: &str) -> bool {
        self.sessions
            .get(actor_id)
            .is_some_and(|s| s.undo.can_undo())
    }

    fn undo_redo(&mut self, actor_id: &str, undo: bool) -> Result<bool> {
        let Some(s) = self.sessions.get_mut(actor_id) else {
            return Ok(false);
        };
        pull(&self.doc, s)?;
        let done = if undo { s.undo.undo()? } else { s.undo.redo()? };
        push(&self.doc, s)?;
        Ok(done)
    }

    /// Undo every transaction `actor_id` made after `since`. Returns how many.
    pub fn revert_since(&mut self, actor_id: &str, since: &Version) -> Result<usize> {
        let vv = self
            .doc
            .frontiers_to_vv(&since.0)
            .ok_or_else(|| Error::UnknownVersion(since.to_string()))?;
        let Some(s) = self.sessions.get_mut(actor_id) else {
            return Ok(0);
        };
        pull(&self.doc, s)?;
        let peer = s.doc.peer_id();
        let mut n = 0;
        while let Some(LoroValue::I64(start)) = s.undo.top_undo_value() {
            if vv.includes_id(ID::new(peer, start as i32)) || !s.undo.undo()? {
                break;
            }
            n += 1;
        }
        push(&self.doc, s)?;
        Ok(n)
    }

    // ---- staleness -------------------------------------------------------------------

    /// Who changed which nodes/annotations since `since`, grouped by actor.
    pub fn changes_since(&self, since: &Version) -> Result<Vec<ActorChanges>> {
        let vv = self
            .doc
            .frontiers_to_vv(&since.0)
            .ok_or_else(|| Error::UnknownVersion(since.to_string()))?;
        let mut by: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
        let peers = self.doc.get_map(PEERS);
        let actor_of = |peer: u64| get_str(&peers, &peer.to_string()).unwrap_or("unknown".into());
        let anns_root = self.doc.get_list(ANNS).id();
        let spans = self
            .doc
            .find_id_spans_between(&since.0, &self.doc.oplog_frontiers());
        for (&peer, span) in spans.forward.iter() {
            let mut c = span.start;
            while c < span.end {
                let Some(ch) = self.doc.get_change(ID::new(peer, c)) else {
                    break;
                };
                let end = (ch.id.counter + ch.len as i32).min(span.end);
                let entry = by.entry(actor_of(peer)).or_default();
                for cid in self
                    .doc
                    .get_changed_containers_in(ID::new(peer, c), (end - c) as usize)
                {
                    match self.owner_of(&cid, &anns_root) {
                        Some(Owner::Node(id)) => entry.0.insert(id),
                        Some(Owner::Ann(id)) => entry.1.insert(id),
                        None => false,
                    };
                }
                c = end;
            }
        }
        // Tree creates/moves/deletes don't touch the node's meta map; use last-move ids.
        let tree = self.tree();
        for n in tree.get_nodes(true) {
            if let Some(mv) = tree.get_last_move_id(&n.id)
                && !vv.includes_id(mv)
                && let Some(id) = self.node_id_of(n.id)
            {
                by.entry(actor_of(mv.peer)).or_default().0.insert(id);
            }
        }
        Ok(by
            .into_iter()
            .filter(|(_, (n, a))| !n.is_empty() || !a.is_empty())
            .map(|(actor, (n, a))| ActorChanges {
                actor,
                nodes: n.into_iter().collect(),
                annotations: a.into_iter().collect(),
            })
            .collect())
    }

    fn changes_by_others(&self, since: &Version, actor: &str) -> Result<Vec<ActorChanges>> {
        let mut v = self.changes_since(since)?;
        v.retain(|c| c.actor != actor);
        Ok(v)
    }

    fn owner_of(&self, cid: &ContainerID, anns_root: &ContainerID) -> Option<Owner> {
        // Path entries are (container, how it's reached from the previous entry), root first.
        let path = self.doc.get_path_to_container(cid)?;
        if let Some(t) = path.iter().find_map(|(_, i)| match i {
            Index::Node(t) => Some(*t),
            _ => None,
        }) {
            return self.node_id_of(t).map(Owner::Node);
        }
        match (path.first(), path.get(1)) {
            (Some((root, _)), Some((ann, _))) if root == anns_root => {
                match self.doc.get_container(ann.clone())? {
                    Container::Map(m) => get_str(&m, "id").map(Owner::Ann),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ---- reads -----------------------------------------------------------------------

    pub(crate) fn tree(&self) -> LoroTree {
        self.doc.get_tree(NODES)
    }

    fn node_id_of(&self, t: TreeID) -> Option<String> {
        get_str(&self.tree().get_meta(t).ok()?, "id")
    }

    pub(crate) fn tree_id(&self, id: &str) -> Option<TreeID> {
        let tree = self.tree();
        tree.get_nodes(false).into_iter().map(|n| n.id).find(|t| {
            tree.get_meta(*t)
                .ok()
                .and_then(|m| get_str(&m, "id"))
                .as_deref()
                == Some(id)
        })
    }

    pub(crate) fn children_of(&self, parent: TreeParentId) -> Vec<TreeID> {
        self.tree().children(parent).unwrap_or_default()
    }

    pub(crate) fn read_node(&self, t: TreeID) -> Option<Node> {
        let tree = self.tree();
        let Json::Object(mut m) = to_json(&tree.get_meta(t).ok()?.get_deep_value()) else {
            return None;
        };
        let mut take_str = |k: &str| match m.remove(k) {
            Some(Json::String(s)) => s,
            _ => String::new(),
        };
        let id = take_str("id");
        let kind = NodeKind::parse(&take_str("type"))?;
        let name = take_str("name");
        let created_by = take_str("created_by");
        let updated_by = take_str("updated_by");
        let text = match m.remove("text") {
            Some(Json::String(s)) => Some(s),
            _ => kind.has_text().then(String::new),
        };
        let mut num = |k: &str| m.remove(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let (x, y, w, h, rot) = (num("x"), num("y"), num("w"), num("h"), num("rot"));
        let locked = m
            .remove("locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let parent = match tree.parent(t) {
            Some(TreeParentId::Node(p)) => self.node_id_of(p),
            _ => None,
        };
        let children = self
            .children_of(TreeParentId::Node(t))
            .into_iter()
            .filter_map(|c| self.node_id_of(c))
            .collect();
        Some(Node {
            id,
            kind,
            name,
            x,
            y,
            w,
            h,
            rot,
            locked,
            created_by,
            updated_by,
            parent,
            children,
            text,
            props: m.into_iter().collect(),
        })
    }

    pub fn node(&self, id: &str) -> Option<Node> {
        self.read_node(self.tree_id(id)?)
    }

    /// All nodes, depth-first in z-order (bottom first).
    pub fn nodes(&self) -> Vec<Node> {
        fn walk(c: &Canvas, p: TreeParentId, out: &mut Vec<Node>) {
            for t in c.children_of(p) {
                out.extend(c.read_node(t));
                walk(c, TreeParentId::Node(t), out);
            }
        }
        let mut out = vec![];
        walk(self, TreeParentId::Root, &mut out);
        out
    }

    pub fn annotations(&self) -> Vec<Annotation> {
        let list = self.doc.get_list(ANNS);
        (0..list.len())
            .filter_map(|i| match list.get(i)? {
                ValueOrContainer::Container(Container::Map(m)) => read_annotation(&m),
                _ => None,
            })
            .collect()
    }

    pub fn annotation(&self, id: &str) -> Option<Annotation> {
        self.annotations().into_iter().find(|a| a.id == id)
    }

    pub fn asset(&self, id: &str) -> Option<Asset> {
        match self.doc.get_map(ASSETS).get(id)? {
            ValueOrContainer::Value(v) => serde_json::from_value(to_json(&v)).ok(),
            _ => None,
        }
    }

    /// Display name of an actor that has written to this canvas.
    pub fn actor_name(&self, id: &str) -> Option<String> {
        match self.doc.get_map(ACTORS).get(id)? {
            ValueOrContainer::Value(v) => to_json(&v)["name"].as_str().map(String::from),
            _ => None,
        }
    }
}

enum Owner {
    Node(String),
    Ann(String),
}

fn read_annotation(m: &LoroMap) -> Option<Annotation> {
    let Json::Object(v) = to_json(&m.get_deep_value()) else {
        return None;
    };
    let s = |k: &str| v.get(k).and_then(Json::as_str).map(String::from);
    Some(Annotation {
        id: s("id")?,
        node_id: s("node_id")?,
        author: s("author").unwrap_or_default(),
        status: Status::parse(&s("status")?)?,
        anchor: serde_json::from_value(v.get("anchor")?.clone()).ok()?,
        thread: serde_json::from_value(v.get("thread").cloned().unwrap_or_default())
            .unwrap_or_default(),
        assignee: s("assignee"),
        outcome: serde_json::from_value(v.get("outcome").cloned().unwrap_or_default())
            .unwrap_or_default(),
        created_at: v.get("created_at").and_then(Json::as_i64).unwrap_or(0),
    })
}

pub(crate) fn get_str(m: &LoroMap, key: &str) -> Option<String> {
    match m.get(key)? {
        ValueOrContainer::Value(LoroValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

pub(crate) fn to_loro(v: &Json) -> LoroValue {
    match v {
        Json::Null => LoroValue::Null,
        Json::Bool(b) => LoroValue::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => LoroValue::I64(i),
            None => LoroValue::Double(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => s.as_str().into(),
        Json::Array(a) => a.iter().map(to_loro).collect::<Vec<_>>().into(),
        Json::Object(o) => o
            .iter()
            .map(|(k, v)| (k.clone(), to_loro(v)))
            .collect::<HashMap<_, _>>()
            .into(),
    }
}

pub(crate) fn to_json(v: &LoroValue) -> Json {
    match v {
        LoroValue::Null | LoroValue::Binary(_) | LoroValue::Container(_) => Json::Null,
        LoroValue::Bool(b) => Json::Bool(*b),
        LoroValue::Double(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        LoroValue::I64(i) => Json::from(*i),
        LoroValue::String(s) => Json::String(s.to_string()),
        LoroValue::List(l) => Json::Array(l.iter().map(to_json).collect()),
        LoroValue::Map(m) => Json::Object(m.iter().map(|(k, v)| (k.clone(), to_json(v))).collect()),
    }
}

/// Words that can't be ids because they'd be ambiguous in the text format.
pub(crate) fn is_reserved(s: &str) -> bool {
    NodeKind::parse(s).is_some()
        || Status::parse(s).is_some()
        || ["root", "style", "delete", "null", "true", "false"].contains(&s)
}

fn check_id(id: &str) -> Result<()> {
    let ok = (1..=32).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && !is_reserved(id);
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "bad id `{id}` (use [a-z0-9_-], not a keyword)"
        )))
    }
}

/// 6-char base36 id derived from a Loro op id (unique per replica op), with a salt for
/// the rare collision.
fn short_id(peer: u64, counter: i32, salt: u64) -> String {
    let mut h = peer ^ ((counter as u64) << 32 | salt).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (0..6)
        .map(|_| {
            let d = (h % 36) as u32;
            h /= 36;
            char::from_digit(d, 36).unwrap()
        })
        .collect()
}

const FAKE_PEER: u64 = u64::MAX - 1;

/// A transaction in progress on an actor's replica. In `dry` mode it validates against
/// an overlay index and writes nothing.
pub(crate) struct Tx<'a> {
    doc: &'a LoroDoc,
    tree: LoroTree,
    anns: LoroList,
    actor: &'a Actor,
    dry: bool,
    nodes: HashMap<String, (TreeID, NodeKind)>,
    ann_ids: HashSet<String>,
    fake: i32,
    pub created: Vec<String>,
    pub touched: BTreeSet<String>,
    pub warnings: Vec<String>,
}

impl<'a> Tx<'a> {
    fn new(doc: &'a LoroDoc, actor: &'a Actor, dry: bool) -> Self {
        let tree = doc.get_tree(NODES);
        let nodes = tree
            .get_nodes(false)
            .into_iter()
            .filter_map(|n| {
                let m = tree.get_meta(n.id).ok()?;
                Some((
                    get_str(&m, "id")?,
                    (n.id, NodeKind::parse(&get_str(&m, "type")?)?),
                ))
            })
            .collect();
        let anns = doc.get_list(ANNS);
        let ann_ids = (0..anns.len())
            .filter_map(|i| match anns.get(i)? {
                ValueOrContainer::Container(Container::Map(m)) => get_str(&m, "id"),
                _ => None,
            })
            .collect();
        Self {
            doc,
            tree,
            anns,
            actor,
            dry,
            nodes,
            ann_ids,
            fake: 0,
            created: vec![],
            touched: BTreeSet::new(),
            warnings: vec![],
        }
    }

    pub fn command(&mut self, c: &Command) -> Result<()> {
        match c {
            Command::CreateNode {
                id,
                kind,
                parent,
                index,
                name,
                props,
                text,
            } => {
                let parent = parent.as_deref();
                self.create_node(
                    id.as_deref(),
                    *kind,
                    parent,
                    *index,
                    name,
                    props,
                    text.as_deref(),
                )?;
            }
            Command::UpdateNode { id, props } => self.update_node(id, props)?,
            Command::SetText { id, text } => self.set_text(id, text)?,
            Command::MoveNode { id, parent, index } => {
                self.move_node(id, parent.as_deref(), *index)?
            }
            Command::DeleteNode { id } => self.delete_node(id)?,
            Command::PutAsset(a) => self.put_asset(a)?,
            Command::AddAnnotation {
                id,
                node_id,
                anchor,
                text,
                assignee,
            } => {
                self.add_annotation(
                    id.as_deref(),
                    node_id,
                    anchor,
                    Status::Open,
                    text.as_deref(),
                    assignee.as_deref(),
                )?;
            }
            Command::UpdateAnnotation {
                id,
                status,
                anchor,
                assignee,
                outcome,
            } => {
                let assignee = assignee.as_ref().map(|a| a.as_deref());
                self.update_annotation(id, *status, anchor.as_ref(), assignee, outcome.as_deref())?
            }
            Command::Reply { id, text } => self.reply(id, text)?,
            Command::DeleteAnnotation { id } => self.delete_annotation(id)?,
        }
        Ok(())
    }

    pub fn has_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    pub fn has_annotation(&self, id: &str) -> bool {
        self.ann_ids.contains(id)
    }

    pub fn node_kind(&self, id: &str) -> Result<NodeKind> {
        Ok(self.node(id)?.1)
    }

    fn node(&self, id: &str) -> Result<(TreeID, NodeKind)> {
        self.nodes
            .get(id)
            .copied()
            .ok_or_else(|| Error::UnknownNode(id.into()))
    }

    fn gen_id(&self, peer: u64, counter: i32) -> String {
        (0..)
            .map(|salt| short_id(peer, counter, salt))
            .find(|id| {
                !self.nodes.contains_key(id) && !self.ann_ids.contains(id) && !is_reserved(id)
            })
            .unwrap()
    }

    fn fake_id(&mut self) -> (String, TreeID) {
        self.fake += 1;
        (format!("~{}", self.fake), TreeID::new(FAKE_PEER, self.fake))
    }

    fn check_new_id(&self, id: Option<&str>) -> Result<()> {
        if let Some(id) = id {
            check_id(id)?;
            if self.nodes.contains_key(id) || self.ann_ids.contains(id) {
                return Err(Error::Invalid(format!("id `{id}` already exists")));
            }
        }
        Ok(())
    }

    /// Validate props and convert to Loro values (`None` = delete).
    fn check_props(
        &mut self,
        kind: NodeKind,
        props: &Props,
    ) -> Result<Vec<(String, Option<LoroValue>)>> {
        let mut out = vec![];
        for (k, v) in props {
            let val = match (k.as_str(), v) {
                (_, Json::Null) => None,
                ("x" | "y" | "w" | "h" | "rot", Json::Number(n)) => {
                    Some(LoroValue::Double(n.as_f64().unwrap_or(0.0)))
                }
                ("name", Json::String(_)) | ("locked", Json::Bool(_)) => Some(to_loro(v)),
                ("x" | "y" | "w" | "h" | "rot" | "name" | "locked", _) => {
                    return Err(Error::Invalid(format!("bad value for `{k}`: {v}")));
                }
                ("id" | "type" | "created_by" | "updated_by" | "text", _) => {
                    return Err(Error::Invalid(format!("`{k}` can't be set as a prop")));
                }
                _ => match kind.prop_type(k) {
                    Some(t) => Some(to_loro(&t.coerce(k, v.clone())?)),
                    None => {
                        if !k
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                            || k.is_empty()
                        {
                            return Err(Error::Invalid(format!("bad prop name `{k}`")));
                        }
                        if !self.dry {
                            self.warnings.push(format!("unknown prop `{k}` on {kind}"));
                        }
                        Some(to_loro(v))
                    }
                },
            };
            out.push((k.clone(), val));
        }
        Ok(out)
    }

    fn write_props(&self, meta: &LoroMap, vals: Vec<(String, Option<LoroValue>)>) -> Result<()> {
        for (k, v) in vals {
            match v {
                Some(v) => meta.insert(&k, v)?,
                None => meta.delete(&k)?,
            }
        }
        meta.insert("updated_by", self.actor.id.as_str())?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_node(
        &mut self,
        id: Option<&str>,
        kind: NodeKind,
        parent: Option<&str>,
        index: Option<usize>,
        name: &str,
        props: &Props,
        text: Option<&str>,
    ) -> Result<String> {
        self.check_new_id(id)?;
        let parent = match parent {
            Some(p) => TreeParentId::Node(self.node(p)?.0),
            None => TreeParentId::Root,
        };
        if text.is_some() && !kind.has_text() {
            return Err(Error::Invalid(format!("{kind} nodes have no text")));
        }
        let vals = self.check_props(kind, props)?;
        let (id, t) = if self.dry {
            let (fid, t) = self.fake_id();
            (id.map_or(fid, String::from), t)
        } else {
            let t = match index {
                Some(i) => {
                    let n = self.tree.children_num(parent).unwrap_or(0);
                    self.tree.create_at(parent, i.min(n))?
                }
                None => self.tree.create(parent)?,
            };
            let id = id.map_or_else(|| self.gen_id(t.peer, t.counter), String::from);
            let meta = self.tree.get_meta(t)?;
            meta.insert("id", id.as_str())?;
            meta.insert("type", kind.to_string())?;
            if !name.is_empty() {
                meta.insert("name", name)?;
            }
            meta.insert("created_by", self.actor.id.as_str())?;
            self.write_props(&meta, vals)?;
            if let Some(s) = text {
                meta.ensure_mergeable_text("text")?.insert(0, s)?;
            }
            (id, t)
        };
        self.nodes.insert(id.clone(), (t, kind));
        self.created.push(id.clone());
        self.touched.insert(id.clone());
        Ok(id)
    }

    pub fn update_node(&mut self, id: &str, props: &Props) -> Result<()> {
        let (t, kind) = self.node(id)?;
        let mut vals = self.check_props(kind, props)?;
        // Skip values that are already set, so echoing a read back is a no-op.
        if t.peer != FAKE_PEER {
            let meta = self.tree.get_meta(t)?;
            vals.retain(|(k, v)| {
                let cur = match meta.get(k) {
                    Some(ValueOrContainer::Value(v)) => Some(v),
                    _ => None,
                };
                cur != *v
            });
        }
        if vals.is_empty() {
            return Ok(());
        }
        self.touched.insert(id.into());
        if !self.dry {
            self.write_props(&self.tree.get_meta(t)?, vals)?;
        }
        Ok(())
    }

    pub fn set_text(&mut self, id: &str, text: &str) -> Result<()> {
        let (t, kind) = self.node(id)?;
        if !kind.has_text() {
            return Err(Error::Invalid(format!("{kind} nodes have no text")));
        }
        if t.peer != FAKE_PEER
            && let Some(ValueOrContainer::Container(Container::Text(cur))) =
                self.tree.get_meta(t)?.get("text")
            && cur.to_string() == text
        {
            return Ok(());
        }
        self.touched.insert(id.into());
        if !self.dry {
            let meta = self.tree.get_meta(t)?;
            let lt = meta.ensure_mergeable_text("text")?;
            lt.update(text, UpdateOptions::default())
                .map_err(|_| Error::Invalid("text diff timed out".into()))?;
            meta.insert("updated_by", self.actor.id.as_str())?;
        }
        Ok(())
    }

    pub fn move_node(
        &mut self,
        id: &str,
        parent: Option<&str>,
        index: Option<usize>,
    ) -> Result<()> {
        let (t, _) = self.node(id)?;
        let target = match parent {
            Some(p) => TreeParentId::Node(self.node(p)?.0),
            None => TreeParentId::Root,
        };
        // Cycle check (fake nodes from this dry run have no tree ancestry).
        let mut cur = target;
        while let TreeParentId::Node(p) = cur {
            if p == t {
                return Err(Error::Invalid(format!(
                    "can't move `{id}` into its own subtree"
                )));
            }
            if p.peer == FAKE_PEER {
                break;
            }
            cur = self.tree.parent(p).unwrap_or(TreeParentId::Root);
        }
        self.touched.insert(id.into());
        if !self.dry {
            match index {
                Some(i) => {
                    let mut n = self.tree.children_num(target).unwrap_or(0);
                    if self.tree.parent(t) == Some(target) {
                        n -= 1;
                    }
                    self.tree.mov_to(t, target, i.min(n))?;
                }
                None => self.tree.mov(t, target)?,
            }
            self.tree
                .get_meta(t)?
                .insert("updated_by", self.actor.id.as_str())?;
        }
        Ok(())
    }

    pub fn delete_node(&mut self, id: &str) -> Result<()> {
        let (t, _) = self.node(id)?;
        let mut gone = HashSet::from([t]);
        let mut stack = vec![t];
        while let Some(p) = stack.pop() {
            for c in self
                .tree
                .children(TreeParentId::Node(p))
                .unwrap_or_default()
            {
                gone.insert(c);
                stack.push(c);
            }
        }
        self.nodes.retain(|_, (x, _)| !gone.contains(x));
        self.touched.insert(id.into());
        if !self.dry {
            self.tree.delete(t)?;
        }
        Ok(())
    }

    pub fn put_asset(&mut self, a: &Asset) -> Result<()> {
        if a.id.is_empty() {
            return Err(Error::Invalid("asset id is empty".into()));
        }
        // Content-addressed and immutable: the first record wins.
        let assets = self.doc.get_map(ASSETS);
        if assets.get(&a.id).is_none() {
            self.touched.insert(a.id.clone());
            if !self.dry {
                assets.insert(&a.id, to_loro(&serde_json::to_value(a)?))?;
            }
        }
        Ok(())
    }

    fn find_ann(&self, id: &str) -> Result<Option<(usize, LoroMap)>> {
        if !self.ann_ids.contains(id) {
            return Err(Error::UnknownAnnotation(id.into()));
        }
        Ok((0..self.anns.len()).find_map(|i| match self.anns.get(i)? {
            ValueOrContainer::Container(Container::Map(m))
                if get_str(&m, "id").as_deref() == Some(id) =>
            {
                Some((i, m))
            }
            _ => None,
        }))
    }

    fn check_anchor(a: &Anchor) -> Result<()> {
        let poly_ok = |p: &Vec<[f64; 2]>| p.len() >= 3;
        let ok = match a {
            Anchor::Polygon { points } => poly_ok(points),
            Anchor::Range { t0, t1 } => t0 <= t1,
            Anchor::TextRange { start, end } => start <= end,
            Anchor::Frame {
                region: Some(Region::Polygon { points }),
                ..
            }
            | Anchor::Page {
                region: Some(Region::Polygon { points }),
                ..
            } => poly_ok(points),
            _ => true,
        };
        if ok {
            Ok(())
        } else {
            Err(Error::Invalid(format!("bad anchor {a:?}")))
        }
    }

    pub fn add_annotation(
        &mut self,
        id: Option<&str>,
        node_id: &str,
        anchor: &Anchor,
        status: Status,
        text: Option<&str>,
        assignee: Option<&str>,
    ) -> Result<String> {
        self.check_new_id(id)?;
        self.node(node_id)?;
        Self::check_anchor(anchor)?;
        let id = if self.dry {
            id.map_or_else(|| self.fake_id().0, String::from)
        } else {
            let m = self.anns.push_container(LoroMap::new())?;
            let id = match (id, m.id()) {
                (Some(id), _) => id.to_string(),
                (None, ContainerID::Normal { peer, counter, .. }) => self.gen_id(peer, counter),
                (None, _) => unreachable!("child containers are never root"),
            };
            m.insert("id", id.as_str())?;
            m.insert("node_id", node_id)?;
            m.insert("author", self.actor.id.as_str())?;
            m.insert("status", status.as_str())?;
            m.insert("anchor", to_loro(&serde_json::to_value(anchor)?))?;
            m.insert("created_at", now_ms())?;
            if let Some(a) = assignee {
                m.insert("assignee", a)?;
            }
            m.ensure_mergeable_list("outcome")?;
            let thread = m.ensure_mergeable_list("thread")?;
            if let Some(t) = text {
                thread.push(self.message(t)?)?;
            }
            id
        };
        self.ann_ids.insert(id.clone());
        self.created.push(id.clone());
        self.touched.insert(id.clone());
        Ok(id)
    }

    fn message(&self, text: &str) -> Result<LoroValue> {
        let m = Message {
            author: self.actor.id.clone(),
            text: text.into(),
            at: now_ms(),
        };
        Ok(to_loro(&serde_json::to_value(m)?))
    }

    pub fn update_annotation(
        &mut self,
        id: &str,
        status: Option<Status>,
        anchor: Option<&Anchor>,
        assignee: Option<Option<&str>>,
        outcome: Option<&[String]>,
    ) -> Result<()> {
        let found = self.find_ann(id)?;
        if let Some(a) = anchor {
            Self::check_anchor(a)?;
        }
        for n in outcome.unwrap_or_default() {
            self.node(n)?;
        }
        // Drop fields that already have the requested value.
        let cur = found.as_ref().and_then(|(_, m)| read_annotation(m));
        let cur = cur.as_ref();
        let status = status.filter(|s| cur.is_none_or(|c| c.status != *s));
        let anchor = anchor.filter(|a| cur.is_none_or(|c| c.anchor != **a));
        let assignee = assignee.filter(|a| cur.is_none_or(|c| c.assignee.as_deref() != *a));
        let outcome = outcome.filter(|o| cur.is_none_or(|c| c.outcome != *o));
        if status.is_none() && anchor.is_none() && assignee.is_none() && outcome.is_none() {
            return Ok(());
        }
        self.touched.insert(id.into());
        let Some((_, m)) = found.filter(|_| !self.dry) else {
            return Ok(());
        };
        if let Some(s) = status {
            m.insert("status", s.as_str())?;
        }
        if let Some(a) = anchor {
            m.insert("anchor", to_loro(&serde_json::to_value(a)?))?;
        }
        match assignee {
            Some(Some(a)) => m.insert("assignee", a)?,
            Some(None) => m.delete("assignee")?,
            None => {}
        }
        if let Some(want) = outcome {
            let list = m.ensure_mergeable_list("outcome")?;
            // Remove ids not wanted (back to front), then append missing ones.
            let have: Vec<String> = (0..list.len())
                .map(|i| match list.get(i) {
                    Some(ValueOrContainer::Value(LoroValue::String(s))) => s.to_string(),
                    _ => String::new(),
                })
                .collect();
            for (i, h) in have.iter().enumerate().rev() {
                if !want.contains(h) {
                    list.delete(i, 1)?;
                }
            }
            for w in want.iter().filter(|w| !have.contains(w)) {
                list.push(w.as_str())?;
            }
        }
        Ok(())
    }

    pub fn reply(&mut self, id: &str, text: &str) -> Result<()> {
        let found = self.find_ann(id)?;
        self.touched.insert(id.into());
        if let Some((_, m)) = found.filter(|_| !self.dry) {
            m.ensure_mergeable_list("thread")?
                .push(self.message(text)?)?;
        }
        Ok(())
    }

    pub fn delete_annotation(&mut self, id: &str) -> Result<()> {
        let found = self.find_ann(id)?;
        self.ann_ids.remove(id);
        self.touched.insert(id.into());
        if let Some((i, _)) = found.filter(|_| !self.dry) {
            self.anns.delete(i, 1)?;
        }
        Ok(())
    }
}
