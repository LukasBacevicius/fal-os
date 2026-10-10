//! Incremental doc → render mapping.
//!
//! [`ChangeFeed`] subscribes to the canvas' Loro doc and records *which* tree nodes changed
//! (created / moved / deleted / meta edited) and whether annotations or assets changed. It does
//! not read the doc inside the callback; [`Scene::apply`] later re-reads only the touched nodes
//! and forwards minimal operations to a [`RenderSink`] (the engine in the browser, a mock in
//! tests). Local transactions and remote imports go through the same path, so the canvas never
//! rebuilds everything per change.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use falos_doc::loro::event::{Diff, DiffEvent};
use falos_doc::loro::{ContainerID, Index, Subscription, TreeExternalDiff, TreeID, TreeParentId};
use falos_doc::{Canvas, Node, NodeKind};
use falos_engine::Handle;

/// What changed in the doc since the last [`ChangeFeed::take`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocChanges {
    /// Nodes created, moved, or with edited meta (props, name, text).
    pub dirty: HashSet<TreeID>,
    pub deleted: HashSet<TreeID>,
    /// Tree structure changed (z-order / parents): the draw order must be recomputed.
    pub structure: bool,
    pub annotations: bool,
    pub assets: bool,
    /// Anything at all changed.
    pub any: bool,
}

impl DocChanges {
    fn record(&mut self, e: &DiffEvent) {
        for d in &e.events {
            self.any = true;
            if let Diff::Tree(td) = &d.diff {
                self.structure = true;
                for item in &td.diff {
                    match item.action {
                        TreeExternalDiff::Delete { .. } => {
                            self.dirty.remove(&item.target);
                            self.deleted.insert(item.target);
                        }
                        _ => {
                            self.deleted.remove(&item.target);
                            self.dirty.insert(item.target);
                        }
                    }
                }
                continue;
            }
            if let Some(t) = d.path.iter().find_map(|(_, i)| match i {
                Index::Node(t) => Some(*t),
                _ => None,
            }) {
                self.dirty.insert(t);
                continue;
            }
            let root = d.path.first().map(|(c, _)| c).unwrap_or(d.target);
            if let ContainerID::Root { name, .. } = root {
                match name.as_str() {
                    "annotations" => self.annotations = true,
                    "assets" => self.assets = true,
                    _ => {}
                }
            }
        }
    }
}

/// Collects doc changes from a Loro subscription. Keep it alive as long as the canvas.
pub struct ChangeFeed {
    changes: Arc<Mutex<DocChanges>>,
    _sub: Subscription,
}

impl ChangeFeed {
    pub fn new(canvas: &Canvas) -> Self {
        let changes: Arc<Mutex<DocChanges>> = Arc::default();
        let sink = changes.clone();
        let sub = canvas.doc().subscribe_root(Arc::new(move |e| {
            if let Ok(mut c) = sink.lock() {
                c.record(&e);
            }
        }));
        Self { changes, _sub: sub }
    }

    pub fn take(&self) -> DocChanges {
        self.changes.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default()
    }
}

/// A prop value as plain text (strings unquoted).
fn json_str(v: &serde_json::Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), String::from)
}

/// The render-relevant view of one doc node.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneNode {
    pub id: String,
    pub tree: TreeID,
    pub kind: NodeKind,
    pub name: String,
    /// World rect `[x, y, w, h]`.
    pub rect: [f32; 4],
    pub asset: Option<String>,
    pub parent: Option<String>,
    /// Text content (text / note nodes).
    pub text: Option<String>,
    /// Props that change how a card is drawn (note color, generation status/progress/model/
    /// prompt/error), as a compact string. A change refetches the card's pixels.
    pub card: String,
    /// Engine handle, `None` for nodes that are not drawn (groups, connectors, empty rects).
    pub handle: Option<Handle>,
}

