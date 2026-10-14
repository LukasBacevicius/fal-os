//! `CanvasApp`: the wasm-bindgen surface used by the web UI (apps/web).
//!
//! All state lives in one `App` behind `Rc<RefCell<…>>`. Every exported method borrows it
//! briefly, queues UI events, releases the borrow and only then calls the JS listener, so a
//! listener may call straight back into the app. Async work (hashing, decoding, tile
//! production) runs in `spawn_local` tasks that re-borrow only to publish results.

mod media;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::{Rc, Weak};

use falos_doc::loro::VersionVector;
use falos_doc::{Actor, Asset, Canvas, Command, NodeKind, Props, ReadOpts, TxResult};
use falos_engine::web_video::{IngestMode, WebVideos};
use falos_engine::{Camera, Content as EngineContent, GenTile, Handle, Kind, NodeDesc, OverlayPrim, Pixels, Renderer, Tier};
use js_sys::{Reflect, Uint8Array};
use serde_json::{Value as Json, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::api;
use crate::interact::{Ctx, Effect, Interaction, Mods, PickRegion, Tool};
use crate::overlay::{self, ActivityFx, CursorView, OverlayInput, Palette};
use crate::scene::{ChangeFeed, RenderSink, Scene, SceneNode};
use crate::sync::{Incoming, LocalOnly, Outgoing, Presence, SyncTransport};
use media::{Content, MediaStore, classify, content_of, source_spec};

const MAX_INFLIGHT: usize = 6;
/// Generating tiles and activity outlines redraw at this interval (ms) while visible; both
/// change in discrete steps (~2 per second), so 10 Hz is plenty.
const ANIM_MS: f64 = 100.0;

type RafSlot = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

struct Gpu {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

struct App {
    me: Weak<RefCell<App>>,
    canvas_el: web_sys::HtmlCanvasElement,
    gpu: Gpu,
    camera: Camera,
    dpr: f32,
    doc: Canvas,
    feed: ChangeFeed,
    actor: Actor,
    scene: Scene,
    ix: Interaction,
    media: MediaStore,
    videos: WebVideos,
    handles: HashMap<Handle, String>,
    todo: VecDeque<(Handle, Tier)>,
    inflight: usize,
    failed: HashSet<String>,
    presence: Vec<Presence>,
    /// Soft claims from the host: node ids (outlined in the overlay).
    claims: Vec<String>,
    /// Transient agent-activity effects; `node` (when set) makes the effect follow the node.
    fx: Vec<(Option<String>, ActivityFx)>,
    /// Frames/groups agents are working on: a steady outline in the chat color.
    working: Vec<(String, u32)>,
    reduce_motion: bool,
    /// Nodes highlighted from outside the canvas (layers row hover).
    highlight: Vec<String>,
    /// Frame highlighted as the target of a drag from outside the canvas (files, library).
    drop_target: Option<String>,
    /// Asset id → (source badge, uploaded to fal), refreshed when asset records change.
    sources: HashMap<String, (u32, bool)>,
    /// Lineage view: the nodes kept bright (the selection's ancestors and descendants);
    /// everything else is dimmed. `None` = off.
    lineage: Option<HashSet<String>>,
    palette: Palette,
    /// Generation nodes removed recently (id → ms): when a media node with the same id
    /// appears (the host converts a finished generation in place), it reveals with a crossfade.
    gen_removed: HashMap<String, f64>,
    /// Media nodes revealing from a generating tile.
    reveal: HashSet<String>,
    /// Animation clock (s): stands still during heavy gestures.
    anim_time: f64,
    last_tick: f64,
    last_anim_frame: f64,
    transport: Box<dyn SyncTransport>,
    js_inbox: Rc<RefCell<Vec<Incoming>>>,
    sent_vv: VersionVector,
    events: Vec<(&'static str, Json)>,
    listener: Option<js_sys::Function>,
    dirty: bool,
    cursor: &'static str,
    frame_times: Vec<f64>,
    last_frame: f64,
    last_stats: f64,
    last_camera: Camera,
    stats: falos_engine::FrameStats,
    raf: Option<RafSlot>,
    /// This app's GPU device (see [`LOST_DEVICES`]); `lost` once it is gone: nothing renders,
    /// and a `device_lost` event tells the page to recreate the app (the last frame stays).
    gpu_id: u32,
    lost: bool,
}

/// Devices whose `lost` promise resolved: (gpu id, reason). wgpu's callback must be `Send`, so
/// it can't hold the app; `tick` picks its entry up.
static LOST_DEVICES: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());
static NEXT_GPU_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The canvas client. Create with `CanvasApp.create(canvas, options)`.
#[wasm_bindgen]
pub struct CanvasApp {
    app: Rc<RefCell<App>>,
}

fn err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn now_ms() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}

fn to_js(v: &Json) -> JsValue {
    serde_wasm(v)
}

fn serde_wasm(v: &Json) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

fn from_js(v: &JsValue) -> Json {
    if v.is_undefined() || v.is_null() {
        return Json::Null;
    }
    js_sys::JSON::stringify(v).ok().and_then(|s| s.as_string()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Json::Null)
}

/// Decodes Figma clipboard HTML (`text/html` of a Figma ⌘C) into an import plan (JSON
/// text, see `falos_figma::Plan`). Errors when the HTML carries no Figma scene.
#[wasm_bindgen(js_name = figmaPlan)]
pub fn figma_plan(html: &str) -> Result<String, JsValue> {
    let plan = falos_figma::plan_html(html).map_err(|e| JsValue::from_str(&e.to_string()))?;
    serde_json::to_string(&plan).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Warn);
}

// ---- engine sink ------------------------------------------------------------------------------

struct EngineSink<'a> {
    r: &'a mut Renderer,
    media: &'a MediaStore,
    videos: &'a WebVideos,
    handles: &'a mut HashMap<Handle, String>,
    todo: &'a mut VecDeque<(Handle, Tier)>,
    palette: &'a Palette,
    gen_removed: &'a mut HashMap<String, f64>,
    reveal: &'a mut HashSet<String>,
}

/// On-screen font sizes (CSS px) between which the page's DOM text fades in over a text
/// raster (`MIN_FONT_PX` / `FULL_FONT_PX` in TextLayer.tsx).
const TEXT_MIN_FONT_PX: f32 = 3.5;
const TEXT_FULL_FONT_PX: f32 = 6.5;

/// Stable per-node seed for the generating tile's dots.
fn seed_of(id: &str) -> u32 {
    id.bytes().fold(2_166_136_261u32, |h, b| (h ^ b as u32).wrapping_mul(16_777_619))
}

fn desc_for(n: &SceneNode, media: &MediaStore, pal: &Palette, reveal: bool) -> NodeDesc {
    let mut d = NodeDesc {
        pos: [n.rect[0], n.rect[1]],
        size: [n.rect[2], n.rect[3]],
        color: pal.panel,
        radius: 0.0,
        ..Default::default()
    };
    let content = content_of(n);
    // Corner radius in canvas units: negative tells the shader it scales with zoom.
    let radius = n.corner_radius();
    if radius > 0.0 {
        d.radius = -radius;
    }
    match content {
        Content::Image | Content::Video => {
            d.kind = if matches!(content, Content::Video) { Kind::Video } else { Kind::Image };
            d.content = EngineContent::External;
            d.aspect = n.asset.as_deref().and_then(|a| media.aspect(a)).unwrap_or(0.0);
            if n.transparent() {
                d.color = 0;
            }
            if reveal {
                d.color = pal.tile;
                d.generating = Some(GenTile { seed: seed_of(&n.id), progress: -1.0, reveal: true });
            }
        }
        Content::Card => {
            d.kind = Kind::Image;
            d.content = EngineContent::External;
        }
        Content::TextBox => {
            // The text raster (greeked when tiny); the page's DOM text fades in over it.
            d.kind = Kind::Image;
            d.content = EngineContent::External;
            d.color = pal.panel;
            d.text = true;
            if n.styled_text() {
                // Bare text (no backdrop). Its DOM twin is transparent too, so instead of being
                // covered, the raster fades out as the DOM text fades in (TextLayer.tsx).
                d.color = 0;
                let fs = n.card_field(1).parse::<f32>().ok().filter(|v| *v > 0.0).unwrap_or(12.5);
                d.text_fade = Some([TEXT_MIN_FONT_PX / fs, TEXT_FULL_FONT_PX / fs]);
            }
        }
        Content::Generating => {
            d.kind = Kind::Flat;
            d.content = EngineContent::None;
            d.color = pal.tile;
            let mut f = n.card.split('\u{1}');
            let status = f.next().unwrap_or("");
            let progress = f.next().and_then(|p| p.parse::<f32>().ok()).filter(|_| status == "running").unwrap_or(-1.0);
            d.generating = Some(GenTile { seed: seed_of(&n.id), progress: progress.clamp(-1.0, 1.0), reveal: false });
        }
        Content::None => {
            // Frames: their background color (`fill`) or none; unfilled ones get the overlay's
            // 1 px outline.
            d.kind = Kind::Flat;
            d.content = EngineContent::None;
            d.color = n.frame_fill().unwrap_or(0);
        }
    }
    d
}

impl EngineSink<'_> {
    fn attach_video(&mut self, h: Handle, n: &SceneNode) {
        if let (Content::Video, Some(a)) = (content_of(n), n.asset.as_deref().and_then(|a| self.media.get(a))) {
            self.videos.set_src(h, &a.url);
        }
    }
}

impl EngineSink<'_> {
    fn desc(&self, n: &SceneNode) -> NodeDesc {
        desc_for(n, self.media, self.palette, self.reveal.contains(&n.id))
    }
}

