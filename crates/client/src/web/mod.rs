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
use falos_doc::{Actor, Anchor, Annotation, Asset, Canvas, Command, NodeKind, Props, ReadOpts, Region, TxResult};
use falos_engine::web_video::{IngestMode, WebVideos};
use falos_engine::{Camera, Content as EngineContent, Handle, Kind, NodeDesc, OverlayPrim, Pixels, Renderer, Tier};
use js_sys::{Reflect, Uint8Array};
use serde_json::{Value as Json, json};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::api;
use crate::interact::{Ctx, DraftRegion, Effect, Interaction, Mods, Pin, Tool};
use crate::overlay::{self, CursorView, OverlayInput, PinView, rgba};
use crate::scene::{ChangeFeed, RenderSink, Scene, SceneNode};
use crate::sync::{Incoming, LocalOnly, Outgoing, Presence, SyncTransport};
use media::{Content, MediaStore, classify, content_of, source_spec};

const MAX_INFLIGHT: usize = 6;
const BACKGROUND: [f64; 4] = [0.055, 0.055, 0.063, 1.0];
const PLACEHOLDER: u32 = rgba(36, 36, 42, 255);
const FRAME_FILL: u32 = rgba(24, 24, 28, 255);

type RafSlot = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

struct Gpu {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

struct Draft {
    node: String,
    region: DraftRegion,
    /// Video playhead when the draft was placed (frame annotations).
    time: Option<f64>,
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
    annotations: Vec<Annotation>,
    selected_annotation: Option<String>,
    held_video: Option<Handle>,
    draft: Option<Draft>,
    presence: Vec<Presence>,
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
    started: f64,
    raf: Option<RafSlot>,
}

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
}