impl SceneNode {
    /// `rect` is in the node's parent space; [`Scene::apply`] offsets it to world space.
    pub fn from_doc(n: &Node, tree: TreeID) -> Self {
        let card = match n.kind {
            NodeKind::Note => n.props.get("color").map(json_str).unwrap_or_default(),
            NodeKind::Generation => {
                let p = |k: &str| n.props.get(k).map(json_str).unwrap_or_default();
                let prompt = n.props.get("params").and_then(|v| v.get("prompt")).and_then(|v| v.as_str()).unwrap_or("");
                let prompt: String = prompt.chars().take(400).collect();
                format!("{}\u{1}{}\u{1}{}\u{1}{}\u{1}{prompt}", p("status"), p("progress"), p("model"), p("error"))
            }
            _ => String::new(),
        };
        Self {
            card,
            id: n.id.clone(),
            tree,
            kind: n.kind,
            name: n.name.clone(),
            rect: [n.x as f32, n.y as f32, n.w as f32, n.h as f32],
            asset: n.props.get("asset").and_then(|v| v.as_str()).map(String::from),
            parent: n.parent.clone(),
            text: n.text.clone(),
            handle: None,
        }
    }

    /// Whether the engine draws this node.
    pub fn renderable(&self) -> bool {
        !matches!(self.kind, NodeKind::Group | NodeKind::Connector) && self.rect[2] > 0.0 && self.rect[3] > 0.0
    }

    pub fn contains(&self, p: [f32; 2]) -> bool {
        let r = self.rect;
        p[0] >= r[0] && p[1] >= r[1] && p[0] <= r[0] + r[2] && p[1] <= r[1] + r[3]
    }

    /// Node-local normalized coordinates (0..1) of a world point.
    pub fn to_local(&self, p: [f32; 2]) -> [f32; 2] {
        let r = self.rect;
        [(p[0] - r[0]) / r[2].max(1e-6), (p[1] - r[1]) / r[3].max(1e-6)]
    }

    /// World point of node-local normalized coordinates.
    pub fn to_world(&self, l: [f32; 2]) -> [f32; 2] {
        let r = self.rect;
        [r[0] + l[0] * r[2], r[1] + l[1] * r[3]]
    }
}

/// Receives the minimal render operations produced by [`Scene::apply`].
pub trait RenderSink {
    fn add(&mut self, n: &SceneNode) -> Handle;
    /// `content` is true when the node's pixels must be refetched (asset or text changed).
    fn update(&mut self, h: Handle, n: &SceneNode, content: bool);
    fn remove(&mut self, h: Handle, n: &SceneNode);
    /// Full draw order, bottom first.
    fn set_order(&mut self, order: &[Handle]);
}

#[derive(Default)]
pub struct Scene {
    nodes: HashMap<String, SceneNode>,
    by_tree: HashMap<TreeID, String>,
    /// Node ids in draw order (depth-first z-order, bottom first).
    order: Vec<String>,
}