impl RenderSink for EngineSink<'_> {
    fn add(&mut self, n: &SceneNode) -> Handle {
        if self.gen_removed.remove(&n.id).is_some() && matches!(content_of(n), Content::Image | Content::Video) {
            self.reveal.insert(n.id.clone());
        }
        let h = self.r.add(self.desc(n));
        self.handles.insert(h, n.id.clone());
        self.attach_video(h, n);
        if !matches!(content_of(n), Content::None | Content::Generating) {
            self.todo.push_front((h, Tier::Low));
        }
        h
    }

    fn update(&mut self, h: Handle, n: &SceneNode, mut content: bool) {
        // A placeholder image that just got its pixels (streaming paste): dots until they are
        // resident, then the crossfade, like a finished generation. Transparent ones (vector
        // art) just appear: the reveal draws over an opaque tile color.
        if matches!(content_of(n), Content::Image)
            && !n.transparent()
            && self.r.desc(h).is_some_and(|o| o.generating.is_some_and(|g| !g.reveal))
        {
            self.reveal.insert(n.id.clone());
        }
        let d = self.desc(n);
        if let (Content::Card, Some(old)) = (content_of(n), self.r.desc(h)) {
            let a0 = old.size[0] / old.size[1].max(1e-3);
            let a1 = d.size[0] / d.size[1].max(1e-3);
            content |= (a0 - a1).abs() / a1.max(1e-3) > 0.02;
        }
        // Text rasters are laid out for the box: a resize re-wraps them.
        if let (Content::TextBox, Some(old)) = (content_of(n), self.r.desc(h)) {
            content |= (0..2).any(|i| (old.size[i] - d.size[i]).abs() > 0.5);
        }
        self.r.update(h, d);
        if content {
            self.r.invalidate(h);
            self.attach_video(h, n);
            self.todo.push_front((h, Tier::Low));
        }
    }

    fn remove(&mut self, h: Handle, n: &SceneNode) {
        if n.kind == NodeKind::Generation {
            let now = now_ms();
            self.gen_removed.retain(|_, t| now - *t < 10_000.0);
            self.gen_removed.insert(n.id.clone(), now);
        }
        self.reveal.remove(&n.id);
        self.r.remove(h);
        self.handles.remove(&h);
        self.videos.remove(h);
        self.todo.retain(|t| t.0 != h);
    }

    fn set_order(&mut self, order: &[Handle]) {
        self.r.set_order(order);
    }
}

fn round4(v: f32) -> f64 {
    (v as f64 * 10_000.0).round() / 10_000.0
}

fn region_json(r: &PickRegion) -> Json {
    match *r {
        PickRegion::Point([x, y]) => json!({"kind": "point", "x": round4(x), "y": round4(y)}),
        PickRegion::Box([x, y, w, h]) => json!({"kind": "box", "x": round4(x), "y": round4(y), "w": round4(w), "h": round4(h)}),
    }
}

// ---- app --------------------------------------------------------------------------------------

impl App {
    fn emit(&mut self, kind: &'static str, payload: Json) {
        self.events.push((kind, payload));
    }

    fn sink_apply(&mut self, f: impl FnOnce(&mut Scene, &Canvas, &mut EngineSink)) {
        let mut sink = EngineSink {
            r: &mut self.gpu.renderer,
            media: &self.media,
            videos: &self.videos,
            handles: &mut self.handles,
            todo: &mut self.todo,
            palette: &self.palette,
            gen_removed: &mut self.gen_removed,
            reveal: &mut self.reveal,
        };
        f(&mut self.scene, &self.doc, &mut sink);
    }

    /// Pulls recorded doc changes into the scene/engine (incremental) and notifies the UI.
    fn sync_doc(&mut self) {
        let ch = self.feed.take();
        if !ch.any {
            return;
        }
        self.sink_apply(|scene, doc, sink| scene.apply(doc, &ch, sink));
        self.refresh_sources(ch.assets);
        if self.ix.prune(&self.scene) {
            let sel = self.ix.selection.clone();
            self.emit("selection", json!(sel));
        }
        let nodes = ch.structure || !ch.dirty.is_empty() || !ch.deleted.is_empty();
        self.emit("doc", json!({"version": self.doc.version().to_string(), "nodes": nodes, "assets": ch.assets}));
        self.flush_transport();
        self.dirty = true;
    }

    /// Sends local changes the host hasn't seen. While disconnected nothing is marked as sent,
    /// so edits made offline go out after a reconnect.
    fn flush_transport(&mut self) {
        if !self.transport.connected() {
            return;
        }
        if let Ok(bytes) = self.doc.export_updates(&self.sent_vv)
            && !bytes.is_empty()
        {
            self.transport.send(Outgoing::Update(bytes));
        }
        self.sent_vv = self.doc.vv();
    }

    /// Imports host bytes without echoing them back: ops that arrive from the host count as
    /// sent, local pending ops stay pending.
    fn import_remote(&mut self, bytes: &[u8]) -> Result<(), String> {
        let before = self.doc.vv();
        self.doc.import(bytes).map_err(|e| e.to_string())?;
        for (peer, counter) in self.doc.vv().iter() {
            if before.get(peer).is_none_or(|c| c < counter) {
                self.sent_vv.insert(*peer, *counter);
            }
        }
        Ok(())
    }

    fn commit(&mut self, cmds: &[Command]) -> Result<TxResult, String> {
        self.commit_as(cmds, true)
    }

    /// `tracked: false`: not an undo step of its own (see `Canvas::apply_untracked`).
    fn commit_as(&mut self, cmds: &[Command], tracked: bool) -> Result<TxResult, String> {
        if cmds.is_empty() {
            return Err("no commands".into());
        }
        let r = if tracked { self.doc.apply(&self.actor, None, cmds) } else { self.doc.apply_untracked(&self.actor, cmds) };
        let r = r.map_err(|e| e.to_string());
        if let Err(e) = &r {
            self.emit("error", json!({"message": e}));
        }
        self.sync_doc();
        r
    }

    fn build_overlay(&self) -> Vec<OverlayPrim> {
        let cursors: Vec<CursorView> = self
            .presence
            .iter()
            .filter(|p| p.actor != self.actor.id)
            .filter_map(|p| Some(CursorView { world: p.cursor?, color: overlay::parse_color(&p.color) }))
            .collect();
        let t = self.anim_time as f32;
        // Frames/groups agents work in: a steady outline in the chat color.
        let mut activity: Vec<ActivityFx> = Vec::with_capacity(self.fx.len() + self.working.len());
        for (id, color) in &self.working {
            let Some(n) = self.scene.get(id) else { continue };
            activity.push(ActivityFx { rect: n.rect, shape: OverlayPrim::RECT, color: *color, t0: 0.0, dur: 0.0, seed: 0.0 });
        }
        for (node, f) in &self.fx {
            if t < f.t0 && f.shape == OverlayPrim::RECT {
                continue;
            }
            let rect = node.as_deref().and_then(|id| self.scene.get(id)).map_or(f.rect, |n| n.rect);
            activity.push(ActivityFx { rect, ..f.clone() });
        }
        let highlight: Vec<[f32; 4]> = self.highlight.iter().filter_map(|id| self.scene.get(id)).map(|n| n.rect).collect();
        let outlines: Vec<[f32; 4]> = self
            .scene
            .iter()
            .filter(|n| n.handle.is_some() && n.kind != NodeKind::Generation && matches!(content_of(n), Content::None | Content::Card | Content::TextBox))
            // Filled frames and styled text show their own edges, like in a design tool.
            .filter(|n| n.frame_fill().is_none() && !n.styled_text())
            .map(|n| n.rect)
            .collect();
        let claims: Vec<[f32; 4]> = self.claims.iter().filter_map(|id| self.scene.get(id)).map(|n| n.rect).collect();
        let dragging = self.ix.dragging();
        let hover = self.ix.hover.as_deref();
        let badges: Vec<overlay::BadgeView> = self
            .scene
            .iter()
            .filter(|n| n.handle.is_some() && n.kind != NodeKind::Generation)
            .filter_map(|n| {
                let src = n.asset.as_ref().and_then(|a| self.sources.get(a)).copied();
                let final_role = n.role.as_deref() == Some("final");
                if src.is_none() && !final_role {
                    return None;
                }
                Some(overlay::BadgeView {
                    rect: n.rect,
                    source: src.map(|s| s.0),
                    uploaded: src.is_some_and(|s| s.1),
                    final_role,
                    force: !dragging && (hover == Some(n.id.as_str()) || self.ix.selection.iter().any(|s| s == &n.id)),
                })
            })
            .collect();
        let dim: Vec<[f32; 4]> = match &self.lineage {
            Some(keep) => self
                .scene
                .iter()
                .filter(|n| n.handle.is_some() && !matches!(n.kind, NodeKind::Frame | NodeKind::Group) && !keep.contains(&n.id))
                .map(|n| n.rect)
                .collect(),
            None => Vec::new(),
        };
        overlay::build(&OverlayInput {
            scene: &self.scene,
            palette: &self.palette,
            selection: &self.ix.selection,
            hover: if dragging || self.ix.tool != Tool::Select { None } else { self.ix.hover.as_deref() },
            show_handles: self.ix.tool == Tool::Select,
            marquee: self.ix.marquee(),
            pick: self.ix.pick_box().map(|b| b.1),
            outlines: &outlines,
            claims: &claims,
            cursors: &cursors,
            activity: &activity,
            highlight: &highlight,
            drop_target: self.ix.drop_target().or(self.drop_target.as_deref()).and_then(|id| self.scene.get(id)).map(|n| n.rect),
            badges: &badges,
            dim: &dim,
            dpr: self.dpr,
            px_world: 1.0 / self.camera.zoom.max(1e-3),
        })
    }

    fn with_ctx<R>(&mut self, f: impl FnOnce(&mut Interaction, &mut Ctx) -> R) -> R {
        let mut ctx = Ctx { scene: &self.scene, camera: &mut self.camera, dpr: self.dpr, now: now_ms() };
        f(&mut self.ix, &mut ctx)
    }

