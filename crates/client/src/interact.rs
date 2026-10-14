//! Canvas input: tools, pointer/wheel/key handling, selection, move/resize/marquee and
//! Alt-click / Alt-drag context picks on media (chat context chips). Platform-free: takes screen points in physical pixels and returns
//! [`Effect`]s that the app applies (renderer previews, doc commits, UI events).

use falos_doc::{Command, NodeKind, Props};
use falos_engine::Camera;
use serde::Serialize;
use serde_json::json;

use crate::scene::{Scene, SceneNode};
use crate::snap::{self, Edges, Line};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Select,
    Hand,
}

impl Tool {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "select" => Some(Tool::Select),
            "hand" => Some(Tool::Hand),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    /// Cmd on macOS, Ctrl elsewhere.
    pub meta: bool,
}

/// A spot or region picked on a media node (Alt-click / Alt-drag), in node-local normalized
/// coordinates (0..1).
#[derive(Clone, Debug, PartialEq)]
pub enum PickRegion {
    Point([f32; 2]),
    Box([f32; 4]),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Live rects while dragging (renderer + scene only; committed on release).
    Preview(Vec<(String, [f32; 4])>),
    /// One doc transaction (one undo step).
    Commit(Vec<Command>),
    Select(Vec<String>),
    /// A spot / region picked on a media node (chat context).
    Pick { node: String, region: PickRegion },
    Hover { node: Option<String> },
    Camera,
    Cursor(&'static str),
    Tool(Tool),
    Undo,
    Redo,
    FitAll,
    /// Overlay must be rebuilt (marquee, pick box, hover changed).
    Overlay,
}

#[derive(Clone, Debug)]
enum Drag {
    Pan { last: [f32; 2] },
    /// `target`: the frame under the pointer the moved nodes would land in (`None` = the
    /// canvas), highlighted while dragging and applied on release (reparenting).
    /// `bounds`: the moved items' union at the start; `cands`: other nodes' rects to snap to;
    /// `t0`: press time (a long press drops the drag threshold); `d`: the last applied delta.
    Move {
        start: [f32; 2],
        items: Vec<(String, [f32; 4])>,
        moved: bool,
        target: Option<String>,
        bounds: [f32; 4],
        cands: Vec<[f32; 4]>,
        t0: f64,
        d: [f32; 2],
    },
    /// `init`: the rect before the gesture (Esc restores it); `orig`/`start` shift while Space
    /// moves the box; `cur`: the rect shown now (committed on release).
    Resize {
        id: String,
        handle: usize,
        init: [f32; 4],
        orig: [f32; 4],
        start: [f32; 2],
        last: [f32; 2],
        keep_aspect: bool,
        cands: Vec<[f32; 4]>,
        cur: [f32; 4],
    },
    Marquee { start: [f32; 2], cur: [f32; 2], base: Vec<String> },
    Pick { node: String, start: [f32; 2], cur: [f32; 2], start_px: [f32; 2] },
}

/// Handle size, in CSS pixels.
const HANDLE_PX: f32 = 8.0;
/// Drag threshold (CSS px), and the precise one after holding the press still for
/// [`HOLD_MS`] (micro-moves without accidental drags).
const DRAG_SLOP_PX: f32 = 3.0;
const HOLD_SLOP_PX: f32 = 1.0;
const HOLD_MS: f64 = 500.0;
/// Nodes smaller than this on screen (CSS px) get no handles and no edge resize, so they stay
/// draggable.
pub const TINY_PX: f32 = 16.0;
/// Edges resize along their whole length within this distance (CSS px).
const EDGE_PX: f32 = 4.0;
/// At most this many other nodes are snap candidates (the closest to the view).
const MAX_SNAP_CANDS: usize = 4000;
/// Hover intent: switching the hover to a container (frame/group) or to nothing waits this
/// long, so crossing a parent's empty area between two children never flashes the parent.
pub const HOVER_GRACE_MS: f64 = 140.0;
/// Pointer speed (CSS px/ms) above which hover changes wait for the pointer to slow down.
const HOVER_FAST: f32 = 2.0;
/// No hover changes this long after a wheel/pinch event.
const HOVER_WHEEL_MS: f64 = 160.0;

pub struct Interaction {
    pub tool: Tool,
    pub selection: Vec<String>,
    pub hover: Option<String>,
    pub space: bool,
    /// The group the user double-clicked into: clicks select inside it instead of the group.
    pub entered: Option<String>,
    /// ⌥ held (distances to the hovered node).
    pub alt: bool,
    drag: Option<Drag>,
    /// Snap guides (world lines): live while dragging, and for a moment after a nudge.
    guides: Vec<Line>,
    guides_until: f64,
    /// A hover change waiting for its grace period: target and since when (ms).
    pending_hover: Option<(Option<String>, f64)>,
    last_move: Option<([f32; 2], f64)>,
    last_wheel: f64,
}

impl Default for Interaction {
    fn default() -> Self {
        Self {
            tool: Tool::Select,
            selection: Vec::new(),
            hover: None,
            space: false,
            entered: None,
            alt: false,
            drag: None,
            guides: Vec::new(),
            guides_until: 0.0,
            pending_hover: None,
            last_move: None,
            last_wheel: f64::NEG_INFINITY,
        }
    }
}

pub struct Ctx<'a> {
    pub scene: &'a Scene,
    pub camera: &'a mut Camera,
    /// Device pixel ratio (screen points are physical pixels).
    pub dpr: f32,
    /// Monotonic time in ms (hover intent).
    pub now: f64,
}

fn is_container(n: &SceneNode) -> bool {
    matches!(n.kind, NodeKind::Frame | NodeKind::Group)
}

/// Whether `id` is `ancestor` or inside it.
fn within(scene: &Scene, id: &str, ancestor: &str) -> bool {
    let mut cur = Some(id);
    while let Some(n) = cur.and_then(|i| scene.get(i)) {
        if n.id == ancestor {
            return true;
        }
        cur = n.parent.as_deref();
    }
    false
}

/// `ids` without the ones inside another of `ids`: a moved or deleted container carries its
/// children, so acting on both would move them twice (or delete a node that is already gone).
fn roots(scene: &Scene, ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter(|id| {
            let mut cur = scene.get(id).and_then(|n| n.parent.clone());
            while let Some(p) = cur {
                if ids.contains(&p) {
                    return false;
                }
                cur = scene.get(&p).and_then(|n| n.parent.clone());
            }
            true
        })
        .cloned()
        .collect()
}

/// The frame a drop at world point `w` lands in: the topmost (deepest) frame under it that
/// is not one of `exclude` or inside them. Groups are never drop targets
/// (docs/research/brilliant-canvas-interaction.md, drag reparenting).
pub fn drop_frame(scene: &Scene, w: [f32; 2], exclude: &[String]) -> Option<String> {
    scene
        .order()
        .iter()
        .rev()
        .filter_map(|id| scene.get(id))
        .find(|n| n.kind == NodeKind::Frame && n.contains(w) && !exclude.iter().any(|e| within(scene, &n.id, e)))
        .map(|n| n.id.clone())
}