impl Scene {
    pub fn get(&self, id: &str) -> Option<&SceneNode> {
        self.nodes.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut SceneNode> {
        self.nodes.get_mut(id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Ids in draw order, bottom first.
    pub fn order(&self) -> &[String] {
        &self.order
    }

    pub fn iter(&self) -> impl Iterator<Item = &SceneNode> {
        self.order.iter().filter_map(|id| self.nodes.get(id))
    }

    /// Topmost drawn node under a world point.
    pub fn hit_test(&self, p: [f32; 2]) -> Option<&SceneNode> {
        self.order.iter().rev().filter_map(|id| self.nodes.get(id)).find(|n| n.handle.is_some() && n.contains(p))
    }

    /// Drawn nodes intersecting a world rect (marquee).
    pub fn intersecting(&self, r: [f32; 4]) -> Vec<String> {
        self.iter()
            .filter(|n| n.handle.is_some())
            .filter(|n| {
                let a = n.rect;
                a[0] < r[0] + r[2] && a[0] + a[2] > r[0] && a[1] < r[1] + r[3] && a[1] + a[3] > r[1]
            })
            .map(|n| n.id.clone())
            .collect()
    }

    /// World bounds of the given nodes (`None` if none exist).
    pub fn bounds_of<'a>(&self, ids: impl IntoIterator<Item = &'a String>) -> Option<[f32; 4]> {
        let mut b: Option<[f32; 4]> = None;
        for n in ids.into_iter().filter_map(|id| self.nodes.get(id)) {
            let r = n.rect;
            b = Some(match b {
                None => r,
                Some(o) => {
                    let (x0, y0) = (o[0].min(r[0]), o[1].min(r[1]));
                    let (x1, y1) = ((o[0] + o[2]).max(r[0] + r[2]), (o[1] + o[3]).max(r[1] + r[3]));
                    [x0, y0, x1 - x0, y1 - y0]
                }
            });
        }
        b
    }

    /// Removes everything (e.g. before loading another snapshot).
    pub fn clear(&mut self, sink: &mut impl RenderSink) {
        for n in self.nodes.values() {
            if let Some(h) = n.handle {
                sink.remove(h, n);
            }
        }
        self.nodes.clear();
        self.by_tree.clear();
        self.order.clear();
    }

    /// Full sync from the doc (initial load). Equivalent to applying "every node is dirty".
    pub fn rebuild(&mut self, canvas: &Canvas, sink: &mut impl RenderSink) {
        let tree = canvas.tree();
        let changes = DocChanges {
            dirty: tree.get_nodes(false).into_iter().map(|n| n.id).collect(),
            structure: true,
            any: true,
            ..Default::default()
        };
        self.clear(sink);
        self.apply(canvas, &changes, sink);
    }

    /// Ids of `id`'s descendants currently in the scene (draw order is depth-first, so the
    /// subtree follows the node contiguously).
    pub fn descendants(&self, id: &str) -> Vec<String> {
        let Some(i) = self.order.iter().position(|o| o == id) else { return Vec::new() };
        let mut inside: HashSet<&str> = HashSet::from([id]);
        let mut out = Vec::new();
        for o in &self.order[i + 1..] {
            match self.nodes.get(o).and_then(|n| n.parent.as_deref()) {
                Some(p) if inside.contains(p) => {
                    inside.insert(o);
                    out.push(o.clone());
                }
                _ => break,
            }
        }
        out
    }

    /// World position of a node's parent (its origin); `[0, 0]` for top-level nodes.
    pub fn parent_origin(&self, id: &str) -> [f32; 2] {
        self.nodes
            .get(id)
            .and_then(|n| n.parent.as_deref())
            .and_then(|p| self.nodes.get(p))
            .map_or([0.0, 0.0], |p| [p.rect[0], p.rect[1]])
    }

    /// Applies recorded changes: re-reads only touched nodes and emits minimal sink ops.
    /// Doc positions are parent-relative; scene rects are world rects, so a moved node's
    /// descendants are refreshed too, parents before children.
    pub fn apply(&mut self, canvas: &Canvas, ch: &DocChanges, sink: &mut impl RenderSink) {
        let tree = canvas.tree();
        let mut gone: Vec<TreeID> = ch.deleted.iter().copied().collect();
        if !ch.deleted.is_empty() {
            // Deleting a node deletes its subtree; the diff only names the subtree root.
            gone.extend(self.by_tree.keys().copied().filter(|t| tree.is_node_deleted(t).unwrap_or(true)));
        }
        for t in gone {
            self.remove_tree(t, sink);
        }
        let mut todo: HashSet<TreeID> = HashSet::new();
        let mut stack: Vec<TreeID> = Vec::new();
        for &t in &ch.dirty {
            if tree.is_node_deleted(&t).unwrap_or(true) {
                self.remove_tree(t, sink);
                continue;
            }
            stack.push(t);
        }
        while let Some(t) = stack.pop() {
            if todo.insert(t) {
                stack.extend(tree.children(TreeParentId::Node(t)).unwrap_or_default());
            }
        }
        let depth = |t: TreeID| {
            let (mut d, mut cur) = (0usize, t);
            while let Some(TreeParentId::Node(p)) = tree.parent(cur) {
                d += 1;
                cur = p;
                if d > 256 {
                    break;
                }
            }
            d
        };
        let mut todo: Vec<(usize, TreeID)> = todo.into_iter().map(|t| (depth(t), t)).collect();
        todo.sort_by_key(|(d, _)| *d);
        for (_, t) in todo {
            match canvas.node_by_tree(t) {
                Some(n) => {
                    let mut sn = SceneNode::from_doc(&n, t);
                    if let Some(p) = sn.parent.as_deref().and_then(|p| self.nodes.get(p)) {
                        sn.rect[0] += p.rect[0];
                        sn.rect[1] += p.rect[1];
                    }
                    self.upsert(sn, sink)
                }
                None => self.remove_tree(t, sink),
            }
        }
        if ch.structure {
            self.order = self.walk_order(canvas);
            let handles: Vec<Handle> = self.order.iter().filter_map(|id| self.nodes.get(id)?.handle).collect();
            sink.set_order(&handles);
        }
    }

    fn upsert(&mut self, mut n: SceneNode, sink: &mut impl RenderSink) {
        match self.nodes.get_mut(&n.id) {
            Some(cur) => {
                n.handle = cur.handle;
                if *cur == n {
                    return;
                }
                let content = cur.asset != n.asset || cur.text != n.text || cur.name != n.name || cur.card != n.card;
                match (n.handle, n.renderable()) {
                    (Some(h), true) => sink.update(h, &n, content),
                    (Some(h), false) => {
                        sink.remove(h, cur);
                        n.handle = None;
                    }
                    (None, true) => n.handle = Some(sink.add(&n)),
                    (None, false) => {}
                }
                *cur = n;
            }
            None => {
                if n.renderable() {
                    n.handle = Some(sink.add(&n));
                }
                self.by_tree.insert(n.tree, n.id.clone());
                // New nodes land on top until the next structural pass.
                self.order.push(n.id.clone());
                self.nodes.insert(n.id.clone(), n);
            }
        }
    }

    fn remove_tree(&mut self, t: TreeID, sink: &mut impl RenderSink) {
        let Some(id) = self.by_tree.remove(&t) else { return };
        if let Some(n) = self.nodes.remove(&id) {
            if let Some(h) = n.handle {
                sink.remove(h, &n);
            }
            self.order.retain(|o| *o != id);
        }
    }

    fn walk_order(&self, canvas: &Canvas) -> Vec<String> {
        let tree = canvas.tree();
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<TreeID> = tree.children(TreeParentId::Root).unwrap_or_default();
        stack.reverse();
        while let Some(t) = stack.pop() {
            if let Some(id) = self.by_tree.get(&t) {
                out.push(id.clone());
            }
            let mut kids = tree.children(TreeParentId::Node(t)).unwrap_or_default();
            kids.reverse();
            stack.extend(kids);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use falos_doc::{Actor, Command, Props};
    use serde_json::json;

    #[derive(Default)]
    struct Mock {
        next: Handle,
        live: HashMap<Handle, SceneNode>,
        log: Vec<String>,
        order: Vec<Handle>,
    }

    impl RenderSink for Mock {
        fn add(&mut self, n: &SceneNode) -> Handle {
            let h = self.next;
            self.next += 1;
            self.live.insert(h, n.clone());
            self.log.push(format!("add {}", n.id));
            h
        }
        fn update(&mut self, h: Handle, n: &SceneNode, content: bool) {
            self.live.insert(h, n.clone());
            self.log.push(format!("update {}{}", n.id, if content { " +content" } else { "" }));
        }
        fn remove(&mut self, h: Handle, n: &SceneNode) {
            self.live.remove(&h);
            self.log.push(format!("remove {}", n.id));
        }
        fn set_order(&mut self, order: &[Handle]) {
            self.order = order.to_vec();
        }
    }

    fn me() -> Actor {
        Actor::human("me", "Me")
    }

    fn props(v: serde_json::Value) -> Props {
        serde_json::from_value(v).unwrap()
    }

    fn image(id: &str, x: f64) -> Command {
        Command::CreateNode {
            id: Some(id.into()),
            kind: NodeKind::Image,
            parent: None,
            index: None,
            name: id.into(),
            props: props(json!({"x": x, "y": 0, "w": 100, "h": 50, "asset": format!("sha256:{id}")})),
            text: None,
        }
    }

    struct Fixture {
        canvas: Canvas,
        feed: ChangeFeed,
        scene: Scene,
        sink: Mock,
    }

    impl Fixture {
        fn new() -> Self {
            let canvas = Canvas::new("c", "Test");
            let feed = ChangeFeed::new(&canvas);
            Self { canvas, feed, scene: Scene::default(), sink: Mock::default() }
        }
        fn apply(&mut self, cmds: &[Command]) {
            self.canvas.apply(&me(), None, cmds).unwrap();
            self.sync();
        }
        fn sync(&mut self) {
            let ch = self.feed.take();
            self.scene.apply(&self.canvas, &ch, &mut self.sink);
        }
        fn log(&mut self) -> Vec<String> {
            std::mem::take(&mut self.sink.log)
        }
        fn handle(&self, id: &str) -> Handle {
            self.scene.get(id).unwrap().handle.unwrap()
        }
    }

    #[test]
    fn creates_map_to_engine_adds_in_z_order() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0), image("bbb", 200.0), image("ccc", 400.0)]);
        // `add` order follows the doc event order; compare as a set.
        let mut log = f.log();
        log.sort();
        assert_eq!(log, vec!["add aaa", "add bbb", "add ccc"]);
        assert_eq!(f.scene.order(), ["aaa", "bbb", "ccc"]);
        assert_eq!(f.sink.order, vec![f.handle("aaa"), f.handle("bbb"), f.handle("ccc")]);
        assert_eq!(f.sink.live[&f.handle("bbb")].rect, [200.0, 0.0, 100.0, 50.0]);
    }