fn desc_for(n: &SceneNode, media: &MediaStore) -> NodeDesc {
    let mut d = NodeDesc {
        pos: [n.rect[0], n.rect[1]],
        size: [n.rect[2], n.rect[3]],
        color: PLACEHOLDER,
        radius: 3.0,
        ..Default::default()
    };
    match content_of(n) {
        Content::Image => {
            d.kind = Kind::Image;
            d.content = EngineContent::External;
            d.aspect = n.asset.as_deref().and_then(|a| media.aspect(a)).unwrap_or(0.0);
        }
        Content::Video => {
            d.kind = Kind::Video;
            d.content = EngineContent::External;
            d.aspect = n.asset.as_deref().and_then(|a| media.aspect(a)).unwrap_or(0.0);
        }
        Content::Card => {
            d.kind = Kind::Image;
            d.content = EngineContent::External;
            d.radius = 12.0;
        }
        Content::None => {
            d.kind = Kind::Flat;
            d.content = EngineContent::None;
            d.color = FRAME_FILL;
            d.radius = 6.0;
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

impl RenderSink for EngineSink<'_> {
    fn add(&mut self, n: &SceneNode) -> Handle {
        let h = self.r.add(desc_for(n, self.media));
        self.handles.insert(h, n.id.clone());
        self.attach_video(h, n);
        if !matches!(content_of(n), Content::None) {
            self.todo.push_front((h, Tier::Low));
        }
        h
    }

    fn update(&mut self, h: Handle, n: &SceneNode, mut content: bool) {
        let d = desc_for(n, self.media);
        if let (Content::Card, Some(old)) = (content_of(n), self.r.desc(h)) {
            let a0 = old.size[0] / old.size[1].max(1e-3);
            let a1 = d.size[0] / d.size[1].max(1e-3);
            content |= (a0 - a1).abs() / a1.max(1e-3) > 0.02;
        }
        self.r.update(h, d);
        if content {
            self.r.invalidate(h);
            self.attach_video(h, n);
            self.todo.push_front((h, Tier::Low));
        }
    }

    fn remove(&mut self, h: Handle, _n: &SceneNode) {
        self.r.remove(h);
        self.handles.remove(&h);
        self.videos.remove(h);
        self.todo.retain(|t| t.0 != h);
    }

    fn set_order(&mut self, order: &[Handle]) {
        self.r.set_order(order);
    }
}

/// Normalized pin point and box of an anchor (node-local 0..1).
fn anchor_geometry(a: &Anchor) -> ([f32; 2], Option<[f32; 4]>) {
    fn region(r: &Region) -> ([f32; 2], Option<[f32; 4]>) {
        match r {
            Region::Point { x, y } => ([*x as f32, *y as f32], None),
            Region::Box { x, y, w, h } => ([(*x + *w) as f32, *y as f32], Some([*x as f32, *y as f32, *w as f32, *h as f32])),
            Region::Polygon { points } => (points.first().map_or([1.0, 0.0], |p| [p[0] as f32, p[1] as f32]), None),
            Region::Mask { .. } => ([1.0, 0.0], None),
        }
    }
    match a {
        Anchor::Point { x, y } => region(&Region::Point { x: *x, y: *y }),
        Anchor::Box { x, y, w, h } => region(&Region::Box { x: *x, y: *y, w: *w, h: *h }),
        Anchor::Polygon { points } => region(&Region::Polygon { points: points.clone() }),
        Anchor::Frame { region: Some(r), .. } | Anchor::Page { region: Some(r), .. } => region(r),
        _ => ([1.0, 0.0], None),
    }
}

fn draft_anchor(region: &DraftRegion, time: Option<f64>) -> Anchor {
    let r = match *region {
        DraftRegion::Point([x, y]) => Region::Point { x: round4(x), y: round4(y) },
        DraftRegion::Box([x, y, w, h]) => Region::Box { x: round4(x), y: round4(y), w: round4(w), h: round4(h) },
    };
    match (time, r) {
        (Some(t), r) => Anchor::Frame { t: (t * 1000.0).round() / 1000.0, region: Some(r) },
        (None, Region::Point { x, y }) => Anchor::Point { x, y },
        (None, Region::Box { x, y, w, h }) => Anchor::Box { x, y, w, h },
        (None, _) => Anchor::Point { x: 0.5, y: 0.5 },
    }
}

fn round4(v: f32) -> f64 {
    (v as f64 * 10_000.0).round() / 10_000.0
}

fn region_json(r: &DraftRegion) -> Json {
    match *r {
        DraftRegion::Point([x, y]) => json!({"kind": "point", "x": round4(x), "y": round4(y)}),
        DraftRegion::Box([x, y, w, h]) => json!({"kind": "box", "x": round4(x), "y": round4(y), "w": round4(w), "h": round4(h)}),
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
        if ch.annotations || !ch.deleted.is_empty() || !self.annotations.is_empty() && !ch.dirty.is_empty() {
            self.annotations = self.doc.annotations();
            if let Some(id) = &self.selected_annotation
                && !self.annotations.iter().any(|a| &a.id == id)
            {
                self.select_annotation(None);
            }
        }
        if self.ix.prune(&self.scene) {
            let sel = self.ix.selection.clone();
            self.emit("selection", json!(sel));
        }
        if let Some(d) = &self.draft
            && self.scene.get(&d.node).is_none()
        {
            self.cancel_draft();
        }
        let nodes = ch.structure || !ch.dirty.is_empty() || !ch.deleted.is_empty();
        self.emit("doc", json!({"version": self.doc.version().to_string(), "nodes": nodes, "annotations": ch.annotations, "assets": ch.assets}));
        self.flush_transport();
        self.dirty = true;
    }

    fn flush_transport(&mut self) {
        if self.transport.connected()
            && let Ok(bytes) = self.doc.export_updates(&self.sent_vv)
            && !bytes.is_empty()
        {
            self.transport.send(Outgoing::Update(bytes));
        }
        self.sent_vv = self.doc.vv();
    }

    fn commit(&mut self, cmds: &[Command]) -> Result<TxResult, String> {
        if cmds.is_empty() {
            return Err("no commands".into());
        }
        let r = self.doc.apply(&self.actor, None, cmds).map_err(|e| e.to_string());
        if let Err(e) = &r {
            self.emit("error", json!({"message": e}));
        }
        self.sync_doc();
        r
    }

    fn pins(&self) -> Vec<(Pin, PinView)> {
        self.annotations
            .iter()
            .filter_map(|a| {
                let n = self.scene.get(&a.node_id)?;
                n.handle?;
                let (pt, region) = anchor_geometry(&a.anchor);
                let world = n.to_world(pt);
                let region = region.map(|r| {
                    let p = n.to_world([r[0], r[1]]);
                    [p[0], p[1], r[2] * n.rect[2], r[3] * n.rect[3]]
                });
                Some((
                    Pin { id: a.id.clone(), world },
                    PinView {
                        world,
                        region,
                        status: a.status.as_str().to_string(),
                        selected: self.selected_annotation.as_deref() == Some(a.id.as_str()),
                        hovered: self.ix.hover_pin.as_deref() == Some(a.id.as_str()),
                    },
                ))
            })
            .collect()
    }

    fn build_overlay(&self) -> Vec<OverlayPrim> {
        let pins: Vec<PinView> = self.pins().into_iter().map(|p| p.1).collect();
        let draft = self.draft.as_ref().and_then(|d| {
            let n = self.scene.get(&d.node)?;
            Some(match d.region {
                DraftRegion::Point(p) => (n.to_world(p), None),
                DraftRegion::Box([x, y, w, h]) => {
                    let p = n.to_world([x, y]);
                    (n.to_world([x + w, y]), Some([p[0], p[1], w * n.rect[2], h * n.rect[3]]))
                }
            })
        });
        let cursors: Vec<CursorView> = self
            .presence
            .iter()
            .filter(|p| p.actor != self.actor.id)
            .filter_map(|p| Some(CursorView { world: p.cursor?, color: overlay::parse_color(&p.color, 255).unwrap_or(overlay::ACCENT) }))
            .collect();
        let dragging = self.ix.dragging();
        overlay::build(&OverlayInput {
            scene: &self.scene,
            selection: &self.ix.selection,
            hover: if dragging || self.ix.tool != Tool::Select { None } else { self.ix.hover.as_deref() },
            show_handles: self.ix.tool == Tool::Select,
            marquee: self.ix.marquee(),
            drawing: self.ix.annotate_box().map(|b| b.1),
            pins: &pins,
            draft,
            cursors: &cursors,
            dpr: self.dpr,
        })
    }

    fn with_ctx<R>(&mut self, f: impl FnOnce(&mut Interaction, &mut Ctx) -> R) -> R {
        let pins: Vec<Pin> = self.pins().into_iter().map(|p| p.0).collect();
        let mut ctx = Ctx { scene: &self.scene, camera: &mut self.camera, pins: &pins, dpr: self.dpr };
        f(&mut self.ix, &mut ctx)
    }

    fn handle_fx(&mut self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Preview(items) => {
                    for (id, r) in items {
                        if let Some(n) = self.scene.get_mut(&id) {
                            n.rect = r;
                            if let Some(h) = n.handle {
                                self.gpu.renderer.set_rect(h, [r[0], r[1]], [r[2], r[3]]);
                            }
                        }
                    }
                    self.dirty = true;
                }
                Effect::Commit(cmds) => {
                    let _ = self.commit(&cmds);
                }
                Effect::Select(ids) => {
                    self.emit("selection", json!(ids));
                    self.dirty = true;
                }
                Effect::Draft { node, region } => self.start_draft(node, region),
                Effect::CancelDraft => self.cancel_draft(),
                Effect::OpenAnnotation(id) => self.select_annotation(Some(id)),
                Effect::CloseAnnotation => self.select_annotation(None),
                Effect::Hover { node, annotation } => self.emit("hover", json!({"node": node, "annotation": annotation})),
                Effect::Camera | Effect::Overlay => self.dirty = true,
                Effect::Cursor(c) => {
                    if c != self.cursor {
                        self.cursor = c;
                        let _ = self.canvas_el.style().set_property("cursor", c);
                    }
                }
                Effect::Tool(t) => {
                    if t != Tool::Annotate {
                        self.cancel_draft();
                    }
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

    fn start_draft(&mut self, node: String, region: DraftRegion) {
        let mut time = None;
        if let Some(n) = self.scene.get(&node)
            && n.kind == NodeKind::Video
            && let Some(h) = n.handle
        {
            let t = self.videos.current_time(h);
            self.hold_video(Some((h, t)));
            time = Some(t);
        }
        self.select_annotation_inner(None, false);
        let (screen, rect) = self.draft_screen(&node, &region).unzip();
        self.emit("draft", json!({"nodeId": node, "region": region_json(&region), "time": time, "screen": screen, "rect": rect}));
        self.draft = Some(Draft { node, region, time });
        self.dirty = true;
    }

    /// Pin point and region rect of a draft, in CSS pixels (for placing the composer).
    fn draft_screen(&self, node: &str, region: &DraftRegion) -> Option<([f32; 2], [f32; 4])> {
        let n = self.scene.get(node)?;
        let (pin, a, b) = match *region {
            DraftRegion::Point(p) => (n.to_world(p), n.to_world(p), n.to_world(p)),
            DraftRegion::Box([x, y, w, h]) => (n.to_world([x + w, y]), n.to_world([x, y]), n.to_world([x + w, y + h])),
        };
        let (a, b) = (self.css_point(a), self.css_point(b));
        Some((self.css_point(pin), [a[0], a[1], b[0] - a[0], b[1] - a[1]]))
    }

    fn cancel_draft(&mut self) {
        if self.draft.take().is_some() {
            if self.selected_annotation.is_none() {
                self.hold_video(None);
            }
            self.emit("draft", Json::Null);
            self.dirty = true;
        }
    }

    fn hold_video(&mut self, hold: Option<(Handle, f64)>) {
        if let Some(h) = self.held_video.take()
            && hold.is_none_or(|(n, _)| n != h)
        {
            self.videos.release(h);
        }
        if let Some((h, t)) = hold {
            self.videos.hold(h, t);
            self.held_video = Some(h);
        }
        self.dirty = true;
    }

    fn select_annotation(&mut self, id: Option<String>) {
        self.select_annotation_inner(id, true);
    }

    fn select_annotation_inner(&mut self, id: Option<String>, cancel_draft: bool) {
        if cancel_draft && id.is_some() {
            self.cancel_draft();
        }
        let ann = id.as_ref().and_then(|id| self.annotations.iter().find(|a| &a.id == id)).cloned();
        let hold = ann.as_ref().and_then(|a| {
            let n = self.scene.get(&a.node_id)?;
            match (&a.anchor, n.kind, n.handle) {
                (Anchor::Frame { t, .. } | Anchor::Time { t }, NodeKind::Video, Some(h)) => Some((h, *t)),
                _ => None,
            }
        });
        if hold.is_some() || self.draft.is_none() {
            self.hold_video(hold);
        }
        if self.selected_annotation != id {
            self.selected_annotation = id.clone();
            self.emit("annotation-select", json!({"id": id}));
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

    fn rebuild_scene(&mut self) {
        self.sink_apply(|scene, doc, sink| scene.rebuild(doc, sink));
        let _ = self.feed.take();
        self.annotations = self.doc.annotations();
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
            let Some((key, spec)) = source_spec(n, &self.media) else {
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
        let mut incoming = self.transport.poll();
        incoming.append(&mut self.js_inbox.borrow_mut());
        for msg in incoming {
            match msg {
                Incoming::Update(bytes) => {
                    if let Err(e) = self.doc.import(&bytes) {
                        log::warn!("import failed: {e}");
                    }
                    self.sync_doc();
                    self.sent_vv = self.doc.vv();
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
        if self.dirty || self.gpu.renderer.needs_redraw() {
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
                    "videos": s.active_videos, "zoom": self.camera.zoom / self.dpr,
                }),
            );
            self.frame_times.clear();
        }
    }

    fn render(&mut self, now: f64) {
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
        self.stats = self.gpu.renderer.render(&view, &self.camera, ((now - self.started) / 1000.0) as f32);
        self.gpu.renderer.present(frame);
        if self.last_frame > 0.0 && now - self.last_frame < 250.0 {
            self.frame_times.push(now - self.last_frame);
        }
        self.last_frame = now;
        if self.camera != self.last_camera {
            self.last_camera = self.camera;
            self.emit("camera", self.camera_json());
            if let Some((screen, rect)) = self.draft.as_ref().and_then(|d| self.draft_screen(&d.node, &d.region)) {
                self.emit("draft-move", json!({"screen": screen, "rect": rect}));
            }
        }
    }

    fn camera_json(&self) -> Json {
        let c = &self.camera;
        json!({"x": c.offset[0], "y": c.offset[1], "zoom": c.zoom / self.dpr,
               "width": c.viewport[0] / self.dpr, "height": c.viewport[1] / self.dpr})
    }

    fn resize(&mut self, w: f32, h: f32, dpr: f32) {
        let max = self.gpu.renderer.device().limits().max_texture_dimension_2d;
        let pw = ((w * dpr).round() as u32).clamp(1, max);
        let ph = ((h * dpr).round() as u32).clamp(1, max);
        if dpr != self.dpr && self.dpr > 0.0 {
            // Keep the same CSS zoom across DPR changes (moving between displays).
            self.camera.zoom *= dpr / self.dpr;
        }
        self.dpr = dpr;
        self.canvas_el.set_width(pw);
        self.canvas_el.set_height(ph);
        self.gpu.config.width = pw;
        self.gpu.config.height = ph;
        self.gpu.surface.configure(self.gpu.renderer.device(), &self.gpu.config);
        let old = self.camera.viewport;
        self.camera.viewport = [pw as f32, ph as f32];
        if old[0] > 0.0 {
            // Keep the view centered while the window resizes.
            self.camera.offset[0] -= (pw as f32 - old[0]) * 0.5 / self.camera.zoom;
            self.camera.offset[1] -= (ph as f32 - old[1]) * 0.5 / self.camera.zoom;
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
        let waiting: Vec<(Handle, SceneNode)> =
            self.scene.iter().filter(|n| n.asset.as_deref() == Some(id)).filter_map(|n| Some((n.handle?, n.clone()))).collect();
        for (h, n) in waiting {
            let mut sink = EngineSink {
                r: &mut self.gpu.renderer,
                media: &self.media,
                videos: &self.videos,
                handles: &mut self.handles,
                todo: &mut self.todo,
            };
            sink.update(h, &n, true);
        }
        self.dirty = true;
    }

    fn learn_dims(&mut self, h: Handle, w: u32, ht: u32, duration: f64) {
        let Some(n) = self.handles.get(&h).and_then(|id| self.scene.get(id)) else { return };
        let Some(asset) = n.asset.clone() else { return };
        if !matches!(content_of(n), Content::Image | Content::Video) {
            return;
        }
        let Some(entry) = self.media.assets.get_mut(&asset) else { return };
        if entry.dims != Some((w, ht)) && w > 0 && ht > 0 {
            entry.dims = Some((w, ht));
            entry.duration = duration;
            let hs: Vec<(Handle, SceneNode)> =
                self.scene.iter().filter(|n| n.asset.as_deref() == Some(asset.as_str())).filter_map(|n| Some((n.handle?, n.clone()))).collect();
            for (h, n) in hs {
                let d = desc_for(&n, &self.media);
                self.gpu.renderer.update(h, d);
            }
        }
    }

    /// Creates nodes for imported files as one transaction; returns the new node ids.
    fn create_imported(&mut self, items: Vec<Imported>, at: [f32; 2]) -> Vec<String> {
        let mut cmds = Vec::new();
        let gap = 40.0;
        let sizes: Vec<[f32; 2]> = items.iter().map(|i| i.size()).collect();
        let total_w: f32 = sizes.iter().map(|s| s[0]).sum::<f32>() + gap * (sizes.len().saturating_sub(1)) as f32;
        let mut x = at[0] - total_w * 0.5;
        for (item, size) in items.iter().zip(&sizes) {
            cmds.push(Command::PutAsset(Asset { id: item.id.clone(), mime: Some(item.mime.clone()), uri: None, lineage: None }));
            let mut props: Props = serde_json::from_value(json!({
                "x": x.round(), "y": (at[1] - size[1] * 0.5).round(), "w": size[0].round(), "h": size[1].round(),
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
                parent: None,
                index: None,
                name: item.name.clone(),
                props,
                text: item.text.clone(),
            });
            x += size[0] + gap;
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
            let current = a.handles.get(&h).and_then(|id| a.scene.get(id)).and_then(|n| source_spec(n, &a.media)).map(|s| s.0);
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
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let (w, h) = (canvas.width().max(1), canvas.height().max(1));
        let mut config = surface.get_default_config(&adapter, w, h).ok_or_else(|| err("surface config"))?;
        config.format = format;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let mut renderer = Renderer::new(device, queue, format, falos_engine::Budgets::default());
        renderer.set_background(BACKGROUND);
        let videos = WebVideos::new(IngestMode::Import);
        renderer.set_video_source(Box::new(videos.clone()));

        let feed = ChangeFeed::new(&doc);
        let now = now_ms();
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
                annotations: Vec::new(),
                selected_annotation: None,
                held_video: None,
                draft: None,
                presence: Vec::new(),
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
                started: now,
                raf: None,
            })
        });
        app.borrow_mut().fit_all();
        Ok(CanvasApp { app })
    }

    /// Event listener `(type, payload) => void`. Types: selection, doc, hover, camera, draft,
    /// draft-move, annotation-select, tool, asset, stats, error, claims, jobs.
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

    /// One frame of work (sync, media, render if needed). Called by the rAF loop.
    pub fn tick(&self) {
        self.app.borrow_mut().tick(now_ms());
        dispatch(&self.app);
    }

    /// CSS size of the canvas and the device pixel ratio.
    pub fn resize(&self, width: f32, height: f32, dpr: f32) {
        self.app.borrow_mut().resize(width, height, dpr);
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
        if a.ix.hover.is_some() || a.ix.hover_pin.is_some() {
            a.ix.hover = None;
            a.ix.hover_pin = None;
            a.emit("hover", json!({"node": null, "annotation": null}));
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

    #[wasm_bindgen(js_name = screenToWorld)]
    pub fn screen_to_world(&self, x: f32, y: f32) -> Vec<f32> {
        let a = self.app.borrow();
        a.camera.screen_to_world([x * a.dpr, y * a.dpr]).to_vec()
    }

    // ---- media ----

    /// Imports files dropped at CSS point (x, y). Resolves to the new node ids.
    #[wasm_bindgen(js_name = importFiles)]
    pub fn import_files(&self, files: js_sys::Array, x: f32, y: f32) -> js_sys::Promise {
        let app = self.app.clone();
        let files: Vec<web_sys::File> = files.iter().filter_map(|f| f.dyn_into().ok()).collect();
        wasm_bindgen_futures::future_to_promise(async move {
            let at = {
                let a = app.borrow();
                a.camera.screen_to_world([x * a.dpr, y * a.dpr])
            };
            let mut items = Vec::new();
            for f in files {
                let name = f.name();
                let mime = f.type_();
                let blob: web_sys::Blob = f.into();
                match import_one(&app, blob, &name, &mime).await {
                    Ok(item) => items.push(item),
                    Err(e) => {
                        app.borrow_mut().emit("error", json!({"message": format!("import {name}: {}", e.as_string().unwrap_or_default())}));
                    }
                }
            }
            let ids = app.borrow_mut().create_imported(items, at);
            dispatch(&app);
            Ok(to_js(&json!(ids)))
        })
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
            let ids = app.borrow_mut().create_imported(vec![item], at);
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
        self.app.borrow().media.get(id).map(|a| a.blob.clone())
    }

    /// Object URL of a local asset (for previews in the UI).
    #[wasm_bindgen(js_name = assetUrl)]
    pub fn asset_url(&self, id: &str) -> Option<String> {
        self.app.borrow().media.get(id).map(|a| a.url.clone())
    }

    /// Local assets: `[{id, mime, name, size}]`.
    pub fn assets(&self) -> JsValue {
        let a = self.app.borrow();
        let list: Vec<Json> = a.media.assets.iter().map(|(id, e)| json!({"id": id, "mime": e.mime, "name": e.name, "size": e.blob.size()})).collect();
        to_js(&Json::Array(list))
    }

    /// Current playhead (seconds) of a video node.
    #[wasm_bindgen(js_name = videoTime)]
    pub fn video_time(&self, node_id: &str) -> f64 {
        let a = self.app.borrow();
        a.scene.get(node_id).and_then(|n| n.handle).map_or(0.0, |h| a.videos.current_time(h))
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

    /// One node with its asset (lineage) and annotations.
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

    // ---- annotations ----

    pub fn annotations(&self) -> JsValue {
        to_js(&Json::Array(self.app.borrow().annotations.iter().map(api::annotation_json).collect()))
    }

    /// Posts the pending draft (placed with the annotate tool) with its first message.
    #[wasm_bindgen(js_name = submitDraft)]
    pub fn submit_draft(&self, text: String, assignee: Option<String>) -> Result<String, JsValue> {
        let mut a = self.app.borrow_mut();
        let Some(d) = a.draft.take() else { return Err(err("no draft")) };
        let anchor = draft_anchor(&d.region, d.time);
        let r = a.commit(&[Command::AddAnnotation {
            id: None,
            node_id: d.node,
            anchor,
            text: Some(text).filter(|t| !t.trim().is_empty()),
            assignee: assignee.filter(|s| !s.is_empty()),
        }]);
        a.emit("draft", Json::Null);
        let id = r.map_err(err)?.created.into_iter().next().unwrap_or_default();
        a.select_annotation(Some(id.clone()));
        drop(a);
        dispatch(&self.app);
        Ok(id)
    }

    #[wasm_bindgen(js_name = cancelDraft)]
    pub fn cancel_draft(&self) {
        self.app.borrow_mut().cancel_draft();
        dispatch(&self.app);
    }

    /// Adds an annotation with an explicit anchor (`{kind: "point", x, y}` etc., normalized).
    #[wasm_bindgen(js_name = addAnnotation)]
    pub fn add_annotation(&self, node_id: String, anchor: JsValue, text: String, assignee: Option<String>) -> Result<String, JsValue> {
        let anchor: Anchor = serde_json::from_value(from_js(&anchor)).map_err(err)?;
        let r = self.app.borrow_mut().commit(&[Command::AddAnnotation {
            id: None,
            node_id,
            anchor,
            text: Some(text),
            assignee: assignee.filter(|s| !s.is_empty()),
        }]);
        dispatch(&self.app);
        Ok(r.map_err(err)?.created.into_iter().next().unwrap_or_default())
    }

    pub fn reply(&self, id: String, text: String) -> Result<(), JsValue> {
        let r = self.app.borrow_mut().commit(&[Command::Reply { id, text }]);
        dispatch(&self.app);
        r.map(|_| ()).map_err(err)
    }

    /// Sets status (`open` | `in_progress` | `resolved`).
    #[wasm_bindgen(js_name = setAnnotationStatus)]
    pub fn set_annotation_status(&self, id: String, status: &str) -> Result<(), JsValue> {
        let status = falos_doc::Status::parse(status).ok_or_else(|| err("bad status"))?;
        let r = self.app.borrow_mut().commit(&[Command::UpdateAnnotation { id, status: Some(status), anchor: None, assignee: None, outcome: None }]);
        dispatch(&self.app);
        r.map(|_| ()).map_err(err)
    }

    /// Assigns to an actor id (`claude`, `codex`, `fal-agent`, …); empty/None unassigns.
    #[wasm_bindgen(js_name = setAnnotationAssignee)]
    pub fn set_annotation_assignee(&self, id: String, assignee: Option<String>) -> Result<(), JsValue> {
        let r = self.app.borrow_mut().commit(&[Command::UpdateAnnotation {
            id,
            status: None,
            anchor: None,
            assignee: Some(assignee.filter(|s| !s.is_empty())),
            outcome: None,
        }]);
        dispatch(&self.app);
        r.map(|_| ()).map_err(err)
    }

    #[wasm_bindgen(js_name = deleteAnnotation)]
    pub fn delete_annotation(&self, id: String) -> Result<(), JsValue> {
        let r = self.app.borrow_mut().commit(&[Command::DeleteAnnotation { id }]);
        dispatch(&self.app);
        r.map(|_| ()).map_err(err)
    }

    /// Highlights an annotation (holds a video on its frame); `focus` pans it into view.
    #[wasm_bindgen(js_name = selectAnnotation)]
    pub fn select_annotation(&self, id: Option<String>, focus: bool) {
        let mut a = self.app.borrow_mut();
        a.select_annotation(id.clone());
        if focus
            && let Some(ann) = id.and_then(|id| a.annotations.iter().find(|x| x.id == id).cloned())
            && let Some(n) = a.scene.get(&ann.node_id)
        {
            let r = n.rect;
            a.focus_rect(r);
        }
        drop(a);
        dispatch(&self.app);
    }

    #[wasm_bindgen(js_name = selectedAnnotation)]
    pub fn selected_annotation(&self) -> Option<String> {
        self.app.borrow().selected_annotation.clone()
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
    #[wasm_bindgen(js_name = loadSnapshot)]
    pub fn load_snapshot(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let doc = Canvas::from_snapshot(bytes).map_err(err)?;
        let mut a = self.app.borrow_mut();
        a.cancel_draft();
        a.select_annotation(None);
        a.feed = ChangeFeed::new(&doc);
        a.sink_apply(|scene, _, sink| scene.clear(sink));
        a.doc = doc;
        a.sent_vv = a.doc.vv();
        a.rebuild_scene();
        a.fit_all();
        let v = a.doc.version().to_string();
        a.emit("doc", json!({"version": v, "nodes": true, "annotations": true, "assets": true}));
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
        a.doc.import(bytes).map_err(err)?;
        a.sync_doc();
        a.sent_vv = a.doc.vv();
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

    /// Replaces the presence list directly (local simulation, or a transport bridge).
    #[wasm_bindgen(js_name = setPresence)]
    pub fn set_presence(&self, presence: JsValue) {
        let mut a = self.app.borrow_mut();
        a.presence = serde_json::from_value(from_js(&presence)).unwrap_or_default();
        a.dirty = true;
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