/// Handle positions (0 TL, 1 T, 2 TR, 3 R, 4 BR, 5 B, 6 BL, 7 L) of a world rect.
pub fn handle_points(r: [f32; 4]) -> [[f32; 2]; 8] {
    let [x, y, w, h] = r;
    let (cx, cy, x1, y1) = (x + w * 0.5, y + h * 0.5, x + w, y + h);
    [[x, y], [cx, y], [x1, y], [x1, cy], [x1, y1], [cx, y1], [x, y1], [x, cy]]
}

const HANDLE_CURSORS: [&str; 8] =
    ["nwse-resize", "ns-resize", "nesw-resize", "ew-resize", "nwse-resize", "ns-resize", "nesw-resize", "ew-resize"];

/// New rect when dragging `handle` of `orig` by a world delta.
pub fn resize_rect(orig: [f32; 4], handle: usize, d: [f32; 2], keep_aspect: bool) -> [f32; 4] {
    let [x, y, w, h] = orig;
    let left = matches!(handle, 0 | 6 | 7);
    let right = matches!(handle, 2..=4);
    let top = matches!(handle, 0..=2);
    let bottom = matches!(handle, 4..=6);
    let min = 4.0;
    let mut nw = if left { w - d[0] } else if right { w + d[0] } else { w };
    let mut nh = if top { h - d[1] } else if bottom { h + d[1] } else { h };
    nw = nw.max(min);
    nh = nh.max(min);
    let corner = handle.is_multiple_of(2);
    if keep_aspect && corner {
        let (sx, sy) = (nw / w.max(1e-6), nh / h.max(1e-6));
        let s = if (sx - 1.0).abs() > (sy - 1.0).abs() { sx } else { sy };
        nw = (w * s).max(min);
        nh = (h * s).max(min);
    }
    let nx = if left { x + w - nw } else if right { x } else { x + (w - nw) * 0.5 };
    let ny = if top { y + h - nh } else if bottom { y } else { y + (h - nh) * 0.5 };
    [nx, ny, nw, nh]
}

fn rect_props(r: [f32; 4]) -> Props {
    let round = |v: f32| (v as f64 * 100.0).round() / 100.0;
    serde_json::from_value(json!({"x": round(r[0]), "y": round(r[1]), "w": round(r[2]), "h": round(r[3])})).unwrap_or_default()
}

fn is_media(n: &SceneNode) -> bool {
    matches!(n.kind, NodeKind::Image | NodeKind::Video | NodeKind::Model3d | NodeKind::Vector | NodeKind::Document)
}

/// Nodes whose pixels can be pointed at (spot / region / frame chips).
fn pickable(n: &SceneNode) -> bool {
    matches!(n.kind, NodeKind::Image | NodeKind::Video | NodeKind::Vector) && n.asset.is_some()
}

/// World rects other nodes snap against: drawn nodes and groups near the view, except the
/// moved ones and everything inside them.
fn snap_cands(scene: &Scene, camera: &Camera, exclude: &[String]) -> Vec<[f32; 4]> {
    let z = camera.zoom.max(1e-6);
    let (vw, vh) = (camera.viewport[0] / z, camera.viewport[1] / z);
    // The view plus half a view on each side.
    let view = [camera.offset[0] - vw * 0.5, camera.offset[1] - vh * 0.5, vw * 2.0, vh * 2.0];
    let near = |r: [f32; 4]| r[0] <= view[0] + view[2] && r[0] + r[2] >= view[0] && r[1] <= view[1] + view[3] && r[1] + r[3] >= view[1];
    let mut out: Vec<[f32; 4]> = scene
        .iter()
        .filter(|n| (n.handle.is_some() || n.kind == NodeKind::Group) && near(n.rect))
        .filter(|n| !exclude.iter().any(|e| within(scene, &n.id, e)))
        .map(|n| n.rect)
        .collect();
    if out.len() > MAX_SNAP_CANDS {
        let c = [view[0] + view[2] * 0.5, view[1] + view[3] * 0.5];
        let dist = |r: &[f32; 4]| (r[0] + r[2] * 0.5 - c[0]).abs() + (r[1] + r[3] * 0.5 - c[1]).abs();
        out.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
        out.truncate(MAX_SNAP_CANDS);
    }
    out
}

/// Snap threshold in world units.
fn snap_thr(ctx: &Ctx) -> f32 {
    snap::SNAP_PX * ctx.dpr / ctx.camera.zoom.max(1e-6)
}

/// A move's delta after the axis lock (⇧) and snapping (off while ⌘/Ctrl is held), and the
/// guides to show.
pub fn move_delta(raw: [f32; 2], bounds: [f32; 4], cands: &[[f32; 4]], thr: f32, mods: Mods) -> ([f32; 2], Vec<Line>) {
    let mut d = raw;
    // ⇧: only along the dominant axis.
    let lock = mods.shift.then(|| if d[0].abs() >= d[1].abs() { 1 } else { 0 });
    if let Some(a) = lock {
        d[a] = 0.0;
    }
    if mods.meta || d == [0.0, 0.0] {
        return (d, Vec::new());
    }
    let r = [bounds[0] + d[0], bounds[1] + d[1], bounds[2], bounds[3]];
    let mut s = snap::snap(r, Edges::ALL, cands, thr);
    if let Some(a) = lock {
        s.offset[a] = 0.0;
        s.guides = snap::guides(snap::apply(r, Edges::ALL, s.offset), Edges::ALL, cands);
    }
    ([d[0] + s.offset[0], d[1] + s.offset[1]], s.guides)
}

/// A resize step: the new rect (snapping its moving edges unless ⌘/Ctrl is held) and guides.
/// A corner that keeps the aspect ratio snaps one axis and scales the other with it.
pub fn resize_step(orig: [f32; 4], handle: usize, d: [f32; 2], keep_aspect: bool, cands: &[[f32; 4]], thr: f32, snap_on: bool) -> ([f32; 4], Vec<Line>) {
    let r0 = resize_rect(orig, handle, d, keep_aspect);
    if !snap_on || cands.is_empty() {
        return (r0, Vec::new());
    }
    let e = Edges::of_handle(handle);
    let s = snap::snap(r0, e, cands, thr);
    if s.offset == [0.0, 0.0] {
        return (r0, s.guides);
    }
    let r = if keep_aspect && handle.is_multiple_of(2) {
        // Keep the smaller correction; the other side follows the aspect ratio.
        let k = if s.offset[1] == 0.0 || (s.offset[0] != 0.0 && s.offset[0].abs() <= s.offset[1].abs()) { 0 } else { 1 };
        let mut off = [0.0; 2];
        off[k] = s.offset[k];
        let target = snap::apply(r0, e, off);
        let scale = target[k + 2] / orig[k + 2].max(1e-6);
        let (nw, nh) = (orig[2] * scale, orig[3] * scale);
        let dx = if e.left { orig[2] - nw } else { nw - orig[2] };
        let dy = if e.top { orig[3] - nh } else { nh - orig[3] };
        resize_rect(orig, handle, [dx, dy], true)
    } else {
        snap::apply(r0, e, s.offset)
    };
    let g = snap::guides(r, e, cands);
    (r, g)
}

