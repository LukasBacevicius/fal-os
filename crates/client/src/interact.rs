//! Canvas input: tools, pointer/wheel/key handling, selection, move/resize/marquee and
//! Alt-click / Alt-drag context picks on media (chat context chips). Platform-free: takes screen points in physical pixels and returns
//! [`Effect`]s that the app applies (renderer previews, doc commits, UI events).

use falos_doc::{Command, NodeKind, Props};
use falos_engine::Camera;
use serde::Serialize;
use serde_json::json;

use crate::scene::{Scene, SceneNode};

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
    Move { start: [f32; 2], items: Vec<(String, [f32; 4])>, moved: bool },
    Resize { id: String, handle: usize, orig: [f32; 4], start: [f32; 2], keep_aspect: bool },
    Marquee { start: [f32; 2], cur: [f32; 2], base: Vec<String> },
    Pick { node: String, start: [f32; 2], cur: [f32; 2], start_px: [f32; 2] },
}

/// Handle size, in CSS pixels.
const HANDLE_PX: f32 = 8.0;
const DRAG_SLOP_PX: f32 = 3.0;
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
    drag: Option<Drag>,
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
            drag: None,
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

    /// Resolves a pending hover once its grace period is over (call every frame).
    pub fn tick(&mut self, now: f64) -> Vec<Effect> {
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

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn handle_at(&self, ctx: &Ctx, p: [f32; 2]) -> Option<(String, usize)> {
        if self.tool != Tool::Select || self.selection.len() != 1 {
            return None;
        }
        let n = ctx.scene.get(&self.selection[0])?;
        let r = (HANDLE_PX * 0.5 + 3.0) * ctx.dpr;
        handle_points(n.rect).iter().position(|hp| {
            let s = ctx.camera.world_to_screen(*hp);
            (p[0] - s[0]).abs() <= r && (p[1] - s[1]).abs() <= r
        })
        .map(|i| (n.id.clone(), i))
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
                    self.drag = Some(Drag::Resize { id, handle, orig: n.rect, start: w, keep_aspect });
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
                        let items = self
                            .selection
                            .iter()
                            .filter_map(|s| ctx.scene.get(s))
                            .filter(|n| n.handle.is_some() || n.kind == NodeKind::Group)
                            .map(|n| (n.id.clone(), n.rect))
                            .collect();
                        self.drag = Some(Drag::Move { start: w, items, moved: false });
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
        match &mut self.drag {
            Some(Drag::Pan { last }) => {
                ctx.camera.pan(p[0] - last[0], p[1] - last[1]);
                *last = p;
                fx.push(Effect::Camera);
            }
            Some(Drag::Move { start, items, moved }) => {
                let d = [w[0] - start[0], w[1] - start[1]];
                let slop = DRAG_SLOP_PX / ctx.camera.zoom * ctx.dpr;
                if !*moved && d[0].abs() < slop && d[1].abs() < slop {
                    return fx;
                }
                *moved = true;
                fx.push(Effect::Preview(items.iter().map(|(id, r)| (id.clone(), [r[0] + d[0], r[1] + d[1], r[2], r[3]])).collect()));
                fx.push(Effect::Cursor("move"));
            }
            Some(Drag::Resize { id, handle, orig, start, keep_aspect }) => {
                let r = resize_rect(*orig, *handle, [w[0] - start[0], w[1] - start[1]], *keep_aspect);
                fx.push(Effect::Preview(vec![(id.clone(), r)]));
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
        match self.drag.take() {
            Some(Drag::Pan { .. }) => fx.push(Effect::Cursor(if self.space || self.tool == Tool::Hand { "grab" } else { "default" })),
            Some(Drag::Move { start, items, moved: true }) => {
                let d = [w[0] - start[0], w[1] - start[1]];
                let cmds = items
                    .iter()
                    .map(|(id, r)| Command::UpdateNode { id: id.clone(), props: rect_props([r[0] + d[0], r[1] + d[1], r[2], r[3]]) })
                    .collect();
                fx.push(Effect::Commit(cmds));
            }
            Some(Drag::Move { .. }) => {}
            Some(Drag::Resize { id, handle, orig, start, keep_aspect }) => {
                let r = resize_rect(orig, handle, [w[0] - start[0], w[1] - start[1]], keep_aspect);
                if r != orig {
                    fx.push(Effect::Commit(vec![Command::UpdateNode { id, props: rect_props(r) }]));
                }
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
                self.selection = ctx.scene.iter().filter(|n| n.handle.is_some()).map(|n| n.id.clone()).collect();
                fx.push(Effect::Select(self.selection.clone()));
            }
            "Backspace" | "Delete" if !self.selection.is_empty() => {
                let cmds = self.selection.iter().map(|id| Command::DeleteNode { id: id.clone() }).collect();
                self.selection.clear();
                fx.push(Effect::Select(Vec::new()));
                fx.push(Effect::Commit(cmds));
            }
            "Escape" if self.entered.is_some() => {
                // Go up one level: select the entered group, leave it.
                let g = self.entered.take().unwrap_or_default();
                self.entered = ctx.scene.get(&g).and_then(|n| n.parent.clone()).filter(|p| ctx.scene.get(p).is_some_and(|n| n.kind == NodeKind::Group));
                self.selection = vec![g];
                fx.push(Effect::Select(self.selection.clone()));
                fx.push(Effect::Overlay);
            }
            "Escape" => {
                self.drag = None;
                if !self.selection.is_empty() {
                    self.selection.clear();
                    fx.push(Effect::Select(Vec::new()));
                }
                if self.tool != Tool::Select {
                    fx.extend(self.set_tool(Tool::Select));
                }
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
                let cmds = self
                    .selection
                    .iter()
                    .filter_map(|id| ctx.scene.get(id))
                    .map(|n| Command::UpdateNode { id: n.id.clone(), props: rect_props([n.rect[0] + d[0], n.rect[1] + d[1], n.rect[2], n.rect[3]]) })
                    .collect();
                fx.push(Effect::Commit(cmds));
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
    fn corner_resize_keeps_aspect_and_anchors_opposite_corner() {
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 4, [100.0, 0.0], true);
        assert_eq!(r, [0.0, 0.0, 300.0, 150.0]);
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 0, [-100.0, 0.0], true);
        assert_eq!(r, [-100.0, -50.0, 300.0, 150.0]);
    }

    #[test]
    fn edge_resize_is_free() {
        let r = resize_rect([0.0, 0.0, 200.0, 100.0], 3, [50.0, 30.0], true);
        assert_eq!(r, [0.0, 0.0, 250.0, 100.0]);
    }
}