    fn handle_fx(&mut self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Preview(items) => {
                    let ids: HashSet<String> = items.iter().map(|(id, _)| id.clone()).collect();
                    for (id, r) in items {
                        let Some(old) = self.scene.get(&id).map(|n| n.rect) else { continue };
                        self.preview_rect(&id, r);
                        // Children follow a moved frame (doc positions are parent-relative).
                        let d = [r[0] - old[0], r[1] - old[1]];
                        if d != [0.0, 0.0] {
                            for k in self.scene.descendants(&id).into_iter().filter(|k| !ids.contains(k)) {
                                if let Some(kr) = self.scene.get(&k).map(|n| n.rect) {
                                    self.preview_rect(&k, [kr[0] + d[0], kr[1] + d[1], kr[2], kr[3]]);
                                }
                            }
                        }
                    }
                    self.dirty = true;
                }
                Effect::Commit(cmds) => {
                    let moved: Vec<String> = cmds
                        .iter()
                        .filter_map(|c| match c {
                            Command::UpdateNode { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect();
                    let cmds = self.to_parent_space(cmds);
                    if self.commit(&cmds).is_ok() {
                        self.refit_groups(&moved);
                    }
                }
                Effect::Select(ids) => {
                    self.emit("selection", json!(ids));
                    self.dirty = true;
                }
                Effect::Pick { node, region } => self.pick(node, region),
                Effect::Hover { node } => self.emit("hover", json!({"node": node})),
                Effect::Camera | Effect::Overlay => self.dirty = true,
                Effect::Cursor(c) => {
                    if c != self.cursor {
                        self.cursor = c;
                        let _ = self.canvas_el.style().set_property("cursor", c);
                    }
                }
                Effect::Tool(t) => {
                    self.emit("tool", json!(t));
                    self.dirty = true;
                }
                Effect::Undo => {
                    let _ = self.doc.undo(&self.actor.id);
                    self.sync_doc();
                }
                Effect::Redo => {
                    let _ = self.doc.redo(&self.actor.id);
                    self.sync_doc();
                }
                Effect::FitAll => self.fit_all(),
            }
        }
    }

    /// Groups hug their children: after children move or resize, each affected group's
    /// rect becomes the union of its children (children keep their world positions).
    fn refit_groups(&mut self, moved: &[String]) {
        let mut groups: Vec<String> = Vec::new();
        for id in moved {
            let mut cur = self.scene.get(id).and_then(|n| n.parent.clone());
            while let Some(p) = cur {
                let Some(pn) = self.scene.get(&p) else { break };
                if pn.kind != NodeKind::Group {
                    break;
                }
                if !groups.contains(&p) {
                    groups.push(p.clone());
                }
                cur = pn.parent.clone();
            }
        }
        // Innermost first, so outer groups see their inner groups' new bounds.
        groups.sort_by_key(|g| std::cmp::Reverse(self.depth_of(g)));
        for g in groups {
            let kids: Vec<String> = self.scene.iter().filter(|n| n.parent.as_deref() == Some(g.as_str())).map(|n| n.id.clone()).collect();
            let (Some(b), Some(cur)) = (self.scene.bounds_of(&kids), self.scene.get(&g).map(|n| n.rect)) else { continue };
            if (b[0] - cur[0]).abs() < 0.5 && (b[1] - cur[1]).abs() < 0.5 && (b[2] - cur[2]).abs() < 0.5 && (b[3] - cur[3]).abs() < 0.5 {
                continue;
            }
            let o = self.scene.parent_origin(&g);
            let (dx, dy) = (b[0] - cur[0], b[1] - cur[1]);
            let num = |v: f32| json!((v as f64 * 100.0).round() / 100.0);
            let mut cmds = vec![Command::UpdateNode {
                id: g.clone(),
                props: serde_json::from_value(json!({"x": num(b[0] - o[0]), "y": num(b[1] - o[1]), "w": num(b[2]), "h": num(b[3])})).unwrap_or_default(),
            }];
            for k in &kids {
                if let Some(kr) = self.scene.get(k).map(|n| n.rect) {
                    // Child positions are relative to the group's origin, which moved.
                    cmds.push(Command::UpdateNode {
                        id: k.clone(),
                        props: serde_json::from_value(json!({"x": num(kr[0] - cur[0] - dx), "y": num(kr[1] - cur[1] - dy)})).unwrap_or_default(),
                    });
                }
            }
            let _ = self.commit(&cmds);
        }
    }

    fn depth_of(&self, id: &str) -> usize {
        let mut d = 0;
        let mut cur = self.scene.get(id).and_then(|n| n.parent.clone());
        while let Some(p) = cur {
            d += 1;
            cur = self.scene.get(&p).and_then(|n| n.parent.clone());
        }
        d
    }

    fn preview_rect(&mut self, id: &str, r: [f32; 4]) {
        if let Some(n) = self.scene.get_mut(id) {
            n.rect = r;
            if let Some(h) = n.handle {
                self.gpu.renderer.set_rect(h, [r[0], r[1]], [r[2], r[3]]);
            }
        }
    }

    /// Interaction works in world space; the doc stores positions relative to the parent.
    /// A `MoveNode` earlier in the same batch (drag reparenting) changes the parent the
    /// following updates of that node are relative to.
    fn to_parent_space(&self, cmds: Vec<Command>) -> Vec<Command> {
        let mut moved: HashMap<String, Option<String>> = HashMap::new();
        cmds.into_iter()
            .map(|c| match c {
                Command::MoveNode { id, parent, index } => {
                    moved.insert(id.clone(), parent.clone());
                    Command::MoveNode { id, parent, index }
                }
                Command::UpdateNode { id, mut props } => {
                    let o = match moved.get(&id) {
                        Some(p) => p.as_deref().and_then(|p| self.scene.get(p)).map_or([0.0, 0.0], |p| [p.rect[0], p.rect[1]]),
                        None => self.scene.parent_origin(&id),
                    };
                    for (k, off) in [("x", o[0]), ("y", o[1])] {
                        if off != 0.0
                            && let Some(v) = props.get(k).and_then(|v| v.as_f64())
                        {
                            props.insert(k.into(), json!((v - off as f64).round()));
                        }
                    }
                    Command::UpdateNode { id, props }
                }
                c => c,
            })
            .collect()
    }

    /// A spot / region picked on a media node: becomes a chat context chip in the UI. On a
    /// video it carries the current playhead (a frame chip).
    fn pick(&mut self, node: String, region: PickRegion) {
        let time = self
            .scene
            .get(&node)
            .filter(|n| n.kind == NodeKind::Video)
            .and_then(|n| n.handle)
            .map(|h| (self.videos.current_time(h) * 100.0).round() / 100.0);
        self.emit("pick", json!({"nodeId": node, "region": region_json(&region), "time": time}));
        self.dirty = true;
    }

    /// Videos play muted only while hovered or selected; the others show a still.
    fn update_video_focus(&mut self) {
        let focus: Vec<Handle> = self
            .ix
            .hover
            .iter()
            .chain(self.ix.selection.iter())
            .filter_map(|id| self.scene.get(id))
            .filter(|n| n.kind == NodeKind::Video)
            .filter_map(|n| n.handle)
            .collect();
        self.gpu.renderer.set_video_focus(Some(&focus));
        // The selected video samples at high resolution once it outgrows the detail tile.
        let boost: Vec<Handle> = self
            .ix
            .selection
            .iter()
            .filter_map(|id| self.scene.get(id))
            .filter(|n| n.kind == NodeKind::Video)
            .filter_map(|n| n.handle)
            .collect();
        self.gpu.renderer.set_video_boost(&boost);
    }

    fn video_handle(&self, node_id: &str) -> Option<Handle> {
        self.scene.get(node_id).filter(|n| n.kind == NodeKind::Video).and_then(|n| n.handle)
    }

    fn set_palette(&mut self, palette: Palette) {
        if self.palette == palette {
            return;
        }
        self.palette = palette;
        self.gpu.renderer.set_background(palette.bg);
        // Cards are drawn in the theme's colors: redraw them; refresh every node's fill.
        let all: Vec<(Handle, SceneNode)> = self.scene.iter().filter_map(|n| Some((n.handle?, n.clone()))).collect();
        for (h, n) in all {
            let card = matches!(content_of(&n), Content::Card | Content::TextBox);
            let mut sink = EngineSink {
                r: &mut self.gpu.renderer,
                media: &self.media,
                videos: &self.videos,
                handles: &mut self.handles,
                todo: &mut self.todo,
                palette: &self.palette,
                gen_removed: &mut self.gen_removed,
                reveal: &mut self.reveal,
            };
            sink.update(h, &n, card);
        }
        self.dirty = true;
    }

    /// World point → CSS pixels relative to the canvas.
    fn css_point(&self, w: [f32; 2]) -> [f32; 2] {
        let s = self.camera.world_to_screen(w);
        [s[0] / self.dpr, s[1] / self.dpr]
    }

    fn fit_all(&mut self) {
        let ids: Vec<String> = self.scene.iter().filter(|n| n.handle.is_some()).map(|n| n.id.clone()).collect();
        match self.scene.bounds_of(&ids) {
            Some(b) => self.camera.fit_max(b, self.dpr),
            None => {
                self.camera.zoom = self.dpr;
                self.camera.offset = [-self.camera.viewport[0] * 0.5 / self.camera.zoom, -self.camera.viewport[1] * 0.5 / self.camera.zoom];
            }
        }
        self.dirty = true;
    }

    fn focus_rect(&mut self, r: [f32; 4]) {
        let view = [
            self.camera.offset[0],
            self.camera.offset[1],
            self.camera.viewport[0] / self.camera.zoom,
            self.camera.viewport[1] / self.camera.zoom,
        ];
        let inside = r[0] >= view[0] && r[1] >= view[1] && r[0] + r[2] <= view[0] + view[2] && r[1] + r[3] <= view[1] + view[3];
        if !inside {
            let max = self.camera.zoom.max(self.dpr);
            self.camera.fit_max([r[0] - r[2] * 0.15, r[1] - r[3] * 0.15, r[2] * 1.3, r[3] * 1.3], max);
        }
        self.dirty = true;
    }

    /// Fills the badge cache for assets seen on nodes (`all`: asset records changed, so
    /// recompute everything).
    fn refresh_sources(&mut self, all: bool) {
        if all {
            self.sources.clear();
        }
        let missing: Vec<String> = self.scene.iter().filter_map(|n| n.asset.clone()).filter(|a| !self.sources.contains_key(a)).collect();
        for a in missing {
            let rec = self.doc.asset(&a).and_then(|r| serde_json::to_value(r).ok()).unwrap_or(Json::Null);
            self.sources.insert(a, overlay::badge_of(&rec));
        }
    }

    fn rebuild_scene(&mut self) {
        self.sink_apply(|scene, doc, sink| scene.rebuild(doc, sink));
        self.refresh_sources(true);
        let _ = self.feed.take();
        self.ix.prune(&self.scene);
        self.dirty = true;
    }

    /// Spawns tile production for queued and requested tiles, within the in-flight cap.
    fn pump_media(&mut self) {
        for r in self.gpu.renderer.take_requests() {
            if !self.todo.contains(&r) {
                self.todo.push_back(r);
            }
        }
        while self.inflight < MAX_INFLIGHT {
            let Some((h, tier)) = self.todo.pop_front() else { break };
            let Some(id) = self.handles.get(&h) else { continue };
            let Some(n) = self.scene.get(id) else { continue };
            let Some((key, spec)) = source_spec(n, &self.media, self.palette.light) else {
                // Asset not available yet: `register_asset` re-queues it.
                continue;
            };
            if self.failed.contains(&key) {
                continue;
            }
            self.inflight += 1;
            let me = self.me.clone();
            wasm_bindgen_futures::spawn_local(fetch_tile(me, h, tier, key, spec));
        }
    }

    fn tick(&mut self, now: f64) {
        if !self.lost {
            let mut lost = LOST_DEVICES.lock().unwrap();
            if let Some(i) = lost.iter().position(|(id, _)| *id == self.gpu_id) {
                let (_, reason) = lost.remove(i);
                self.lost = true;
                log::warn!("WebGPU device lost: {reason}");
                self.emit("device_lost", serde_json::json!({ "reason": reason }));
            }
        }
        if self.lost {
            return;
        }
        let mut incoming = self.transport.poll();
        incoming.append(&mut self.js_inbox.borrow_mut());
        for msg in incoming {
            match msg {
                Incoming::Update(bytes) => {
                    if let Err(e) = self.import_remote(&bytes) {
                        log::warn!("import failed: {e}");
                    }
                    self.sync_doc();
                }
                Incoming::Presence(p) => {
                    self.presence = p;
                    self.dirty = true;
                }
                Incoming::Claims(v) => self.emit("claims", v),
                Incoming::Jobs(v) => self.emit("jobs", v),
            }
        }
        self.sync_doc();
        self.pump_media();
        let fx = self.ix.tick(now);
        self.handle_fx(fx);
        self.update_video_focus();
        // The animation clock stands still during heavy gestures (drags, camera moves), so
        // generating tiles don't add work to those frames.
        let dt = if self.last_tick > 0.0 { (now - self.last_tick).clamp(0.0, 100.0) } else { 0.0 };
        self.last_tick = now;
        if !self.ix.dragging() && self.camera == self.last_camera {
            self.anim_time += dt / 1000.0;
        }
        // Finished activity effects leave the overlay (and stop asking for frames).
        let t = self.anim_time as f32;
        let before = self.fx.len();
        self.fx.retain(|(_, f)| t < f.t0 + f.dur);
        if self.fx.len() != before {
            self.dirty = true;
        }
        let animate = self.gpu.renderer.animating() && now - self.last_anim_frame >= ANIM_MS;
        if self.dirty || self.gpu.renderer.needs_redraw() || animate {
            self.render(now);
        }
        if now - self.last_stats > 500.0 {
            self.last_stats = now;
            let n = self.frame_times.len().max(1) as f64;
            let avg = self.frame_times.iter().sum::<f64>() / n;
            let s = self.stats;
            self.emit(
                "stats",
                json!({
                    "fps": if self.frame_times.is_empty() { 0.0 } else { (1000.0 / avg.max(1.0)).round() },
                    "frames": self.frame_times.len(),
                    "nodes": s.nodes, "visible": s.visible, "detail": s.detail_resident,
                    "videos": s.active_videos, "hiresVideos": s.hires_videos, "zoom": self.camera.zoom / self.dpr,
                }),
            );
            self.frame_times.clear();
        }
    }

    fn render(&mut self, now: f64) {
        if self.lost {
            return;
        }
        self.dirty = false;
        let prims = self.build_overlay();
        self.gpu.renderer.set_overlay(&prims);
        let frame = match self.gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.gpu.surface.configure(self.gpu.renderer.device(), &self.gpu.config);
                self.dirty = true;
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        self.last_anim_frame = now;
        self.gpu.renderer.set_pixel_ratio(self.dpr);
        self.stats = self.gpu.renderer.render(&view, &self.camera, self.anim_time as f32);
        self.gpu.renderer.present(frame);
        if self.last_frame > 0.0 && now - self.last_frame < 250.0 {
            self.frame_times.push(now - self.last_frame);
        }
        self.last_frame = now;
        if self.camera != self.last_camera {
            self.last_camera = self.camera;
            self.emit("camera", self.camera_json());
        }
    }

    fn camera_json(&self) -> Json {
        let c = &self.camera;
        json!({"x": c.offset[0], "y": c.offset[1], "zoom": c.zoom / self.dpr,
               "width": c.viewport[0] / self.dpr, "height": c.viewport[1] / self.dpr})
    }

    /// `view`: visible size in device pixels (what the camera sees). `back`: the canvas backing
    /// store in device pixels, at least `view`; the platform over-allocates it (e.g. to the
    /// window size in 64 px steps) so live resizes of the visible area only move the GPU
    /// viewport and never reconfigure the swapchain. The canvas element's CSS size follows the
    /// backing store (it is clipped by its container), so pixels map 1:1.
    fn resize_px(&mut self, view: [u32; 2], back: [u32; 2], dpr: f32) {
        let max = self.gpu.renderer.device().limits().max_texture_dimension_2d;
        let back = [back[0].max(view[0]).clamp(1, max), back[1].max(view[1]).clamp(1, max)];
        let view = [view[0].clamp(1, back[0]), view[1].clamp(1, back[1])];
        if dpr != self.dpr && self.dpr > 0.0 {
            // Keep the same CSS zoom across DPR changes (moving between displays).
            self.camera.zoom *= dpr / self.dpr;
        }
        self.dpr = dpr;
        if self.gpu.config.width != back[0] || self.gpu.config.height != back[1] || self.canvas_el.width() != back[0] {
            self.canvas_el.set_width(back[0]);
            self.canvas_el.set_height(back[1]);
            let style = self.canvas_el.style();
            let _ = style.set_property("width", &format!("{}px", back[0] as f32 / dpr));
            let _ = style.set_property("height", &format!("{}px", back[1] as f32 / dpr));
            self.gpu.config.width = back[0];
            self.gpu.config.height = back[1];
            self.gpu.surface.configure(self.gpu.renderer.device(), &self.gpu.config);
        }
        let old = self.camera.viewport;
        let new = [view[0] as f32, view[1] as f32];
        if old != new {
            self.camera.viewport = new;
            if old[0] > 0.0 {
                // Keep the visual center stable (window resize, chat panel collapse).
                self.camera.offset[0] -= (new[0] - old[0]) * 0.5 / self.camera.zoom;
                self.camera.offset[1] -= (new[1] - old[1]) * 0.5 / self.camera.zoom;
            }
        }
        self.dirty = true;
    }

    fn register_asset(&mut self, id: &str, blob: web_sys::Blob, mime: &str, name: &str) {
        let known = self.media.get(id).is_some();
        self.media.insert(id, blob, mime, name);
        if known {
            return;
        }
        // Nodes that were waiting for this asset get their pixels now.
        self.refresh_asset_users(id);
    }

    fn register_remote(&mut self, id: &str, url: &str, mime: &str, name: &str, size: f64) {
        if self.media.get(id).is_some() {
            return;
        }
        self.media.insert_remote(id, url, mime, name, size);
        self.refresh_asset_users(id);
    }

    fn refresh_asset_users(&mut self, id: &str) {
        let waiting: Vec<(Handle, SceneNode)> =
            self.scene.iter().filter(|n| n.asset.as_deref() == Some(id)).filter_map(|n| Some((n.handle?, n.clone()))).collect();
        for (h, n) in waiting {
            let mut sink = EngineSink {
                r: &mut self.gpu.renderer,
                media: &self.media,
                videos: &self.videos,
                handles: &mut self.handles,
                todo: &mut self.todo,
                palette: &self.palette,
                gen_removed: &mut self.gen_removed,
                reveal: &mut self.reveal,
            };
            sink.update(h, &n, true);
        }
        self.failed.retain(|k| k != &format!("a:{id}"));
        self.dirty = true;
    }

    fn learn_dims(&mut self, h: Handle, w: u32, ht: u32, duration: f64) {
        let Some(n) = self.handles.get(&h).and_then(|id| self.scene.get(id)) else { return };
        let Some(asset) = n.asset.clone() else { return };
        if !matches!(content_of(n), Content::Image | Content::Video) {
            return;
        }
        let Some(entry) = self.media.assets.get_mut(&asset) else { return };
        if duration > 0.0 {
            entry.duration = duration;
        }
        if entry.dims != Some((w, ht)) && w > 0 && ht > 0 {
            entry.dims = Some((w, ht));
            let hs: Vec<(Handle, SceneNode)> =
                self.scene.iter().filter(|n| n.asset.as_deref() == Some(asset.as_str())).filter_map(|n| Some((n.handle?, n.clone()))).collect();
            for (h, n) in hs {
                let d = desc_for(&n, &self.media, &self.palette, self.reveal.contains(&n.id));
                self.gpu.renderer.update(h, d);
            }
        }
    }

    /// New nodes for imported files (one transaction; returns the new node ids): a tidy grid (about square, rows top-aligned) centered
    /// on `at` (world), inside `parent` (a frame) when given.
    fn create_imported(&mut self, items: Vec<Imported>, at: [f32; 2], parent: Option<String>) -> Vec<String> {
        let mut cmds = Vec::new();
        let sizes: Vec<[f32; 2]> = items.iter().map(|i| i.size()).collect();
        let parent = parent.filter(|p| self.scene.get(p).is_some_and(|n| matches!(n.kind, NodeKind::Frame | NodeKind::Group)));
        let frame = parent.as_deref().and_then(|p| self.scene.get(p)).map(|n| n.rect);
        let rects = grid_layout(&sizes, at, frame);
        let origin = parent.as_deref().and_then(|p| self.scene.get(p)).map_or([0.0, 0.0], |n| [n.rect[0], n.rect[1]]);
        for (item, r) in items.iter().zip(&rects) {
            cmds.push(Command::PutAsset(Asset { id: item.id.clone(), mime: Some(item.mime.clone()), ..Default::default() }));
            let mut props: Props = serde_json::from_value(json!({
                "x": (r[0] - origin[0]).round(), "y": (r[1] - origin[1]).round(), "w": r[2].round(), "h": r[3].round(),
            }))
            .unwrap_or_default();
            if item.kind != NodeKind::Text {
                props.insert("asset".into(), json!(item.id));
            }
            if item.kind == NodeKind::Video {
                props.insert("loop".into(), json!(true));
                props.insert("muted".into(), json!(true));
            }
            cmds.push(Command::CreateNode {
                id: None,
                kind: item.kind,
                parent: parent.clone(),
                index: None,
                name: item.name.clone(),
                props,
                text: item.text.clone(),
            });
        }
        match self.commit(&cmds) {
            Ok(r) => {
                let ids: Vec<String> = r.created.into_iter().filter(|id| self.scene.get(id).is_some()).collect();
                if let Some(b) = self.scene.bounds_of(&ids) {
                    self.focus_rect(b);
                }
                self.ix.selection = ids.clone();
                self.emit("selection", json!(ids));
                ids
            }
            Err(_) => Vec::new(),
        }
    }
}

/// Grid placement for `sizes` (world): `ceil(sqrt(n))` columns, rows top-aligned, the
/// whole block centered on `at`. Dropped into a `frame` (world rect), the block is scaled
/// down to fit inside it (with a margin) and kept within it. Mirrored by `gridLayout` in
/// apps/web/src/dnd.ts (drop previews), so ghosts and placed nodes line up.
pub fn grid_layout(sizes: &[[f32; 2]], at: [f32; 2], frame: Option<[f32; 4]>) -> Vec<[f32; 4]> {
    const GAP: f32 = 40.0;
    const PAD: f32 = 24.0;
    let n = sizes.len();
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f32).sqrt().ceil() as usize;
    let block = |k: f32| {
        let rows: Vec<&[[f32; 2]]> = sizes.chunks(cols).collect();
        let row_w: Vec<f32> = rows.iter().map(|r| r.iter().map(|s| s[0] * k).sum::<f32>() + GAP * k * (r.len() - 1) as f32).collect();
        let row_h: Vec<f32> = rows.iter().map(|r| r.iter().map(|s| s[1] * k).fold(0.0, f32::max)).collect();
        let total_w = row_w.iter().copied().fold(0.0, f32::max);
        let total_h = row_h.iter().sum::<f32>() + GAP * k * (rows.len() - 1) as f32;
        (rows, row_h, total_w, total_h)
    };
    let (_, _, w1, h1) = block(1.0);
    let k = frame.map_or(1.0, |f| (((f[2] - 2.0 * PAD) / w1).min((f[3] - 2.0 * PAD) / h1)).clamp(0.05, 1.0));
    let (rows, row_h, total_w, total_h) = block(k);
    let mut c = at;
    if let Some(f) = frame {
        let clamp = |v: f32, lo: f32, hi: f32| if lo > hi { (lo + hi) * 0.5 } else { v.clamp(lo, hi) };
        c[0] = clamp(c[0], f[0] + PAD + total_w * 0.5, f[0] + f[2] - PAD - total_w * 0.5);
        c[1] = clamp(c[1], f[1] + PAD + total_h * 0.5, f[1] + f[3] - PAD - total_h * 0.5);
    }
    let mut out = Vec::with_capacity(n);
    let mut y = c[1] - total_h * 0.5;
    for (r, row) in rows.iter().enumerate() {
        let mut x = c[0] - total_w * 0.5;
        for s in row.iter() {
            out.push([x, y, s[0] * k, s[1] * k]);
            x += (s[0] + GAP) * k;
        }
        y += row_h[r] + GAP * k;
    }
    out
}