impl Interaction {
    /// What a click on `hit` selects (Figma rules): the outermost group around it that the
    /// user hasn't entered (double-click enters a group); `deep` (⌘-click) selects `hit`.
    pub fn select_target(&self, scene: &Scene, hit: &SceneNode, deep: bool) -> String {
        if deep {
            return hit.id.clone();
        }
        let mut chain = Vec::new(); // group ancestors, innermost first
        let mut cur = hit.parent.as_deref();
        while let Some(p) = cur.and_then(|id| scene.get(id)) {
            if p.kind == NodeKind::Group {
                chain.push(p.id.clone());
            }
            cur = p.parent.as_deref();
        }
        // The entered group and its ancestors are "open".
        let mut open = Vec::new();
        let mut e = self.entered.as_deref();
        while let Some(n) = e.and_then(|id| scene.get(id)) {
            open.push(n.id.clone());
            e = n.parent.as_deref();
        }
        chain.into_iter().rev().find(|g| !open.contains(g)).unwrap_or_else(|| hit.id.clone())
    }

    fn inside(scene: &Scene, id: &str, ancestor: &str) -> bool {
        let mut cur = Some(id);
        while let Some(n) = cur.and_then(|i| scene.get(i)) {
            if n.id == ancestor {
                return true;
            }
            cur = n.parent.as_deref();
        }
        false
    }

    /// The parent every selected node shares, if any.
    fn common_parent(&self, scene: &Scene) -> Option<String> {
        let mut parents = self.selection.iter().map(|id| scene.get(id).and_then(|n| n.parent.clone()));
        let first = parents.next()??;
        parents.all(|p| p.as_deref() == Some(first.as_str())).then_some(first)
    }

    /// Selects `ids` (siblings) and enters the innermost group around them, so clicks and
    /// ⌘A act at that level.
    fn select_level(&mut self, scene: &Scene, ids: Vec<String>) {
        let mut cur = ids.first().and_then(|id| scene.get(id)).and_then(|n| n.parent.clone());
        self.entered = None;
        while let Some(p) = cur.and_then(|p| scene.get(&p)) {
            if p.kind == NodeKind::Group {
                self.entered = Some(p.id.clone());
                break;
            }
            cur = p.parent.clone();
        }
        self.selection = ids;
    }

    /// Double-click: enters the group around the node under the pointer (selecting one level
    /// deeper). Returns `(entered group, node under the pointer as now selected)`.
    pub fn double_click(&mut self, ctx: &mut Ctx, p: [f32; 2]) -> (Option<String>, Option<String>, Vec<Effect>) {
        let w = ctx.camera.screen_to_world(p);
        let Some(hit) = ctx.scene.hit_test(w) else { return (None, None, Vec::new()) };
        let target = self.select_target(ctx.scene, hit, false);
        if target != hit.id {
            self.entered = Some(target.clone());
            let next = self.select_target(ctx.scene, hit, false);
            self.selection = vec![next.clone()];
            return (Some(target), Some(next.clone()), vec![Effect::Select(vec![next]), Effect::Overlay]);
        }
        (None, Some(hit.id.clone()), Vec::new())
    }

    /// Snap guides to draw now: while dragging, and for a moment after a nudge.
    pub fn guides(&self) -> &[Line] {
        &self.guides
    }

