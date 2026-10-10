//! Canvas input: tools, pointer/wheel/key handling, selection, move/resize/marquee and
//! annotation drafting. Platform-free: takes screen points in physical pixels and returns
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
    Annotate,
}

impl Tool {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "select" => Some(Tool::Select),
            "hand" => Some(Tool::Hand),
            "annotate" => Some(Tool::Annotate),
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

/// An annotation pin as placed on screen by the app (world point of its anchor).
#[derive(Clone, Debug)]
pub struct Pin {
    pub id: String,
    pub world: [f32; 2],
}

/// A region drawn with the annotate tool, in node-local normalized coordinates (0..1).
#[derive(Clone, Debug, PartialEq)]
pub enum DraftRegion {
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
    Draft { node: String, region: DraftRegion },
    CancelDraft,
    OpenAnnotation(String),
    /// Deselect the highlighted annotation (click on empty canvas, Escape).
    CloseAnnotation,
    Hover { node: Option<String>, annotation: Option<String> },
    Camera,
    Cursor(&'static str),
    Tool(Tool),
    Undo,
    Redo,
    FitAll,
    /// Overlay must be rebuilt (marquee, draft box, hover changed).
    Overlay,
}

#[derive(Clone, Debug)]
enum Drag {
    Pan { last: [f32; 2] },
    Move { start: [f32; 2], items: Vec<(String, [f32; 4])>, moved: bool },
    Resize { id: String, handle: usize, orig: [f32; 4], start: [f32; 2], keep_aspect: bool },
    Marquee { start: [f32; 2], cur: [f32; 2], base: Vec<String> },
    Annotate { node: String, start: [f32; 2], cur: [f32; 2], start_px: [f32; 2] },
}

/// Pin hit radius and handle size, in CSS pixels.
const PIN_PX: f32 = 26.0;
const HANDLE_PX: f32 = 8.0;
const DRAG_SLOP_PX: f32 = 3.0;

pub struct Interaction {
    pub tool: Tool,
    pub selection: Vec<String>,
    pub hover: Option<String>,
    pub hover_pin: Option<String>,
    pub space: bool,
    drag: Option<Drag>,
}

impl Default for Interaction {
    fn default() -> Self {
        Self { tool: Tool::Select, selection: Vec::new(), hover: None, hover_pin: None, space: false, drag: None }
    }
}

pub struct Ctx<'a> {
    pub scene: &'a Scene,
    pub camera: &'a mut Camera,
    pub pins: &'a [Pin],
    /// Device pixel ratio (screen points are physical pixels).
    pub dpr: f32,
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

impl Interaction {
    /// Marquee rect in world units while selecting by drag.
    pub fn marquee(&self) -> Option<[f32; 4]> {
        match &self.drag {
            Some(Drag::Marquee { start, cur, .. }) => Some(norm_rect(*start, *cur)),
            _ => None,
        }
    }

    /// Annotation box being drawn: node id and world rect.
    pub fn annotate_box(&self) -> Option<(&str, [f32; 4])> {
        match &self.drag {
            Some(Drag::Annotate { node, start, cur, .. }) => Some((node, norm_rect(*start, *cur))),
            _ => None,
        }
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn pin_at(&self, ctx: &Ctx, p: [f32; 2]) -> Option<String> {
        let r = PIN_PX * ctx.dpr;
        ctx.pins.iter().rev().find_map(|pin| {
            let s = ctx.camera.world_to_screen(pin.world);
            // Pins are drawn above-right of their anchor (see overlay).
            let c = [s[0] + r * 0.5, s[1] - r * 0.5];
            ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2) <= (r * 0.6).powi(2)).then(|| pin.id.clone())
        })
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
        if let Some(id) = self.pin_at(ctx, p) {
            fx.push(Effect::OpenAnnotation(id));
            return fx;
        }
        match self.tool {
            Tool::Select => {
                if let Some((id, handle)) = self.handle_at(ctx, p) {
                    let n = ctx.scene.get(&id).expect("selected node");
                    let keep_aspect = is_media(n) != mods.shift;
                    self.drag = Some(Drag::Resize { id, handle, orig: n.rect, start: w, keep_aspect });
                    return fx;
                }
                match ctx.scene.hit_test(w) {
                    Some(n) => {
                        let id = n.id.clone();
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
                            .filter(|n| n.handle.is_some())
                            .map(|n| (n.id.clone(), n.rect))
                            .collect();
                        self.drag = Some(Drag::Move { start: w, items, moved: false });
                    }
                    None => {
                        let base = if mods.shift { self.selection.clone() } else { Vec::new() };
                        fx.push(Effect::CloseAnnotation);
                        if !mods.shift && !self.selection.is_empty() {
                            self.selection.clear();
                            fx.push(Effect::Select(Vec::new()));
                        }
                        self.drag = Some(Drag::Marquee { start: w, cur: w, base });
                    }
                }
            }
            Tool::Annotate => match ctx.scene.hit_test(w) {
                Some(n) if n.kind != NodeKind::Frame => {
                    self.drag = Some(Drag::Annotate { node: n.id.clone(), start: w, cur: w, start_px: p });
                }
                _ => fx.push(Effect::CancelDraft),
            },
            Tool::Hand => {}
        }
        fx
    }