struct Imported {
    id: String,
    kind: NodeKind,
    name: String,
    mime: String,
    dims: Option<(u32, u32)>,
    text: Option<String>,
}

impl Imported {
    /// Initial world size: media at natural aspect (longest side ≤ 640), cards by type.
    fn size(&self) -> [f32; 2] {
        if let Some((w, h)) = self.dims.filter(|d| d.0 > 0 && d.1 > 0) {
            let (w, h) = (w as f32, h as f32);
            let s = (640.0 / w.max(h)).min(1.0).max(160.0 / w.max(h));
            return [w * s, h * s];
        }
        match self.kind {
            NodeKind::Audio => [360.0, 150.0],
            NodeKind::Model3d => [280.0, 220.0],
            NodeKind::Document => [260.0, 200.0],
            NodeKind::Text => [320.0, 240.0],
            _ => [280.0, 180.0],
        }
    }
}

async fn fetch_tile(me: Weak<RefCell<App>>, h: Handle, tier: Tier, key: String, spec: JsValue) {
    let size = Renderer::tile_size(tier);
    let res = match media::make_tile(&key, &spec, size) {
        Ok(p) => JsFuture::from(p).await,
        Err(e) => Err(e),
    };
    let Some(app) = me.upgrade() else { return };
    let mut a = app.borrow_mut();
    a.inflight = a.inflight.saturating_sub(1);
    match res {
        Ok(v) => {
            let Ok(tile) = Reflect::get(&v, &"tile".into()).and_then(|t| t.dyn_into::<web_sys::ImageBitmap>()) else { return };
            let num = |k: &str| Reflect::get(&v, &k.into()).ok().and_then(|x| x.as_f64()).unwrap_or(0.0);
            // The node may have changed content while we decoded.
            let current = a.handles.get(&h).and_then(|id| a.scene.get(id)).and_then(|n| source_spec(n, &a.media, a.palette.light)).map(|s| s.0);
            if current.as_deref() == Some(key.as_str()) {
                a.gpu.renderer.provide(h, tier, Pixels::External(wgpu::ExternalImageSource::ImageBitmap(tile.clone())));
                a.learn_dims(h, num("width") as u32, num("height") as u32, num("duration"));
                a.dirty = true;
            } else {
                a.gpu.renderer.cancel_request(h, tier);
            }
            tile.close();
        }
        Err(e) => {
            log::warn!("tile for {key} failed: {e:?}");
            a.failed.insert(key);
        }
    }
}