    /// Resolves a pending hover once its grace period is over, and drops guides that have
    /// had their moment (call every frame).
    pub fn tick(&mut self, now: f64) -> Vec<Effect> {
        if !self.guides.is_empty() && self.drag.is_none() && now >= self.guides_until {
            self.guides.clear();
            return vec![Effect::Overlay];
        }
        match &self.pending_hover {
            Some((target, since)) if now - since >= HOVER_GRACE_MS && self.drag.is_none() => {
                let target = target.clone();
                self.pending_hover = None;
                if target != self.hover {
                    self.hover = target.clone();
                    return vec![Effect::Hover { node: target }, Effect::Overlay];
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Marquee rect in world units while selecting by drag.
    pub fn marquee(&self) -> Option<[f32; 4]> {
        match &self.drag {
            Some(Drag::Marquee { start, cur, .. }) => Some(norm_rect(*start, *cur)),
            _ => None,
        }
    }

    /// Region being Alt-dragged on a media node: node id and world rect.
    pub fn pick_box(&self) -> Option<(&str, [f32; 4])> {
        match &self.drag {
            Some(Drag::Pick { node, start, cur, .. }) => Some((node, norm_rect(*start, *cur))),
            _ => None,
        }
    }

    /// The frame the nodes being dragged would drop into (highlighted).
    pub fn drop_target(&self) -> Option<&str> {
        match &self.drag {
            Some(Drag::Move { moved: true, target, .. }) => target.as_deref(),
            _ => None,
        }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The resize handle (or edge, along its whole length) under screen point `p` of the
    /// single selected node: handle index 0 TL, 1 T, 2 TR, 3 R, 4 BR, 5 B, 6 BL, 7 L. Nodes
    /// tiny on screen have none, so they can still be dragged.
    fn handle_at(&self, ctx: &Ctx, p: [f32; 2]) -> Option<(String, usize)> {
        if self.tool != Tool::Select || self.selection.len() != 1 {
            return None;
        }
        let n = ctx.scene.get(&self.selection[0])?;
        let a = ctx.camera.world_to_screen([n.rect[0], n.rect[1]]);
        let b = ctx.camera.world_to_screen([n.rect[0] + n.rect[2], n.rect[1] + n.rect[3]]);
        let (sw, sh) = (b[0] - a[0], b[1] - a[1]);
        if sw.min(sh) < TINY_PX * ctx.dpr {
            return None;
        }
        let r = (HANDLE_PX * 0.5 + 3.0) * ctx.dpr;
        let corner = handle_points(n.rect).iter().position(|hp| {
            let s = ctx.camera.world_to_screen(*hp);
            (p[0] - s[0]).abs() <= r && (p[1] - s[1]).abs() <= r
        });
        if let Some(i) = corner {
            return Some((n.id.clone(), i));
        }
        // Edges: outside within EDGE_PX, inside within EDGE_PX but at most 15 % of the side.
        let out = EDGE_PX * ctx.dpr;
        let inx = out.min(sw * 0.15);
        let iny = out.min(sh * 0.15);
        let in_y = p[1] >= a[1] && p[1] <= b[1];
        let in_x = p[0] >= a[0] && p[0] <= b[0];
        let edge = if in_y && p[0] >= a[0] - out && p[0] <= a[0] + inx {
            Some(7)
        } else if in_y && p[0] <= b[0] + out && p[0] >= b[0] - inx {
            Some(3)
        } else if in_x && p[1] >= a[1] - out && p[1] <= a[1] + iny {
            Some(1)
        } else if in_x && p[1] <= b[1] + out && p[1] >= b[1] - iny {
            Some(5)
        } else {
            None
        };
        edge.map(|i| (n.id.clone(), i))
    }

    pub fn pointer_down(&mut self, ctx: &mut Ctx, p: [f32; 2], button: i16, mods: Mods) -> Vec<Effect> {
        let mut fx = Vec::new();
        let w = ctx.camera.screen_to_world(p);
        if button == 1 || self.space || self.tool == Tool::Hand {
            self.drag = Some(Drag::Pan { last: p });
            fx.push(Effect::Cursor("grabbing"));
            return fx;
        }
        if button != 0 {
            return fx;
        }
        match self.tool {
            Tool::Select => {
                if mods.alt
                    && let Some(n) = ctx.scene.hit_test(w).filter(|n| pickable(n))
                {
                    self.drag = Some(Drag::Pick { node: n.id.clone(), start: w, cur: w, start_px: p });
                    return fx;
                }
                if let Some((id, handle)) = self.handle_at(ctx, p) {
                    let n = ctx.scene.get(&id).expect("selected node");
                    let keep_aspect = is_media(n) != mods.shift;
                    let cands = snap_cands(ctx.scene, ctx.camera, std::slice::from_ref(&id));
                    let r = n.rect;
                    self.drag = Some(Drag::Resize { id, handle, init: r, orig: r, start: w, last: w, keep_aspect, cands, cur: r });
                    return fx;
                }
                match ctx.scene.hit_test(w) {
                    Some(n) => {
                        // Clicking outside the entered group leaves it.
                        if let Some(e) = self.entered.clone()
                            && !Self::inside(ctx.scene, &n.id, &e)
                        {
                            self.entered = None;
                        }
                        let id = self.select_target(ctx.scene, n, mods.meta);
                        if mods.shift {
                            if let Some(i) = self.selection.iter().position(|s| *s == id) {
                                self.selection.remove(i);
                            } else {
                                self.selection.push(id.clone());
                            }
                            fx.push(Effect::Select(self.selection.clone()));
                        } else if !self.selection.contains(&id) {
                            self.selection = vec![id.clone()];
                            fx.push(Effect::Select(self.selection.clone()));
                        }
                        let items: Vec<(String, [f32; 4])> = roots(ctx.scene, &self.selection)
                            .iter()
                            .filter_map(|s| ctx.scene.get(s))
                            .filter(|n| n.handle.is_some() || n.kind == NodeKind::Group)
                            .map(|n| (n.id.clone(), n.rect))
                            .collect();
                        let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
                        let bounds = ctx.scene.bounds_of(&ids).unwrap_or([w[0], w[1], 0.0, 0.0]);
                        let cands = snap_cands(ctx.scene, ctx.camera, &ids);
                        self.drag = Some(Drag::Move { start: w, items, moved: false, target: None, bounds, cands, t0: ctx.now, d: [0.0, 0.0] });
                    }
                    None => {
                        let base = if mods.shift { self.selection.clone() } else { Vec::new() };
                        self.entered = None;
                        if !mods.shift && !self.selection.is_empty() {
                            self.selection.clear();
                            fx.push(Effect::Select(Vec::new()));
                        }
                        self.drag = Some(Drag::Marquee { start: w, cur: w, base });
                    }
                }
            }
            Tool::Hand => {}
        }
        fx
    }

    pub fn pointer_move(&mut self, ctx: &mut Ctx, p: [f32; 2], mods: Mods) -> Vec<Effect> {
        let mut fx = Vec::new();
        let w = ctx.camera.screen_to_world(p);
        if self.alt != mods.alt {
            self.alt = mods.alt;
            fx.push(Effect::Overlay);
        }
        match &mut self.drag {
            Some(Drag::Pan { last }) => {
                ctx.camera.pan(p[0] - last[0], p[1] - last[1]);
                *last = p;
                fx.push(Effect::Camera);
            }
            Some(Drag::Move { start, items, moved, target, bounds, cands, t0, d }) => {
                let raw = [w[0] - start[0], w[1] - start[1]];
                let px = if ctx.now - *t0 >= HOLD_MS { HOLD_SLOP_PX } else { DRAG_SLOP_PX };
                let slop = px / ctx.camera.zoom * ctx.dpr;
                if !*moved && raw[0].abs() < slop && raw[1].abs() < slop {
                    return fx;
                }
                *moved = true;
                let (nd, guides) = move_delta(raw, *bounds, cands, snap_thr(ctx), mods);
                *d = nd;
                self.guides = guides;
                fx.push(Effect::Preview(items.iter().map(|(id, r)| (id.clone(), [r[0] + nd[0], r[1] + nd[1], r[2], r[3]])).collect()));
                fx.push(Effect::Cursor("move"));
                fx.push(Effect::Overlay);
                // Space blocks reparenting: the target stays where it was.
                if !self.space {
                    let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
                    *target = drop_frame(ctx.scene, w, &ids);
                }
            }
            Some(Drag::Resize { id, handle, orig, start, last, keep_aspect, cands, cur, .. }) => {
                // Space moves the box mid-resize (its size stays).
                if self.space {
                    let sh = [w[0] - last[0], w[1] - last[1]];
                    orig[0] += sh[0];
                    orig[1] += sh[1];
                    start[0] += sh[0];
                    start[1] += sh[1];
                }
                *last = w;
                let (r, guides) = resize_step(*orig, *handle, [w[0] - start[0], w[1] - start[1]], *keep_aspect, cands, snap_thr(ctx), !mods.meta);
                *cur = r;
                self.guides = guides;
                fx.push(Effect::Preview(vec![(id.clone(), r)]));
                fx.push(Effect::Overlay);
            }
            Some(Drag::Marquee { start, cur, base }) => {
                *cur = w;
                let mut sel = base.clone();
                let hits = ctx.scene.intersecting(norm_rect(*start, *cur));
                for id in hits {
                    let Some(n) = ctx.scene.get(&id) else { continue };
                    let t = self.select_target(ctx.scene, n, false);
                    if !sel.contains(&t) {
                        sel.push(t);
                    }
                }
                if sel != self.selection {
                    self.selection = sel.clone();
                    fx.push(Effect::Select(sel));
                }
                fx.push(Effect::Overlay);
            }
            Some(Drag::Pick { cur, .. }) => {
                *cur = w;
                fx.push(Effect::Overlay);
            }
            None => fx.extend(self.update_hover(ctx, p, mods)),
        }
        fx
    }

    fn update_hover(&mut self, ctx: &Ctx, p: [f32; 2], mods: Mods) -> Vec<Effect> {
        let mut fx = Vec::new();
        let w = ctx.camera.screen_to_world(p);
        let hit = ctx.scene.hit_test(w);
        // Pointer speed in CSS px/ms.
        let speed = match self.last_move {
            Some((q, t)) if ctx.now > t => ((p[0] - q[0]).hypot(p[1] - q[1]) / ctx.dpr) / (ctx.now - t) as f32,
            _ => 0.0,
        };
        self.last_move = Some((p, ctx.now));
        let node = hit.map(|n| self.select_target(ctx.scene, n, false));
        let cursor = if self.space || self.tool == Tool::Hand {
            "grab"
        } else if let Some((_, i)) = self.handle_at(ctx, p) {
            HANDLE_CURSORS[i]
        } else if mods.alt && hit.is_some_and(pickable) {
            "crosshair"
        } else {
            "default"
        };
        fx.push(Effect::Cursor(cursor));
        if node == self.hover {
            self.pending_hover = None;
            return fx;
        }
        let container = node.as_deref().and_then(|id| ctx.scene.get(id)).is_some_and(is_container);
        let wheeling = ctx.now - self.last_wheel < HOVER_WHEEL_MS;
        if node.is_some() && !container && speed < HOVER_FAST && !wheeling {
            // Entering a leaf (or a group from outside): switch now, child→child directly.
            self.pending_hover = None;
            self.hover = node.clone();
            fx.push(Effect::Hover { node });
            fx.push(Effect::Overlay);
        } else if self.pending_hover.as_ref().is_none_or(|(t, _)| *t != node) {
            // Containers, empty canvas and fast moves wait for the grace period.
            self.pending_hover = Some((node, ctx.now));
        }
        fx
    }

    pub fn pointer_up(&mut self, ctx: &mut Ctx, p: [f32; 2], _mods: Mods) -> Vec<Effect> {
        let mut fx = Vec::new();
        let w = ctx.camera.screen_to_world(p);
        self.guides.clear();
        match self.drag.take() {
            Some(Drag::Pan { .. }) => fx.push(Effect::Cursor(if self.space || self.tool == Tool::Hand { "grab" } else { "default" })),
            // Back where it started (the UI snaps a move back for drag-out / drop on the chat):
            // nothing to commit, and no reparenting.
            Some(Drag::Move { start, moved: true, .. }) if w == start => fx.push(Effect::Overlay),
            Some(Drag::Move { items, moved: true, target, d, .. }) => {
                let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
                let mut cmds = Vec::new();
                for (id, r) in &items {
                    // Reparent top-level and frame children (group children stay in their
                    // group, which refits); nodes whose ancestor also moves go along with it.
                    let n = ctx.scene.get(id);
                    let parent = n.and_then(|n| n.parent.clone());
                    let reparentable = parent.as_deref().is_none_or(|p| ctx.scene.get(p).is_some_and(|p| p.kind == NodeKind::Frame));
                    let carried = parent.as_deref().is_some_and(|p| ids.iter().any(|o| o != id && within(ctx.scene, p, o)));
                    if !self.space && reparentable && !carried && parent != target {
                        cmds.push(Command::MoveNode { id: id.clone(), parent: target.clone(), index: None });
                    }
                    cmds.push(Command::UpdateNode { id: id.clone(), props: rect_props([r[0] + d[0], r[1] + d[1], r[2], r[3]]) });
                }
                fx.push(Effect::Commit(cmds));
                fx.push(Effect::Overlay);
            }
            Some(Drag::Move { .. }) => {}
            Some(Drag::Resize { id, init, cur, .. }) => {
                if cur != init {
                    fx.push(Effect::Commit(vec![Command::UpdateNode { id, props: rect_props(cur) }]));
                }
                fx.push(Effect::Overlay);
            }
            Some(Drag::Marquee { .. }) => fx.push(Effect::Overlay),
            Some(Drag::Pick { node, start, start_px, .. }) => {
                if let Some(n) = ctx.scene.get(&node) {
                    let clamp = |l: [f32; 2]| [l[0].clamp(0.0, 1.0), l[1].clamp(0.0, 1.0)];
                    let a = clamp(n.to_local(start));
                    let b = clamp(n.to_local(w));
                    let far = (p[0] - start_px[0]).abs().max((p[1] - start_px[1]).abs()) > 4.0 * ctx.dpr;
                    let region = if far {
                        PickRegion::Box([a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()])
                    } else {
                        PickRegion::Point(a)
                    };
                    fx.push(Effect::Pick { node, region });
                }
                fx.push(Effect::Overlay);
            }
            None => {}
        }
        fx
    }

    /// Wheel / trackpad: pan, or zoom with Cmd/Ctrl (browsers send pinch as ctrl+wheel).
    pub fn wheel(&mut self, ctx: &mut Ctx, p: [f32; 2], d: [f32; 2], mods: Mods) -> Vec<Effect> {
        self.last_wheel = ctx.now;
        if mods.meta {
            // Zoom speed is per CSS pixel of wheel delta (trackpad pinch sends small deltas).
            let dy = (d[1] / ctx.dpr).clamp(-30.0, 30.0);
            ctx.camera.zoom_at(p, (-dy * 0.01).exp());
        } else if mods.shift && d[0] == 0.0 {
            ctx.camera.pan(-d[1], 0.0);
        } else {
            ctx.camera.pan(-d[0], -d[1]);
        }
        vec![Effect::Camera]
    }

    /// Safari gesture events: relative scale factor around a screen point.
    pub fn pinch(&mut self, ctx: &mut Ctx, p: [f32; 2], factor: f32) -> Vec<Effect> {
        self.last_wheel = ctx.now;
        ctx.camera.zoom_at(p, factor);
        vec![Effect::Camera]
    }

    pub fn set_tool(&mut self, tool: Tool) -> Vec<Effect> {
        self.tool = tool;
        self.drag = None;
        let cursor = match tool {
            Tool::Hand => "grab",
            _ => "default",
        };
        vec![Effect::Tool(tool), Effect::Cursor(cursor), Effect::Overlay]
    }

    /// Returns `None` when the key is not handled (let the browser have it).
    pub fn key_down(&mut self, ctx: &mut Ctx, key: &str, mods: Mods) -> Option<Vec<Effect>> {
        let mut fx = Vec::new();
        let center = [ctx.camera.viewport[0] * 0.5, ctx.camera.viewport[1] * 0.5];
        match key {
            " " => {
                if !self.space {
                    self.space = true;
                    fx.push(Effect::Cursor("grab"));
                }
            }
            "z" | "Z" if mods.meta => fx.push(if mods.shift { Effect::Redo } else { Effect::Undo }),
            "y" if mods.meta => fx.push(Effect::Redo),
            "a" if mods.meta => {
                // The siblings at the current level (top level, or inside the entered group).
                let level = self.entered.clone();
                self.selection = ctx
                    .scene
                    .iter()
                    .filter(|n| n.parent == level && (n.handle.is_some() || n.kind == NodeKind::Group))
                    .map(|n| n.id.clone())
                    .collect();
                fx.push(Effect::Select(self.selection.clone()));
            }
            "Backspace" | "Delete" if !self.selection.is_empty() => {
                let cmds = roots(ctx.scene, &self.selection).into_iter().map(|id| Command::DeleteNode { id }).collect();
                self.selection.clear();
                fx.push(Effect::Select(Vec::new()));
                fx.push(Effect::Commit(cmds));
            }
            // The Esc ladder: cancel a gesture → up to the entered group → children up to
            // their frame or group → deselect → back to the select tool. (Leaving a text edit
            // is the editor's own Esc, before this.)
            "Escape" if self.drag.is_some() => {
                // Cancels a drag in progress: moved / resized nodes snap back; the selection stays.
                match self.drag.take() {
                    Some(Drag::Move { items, moved: true, .. }) => fx.push(Effect::Preview(items)),
                    Some(Drag::Resize { id, init, .. }) => fx.push(Effect::Preview(vec![(id, init)])),
                    _ => {}
                }
                self.guides.clear();
                fx.push(Effect::Overlay);
            }
            "Escape" if self.entered.is_some() => {
                // Go up one level: select the entered group, leave it.
                let g = self.entered.take().unwrap_or_default();
                self.entered = ctx.scene.get(&g).and_then(|n| n.parent.clone()).filter(|p| ctx.scene.get(p).is_some_and(|n| n.kind == NodeKind::Group));
                self.selection = vec![g];
                fx.push(Effect::Select(self.selection.clone()));
                fx.push(Effect::Overlay);
            }
            "Escape" if !self.selection.is_empty() => {
                // Children select their parent frame / group; top-level nodes deselect.
                match self.common_parent(ctx.scene) {
                    Some(p) => self.select_level(ctx.scene, vec![p]),
                    None => self.selection.clear(),
                }
                fx.push(Effect::Select(self.selection.clone()));
                fx.push(Effect::Overlay);
            }
            "Escape" => {
                if self.tool != Tool::Select {
                    fx.extend(self.set_tool(Tool::Select));
                }
                fx.push(Effect::Overlay);
            }
            // ⏎ selects the children of the selected frames / groups; ⇧⏎ their parents.
            "Enter" if !mods.meta && !self.selection.is_empty() => {
                let next: Vec<String> = if mods.shift {
                    let mut ps: Vec<String> = Vec::new();
                    for id in &self.selection {
                        if let Some(p) = ctx.scene.get(id).and_then(|n| n.parent.clone())
                            && !ps.contains(&p)
                        {
                            ps.push(p);
                        }
                    }
                    ps
                } else {
                    let containers: Vec<&String> = self.selection.iter().filter(|id| ctx.scene.get(id).is_some_and(is_container)).collect();
                    ctx.scene
                        .iter()
                        .filter(|n| n.parent.as_ref().is_some_and(|p| containers.contains(&p)) && (n.handle.is_some() || n.kind == NodeKind::Group))
                        .map(|n| n.id.clone())
                        .collect()
                };
                if next.is_empty() {
                    return None;
                }
                self.select_level(ctx.scene, next);
                fx.push(Effect::Select(self.selection.clone()));
                fx.push(Effect::Overlay);
            }
            "v" | "V" if !mods.meta => fx.extend(self.set_tool(Tool::Select)),
            "h" | "H" if !mods.meta => fx.extend(self.set_tool(Tool::Hand)),
            "!" | "1" if mods.shift => fx.push(Effect::FitAll),
            ")" | "0" if mods.shift => {
                let f = ctx.dpr / ctx.camera.zoom;
                ctx.camera.zoom_at(center, f);
                fx.push(Effect::Camera);
            }
            "=" | "+" => {
                ctx.camera.zoom_at(center, 1.25);
                fx.push(Effect::Camera);
            }
            "-" | "_" => {
                ctx.camera.zoom_at(center, 0.8);
                fx.push(Effect::Camera);
            }
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" if !self.selection.is_empty() => {
                let step = if mods.shift { 10.0 } else { 1.0 };
                let d = match key {
                    "ArrowLeft" => [-step, 0.0],
                    "ArrowRight" => [step, 0.0],
                    "ArrowUp" => [0.0, -step],
                    _ => [0.0, step],
                };
                let moved = roots(ctx.scene, &self.selection);
                let cmds = moved
                    .iter()
                    .filter_map(|id| ctx.scene.get(id))
                    .map(|n| Command::UpdateNode { id: n.id.clone(), props: rect_props([n.rect[0] + d[0], n.rect[1] + d[1], n.rect[2], n.rect[3]]) })
                    .collect();
                fx.push(Effect::Commit(cmds));
                // What the nudged box lines up with shows for a moment.
                if let Some(b) = ctx.scene.bounds_of(&moved) {
                    let r = [b[0] + d[0], b[1] + d[1], b[2], b[3]];
                    self.guides = snap::guides(r, Edges::ALL, &snap_cands(ctx.scene, ctx.camera, &moved));
                    self.guides_until = ctx.now + snap::NUDGE_GUIDES_MS;
                    fx.push(Effect::Overlay);
                }
            }
            _ => return None,
        }
        Some(fx)
    }

    pub fn key_up(&mut self, key: &str) -> Vec<Effect> {
        if key == " " && self.space {
            self.space = false;
            return vec![Effect::Cursor(if self.tool == Tool::Hand { "grab" } else { "default" })];
        }
        Vec::new()
    }

    /// Drops selected ids that no longer exist in the scene.
    pub fn prune(&mut self, scene: &Scene) -> bool {
        let before = self.selection.len();
        self.selection.retain(|id| scene.get(id).is_some());
        if self.hover.as_ref().is_some_and(|h| scene.get(h).is_none()) {
            self.hover = None;
        }
        if self.entered.as_ref().is_some_and(|e| scene.get(e).is_none()) {
            self.entered = None;
        }
        before != self.selection.len()
    }
}

fn norm_rect(a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::RenderSink;
    use falos_doc::{Actor, Canvas};
    use falos_engine::Handle;

    struct Sink(Handle);
    impl RenderSink for Sink {
        fn add(&mut self, _: &SceneNode) -> Handle {
            self.0 += 1;
            self.0
        }
        fn update(&mut self, _: Handle, _: &SceneNode, _: bool) {}
        fn remove(&mut self, _: Handle, _: &SceneNode) {}
        fn set_order(&mut self, _: &[Handle]) {}
    }

    /// frame F (0,0 400x200) with images a (10,10 100x100) and b (200,10 100x100);
    /// group G (500,0) with image c inside.
    fn scene() -> Scene {
        let mut c = Canvas::new("c", "t");
        let node = |id: &str, kind, parent: Option<&str>, r: [f64; 4]| Command::CreateNode {
            id: Some(id.into()),
            kind,
            parent: parent.map(String::from),
            index: None,
            name: id.into(),
            props: serde_json::from_value(json!({"x": r[0], "y": r[1], "w": r[2], "h": r[3], "asset": "sha256:x"})).unwrap(),
            text: None,
        };
        c.apply(
            &Actor::human("me", "me"),
            None,
            &[
                node("fff", NodeKind::Frame, None, [0.0, 0.0, 400.0, 200.0]),
                node("aaa", NodeKind::Image, Some("fff"), [10.0, 10.0, 100.0, 100.0]),
                node("bbb", NodeKind::Image, Some("fff"), [200.0, 10.0, 100.0, 100.0]),
                node("ggg", NodeKind::Group, None, [500.0, 0.0, 100.0, 100.0]),
                node("ccc", NodeKind::Image, Some("ggg"), [0.0, 0.0, 100.0, 100.0]),
            ],
        )
        .unwrap();
        let mut s = Scene::default();
        s.rebuild(&c, &mut Sink(0));
        s
    }

    fn hovered(fx: &[Effect]) -> Option<Option<String>> {
        fx.iter().find_map(|e| match e {
            Effect::Hover { node } => Some(node.clone()),
            _ => None,
        })
    }

    #[test]
    fn hover_goes_child_to_child_without_flashing_the_parent() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut at = |ix: &mut Interaction, p: [f32; 2], now: f64| {
            let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now };
            ix.pointer_move(&mut ctx, p, Mods::default())
        };
        assert_eq!(hovered(&at(&mut ix, [50.0, 50.0], 0.0)), Some(Some("aaa".into())));
        // Crossing the frame's empty area: nothing changes yet…
        assert_eq!(hovered(&at(&mut ix, [150.0, 50.0], 300.0)), None);
        assert_eq!(hovered(&ix.tick(350.0)), None);
        // …and entering the sibling within the grace period switches directly.
        assert_eq!(hovered(&at(&mut ix, [250.0, 50.0], 400.0)), Some(Some("bbb".into())));
        assert_eq!(hovered(&ix.tick(1000.0)), None);
        // Resting on the frame's own area hovers it after the grace period.
        at(&mut ix, [150.0, 150.0], 2000.0);
        assert_eq!(hovered(&ix.tick(2000.0 + HOVER_GRACE_MS)), Some(Some("fff".into())));
    }

    #[test]
    fn select_all_delete_and_nudge_act_on_top_level_nodes_only() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        ix.key_down(&mut ctx, "a", Mods { meta: true, ..Default::default() });
        let mut sel = ix.selection.clone();
        sel.sort();
        assert_eq!(sel, vec!["fff".to_string(), "ggg".to_string()]);
        // A frame and its child (marquee, layers): the child goes along with the frame.
        ix.selection = vec!["fff".into(), "aaa".into()];
        let fx = ix.key_down(&mut ctx, "ArrowRight", Mods::default()).unwrap();
        let ids: Vec<String> = fx
            .iter()
            .flat_map(|e| match e {
                Effect::Commit(c) => c.iter().map(|c| match c {
                    Command::UpdateNode { id, .. } | Command::DeleteNode { id } => id.clone(),
                    _ => String::new(),
                }).collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(ids, vec!["fff".to_string()]);
        let fx = ix.key_down(&mut ctx, "Delete", Mods::default()).unwrap();
        let deleted: Vec<String> = fx
            .iter()
            .flat_map(|e| match e {
                Effect::Commit(c) => c.iter().filter_map(|c| match c {
                    Command::DeleteNode { id } => Some(id.clone()),
                    _ => None,
                }).collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(deleted, vec!["fff".to_string()]);
    }

    #[test]
    fn clicks_select_the_group_then_double_click_enters_it() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        ix.pointer_down(&mut ctx, [550.0, 50.0], 0, Mods::default());
        ix.pointer_up(&mut ctx, [550.0, 50.0], Mods::default());
        assert_eq!(ix.selection, vec!["ggg".to_string()]);
        let (entered, node, _) = ix.double_click(&mut ctx, [550.0, 50.0]);
        assert_eq!((entered.as_deref(), node.as_deref()), (Some("ggg"), Some("ccc")));
        assert_eq!(ix.selection, vec!["ccc".to_string()]);
        // Esc goes back up to the group.
        ix.key_down(&mut ctx, "Escape", Mods::default());
        assert_eq!(ix.selection, vec!["ggg".to_string()]);
        assert!(ix.entered.is_none());
        // ⌘-click deep-selects.
        ix.pointer_down(&mut ctx, [550.0, 50.0], 0, Mods { meta: true, ..Default::default() });
        assert_eq!(ix.selection, vec!["ccc".to_string()]);
        // Frame children are selected directly.
        ix.pointer_down(&mut ctx, [50.0, 50.0], 0, Mods::default());
        assert_eq!(ix.selection, vec!["aaa".to_string()]);
    }

    #[test]
    fn dragging_out_of_a_frame_reparents_and_esc_cancels() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        // a (in frame F) dragged onto empty canvas: moves to the top level.
        ix.pointer_down(&mut ctx, [50.0, 50.0], 0, Mods::default());
        ix.pointer_move(&mut ctx, [50.0, 400.0], Mods::default());
        assert_eq!(ix.drop_target(), None);
        let fx = ix.pointer_up(&mut ctx, [50.0, 400.0], Mods::default());
        let cmds = fx.iter().find_map(|e| match e { Effect::Commit(c) => Some(c.clone()), _ => None }).unwrap();
        assert!(matches!(&cmds[0], Command::MoveNode { id, parent: None, .. } if id == "aaa"));
        // Back over the frame: it is the drop target (highlighted) and nothing moves on Esc.
        ix.pointer_down(&mut ctx, [250.0, 50.0], 0, Mods::default());
        ix.pointer_move(&mut ctx, [300.0, 150.0], Mods::default());
        assert_eq!(ix.drop_target(), Some("fff"));
        let fx = ix.key_down(&mut ctx, "Escape", Mods::default()).unwrap();
        assert!(fx.iter().any(|e| matches!(e, Effect::Preview(p) if p[0].1 == [200.0, 10.0, 100.0, 100.0])));
        assert!(ix.pointer_up(&mut ctx, [300.0, 150.0], Mods::default()).iter().all(|e| !matches!(e, Effect::Commit(_))));
    }

    #[test]
    fn corner_resize_keeps_aspect_and_anchors_opposite_corner() {
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 4, [100.0, 0.0], true);
        assert_eq!(r, [0.0, 0.0, 300.0, 150.0]);
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 0, [-100.0, 0.0], true);
        assert_eq!(r, [-100.0, -50.0, 300.0, 150.0]);
    }

    fn committed(fx: &[Effect]) -> Vec<(String, [f64; 2])> {
        fx.iter()
            .flat_map(|e| match e {
                Effect::Commit(c) => c
                    .iter()
                    .filter_map(|c| match c {
                        Command::UpdateNode { id, props } => Some((id.clone(), [props["x"].as_f64()?, props["y"].as_f64()?])),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn previewed(fx: &[Effect]) -> Option<[f32; 4]> {
        fx.iter().find_map(|e| match e {
            Effect::Preview(p) => Some(p[0].1),
            _ => None,
        })
    }

    #[test]
    fn drags_snap_lock_axis_and_cmd_turns_snapping_off() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        // Group G (500,0 100×100) dragged so its left edge lands 2 px right of F's right edge (400).
        ix.pointer_down(&mut ctx, [550.0, 50.0], 0, Mods::default());
        let fx = ix.pointer_move(&mut ctx, [452.0, 350.0], Mods::default());
        assert_eq!(previewed(&fx).unwrap()[0], 400.0);
        assert!(!ix.guides().is_empty());
        // ⌘ held: no snapping, no guides.
        let fx = ix.pointer_move(&mut ctx, [452.0, 350.0], Mods { meta: true, ..Default::default() });
        assert_eq!(previewed(&fx).unwrap()[0], 402.0);
        assert!(ix.guides().is_empty());
        // ⇧: the dominant axis only (y moved 300, x 98 → x stays).
        let fx = ix.pointer_move(&mut ctx, [452.0, 350.0], Mods { shift: true, meta: true, ..Default::default() });
        assert_eq!(previewed(&fx).unwrap()[0..2], [500.0, 300.0]);
        let fx = ix.pointer_up(&mut ctx, [452.0, 350.0], Mods { shift: true, meta: true, ..Default::default() });
        assert_eq!(committed(&fx), vec![("ggg".to_string(), [500.0, 300.0])]);
        assert!(ix.guides().is_empty());
    }

    #[test]
    fn a_long_press_drops_the_drag_threshold() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction::default();
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        ix.pointer_down(&mut ctx, [550.0, 50.0], 0, Mods::default());
        // 2 px right away: below the 3 px threshold.
        assert!(previewed(&ix.pointer_move(&mut ctx, [552.0, 50.0], Mods::default())).is_none());
        // After holding 500 ms, 1 px is a drag (⌘: no snapping, so the 1 px stays 1 px).
        ctx.now = 600.0;
        ix.pointer_move(&mut ctx, [550.0, 50.0], Mods::default());
        let fx = ix.pointer_move(&mut ctx, [551.0, 50.0], Mods { meta: true, ..Default::default() });
        assert_eq!(previewed(&fx).unwrap()[0], 501.0);
    }

    #[test]
    fn edges_resize_along_their_length_and_tiny_nodes_have_no_handles() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let ix = Interaction { selection: vec!["fff".into()], ..Default::default() };
        let ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        // F is 400×200: its right edge at x 400, a third of the way down (no handle there).
        assert_eq!(ix.handle_at(&ctx, [402.0, 70.0]).map(|h| h.1), Some(3));
        assert_eq!(ix.handle_at(&ctx, [150.0, 198.0]).map(|h| h.1), Some(5));
        assert_eq!(ix.handle_at(&ctx, [200.0, 100.0]), None);
        // Zoomed out so F is 12×6 px on screen: no handles, no edges; it drags.
        ctx.camera.zoom = 0.03;
        assert_eq!(ix.handle_at(&ctx, [12.0, 3.0]), None);
    }

    #[test]
    fn space_moves_the_box_mid_resize_and_resize_snaps() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction { selection: vec!["fff".into()], ..Default::default() };
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        // Right edge of F dragged to 498: snaps to G's left edge (500).
        ix.pointer_down(&mut ctx, [400.0, 70.0], 0, Mods::default());
        let fx = ix.pointer_move(&mut ctx, [498.0, 70.0], Mods::default());
        assert_eq!(previewed(&fx).unwrap(), [0.0, 0.0, 500.0, 200.0]);
        // Space held: the box follows the pointer at its current size.
        ix.key_down(&mut ctx, " ", Mods::default());
        let fx = ix.pointer_move(&mut ctx, [478.0, 370.0], Mods { meta: true, ..Default::default() });
        assert_eq!(previewed(&fx).unwrap(), [-20.0, 300.0, 498.0, 200.0]);
        ix.key_up(" ");
        // Esc puts it back and keeps the selection.
        let fx = ix.key_down(&mut ctx, "Escape", Mods::default()).unwrap();
        assert_eq!(previewed(&fx).unwrap(), [0.0, 0.0, 400.0, 200.0]);
        assert_eq!(ix.selection, vec!["fff".to_string()]);
    }

    #[test]
    fn esc_ladder_and_enter_walk_the_hierarchy() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction { selection: vec!["fff".into()], ..Default::default() };
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 0.0 };
        // ⏎: the frame's children.
        ix.key_down(&mut ctx, "Enter", Mods::default()).unwrap();
        let mut sel = ix.selection.clone();
        sel.sort();
        assert_eq!(sel, vec!["aaa".to_string(), "bbb".to_string()]);
        // Esc: back to the frame, then nothing.
        ix.key_down(&mut ctx, "Escape", Mods::default());
        assert_eq!(ix.selection, vec!["fff".to_string()]);
        ix.key_down(&mut ctx, "Escape", Mods::default());
        assert!(ix.selection.is_empty());
        // ⇧⏎ on a group child: the group (and ⏎ on a leaf isn't ours: media previews).
        ix.selection = vec!["ccc".into()];
        assert!(ix.key_down(&mut ctx, "Enter", Mods::default()).is_none());
        ix.key_down(&mut ctx, "Enter", Mods { shift: true, ..Default::default() });
        assert_eq!(ix.selection, vec!["ggg".to_string()]);
        ix.key_down(&mut ctx, "Enter", Mods::default());
        assert_eq!((ix.selection.clone(), ix.entered.as_deref()), (vec!["ccc".to_string()], Some("ggg")));
    }