    pub fn pointer_move(&mut self, ctx: &mut Ctx, p: [f32; 2], _mods: Mods) -> Vec<Effect> {
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
                for id in ctx.scene.intersecting(norm_rect(*start, *cur)) {
                    if !sel.contains(&id) {
                        sel.push(id);
                    }
                }
                if sel != self.selection {
                    self.selection = sel.clone();
                    fx.push(Effect::Select(sel));
                }
                fx.push(Effect::Overlay);
            }
            Some(Drag::Annotate { cur, .. }) => {
                *cur = w;
                fx.push(Effect::Overlay);
            }
            None => fx.extend(self.update_hover(ctx, p)),
        }
        fx
    }

    fn update_hover(&mut self, ctx: &Ctx, p: [f32; 2]) -> Vec<Effect> {
        let mut fx = Vec::new();
        let w = ctx.camera.screen_to_world(p);
        let pin = self.pin_at(ctx, p);
        let node = if pin.is_some() { None } else { ctx.scene.hit_test(w).map(|n| n.id.clone()) };
        let cursor = if self.space || self.tool == Tool::Hand {
            "grab"
        } else if pin.is_some() {
            "pointer"
        } else if let Some((_, i)) = self.handle_at(ctx, p) {
            HANDLE_CURSORS[i]
        } else if self.tool == Tool::Annotate {
            if node.is_some() { "crosshair" } else { "default" }
        } else {
            "default"
        };
        fx.push(Effect::Cursor(cursor));
        if node != self.hover || pin != self.hover_pin {
            self.hover = node.clone();
            self.hover_pin = pin.clone();
            fx.push(Effect::Hover { node, annotation: pin });
            fx.push(Effect::Overlay);
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
            Some(Drag::Annotate { node, start, start_px, .. }) => {
                if let Some(n) = ctx.scene.get(&node) {
                    let clamp = |l: [f32; 2]| [l[0].clamp(0.0, 1.0), l[1].clamp(0.0, 1.0)];
                    let a = clamp(n.to_local(start));
                    let b = clamp(n.to_local(w));
                    let far = (p[0] - start_px[0]).abs().max((p[1] - start_px[1]).abs()) > 4.0 * ctx.dpr;
                    let region = if far {
                        DraftRegion::Box([a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()])
                    } else {
                        DraftRegion::Point(a)
                    };
                    fx.push(Effect::Draft { node, region });
                }
                fx.push(Effect::Overlay);
            }
            None => {}
        }
        fx
    }

    /// Wheel / trackpad: pan, or zoom with Cmd/Ctrl (browsers send pinch as ctrl+wheel).
    pub fn wheel(&mut self, ctx: &mut Ctx, p: [f32; 2], d: [f32; 2], mods: Mods) -> Vec<Effect> {
        if mods.meta {
            let dy = d[1].clamp(-60.0, 60.0);
            ctx.camera.zoom_at(p, (-dy * 0.012).exp());
        } else if mods.shift && d[0] == 0.0 {
            ctx.camera.pan(-d[1], 0.0);
        } else {
            ctx.camera.pan(-d[0], -d[1]);
        }
        vec![Effect::Camera]
    }

    /// Safari gesture events: relative scale factor around a screen point.
    pub fn pinch(&mut self, ctx: &mut Ctx, p: [f32; 2], factor: f32) -> Vec<Effect> {
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
            "Escape" => {
                self.drag = None;
                if !self.selection.is_empty() {
                    self.selection.clear();
                    fx.push(Effect::Select(Vec::new()));
                }
                fx.push(Effect::CloseAnnotation);
                fx.push(Effect::CancelDraft);
                if self.tool != Tool::Select {
                    fx.extend(self.set_tool(Tool::Select));
                }
                fx.push(Effect::Overlay);
            }
            "v" | "V" if !mods.meta => fx.extend(self.set_tool(Tool::Select)),
            "h" | "H" if !mods.meta => fx.extend(self.set_tool(Tool::Hand)),
            "c" | "C" if !mods.meta => fx.extend(self.set_tool(Tool::Annotate)),
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
        before != self.selection.len()
    }
}

fn norm_rect(a: [f32; 2], b: [f32; 2]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), (a[0] - b[0]).abs(), (a[1] - b[1]).abs()]
}

#[cfg(test)]
mod tests {
    use super::*;

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