    #[test]
    fn updates_touch_only_the_changed_node() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0), image("bbb", 200.0)]);
        f.log();
        f.apply(&[Command::UpdateNode { id: "bbb".into(), props: props(json!({"x": 250})) }]);
        assert_eq!(f.log(), vec!["update bbb"]);
        assert_eq!(f.sink.live[&f.handle("bbb")].rect[0], 250.0);
        f.apply(&[Command::UpdateNode { id: "aaa".into(), props: props(json!({"asset": "sha256:new"})) }]);
        assert_eq!(f.log(), vec!["update aaa +content"]);
    }

    #[test]
    fn deletes_and_reorders() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0), image("bbb", 200.0), image("ccc", 400.0)]);
        f.log();
        let (a, c) = (f.handle("aaa"), f.handle("ccc"));
        f.apply(&[Command::DeleteNode { id: "bbb".into() }]);
        assert_eq!(f.log(), vec!["remove bbb"]);
        assert_eq!(f.sink.order, vec![a, c]);
        // Bring `aaa` to the top.
        f.apply(&[Command::MoveNode { id: "aaa".into(), parent: None, index: None }]);
        // A move only touches `updated_by` (not rendered): no adds/removes, just a new order.
        assert!(f.log().iter().all(|l| l.starts_with("update aaa")));
        assert_eq!(f.scene.order(), ["ccc", "aaa"]);
        assert_eq!(f.sink.order, vec![c, a]);
    }

    #[test]
    fn deleting_a_frame_removes_its_children() {
        let mut f = Fixture::new();
        f.apply(&[
            Command::CreateNode {
                id: Some("frm".into()),
                kind: NodeKind::Frame,
                parent: None,
                index: None,
                name: "F".into(),
                props: props(json!({"w": 1000, "h": 1000})),
                text: None,
            },
            Command::CreateNode {
                id: Some("kid".into()),
                kind: NodeKind::Image,
                parent: Some("frm".into()),
                index: None,
                name: String::new(),
                props: props(json!({"x": 10, "y": 10, "w": 100, "h": 100})),
                text: None,
            },
        ]);
        assert_eq!(f.scene.order(), ["frm", "kid"]);
        f.log();
        f.apply(&[Command::DeleteNode { id: "frm".into() }]);
        let mut log = f.log();
        log.sort();
        assert_eq!(log, vec!["remove frm", "remove kid"]);
        assert!(f.scene.is_empty());
        assert!(f.sink.live.is_empty());
    }

    #[test]
    fn children_are_positioned_relative_to_their_parent() {
        let mut f = Fixture::new();
        let node = |id: &str, kind, parent: Option<&str>, x: f64| Command::CreateNode {
            id: Some(id.into()),
            kind,
            parent: parent.map(String::from),
            index: None,
            name: String::new(),
            props: props(json!({"x": x, "y": 10, "w": 100, "h": 100})),
            text: None,
        };
        f.apply(&[
            node("frm", NodeKind::Frame, None, 1000.0),
            node("grp", NodeKind::Frame, Some("frm"), 50.0),
            node("kid", NodeKind::Note, Some("grp"), 5.0),
        ]);
        assert_eq!(f.scene.get("kid").unwrap().rect, [1055.0, 30.0, 100.0, 100.0]);
        assert_eq!(f.scene.descendants("frm"), vec!["grp".to_string(), "kid".to_string()]);
        assert_eq!(f.scene.parent_origin("kid"), [1050.0, 20.0]);
        // Moving the frame moves the whole subtree.
        f.apply(&[Command::UpdateNode { id: "frm".into(), props: props(json!({"x": 0})) }]);
        assert_eq!(f.scene.get("kid").unwrap().rect, [55.0, 30.0, 100.0, 100.0]);
        // A rebuild agrees.
        let mut scene = Scene::default();
        scene.rebuild(&f.canvas, &mut Mock::default());
        assert_eq!(scene.get("kid").unwrap().rect, [55.0, 30.0, 100.0, 100.0]);
    }

    #[test]
    fn remote_import_flows_through_the_same_path() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0)]);
        // A second replica (e.g. the host or an agent) edits and sends an update.
        let mut other = Canvas::from_snapshot(&f.canvas.export_snapshot().unwrap()).unwrap();
        let vv = other.vv();
        other.apply(&Actor::agent("claude", "Claude"), None, &[image("bbb", 300.0)]).unwrap();
        f.log();
        f.canvas.import(&other.export_updates(&vv).unwrap()).unwrap();
        f.sync();
        assert_eq!(f.log(), vec!["add bbb"]);
        assert_eq!(f.scene.order(), ["aaa", "bbb"]);
    }

    #[test]
    fn annotations_flag_without_touching_nodes() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0)]);
        f.log();
        f.canvas
            .apply(
                &me(),
                None,
                &[Command::AddAnnotation {
                    id: None,
                    node_id: "aaa".into(),
                    anchor: falos_doc::Anchor::Point { x: 0.5, y: 0.5 },
                    text: Some("hi".into()),
                    assignee: None,
                }],
            )
            .unwrap();
        let ch = f.feed.take();
        assert!(ch.annotations);
        assert!(ch.dirty.is_empty());
        f.scene.apply(&f.canvas, &ch, &mut f.sink);
        assert!(f.log().is_empty());
    }

    #[test]
    fn rebuild_matches_incremental() {
        let mut f = Fixture::new();
        f.apply(&[image("aaa", 0.0), image("bbb", 200.0)]);
        let mut scene = Scene::default();
        let mut sink = Mock::default();
        scene.rebuild(&f.canvas, &mut sink);
        assert_eq!(scene.order(), f.scene.order());
        assert_eq!(sink.live.len(), 2);
    }
}