    #[test]
    fn nudge_guides_linger_then_clear() {
        let scene = scene();
        let mut cam = Camera { zoom: 1.0, viewport: [1000.0, 1000.0], ..Default::default() };
        let mut ix = Interaction { selection: vec!["ggg".into()], ..Default::default() };
        let mut ctx = Ctx { scene: &scene, camera: &mut cam, dpr: 1.0, now: 1000.0 };
        // G (top 0) nudged down 1: its top no longer lines up with F's, nothing to show.
        ix.key_down(&mut ctx, "ArrowDown", Mods::default());
        assert!(ix.guides().is_empty());
        // Nudged left (the scene is static here, so from y 0): the tops line up with F.
        ix.key_down(&mut ctx, "ArrowLeft", Mods::default());
        assert!(!ix.guides().is_empty());
        assert!(ix.tick(1000.0 + snap::NUDGE_GUIDES_MS - 1.0).is_empty());
        assert!(!ix.guides().is_empty());
        ix.tick(1000.0 + snap::NUDGE_GUIDES_MS);
        assert!(ix.guides().is_empty());
    }

    #[test]
    fn edge_resize_is_free() {
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 3, [50.0, 30.0], true);
        assert_eq!(r, [0.0, 0.0, 250.0, 100.0]);
    }
}