fn dispatch(app: &Rc<RefCell<App>>) {
    let (events, listener) = {
        let Ok(mut a) = app.try_borrow_mut() else { return };
        (std::mem::take(&mut a.events), a.listener.clone())
    };
    let Some(f) = listener else { return };
    for (kind, payload) in events {
        let _ = f.call2(&JsValue::NULL, &JsValue::from_str(kind), &to_js(&payload));
    }
}

/// A transport implemented in JS: `{ send(kind, payload), connected?: boolean }`, where kind
/// is `"update"` (payload `Uint8Array`) or `"presence"` (payload object). Incoming traffic is
/// delivered with `CanvasApp.receive(kind, payload)`.
struct JsTransport {
    obj: JsValue,
}

impl SyncTransport for JsTransport {
    fn send(&mut self, msg: Outgoing) {
        let Ok(f) = Reflect::get(&self.obj, &"send".into()).and_then(|f| f.dyn_into::<js_sys::Function>()) else { return };
        let _ = match msg {
            Outgoing::Update(b) => f.call2(&self.obj, &"update".into(), &Uint8Array::from(b.as_slice())),
            Outgoing::Presence(p) => f.call2(&self.obj, &"presence".into(), &to_js(&serde_json::to_value(p).unwrap_or_default())),
        };
    }
    fn poll(&mut self) -> Vec<Incoming> {
        Vec::new()
    }
    fn connected(&self) -> bool {
        Reflect::get(&self.obj, &"connected".into()).ok().and_then(|v| v.as_bool()).unwrap_or(true)
    }
}

// ---- exported API -----------------------------------------------------------------------------

fn mods(shift: bool, alt: bool, meta: bool) -> Mods {
    Mods { shift, alt, meta }
}

#[wasm_bindgen]
impl CanvasApp {
    /// Initializes WebGPU on `canvas` and creates an empty local canvas.
    /// `options`: `{ actor?: {id, name}, canvas?: {id, name} }`.
    pub async fn create(canvas: web_sys::HtmlCanvasElement, options: JsValue) -> Result<CanvasApp, JsValue> {
        let opts = from_js(&options);
        let actor = Actor::human(
            opts["actor"]["id"].as_str().unwrap_or("you"),
            opts["actor"]["name"].as_str().unwrap_or("You"),
        );
        let doc = Canvas::new(opts["canvas"]["id"].as_str().unwrap_or("main"), opts["canvas"]["name"].as_str().unwrap_or("Untitled"));

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone())).map_err(err)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| err(format!("WebGPU adapter unavailable: {e}")))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("falos"),
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(err)?;
        let gpu_id = NEXT_GPU_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        device.set_device_lost_callback(move |reason, msg| {
            LOST_DEVICES.lock().unwrap().push((gpu_id, format!("{reason:?}: {msg}")));
        });
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let (w, h) = (canvas.width().max(1), canvas.height().max(1));
        let mut config = surface.get_default_config(&adapter, w, h).ok_or_else(|| err("surface config"))?;
        config.format = format;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let mut renderer = Renderer::new(device, queue, format, falos_engine::Budgets::default());
        renderer.set_background(overlay::DARK.bg);
        let videos = WebVideos::new(IngestMode::Import);
        renderer.set_video_source(Box::new(videos.clone()));

        let feed = ChangeFeed::new(&doc);
        let app = Rc::new_cyclic(|me| {
            RefCell::new(App {
                me: me.clone(),
                canvas_el: canvas,
                gpu: Gpu { surface, config, renderer },
                camera: Camera { zoom: 1.0, viewport: [w as f32, h as f32], ..Default::default() },
                dpr: 1.0,
                sent_vv: doc.vv(),
                doc,
                feed,
                actor,
                scene: Scene::default(),
                ix: Interaction::default(),
                media: MediaStore::default(),
                videos,
                handles: HashMap::new(),
                todo: VecDeque::new(),
                inflight: 0,
                failed: HashSet::new(),
                presence: Vec::new(),
                claims: Vec::new(),
                fx: Vec::new(),
                working: Vec::new(),
                reduce_motion: false,
                highlight: Vec::new(),
                drop_target: None,
                sources: HashMap::new(),
                lineage: None,
                palette: overlay::DARK,
                gen_removed: HashMap::new(),
                reveal: HashSet::new(),
                anim_time: 0.0,
                last_tick: 0.0,
                last_anim_frame: 0.0,
                transport: Box::new(LocalOnly),
                js_inbox: Rc::default(),
                events: Vec::new(),
                listener: None,
                dirty: true,
                cursor: "default",
                frame_times: Vec::new(),
                last_frame: 0.0,
                last_stats: 0.0,
                last_camera: Camera::default(),
                stats: Default::default(),
                raf: None,
                gpu_id,
                lost: false,
            })
        });
        app.borrow_mut().fit_all();
        Ok(CanvasApp { app })
    }

    /// Event listener `(type, payload) => void`. Types: selection, doc, hover, camera, pick,
    /// tool, asset, stats, error, claims, jobs.
    pub fn on(&self, listener: js_sys::Function) {
        self.app.borrow_mut().listener = Some(listener);
    }

    /// Starts the requestAnimationFrame loop. `tick()` can also be driven by a timer when the
    /// page is hidden (rAF pauses), e.g. so agents' edits keep streaming in.
    pub fn start(&self) {
        let slot: RafSlot = Rc::default();
        let weak = Rc::downgrade(&self.app);
        let slot2 = slot.clone();
        *slot.borrow_mut() = Some(Closure::new(move |t: f64| {
            let Some(app) = weak.upgrade() else { return };
            if app.borrow().raf.is_none() {
                return;
            }
            app.borrow_mut().tick(t);
            dispatch(&app);
            if let (Some(w), Some(cb)) = (web_sys::window(), slot2.borrow().as_ref()) {
                let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
            }
        }));
        self.app.borrow_mut().raf = Some(slot.clone());
        if let (Some(w), Some(cb)) = (web_sys::window(), slot.borrow().as_ref()) {
            let _ = w.request_animation_frame(cb.as_ref().unchecked_ref());
        }
    }

    pub fn stop(&self) {
        self.app.borrow_mut().raf = None;
    }

    /// Debug / recovery checks: destroy the GPU device as if the GPU went away. The app emits
    /// `device_lost` on its next tick.
    #[wasm_bindgen(js_name = loseDevice)]
    pub fn lose_device(&self) {
        self.app.borrow().gpu.renderer.device().destroy();
    }

    /// Whether the GPU device is gone (see `device_lost`).
    #[wasm_bindgen(js_name = deviceLost)]
    pub fn device_lost(&self) -> bool {
        self.app.borrow().lost
    }

    /// One frame of work (sync, media, render if needed). Called by the rAF loop.
    pub fn tick(&self) {
        self.app.borrow_mut().tick(now_ms());
        dispatch(&self.app);
    }

    /// CSS size of the canvas and the device pixel ratio.
    pub fn resize(&self, width: f32, height: f32, dpr: f32) {
        let px = |v: f32| (v * dpr).round().max(1.0) as u32;
        self.resize_px(px(width), px(height), px(width), px(height), dpr);
    }

    /// Resizes in device pixels (see `App::resize_px`) and draws a frame right away, in the
    /// same task: the browser never composites a cleared or stretched canvas. Call it from a
    /// ResizeObserver callback (`devicePixelContentBoxSize` gives exact device pixels).
    #[wasm_bindgen(js_name = resizePx)]
    pub fn resize_px(&self, view_w: u32, view_h: u32, back_w: u32, back_h: u32, dpr: f32) {
        {
            let mut a = self.app.borrow_mut();
            a.resize_px([view_w, view_h], [back_w, back_h], dpr);
            a.sync_doc();
            a.render(now_ms());
        }
        dispatch(&self.app);
    }

    // ---- input (CSS pixels relative to the canvas) ----

    #[wasm_bindgen(js_name = pointerDown)]
    pub fn pointer_down(&self, x: f32, y: f32, button: i16, shift: bool, alt: bool, meta: bool) {
        let mut a = self.app.borrow_mut();
        let p = [x * a.dpr, y * a.dpr];
        let fx = a.with_ctx(|ix, ctx| ix.pointer_down(ctx, p, button, mods(shift, alt, meta)));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    #[wasm_bindgen(js_name = pointerMove)]
    pub fn pointer_move(&self, x: f32, y: f32, shift: bool, alt: bool, meta: bool) {
        let mut a = self.app.borrow_mut();
        let p = [x * a.dpr, y * a.dpr];
        let fx = a.with_ctx(|ix, ctx| ix.pointer_move(ctx, p, mods(shift, alt, meta)));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    #[wasm_bindgen(js_name = pointerUp)]
    pub fn pointer_up(&self, x: f32, y: f32, shift: bool, alt: bool, meta: bool) {
        let mut a = self.app.borrow_mut();
        let p = [x * a.dpr, y * a.dpr];
        let fx = a.with_ctx(|ix, ctx| ix.pointer_up(ctx, p, mods(shift, alt, meta)));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    #[wasm_bindgen(js_name = pointerLeave)]
    pub fn pointer_leave(&self) {
        let mut a = self.app.borrow_mut();
        if a.ix.hover.is_some() {
            a.ix.hover = None;
            a.emit("hover", json!({"node": null}));
            a.dirty = true;
        }
        drop(a);
        dispatch(&self.app);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn wheel(&self, x: f32, y: f32, dx: f32, dy: f32, shift: bool, alt: bool, meta: bool) {
        let mut a = self.app.borrow_mut();
        let d = a.dpr;
        let fx = a.with_ctx(|ix, ctx| ix.wheel(ctx, [x * d, y * d], [dx * d, dy * d], mods(shift, alt, meta)));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    /// Relative zoom (Safari `gesturechange` scale ratio) around a CSS point.
    pub fn pinch(&self, x: f32, y: f32, factor: f32) {
        let mut a = self.app.borrow_mut();
        let d = a.dpr;
        let fx = a.with_ctx(|ix, ctx| ix.pinch(ctx, [x * d, y * d], factor));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    /// Returns true when the key was handled (the caller should `preventDefault`).
    #[wasm_bindgen(js_name = keyDown)]
    pub fn key_down(&self, key: &str, shift: bool, alt: bool, meta: bool) -> bool {
        let mut a = self.app.borrow_mut();
        let fx = a.with_ctx(|ix, ctx| ix.key_down(ctx, key, mods(shift, alt, meta)));
        let handled = fx.is_some();
        a.handle_fx(fx.unwrap_or_default());
        drop(a);
        dispatch(&self.app);
        handled
    }

    #[wasm_bindgen(js_name = keyUp)]
    pub fn key_up(&self, key: &str) {
        let mut a = self.app.borrow_mut();
        let fx = a.ix.key_up(key);
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
    }

    #[wasm_bindgen(js_name = setTool)]
    pub fn set_tool(&self, tool: &str) {
        let mut a = self.app.borrow_mut();
        if let Some(t) = Tool::parse(tool) {
            let fx = a.ix.set_tool(t);
            a.handle_fx(fx);
        }
        drop(a);
        dispatch(&self.app);
    }

    pub fn tool(&self) -> String {
        serde_json::to_value(self.app.borrow().ix.tool).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
    }

    // ---- camera ----

    /// `{x, y, zoom, width, height}`: world offset of the top-left corner, CSS zoom (1 = 100%).
    pub fn camera(&self) -> JsValue {
        to_js(&self.app.borrow().camera_json())
    }

    #[wasm_bindgen(js_name = setCamera)]
    pub fn set_camera(&self, x: f32, y: f32, zoom: f32) {
        let mut a = self.app.borrow_mut();
        a.camera.offset = [x, y];
        a.camera.zoom = (zoom * a.dpr).clamp(Camera::MIN_ZOOM, Camera::MAX_ZOOM);
        a.dirty = true;
    }

    /// Zooms to a CSS zoom level around the viewport center.
    #[wasm_bindgen(js_name = zoomTo)]
    pub fn zoom_to(&self, zoom: f32) {
        let mut a = self.app.borrow_mut();
        let c = [a.camera.viewport[0] * 0.5, a.camera.viewport[1] * 0.5];
        let f = zoom * a.dpr / a.camera.zoom;
        a.camera.zoom_at(c, f);
        a.dirty = true;
    }

    #[wasm_bindgen(js_name = fitAll)]
    pub fn fit_all(&self) {
        self.app.borrow_mut().fit_all();
    }

    /// Zooms to fit these nodes (with a margin), up to 200%; nothing if none exist.
    #[wasm_bindgen(js_name = zoomToNodes)]
    pub fn zoom_to_nodes(&self, ids: JsValue) {
        let ids: Vec<String> = serde_json::from_value(from_js(&ids)).unwrap_or_default();
        let mut a = self.app.borrow_mut();
        if let Some(r) = a.scene.bounds_of(&ids) {
            let (mx, my) = (r[2] * 0.1 + 20.0, r[3] * 0.1 + 20.0);
            let max = 2.0 * a.dpr;
            a.camera.fit_max([r[0] - mx, r[1] - my, r[2] + 2.0 * mx, r[3] + 2.0 * my], max);
            a.dirty = true;
        }
    }

    /// Pans/zooms so the node is in view.
    #[wasm_bindgen(js_name = focusNode)]
    pub fn focus_node(&self, id: &str) {
        let mut a = self.app.borrow_mut();
        if let Some(r) = a.scene.get(id).map(|n| n.rect) {
            a.focus_rect(r);
        }
    }

    /// World → CSS pixels relative to the canvas: `[x, y]`.
    #[wasm_bindgen(js_name = worldToScreen)]
    pub fn world_to_screen(&self, x: f32, y: f32) -> Vec<f32> {
        self.app.borrow().css_point([x, y]).to_vec()
    }

    /// Double-click at a CSS point: enters the group around the node there (Figma). Returns
    /// `{entered, node}`: the group entered (if any) and the node under the pointer.
    #[wasm_bindgen(js_name = doubleClick)]
    pub fn double_click(&self, x: f32, y: f32) -> JsValue {
        let mut a = self.app.borrow_mut();
        let p = [x * a.dpr, y * a.dpr];
        let (entered, node, fx) = a.with_ctx(|ix, ctx| ix.double_click(ctx, p));
        a.handle_fx(fx);
        drop(a);
        dispatch(&self.app);
        to_js(&json!({"entered": entered, "node": node}))
    }

    /// Topmost drawn node under a CSS point, if any.
    #[wasm_bindgen(js_name = hitTest)]
    pub fn hit_test(&self, x: f32, y: f32) -> Option<String> {
        let a = self.app.borrow();
        let w = a.camera.screen_to_world([x * a.dpr, y * a.dpr]);
        a.scene.hit_test(w).map(|n| n.id.clone())
    }

    #[wasm_bindgen(js_name = screenToWorld)]
    pub fn screen_to_world(&self, x: f32, y: f32) -> Vec<f32> {
        let a = self.app.borrow();
        a.camera.screen_to_world([x * a.dpr, y * a.dpr]).to_vec()
    }

    // ---- media ----

    /// Imports files dropped at CSS point (x, y), laid out as a grid centered there.
    /// `opts`: `{ parent?: string }` puts them inside that frame (drop into a frame). Emits
    /// `import {done, total}` after each file (drop placeholders show progress). Resolves to
    /// the new node ids.
    #[wasm_bindgen(js_name = importFiles)]
    pub fn import_files(&self, files: js_sys::Array, x: f32, y: f32, opts: JsValue) -> js_sys::Promise {
        let app = self.app.clone();
        let files: Vec<web_sys::File> = files.iter().filter_map(|f| f.dyn_into().ok()).collect();
        let parent = Reflect::get(&opts, &"parent".into()).ok().and_then(|p| p.as_string());
        wasm_bindgen_futures::future_to_promise(async move {
            let at = {
                let a = app.borrow();
                a.camera.screen_to_world([x * a.dpr, y * a.dpr])
            };
            let total = files.len();
            let mut items = Vec::new();
            for (i, f) in files.into_iter().enumerate() {
                let name = f.name();
                let mime = f.type_();
                let blob: web_sys::Blob = f.into();
                match import_one(&app, blob, &name, &mime).await {
                    Ok(item) => items.push(item),
                    Err(e) => {
                        app.borrow_mut().emit("error", json!({"message": format!("import {name}: {}", e.as_string().unwrap_or_default())}));
                    }
                }
                app.borrow_mut().emit("import", json!({"done": i + 1, "total": total}));
                dispatch(&app);
            }
            let ids = app.borrow_mut().create_imported(items, at, parent);
            dispatch(&app);
            Ok(to_js(&json!(ids)))
        })
    }

    /// The frame a drop at CSS point (x, y) would land in (deepest frame under it).
    #[wasm_bindgen(js_name = frameAt)]
    pub fn frame_at(&self, x: f32, y: f32) -> Option<String> {
        let a = self.app.borrow();
        let w = a.camera.screen_to_world([x * a.dpr, y * a.dpr]);
        crate::interact::drop_frame(&a.scene, w, &[])
    }

    /// Highlights a frame as the target of a drag from outside the canvas (`null` clears).
    #[wasm_bindgen(js_name = setDropTarget)]
    pub fn set_drop_target(&self, id: Option<String>) {
        let mut a = self.app.borrow_mut();
        if a.drop_target != id {
            a.drop_target = id;
            a.dirty = true;
        }
    }

    /// Imports a media URL (fetched into a blob) at CSS point (x, y).
    #[wasm_bindgen(js_name = importUrl)]
    pub fn import_url(&self, url: String, x: f32, y: f32) -> js_sys::Promise {
        let app = self.app.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let blob: web_sys::Blob = JsFuture::from(media::fetch_blob(&url)?).await?.dyn_into()?;
            let name = url.rsplit('/').next().unwrap_or("media").split('?').next().unwrap_or("media").to_string();
            let mime = blob.type_();
            let item = import_one(&app, blob, &name, &mime).await?;
            let at = {
                let a = app.borrow();
                a.camera.screen_to_world([x * a.dpr, y * a.dpr])
            };
            let ids = app.borrow_mut().create_imported(vec![item], at, None);
            dispatch(&app);
            Ok(to_js(&json!(ids)))
        })
    }

    /// Makes an asset blob available (e.g. restored from IndexedDB).
    #[wasm_bindgen(js_name = registerAsset)]
    pub fn register_asset(&self, id: &str, blob: web_sys::Blob, mime: &str, name: &str) {
        self.app.borrow_mut().register_asset(id, blob, mime, name);
    }

    #[wasm_bindgen(js_name = assetBlob)]
    pub fn asset_blob(&self, id: &str) -> Option<web_sys::Blob> {
        self.app.borrow().media.get(id).and_then(|a| a.blob.clone())
    }

    /// Object URL of a local asset (for previews in the UI).
    #[wasm_bindgen(js_name = assetUrl)]
    pub fn asset_url(&self, id: &str) -> Option<String> {
        self.app.borrow().media.get(id).map(|a| a.url.clone())
    }

    /// Local assets: `[{id, mime, name, size}]`.
    pub fn assets(&self) -> JsValue {
        let a = self.app.borrow();
        let list: Vec<Json> = a.media.assets.iter().map(|(id, e)| json!({"id": id, "mime": e.mime, "name": e.name, "size": e.size, "remote": e.blob.is_none()})).collect();
        to_js(&Json::Array(list))
    }

    /// Current playhead (seconds) of a video node.
    #[wasm_bindgen(js_name = videoTime)]
    pub fn video_time(&self, node_id: &str) -> f64 {
        let a = self.app.borrow();
        a.scene.get(node_id).and_then(|n| n.handle).map_or(0.0, |h| a.videos.current_time(h))
    }

    /// Playback state of a video node for the transport UI:
    /// `{time, duration, ready, muted, paused, playing, url, width, height}` (null if not a video).
    #[wasm_bindgen(js_name = videoState)]
    pub fn video_state(&self, node_id: &str) -> JsValue {
        let a = self.app.borrow();
        let Some(n) = a.scene.get(node_id).filter(|n| n.kind == NodeKind::Video) else { return JsValue::NULL };
        let asset = n.asset.as_deref().and_then(|id| a.media.get(id));
        let (mut time, mut duration, mut st) = (0.0, 0.0, (false, true, false, false));
        if let Some(h) = n.handle {
            time = a.videos.current_time(h);
            duration = a.videos.duration(h);
            st = a.videos.state(h);
        }
        if !(duration.is_finite() && duration > 0.0) {
            duration = asset.map_or(0.0, |e| e.duration);
        }
        let (w, h) = asset.and_then(|e| e.dims).unwrap_or((0, 0));
        to_js(&json!({
            "time": time, "duration": duration, "ready": st.0, "muted": st.1, "paused": st.2, "playing": st.3,
            "url": asset.map(|e| e.url.clone()), "width": w, "height": h,
        }))
    }

    /// Plays (releases a user pause) or pauses a video node on its current frame. Playback still
    /// follows the canvas rules (hovered / selected, decoder cap).
    #[wasm_bindgen(js_name = videoPlay)]
    pub fn video_play(&self, node_id: &str, play: bool) {
        let mut a = self.app.borrow_mut();
        let Some(h) = a.video_handle(node_id) else { return };
        if play {
            a.videos.release(h);
        } else {
            let t = a.videos.current_time(h);
            a.videos.hold(h, t);
        }
        a.dirty = true;
    }

    #[wasm_bindgen(js_name = videoSeek)]
    pub fn video_seek(&self, node_id: &str, t: f64) {
        let mut a = self.app.borrow_mut();
        let Some(h) = a.video_handle(node_id) else { return };
        let d = a.videos.duration(h);
        a.videos.seek(h, if d > 0.0 { t.clamp(0.0, d - 0.01) } else { t.max(0.0) });
        a.dirty = true;
    }

    #[wasm_bindgen(js_name = videoMute)]
    pub fn video_mute(&self, node_id: &str, muted: bool) {
        let a = self.app.borrow();
        if let Some(h) = a.video_handle(node_id) {
            a.videos.set_muted(h, muted);
        }
    }

    // ---- doc ----

    pub fn selection(&self) -> JsValue {
        to_js(&json!(self.app.borrow().ix.selection))
    }

    #[wasm_bindgen(js_name = setSelection)]
    pub fn set_selection(&self, ids: JsValue) {
        let mut a = self.app.borrow_mut();
        let ids: Vec<String> = serde_json::from_value(from_js(&ids)).unwrap_or_default();
        let ids: Vec<String> = ids.into_iter().filter(|id| a.scene.get(id).is_some()).collect();
        a.ix.selection = ids.clone();
        a.emit("selection", json!(ids));
        a.dirty = true;
        drop(a);
        dispatch(&self.app);
    }

    /// All nodes depth-first in z-order (bottom first).
    pub fn nodes(&self) -> JsValue {
        to_js(&api::nodes_json(&self.app.borrow().doc))
    }

    /// One node with its asset (lineage).
    pub fn node(&self, id: &str) -> JsValue {
        to_js(&api::node_detail_json(&self.app.borrow().doc, id))
    }

    /// Applies commands (`{op, …}` or an array) as one transaction by the local actor.
    pub fn apply(&self, commands: JsValue) -> Result<JsValue, JsValue> {
        let cmds = api::parse_commands(&from_js(&commands).to_string()).map_err(err)?;
        let r = self.app.borrow_mut().commit(&cmds);
        dispatch(&self.app);
        let r = r.map_err(err)?;
        Ok(to_js(&json!({"version": r.version.to_string(), "created": r.created, "warnings": r.warnings})))
    }

    /// Like `apply`, but not an undo step of its own: completes an earlier transaction (e.g.
    /// fills in a pasted image once it has downloaded), so undoing that one removes it all.
    #[wasm_bindgen(js_name = applyUntracked)]
    pub fn apply_untracked(&self, commands: JsValue) -> Result<JsValue, JsValue> {
        let cmds = api::parse_commands(&from_js(&commands).to_string()).map_err(err)?;
        let r = self.app.borrow_mut().commit_as(&cmds, false);
        dispatch(&self.app);
        let r = r.map_err(err)?;
        Ok(to_js(&json!({"version": r.version.to_string(), "created": r.created, "warnings": r.warnings})))
    }

    pub fn undo(&self) -> bool {
        let mut a = self.app.borrow_mut();
        let id = a.actor.id.clone();
        let ok = a.doc.undo(&id).unwrap_or(false);
        a.sync_doc();
        drop(a);
        dispatch(&self.app);
        ok
    }

    pub fn redo(&self) -> bool {
        let mut a = self.app.borrow_mut();
        let id = a.actor.id.clone();
        let ok = a.doc.redo(&id).unwrap_or(false);
        a.sync_doc();
        drop(a);
        dispatch(&self.app);
        ok
    }

    #[wasm_bindgen(js_name = canvasName)]
    pub fn canvas_name(&self) -> String {
        self.app.borrow().doc.name()
    }

    pub fn actor(&self) -> JsValue {
        let a = self.app.borrow();
        to_js(&json!({"id": a.actor.id, "name": a.actor.name}))
    }

    /// `"dark"` (default) or `"light"`: canvas background, outlines and card colors.
    #[wasm_bindgen(js_name = setTheme)]
    pub fn set_theme(&self, theme: &str) {
        let p = if theme == "light" { overlay::LIGHT } else { overlay::DARK };
        self.app.borrow_mut().set_palette(p);
    }

    // ---- text projection ----

    /// The canvas in the FORMAT.md text projection. `opts`: `{root, depth, filter, annotations, attribution}`.
    #[wasm_bindgen(js_name = readText)]
    pub fn read_text(&self, opts: JsValue) -> Result<String, JsValue> {
        let o = from_js(&opts);
        let d = ReadOpts::default();
        let ro = ReadOpts {
            root: o["root"].as_str().map(String::from),
            depth: o["depth"].as_u64().map(|v| v as usize),
            filter: o["filter"].as_str().map(String::from),
            annotations: o["annotations"].as_bool().unwrap_or(d.annotations),
            attribution: o["attribution"].as_bool().unwrap_or(d.attribution),
        };
        self.app.borrow().doc.read(&ro).map_err(err)
    }

    /// Applies an edit in the text format as one transaction.
    #[wasm_bindgen(js_name = applyEdit)]
    pub fn apply_edit(&self, text: &str) -> Result<JsValue, JsValue> {
        let mut a = self.app.borrow_mut();
        let actor = a.actor.clone();
        let r = a.doc.apply_edit(text, &actor, None);
        a.sync_doc();
        drop(a);
        dispatch(&self.app);
        let r = r.map_err(err)?;
        Ok(to_js(&json!({"version": r.version.to_string(), "created": r.created, "warnings": r.warnings})))
    }

    // ---- persistence & sync seam ----

    #[wasm_bindgen(js_name = exportSnapshot)]
    pub fn export_snapshot(&self) -> Result<Uint8Array, JsValue> {
        Ok(Uint8Array::from(self.app.borrow().doc.export_snapshot().map_err(err)?.as_slice()))
    }

    /// Replaces the canvas with a snapshot (local restore or a host bootstrap).
    /// `fit` (default true) zooms to fit the loaded content.
    #[wasm_bindgen(js_name = loadSnapshot)]
    pub fn load_snapshot(&self, bytes: &[u8], fit: Option<bool>) -> Result<(), JsValue> {
        let doc = Canvas::from_snapshot(bytes).map_err(err)?;
        let mut a = self.app.borrow_mut();
        a.feed = ChangeFeed::new(&doc);
        a.doc = doc;
        a.sent_vv = a.doc.vv();
        a.rebuild_scene();
        if fit.unwrap_or(true) {
            a.fit_all();
        }
        let v = a.doc.version().to_string();
        a.emit("doc", json!({"version": v, "nodes": true, "assets": true}));
        let sel = a.ix.selection.clone();
        a.emit("selection", json!(sel));
        drop(a);
        dispatch(&self.app);
        Ok(())
    }

    /// Loro updates since a version vector (`vv()` of the other side; empty = everything).
    #[wasm_bindgen(js_name = exportUpdates)]
    pub fn export_updates(&self, since: &[u8]) -> Result<Uint8Array, JsValue> {
        let a = self.app.borrow();
        let vv = if since.is_empty() { VersionVector::default() } else { VersionVector::decode(since).map_err(err)? };
        Ok(Uint8Array::from(a.doc.export_updates(&vv).map_err(err)?.as_slice()))
    }

    /// Imports Loro updates or a snapshot from elsewhere (merged, CRDT).
    #[wasm_bindgen(js_name = importUpdates)]
    pub fn import_updates(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut a = self.app.borrow_mut();
        a.import_remote(bytes).map_err(err)?;
        a.sync_doc();
        // A resync snapshot may add nothing, but edits made while offline must still go out.
        a.flush_transport();
        drop(a);
        dispatch(&self.app);
        Ok(())
    }

    /// After a reconnect: merge the host's snapshot, then send everything the host lacks.
    /// What was "sent" before can be gone: a host that crashed (or was restarted from disk)
    /// before saving it, or bytes that went into a socket that was already dead. The host's
    /// own version vector (from its snapshot) is the truth, not what we think we sent.
    #[wasm_bindgen(js_name = resyncSnapshot)]
    pub fn resync_snapshot(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let host_vv = Canvas::from_snapshot(bytes).map_err(err)?.vv();
        let mut a = self.app.borrow_mut();
        a.import_remote(bytes).map_err(err)?;
        a.sent_vv = host_vv;
        a.sync_doc();
        a.flush_transport();
        drop(a);
        dispatch(&self.app);
        Ok(())
    }

    /// Encoded version vector of the local replica.
    pub fn vv(&self) -> Uint8Array {
        Uint8Array::from(self.app.borrow().doc.vv().encode().as_slice())
    }

    pub fn version(&self) -> String {
        self.app.borrow().doc.version().to_string()
    }

    /// Attaches a JS transport (see `JsTransport`); `null` returns to local-only.
    #[wasm_bindgen(js_name = setTransport)]
    pub fn set_transport(&self, transport: JsValue) {
        let mut a = self.app.borrow_mut();
        a.transport = if transport.is_null() || transport.is_undefined() { Box::new(LocalOnly) } else { Box::new(JsTransport { obj: transport }) };
    }

    /// Delivers host traffic: `("update", Uint8Array)`, `("presence", [...])`, `("claims", …)`, `("jobs", …)`.
    pub fn receive(&self, kind: &str, payload: JsValue) {
        let msg = match kind {
            "update" => Some(Incoming::Update(Uint8Array::new(&payload).to_vec())),
            "presence" => serde_json::from_value(from_js(&payload)).ok().map(Incoming::Presence),
            "claims" => Some(Incoming::Claims(from_js(&payload))),
            "jobs" => Some(Incoming::Jobs(from_js(&payload))),
            _ => None,
        };
        if let Some(m) = msg {
            let inbox = self.app.borrow().js_inbox.clone();
            inbox.borrow_mut().push(m);
        }
    }

    /// Claim outlines: `[{node}]` (replaces the previous list).
    #[wasm_bindgen(js_name = setClaims)]
    pub fn set_claims(&self, claims: JsValue) {
        let v = from_js(&claims);
        let mut a = self.app.borrow_mut();
        a.claims = v
            .as_array()
            .map(|l| l.iter().filter_map(|c| Some(c["node"].as_str()?.to_string())).collect())
            .unwrap_or_default();
        a.dirty = true;
    }

    /// Makes an asset available by URL (served by the host) instead of a blob: images are
    /// fetched when a tier is needed, videos stream through `<video src>` with byte ranges.
    #[wasm_bindgen(js_name = registerRemoteAsset)]
    pub fn register_remote_asset(&self, id: &str, url: &str, mime: &str, name: &str, size: f64) {
        self.app.borrow_mut().register_remote(id, url, mime, name, size);
    }

    /// Replaces the presence list directly (local simulation, or a transport bridge).
    #[wasm_bindgen(js_name = setPresence)]
    pub fn set_presence(&self, presence: JsValue) {
        let mut a = self.app.borrow_mut();
        a.presence = serde_json::from_value(from_js(&presence)).unwrap_or_default();
        a.dirty = true;
    }

    /// One agent-activity event (UX-V1 §4): `{op: "read"|"write"|"edit"|"delete", nodes: [id],
    /// color: "#rrggbb", rects?: {id: [x, y, w, h]}}` (world rects for nodes that are already
    /// gone, e.g. deletes). Drawn in the chat color; animated on the GPU unless motion is
    /// reduced, in which case it is a short static outline.
    pub fn activity(&self, event: JsValue) {
        let v = from_js(&event);
        let mut a = self.app.borrow_mut();
        let Some(color) = v["color"].as_str().and_then(overlay::parse_color) else { return };
        let t = a.anim_time as f32;
        let reduce = a.reduce_motion;
        let op = v["op"].as_str().unwrap_or("");
        let ids: Vec<String> = v["nodes"].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let mut add = Vec::new();
        for (i, id) in ids.iter().take(24).enumerate() {
            let given = v["rects"][id.as_str()].as_array().and_then(|r| {
                let f: Vec<f32> = r.iter().filter_map(|x| x.as_f64().map(|x| x as f32)).collect();
                (f.len() == 4).then(|| [f[0], f[1], f[2], f[3]])
            });
            let node = a.scene.get(id);
            let Some(rect) = node.map(|n| n.rect).or(given) else { continue };
            let seed = i as f32;
            let fx = |shape, t0, dur| ActivityFx { rect, shape, color, t0, dur, seed };
            let follow = node.is_some().then(|| id.clone());
            if reduce {
                add.push((follow, fx(OverlayPrim::RECT, t, 1.2)));
                continue;
            }
            match op {
                "read" => add.push((follow, fx(OverlayPrim::STEP_DASHED, t, 0.9))),
                "write" => {
                    add.push((follow.clone(), fx(OverlayPrim::STEP, t, 1.0)));
                    // Then the container's children outline one after another (capped).
                    let kids: Vec<(String, [f32; 4])> =
                        a.scene.iter().filter(|c| c.parent.as_deref() == Some(id.as_str())).take(6).map(|c| (c.id.clone(), c.rect)).collect();
                    for (j, (kid, r)) in kids.into_iter().enumerate() {
                        add.push((Some(kid), ActivityFx { rect: r, shape: OverlayPrim::STEP, color, t0: t + 0.3 + j as f32 * 0.15, dur: 1.0, seed }));
                    }
                }
                "edit" => add.push((follow, fx(OverlayPrim::STEP, t, 0.7))),
                "delete" => add.push((None, fx(OverlayPrim::STEP_DASHED, t, 0.8))),
                _ => {}
            }
        }
        // Bounded: the oldest effects give way.
        a.fx.extend(add);
        let n = a.fx.len();
        if n > 96 {
            a.fx.drain(..n - 96);
        }
        a.dirty = true;
    }

    /// Frames/groups (or nodes) agents are working on right now: `[{node, color}]`.
    #[wasm_bindgen(js_name = setWorking)]
    pub fn set_working(&self, list: JsValue) {
        let v = from_js(&list);
        let next: Vec<(String, u32)> = v
            .as_array()
            .map(|l| l.iter().filter_map(|w| Some((w["node"].as_str()?.to_string(), overlay::parse_color(w["color"].as_str()?)?))).collect())
            .unwrap_or_default();
        let mut a = self.app.borrow_mut();
        if a.working != next {
            a.working = next;
            a.dirty = true;
        }
    }

    /// "Reduce motion": activity draws as static outlines instead of stepped ones.
    #[wasm_bindgen(js_name = setReduceMotion)]
    pub fn set_reduce_motion(&self, on: bool) {
        let mut a = self.app.borrow_mut();
        a.reduce_motion = on;
        a.dirty = true;
    }

    /// Outlines nodes from outside the canvas (layers row hover); `[]` clears.
    #[wasm_bindgen(js_name = setHighlight)]
    pub fn set_highlight(&self, ids: JsValue) {
        let ids: Vec<String> = serde_json::from_value(from_js(&ids)).unwrap_or_default();
        let mut a = self.app.borrow_mut();
        if a.highlight != ids {
            a.highlight = ids;
            a.dirty = true;
        }
    }

    /// Lineage view (docs/ASSETS-V1.md §5): nodes outside `ids` are dimmed; `null` turns it off.
    #[wasm_bindgen(js_name = setLineage)]
    pub fn set_lineage(&self, ids: JsValue) {
        let keep: Option<HashSet<String>> = serde_json::from_value(from_js(&ids)).ok().flatten();
        let mut a = self.app.borrow_mut();
        if a.lineage != keep {
            a.lineage = keep;
            a.dirty = true;
        }
    }

    /// Sends the local user's presence through the transport.
    #[wasm_bindgen(js_name = sendPresence)]
    pub fn send_presence(&self, presence: JsValue) {
        if let Ok(p) = serde_json::from_value::<Presence>(from_js(&presence)) {
            self.app.borrow_mut().transport.send(Outgoing::Presence(p));
        }
    }
}

async fn import_one(app: &Rc<RefCell<App>>, blob: web_sys::Blob, name: &str, mime: &str) -> Result<Imported, JsValue> {
    let id = JsFuture::from(media::sha256_hex(&blob)?).await?.as_string().unwrap_or_default();
    let kind = classify(mime, name);
    let mime = if mime.is_empty() { "application/octet-stream".to_string() } else { mime.to_string() };
    let url = {
        let mut a = app.borrow_mut();
        let url = a.media.insert(&id, blob.clone(), &mime, name).url.clone();
        a.emit("asset", json!({"id": id, "mime": mime, "name": name}));
        url
    };
    let mut dims = None;
    let mut duration = 0.0;
    let mut text = None;
    match kind {
        NodeKind::Image => {
            let v = JsFuture::from(media::probe_image(&blob)?).await?;
            dims = dims_of(&v);
        }
        NodeKind::Video => {
            let v = JsFuture::from(media::probe_video(&url)?).await?;
            dims = dims_of(&v);
            duration = Reflect::get(&v, &"duration".into())?.as_f64().unwrap_or(0.0);
            if let Ok(b) = Reflect::get(&v, &"bitmap".into()).and_then(|b| b.dyn_into::<web_sys::ImageBitmap>()) {
                b.close();
            }
        }
        NodeKind::Text => {
            let t = JsFuture::from(media::blob_text(&blob)?).await?.as_string().unwrap_or_default();
            text = Some(t.chars().take(20_000).collect());
        }
        _ => {}
    }
    if let Some(e) = app.borrow_mut().media.assets.get_mut(&id) {
        e.dims = dims.or(e.dims);
        e.duration = duration;
    }
    let name = if kind == NodeKind::Text { name.to_string() } else { name.rsplit_once('.').map_or(name, |(b, _)| b).to_string() };
    Ok(Imported { id, kind, name, mime, dims, text })
}

fn dims_of(v: &JsValue) -> Option<(u32, u32)> {
    let w = Reflect::get(v, &"width".into()).ok()?.as_f64()? as u32;
    let h = Reflect::get(v, &"height".into()).ok()?.as_f64()? as u32;
    Some((w, h))
}
