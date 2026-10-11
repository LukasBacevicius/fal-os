//! Figma scene → fal-os import plan.
//!
//! 1. **Resolve**: the clipboard's `nodeChanges` form a flat list linked by `parentIndex`
//!    (sibling order = the `position` string). Instances carry no children of their own: they
//!    are expanded from their component (`SYMBOL`, shipped in the hidden "Internal Only Canvas")
//!    with the instance's `symbolOverrides`, `derivedSymbolData` (post-layout sizes, transforms
//!    and geometry) and `componentPropAssignments` (visibility, text, instance swaps) applied,
//!    keyed by guid paths of `overrideKey`s. Nested instances recurse with the path prefix
//!    stripped; the outermost instance wins.
//! 2. **Map** the resolved tree to fal-os nodes (positions parent-relative, like the doc):
//!    frames/sections/components → `frame` (solid background → `fill`, corner radius →
//!    `radius`; nested frames without any paint → `group`), groups → `group`, text → `text`,
//!    image-filled rectangles and image backgrounds → `image` (by SHA-1 of the Figma image),
//!    and everything else that draws (vectors, shapes, gradients, strokes, boolean ops, and
//!    whole containers that hold only vector art) → `raster`: a list of draw ops the client
//!    renders into a PNG.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde::Serialize;

use crate::clip::Clip;
use crate::geom::{self, Mat};
use crate::kiwi::{Obj, Value};

pub type Guid = (u64, u64);

fn guid_of(v: &Value) -> Option<Guid> {
    Some((v.f("sessionID")? as u64, v.f("localID")? as u64))
}

// ---- plan types ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub file_key: Option<String>,
    /// Size of the pasted selection; root nodes are placed relative to its top-left.
    pub size: [f64; 2],
    /// Parents come before their children; siblings bottom to top.
    pub nodes: Vec<PlanNode>,
    /// Figma image SHA-1 → signed download URL, for every image the plan references.
    pub images: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub figma_nodes: usize,
    pub resolved: usize,
    pub frames: usize,
    pub groups: usize,
    pub texts: usize,
    pub images: usize,
    pub rasters: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Frame,
    Group,
    Text,
    Image,
    Raster,
    Note,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanNode {
    pub parent: Option<usize>,
    pub kind: Kind,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Frame background (`#rrggbb` or `#rrggbbaa`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// Corner radius in canvas units (frames, images).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// Figma image SHA-1 (image nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raster: Option<Raster>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    pub characters: String,
    pub font_size: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub font_family: String,
    pub font_weight: u32,
    pub italic: bool,
    /// Line height in canvas units (None = the font's normal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f64>,
    pub letter_spacing: f64,
    /// `left | center | right | justify`.
    pub align: String,
    /// `top | center | bottom`.
    pub valign: String,
    /// False for auto-width text (one line per paragraph, no wrapping).
    pub wrap: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Raster {
    /// Draw ops in node-local canvas units (the node's top-left is 0,0).
    pub ops: Vec<Op>,
    /// Figma image SHA-1s the ops draw (they must be loaded first).
    pub images: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub path: String,
    pub evenodd: bool,
    pub m: [f64; 6],
}

/// One draw op. `m` maps the path's coordinates to the raster's.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Op {
    Fill {
        path: String,
        evenodd: bool,
        m: [f64; 6],
        paint: Paint,
    },
    /// A centerline stroke (no precomputed outline): `align` is `center | inside | outside`.
    Stroke {
        path: String,
        m: [f64; 6],
        width: f64,
        align: String,
        cap: String,
        join: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dash: Vec<f64>,
        paint: Paint,
    },
    /// Drop shadow of the path (drawn alone, before the fills).
    Shadow {
        path: String,
        evenodd: bool,
        m: [f64; 6],
        color: String,
        dx: f64,
        dy: f64,
        blur: f64,
    },
    /// Starts a layer: later ops composite into it until the matching `pop`, which draws it
    /// with `opacity`, clipped to `clip` (in raster coordinates via `clip_m`) when present.
    Push {
        opacity: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
        clip_m: [f64; 6],
    },
    Pop,
    /// Boolean operation of the parts (`union | subtract | intersect | xor`), painted.
    Bool {
        mode: String,
        parts: Vec<Part>,
        paints: Vec<Paint>,
        /// Node size: the paints cover `[0, w] × [0, h]` in node space.
        size: [f64; 2],
        /// Maps node space to raster coordinates.
        m: [f64; 6],
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Paint {
    Solid {
        color: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        blend: Option<String>,
    },
    /// `kind`: `linear` (along x from 0 to 1 at y = 0.5), `radial` (circle at 0.5,0.5 with
    /// radius 0.5), `angular`, `diamond`, all in gradient space; `m` maps gradient space to
    /// the path's coordinates.
    Gradient {
        kind: String,
        stops: Vec<(f64, String)>,
        m: [f64; 6],
        opacity: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        blend: Option<String>,
    },
    /// Draws the image's unit square through `m` (into the path's coordinates), clipped.
    Image {
        hash: String,
        m: [f64; 6],
        opacity: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        blend: Option<String>,
    },
}

// ---- resolved tree ------------------------------------------------------------------------

/// A node after instance expansion: its (merged) fields and children.
#[derive(Debug, Clone)]
pub struct RNode {
    pub d: Value,
    pub kids: Vec<RNode>,
}

impl RNode {
    fn ty(&self) -> &str {
        self.d.s("type").unwrap_or("")
    }
    fn name(&self) -> String {
        self.d.s("name").unwrap_or("").to_string()
    }
    fn size(&self) -> (f64, f64) {
        (self.d.f("size.x").unwrap_or(0.0), self.d.f("size.y").unwrap_or(0.0))
    }
    fn transform(&self) -> Mat {
        mat_of(self.d.get("transform"))
    }
    fn visible(&self) -> bool {
        self.d.b("visible") != Some(false) && self.d.f("opacity").is_none_or(|o| o > 0.001)
    }
    fn opacity(&self) -> f64 {
        self.d.f("opacity").unwrap_or(1.0).clamp(0.0, 1.0)
    }
    fn fills(&self) -> Vec<&Value> {
        visible_paints(self.d.get("fillPaints"))
    }
    fn strokes(&self) -> Vec<&Value> {
        if self.d.f("strokeWeight").unwrap_or(1.0) <= 0.0 {
            return vec![];
        }
        visible_paints(self.d.get("strokePaints"))
    }
    fn drop_shadows(&self) -> Vec<&Value> {
        self.d
            .get("effects")
            .map(|e| e.items().iter().filter(|e| e.b("visible") != Some(false) && e.s("type") == Some("DROP_SHADOW")).collect())
            .unwrap_or_default()
    }
    fn is_mask(&self) -> bool {
        self.d.b("mask") == Some(true)
    }
    fn radii(&self) -> [f64; 4] {
        let r = self.d.f("cornerRadius").unwrap_or(0.0);
        if self.d.b("rectangleCornerRadiiIndependent") == Some(true) {
            let g = |k: &str| self.d.f(k).unwrap_or(r);
            [g("rectangleTopLeftCornerRadius"), g("rectangleTopRightCornerRadius"), g("rectangleBottomRightCornerRadius"), g("rectangleBottomLeftCornerRadius")]
        } else {
            [r; 4]
        }
    }
    fn uniform_radius(&self) -> Option<f64> {
        let r = self.radii();
        (r.iter().all(|v| (v - r[0]).abs() < 0.01) && r[0] > 0.0).then_some(r[0])
    }
    fn clips(&self) -> bool {
        // Groups are frames with `resizeToFit` in the file format; they never clip.
        is_frame_like(self.ty()) && self.d.b("frameMaskDisabled") != Some(true) && self.d.b("resizeToFit") != Some(true)
    }
}

fn mat_of(v: Option<&Value>) -> Mat {
    let Some(t) = v else { return Mat::ID };
    let g = |k: &str, d: f64| t.f(k).unwrap_or(d);
    Mat([g("m00", 1.0), g("m01", 0.0), g("m02", 0.0), g("m10", 0.0), g("m11", 1.0), g("m12", 0.0)])
}

fn visible_paints(v: Option<&Value>) -> Vec<&Value> {
    v.map(|p| {
        p.items()
            .iter()
            .filter(|p| p.b("visible") != Some(false) && p.f("opacity").is_none_or(|o| o > 0.001))
            .collect()
    })
    .unwrap_or_default()
}

fn is_frame_like(t: &str) -> bool {
    matches!(t, "FRAME" | "SYMBOL" | "INSTANCE" | "SECTION" | "COMPONENT" | "COMPONENT_SET")
}

fn is_container(t: &str) -> bool {
    is_frame_like(t) || t == "GROUP"
}

fn is_shape(t: &str) -> bool {
    matches!(t, "RECTANGLE" | "ROUNDED_RECTANGLE" | "ELLIPSE" | "VECTOR" | "STAR" | "LINE" | "REGULAR_POLYGON" | "BOOLEAN_OPERATION")
}

/// Fields the importer reads; everything else is dropped when a node is resolved.
const KEEP: &[&str] = &[
    "guid", "type", "name", "visible", "opacity", "size", "transform", "fillPaints", "strokePaints", "strokeWeight",
    "strokeAlign", "strokeCap", "strokeJoin", "dashPattern", "cornerRadius", "rectangleTopLeftCornerRadius",
    "rectangleTopRightCornerRadius", "rectangleBottomLeftCornerRadius", "rectangleBottomRightCornerRadius",
    "rectangleCornerRadiiIndependent", "textData", "fontSize", "fontName", "lineHeight", "letterSpacing",
    "textAlignHorizontal", "textAlignVertical", "textAutoResize", "textCase", "fillGeometry", "strokeGeometry",
    "vectorData", "effects", "symbolData", "derivedSymbolData", "overriddenSymbolID", "componentPropAssignments", "componentPropRefs",
    "overrideKey", "resizeToFit", "frameMaskDisabled", "booleanOperation", "mask", "count", "starInnerScale",
    "internalOnly",
];

fn slim(o: &Obj) -> Obj {
    o.iter().filter(|(k, _)| KEEP.contains(&k.as_ref())).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Overlays `over`'s fields onto `base` (`guidPath` excluded).
fn merge(base: &mut Value, over: &Obj) {
    if let Value::Obj(b) = base {
        for (k, v) in over {
            if k.as_ref() != "guidPath" {
                b.insert(k.clone(), v.clone());
            }
        }
    }
}

type Path = Vec<Guid>;
type Overrides = HashMap<Path, Obj>;
type Props = HashMap<Guid, Value>;

fn path_of(v: &Value) -> Path {
    v.at("guidPath.guids").map(|g| g.items().iter().filter_map(guid_of).collect()).unwrap_or_default()
}

/// Entries under `prefix` with the prefix removed.
fn sub(map: &Overrides, prefix: &[Guid]) -> Overrides {
    map.iter()
        .filter(|(p, _)| p.len() > prefix.len() && p.starts_with(prefix))
        .map(|(p, v)| (p[prefix.len()..].to_vec(), v.clone()))
        .collect()
}

/// `own` overlaid by `outer` (field by field per path).
fn layered(own: impl Iterator<Item = (Path, Obj)>, outer: &Overrides) -> Overrides {
    let mut m: Overrides = HashMap::new();
    for (p, o) in own {
        m.entry(p).or_default().extend(o);
    }
    for (p, o) in outer {
        m.entry(p.clone()).or_default().extend(o.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    m
}

struct Scene<'a> {
    nodes: HashMap<Guid, &'a Obj>,
    kids: HashMap<Guid, Vec<Guid>>,
    warnings: BTreeSet<String>,
    resolved: usize,
}

const MAX_DEPTH: usize = 48;

impl<'a> Scene<'a> {
    fn new(message: &'a Value) -> Self {
        let mut nodes = HashMap::new();
        let mut kids: HashMap<Guid, Vec<(String, Guid)>> = HashMap::new();
        for n in message.get("nodeChanges").map(|v| v.items()).unwrap_or_default() {
            let (Some(o), Some(g)) = (n.obj(), n.get("guid").and_then(guid_of)) else { continue };
            if n.s("phase") == Some("REMOVED") {
                continue;
            }
            nodes.insert(g, o);
            if let Some(p) = n.at("parentIndex.guid").and_then(guid_of) {
                let pos = n.s("parentIndex.position").unwrap_or("").to_string();
                kids.entry(p).or_default().push((pos, g));
            }
        }
        let kids = kids
            .into_iter()
            .map(|(p, mut v)| {
                v.sort();
                (p, v.into_iter().map(|(_, g)| g).collect())
            })
            .collect();
        Scene { nodes, kids, warnings: BTreeSet::new(), resolved: 0 }
    }

    fn children(&self, g: Guid) -> &[Guid] {
        self.kids.get(&g).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn key(&self, g: Guid) -> Guid {
        self.nodes.get(&g).and_then(|o| o.get("overrideKey")).and_then(guid_of).unwrap_or(g)
    }

    /// The selection: children of the visible pages (not the hidden internal canvas).
    fn roots(&self) -> Vec<Guid> {
        let mut pages: Vec<(Guid, &Obj)> = self
            .nodes
            .iter()
            .filter(|(_, o)| o.get("type").and_then(Value::str) == Some("CANVAS"))
            .filter(|(_, o)| {
                o.get("internalOnly").and_then(Value::bool) != Some(true) && o.get("visible").and_then(Value::bool) != Some(false)
            })
            .map(|(g, o)| (*g, *o))
            .collect();
        pages.sort_by_key(|(g, _)| *g);
        pages.iter().flat_map(|(g, _)| self.children(*g).to_vec()).collect()
    }

    fn build(&mut self, g: Guid, depth: usize) -> Option<RNode> {
        let o = *self.nodes.get(&g)?;
        self.resolved += 1;
        let d = Value::Obj(slim(o));
        if d.s("type") == Some("INSTANCE") {
            return Some(self.instance(d, &HashMap::new(), &HashMap::new(), &HashMap::new(), depth));
        }
        if depth > MAX_DEPTH {
            return Some(RNode { d, kids: vec![] });
        }
        let kids = self.children(g).to_vec().into_iter().filter_map(|k| self.build(k, depth + 1)).collect();
        Some(RNode { d, kids })
    }

    fn instance(&mut self, d: Value, ov: &Overrides, dv: &Overrides, outer_props: &Props, depth: usize) -> RNode {
        let own_ov = d.at("symbolData.symbolOverrides").map(|v| v.items().to_vec()).unwrap_or_default();
        let ov = layered(own_ov.iter().filter_map(|o| Some((path_of(o), o.obj()?.clone()))), ov);
        let own_dv = d.get("derivedSymbolData").map(|v| v.items().to_vec()).unwrap_or_default();
        let dv = layered(own_dv.iter().filter_map(|o| Some((path_of(o), o.obj()?.clone()))), dv);
        let mut props: Props = HashMap::new();
        for a in d.get("componentPropAssignments").map(|v| v.items()).unwrap_or_default() {
            if let (Some(id), Some(v)) = (a.get("defID").and_then(guid_of), a.get("varValue")) {
                props.insert(id, v.clone());
            }
        }
        props.extend(outer_props.iter().map(|(k, v)| (*k, v.clone())));
        let swapped = d.get("overriddenSymbolID").and_then(guid_of).filter(|s| self.nodes.contains_key(s));
        let sym = swapped.or_else(|| d.at("symbolData.symbolID").and_then(guid_of));
        let kids = match sym {
            Some(s) if self.nodes.contains_key(&s) && depth <= MAX_DEPTH => self
                .children(s)
                .to_vec()
                .into_iter()
                .filter_map(|c| {
                    let key = self.key(c);
                    self.inst_child(c, vec![key], &ov, &dv, &props, depth + 1)
                })
                .collect(),
            Some(_) => {
                self.warnings.insert(format!("component of instance \"{}\" is missing from the clipboard", d.s("name").unwrap_or("")));
                vec![]
            }
            None => vec![],
        };
        RNode { d, kids }
    }

    fn inst_child(&mut self, g: Guid, path: Path, ov: &Overrides, dv: &Overrides, props: &Props, depth: usize) -> Option<RNode> {
        let o = *self.nodes.get(&g)?;
        self.resolved += 1;
        let mut d = Value::Obj(slim(o));
        if let Some(x) = ov.get(&path) {
            merge(&mut d, x);
        }
        if let Some(x) = dv.get(&path) {
            merge(&mut d, x);
        }
        apply_prop_refs(&mut d, props);
        if d.s("type") == Some("INSTANCE") {
            return Some(self.instance(d, &sub(ov, &path), &sub(dv, &path), props, depth));
        }
        if depth > MAX_DEPTH {
            return Some(RNode { d, kids: vec![] });
        }
        let kids = self
            .children(g)
            .to_vec()
            .into_iter()
            .filter_map(|c| {
                let mut p = path.clone();
                p.push(self.key(c));
                self.inst_child(c, p, ov, dv, props, depth + 1)
            })
            .collect();
        Some(RNode { d, kids })
    }
}

/// Component properties bound to this node's fields (`componentPropRefs`).
fn apply_prop_refs(d: &mut Value, props: &Props) {
    if props.is_empty() {
        return;
    }
    let refs: Vec<(Guid, String)> = d
        .get("componentPropRefs")
        .map(|r| {
            r.items()
                .iter()
                .filter_map(|r| Some((r.get("defID").and_then(guid_of)?, r.s("componentPropNodeField")?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let Value::Obj(o) = d else { return };
    for (id, field) in refs {
        let Some(v) = props.get(&id).and_then(|v| v.get("value")) else { continue };
        match field.as_str() {
            "VISIBLE" => {
                if let Some(b) = v.b("boolValue") {
                    o.insert(Arc::from("visible"), Value::Bool(b));
                }
            }
            "TEXT_DATA" => {
                if let Some(t) = v.get("textDataValue") {
                    o.insert(Arc::from("textData"), t.clone());
                }
            }
            "OVERRIDDEN_SYMBOL_ID" => {
                // `{guid: {sessionID, localID}}`.
                if let Some(s) = v.get("symbolIdValue").map(|s| s.get("guid").unwrap_or(s)) {
                    o.insert(Arc::from("overriddenSymbolID"), s.clone());
                }
            }
            _ => {}
        }
    }
}

// ---- variables ----------------------------------------------------------------------------

/// Color variables: paints bound to a variable carry a cached `color`, but Figma refreshes
/// that cache lazily, so some of it may be from another mode (e.g. light-mode values inside a
/// dark-mode design). The clipboard ships the variables, not the mode an ancestor outside the
/// copy selected, so the mode of each collection is inferred: the one whose values match the
/// most cached colors wins, and bound paints are re-resolved with it.
#[derive(Default)]
struct Vars {
    /// Variable id (`k:<key>` / `g:<guid>`) → (collection id, values by mode).
    vars: HashMap<String, (String, Vec<(Guid, Value)>)>,
    /// Collection id → its modes.
    sets: HashMap<String, Vec<Guid>>,
    /// Inferred mode per collection (both ids of a collection).
    mode: HashMap<String, Guid>,
}

fn ref_id(r: &Value) -> Option<String> {
    if let Some(k) = r.s("assetRef.key") {
        return Some(format!("k:{k}"));
    }
    r.get("guid").and_then(guid_of).map(|(a, b)| format!("g:{a}:{b}"))
}

fn same_color(a: &Value, b: &Value) -> bool {
    ["r", "g", "b", "a"].iter().all(|k| (a.f(k).unwrap_or(1.0) - b.f(k).unwrap_or(1.0)).abs() < 0.004)
}

impl Vars {
    fn new(message: &Value) -> Vars {
        let mut v = Vars::default();
        let nodes = message.get("nodeChanges").map(|n| n.items()).unwrap_or_default();
        let ids = |n: &Value| {
            let mut ids = Vec::new();
            if let Some(k) = n.s("key") {
                ids.push(format!("k:{k}"));
            }
            if let Some((a, b)) = n.get("guid").and_then(guid_of) {
                ids.push(format!("g:{a}:{b}"));
            }
            ids
        };
        for n in nodes {
            match n.s("type") {
                Some("VARIABLE_SET") => {
                    let modes: Vec<Guid> = n.get("variableSetModes").map(|m| m.items().iter().filter_map(|m| m.get("id").and_then(guid_of)).collect()).unwrap_or_default();
                    for id in ids(n) {
                        v.sets.insert(id, modes.clone());
                    }
                }
                Some("VARIABLE") => {
                    let Some(set) = n.get("variableSetID").and_then(ref_id) else { continue };
                    let values: Vec<(Guid, Value)> = n
                        .at("variableDataValues.entries")
                        .map(|e| e.items().iter().filter_map(|e| Some((e.get("modeID").and_then(guid_of)?, e.get("variableData")?.clone()))).collect())
                        .unwrap_or_default();
                    for id in ids(n) {
                        v.vars.insert(id, (set.clone(), values.clone()));
                    }
                }
                _ => {}
            }
        }
        if !v.vars.is_empty() {
            v.infer_modes(nodes);
        }
        v
    }

    /// The color of variable `id`, following aliases; `pick` chooses a collection's mode.
    fn resolve(&self, id: &str, pick: &dyn Fn(&str) -> Option<Guid>, depth: usize) -> Option<Value> {
        let (set, values) = self.vars.get(id)?;
        if depth > 16 || values.is_empty() {
            return None;
        }
        let mode = pick(set);
        let data = mode.and_then(|m| values.iter().find(|(vm, _)| *vm == m)).unwrap_or(&values[0]).1.clone();
        let value = data.get("value")?;
        if let Some(c) = value.get("colorValue") {
            return Some(c.clone());
        }
        self.resolve(&ref_id(value.get("alias")?)?, pick, depth + 1)
    }

    fn infer_modes(&mut self, nodes: &[Value]) {
        // Every (variable, cached color) pair in the scene, overrides included.
        fn walk<'v>(v: &'v Value, out: &mut Vec<(String, &'v Value)>) {
            match v {
                Value::Obj(o) => {
                    if let (Some(alias), Some(color)) = (v.at("colorVar.value.alias"), o.get("color"))
                        && let Some(id) = ref_id(alias)
                    {
                        out.push((id, color));
                    }
                    for x in o.values() {
                        walk(x, out);
                    }
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        let mut bound = Vec::new();
        for n in nodes.iter().filter(|n| !matches!(n.s("type"), Some("VARIABLE" | "VARIABLE_SET"))) {
            walk(n, &mut bound);
        }
        let mut votes: HashMap<(String, Guid), usize> = HashMap::new();
        for (id, cached) in &bound {
            let Some((set, _)) = self.vars.get(id) else { continue };
            for &m in self.sets.get(set).map(|v| v.as_slice()).unwrap_or(&[]) {
                let pick = |s: &str| (s == set).then_some(m);
                if self.resolve(id, &pick, 0).is_some_and(|c| same_color(&c, cached)) {
                    *votes.entry((set.clone(), m)).or_default() += 1;
                }
            }
        }
        let mut best: HashMap<String, (Guid, usize)> = HashMap::new();
        for ((set, m), n) in votes {
            let e = best.entry(set).or_insert((m, 0));
            if n > e.1 || (n == e.1 && m < e.0) {
                *e = (m, n);
            }
        }
        for (set, (m, _)) in best {
            self.mode.insert(set, m);
        }
    }

    /// The paint's color: re-resolved from its variable when bound, else the cached one.
    fn color(&self, p: &Value) -> Option<Value> {
        if let Some(id) = p.at("colorVar.value.alias").and_then(ref_id)
            && let Some(c) = self.resolve(&id, &|s| self.mode.get(s).copied(), 0)
        {
            return Some(c);
        }
        p.get("color").cloned()
    }
}

// ---- mapping ------------------------------------------------------------------------------

fn hex2(v: f64) -> String {
    format!("{:02x}", (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// `#rrggbb` (opaque) or `#rrggbbaa`.
fn hex_color(c: &Value, alpha: f64) -> String {
    let a = c.f("a").unwrap_or(1.0) * alpha;
    let rgb = format!("#{}{}{}", hex2(c.f("r").unwrap_or(0.0)), hex2(c.f("g").unwrap_or(0.0)), hex2(c.f("b").unwrap_or(0.0)));
    if a >= 0.999 { rgb } else { format!("{rgb}{}", hex2(a)) }
}

fn image_hash(p: &Value) -> Option<String> {
    let h = p.at("image.hash")?.bytes()?;
    (!h.is_empty()).then(|| h.iter().map(|b| format!("{b:02x}")).collect())
}

fn blend_of(p: &Value) -> Option<String> {
    let b = p.s("blendMode")?;
    let css = match b {
        "MULTIPLY" => "multiply",
        "SCREEN" => "screen",
        "OVERLAY" => "overlay",
        "DARKEN" => "darken",
        "LIGHTEN" => "lighten",
        "COLOR_DODGE" => "color-dodge",
        "COLOR_BURN" => "color-burn",
        "HARD_LIGHT" => "hard-light",
        "SOFT_LIGHT" => "soft-light",
        "DIFFERENCE" => "difference",
        "EXCLUSION" => "exclusion",
        "HUE" => "hue",
        "SATURATION" => "saturation",
        "COLOR" => "color",
        "LUMINOSITY" => "luminosity",
        _ => return None,
    };
    Some(css.into())
}

fn is_solid_normal(p: &Value) -> bool {
    p.s("type") == Some("SOLID") && blend_of(p).is_none()
}

/// Image paint that maps 1:1 onto our image node (cover crop).
fn simple_image(p: &Value) -> Option<String> {
    let mode = p.s("imageScaleMode").unwrap_or("FILL");
    let ok = matches!(p.s("type"), Some("IMAGE")) && mode == "FILL" && blend_of(p).is_none() && p.f("opacity").unwrap_or(1.0) >= 0.999 && p.f("rotation").unwrap_or(0.0).abs() < 0.01;
    ok.then(|| image_hash(p)).flatten()
}

struct Emit<'s, 'a> {
    scene: &'s mut Scene<'a>,
    vars: Vars,
    blobs: Vec<&'a [u8]>,
    urls: &'a serde_json::Map<String, serde_json::Value>,
    out: Vec<PlanNode>,
    images: BTreeMap<String, String>,
    stats: Stats,
}

/// World bounds accumulator.
#[derive(Clone, Copy)]
struct Bounds([f64; 4]);

impl Bounds {
    const EMPTY: Bounds = Bounds([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY]);
    fn add(&mut self, r: [f64; 4], pad: f64) {
        let b = &mut self.0;
        b[0] = b[0].min(r[0] - pad);
        b[1] = b[1].min(r[1] - pad);
        b[2] = b[2].max(r[0] + r[2] + pad);
        b[3] = b[3].max(r[1] + r[3] + pad);
    }
    fn is_empty(&self) -> bool {
        !(self.0[2] > self.0[0] && self.0[3] > self.0[1])
    }
}

impl<'s, 'a> Emit<'s, 'a> {
    fn push(&mut self, n: PlanNode) -> usize {
        match n.kind {
            Kind::Frame => self.stats.frames += 1,
            Kind::Group => self.stats.groups += 1,
            Kind::Text | Kind::Note => self.stats.texts += 1,
            Kind::Image => self.stats.images += 1,
            Kind::Raster => self.stats.rasters += 1,
        }
        self.out.push(n);
        self.out.len() - 1
    }

    fn use_image(&mut self, hash: &str) -> bool {
        if self.images.contains_key(hash) {
            return true;
        }
        match self.urls.get(hash).and_then(|u| u.as_str()) {
            Some(u) => {
                self.images.insert(hash.to_string(), u.to_string());
                true
            }
            None => {
                self.scene.warnings.insert("some images have no download URL in the clipboard (skipped)".into());
                false
            }
        }
    }

    fn blob(&self, idx: Option<f64>) -> Option<&'a [u8]> {
        self.blobs.get(idx? as usize).copied()
    }

    fn node_at(kind: Kind, name: String, r: [f64; 4], origin: (f64, f64), parent: Option<usize>) -> PlanNode {
        let round = |v: f64| (v * 100.0).round() / 100.0;
        PlanNode {
            parent,
            kind,
            name,
            x: round(r[0] - origin.0),
            y: round(r[1] - origin.1),
            w: round(r[2].max(1.0)),
            h: round(r[3].max(1.0)),
            fill: None,
            radius: None,
            image: None,
            text: None,
            raster: None,
        }
    }

    /// Emits `n` (world transform `w`) under `parent`, whose top-left is `origin` (world).
    fn node(&mut self, n: &RNode, w: Mat, parent: Option<usize>, origin: (f64, f64), root: bool) {
        if !n.visible() || n.is_mask() {
            if n.is_mask() {
                self.scene.warnings.insert("masks are not supported: masked content shows unmasked".into());
            }
            self.stats.skipped += 1;
            return;
        }
        let t = n.ty();
        let (sw, sh) = n.size();
        let bounds = w.bounds(sw, sh);
        match t {
            "TEXT" => self.text(n, &w, bounds, parent, origin),
            "STICKY" => self.sticky(n, bounds, parent, origin),
            _ if is_container(t) || (!is_shape(t) && !n.kids.is_empty()) => {
                if self.vector_art(n) && !root_is_huge(bounds) {
                    self.raster(n, &w, parent, origin, n.name());
                } else {
                    self.container(n, &w, bounds, parent, origin, root);
                }
            }
            "RECTANGLE" | "ROUNDED_RECTANGLE" if w.axis_aligned() && n.strokes().is_empty() && n.drop_shadows().is_empty() && n.opacity() >= 0.999 => {
                let fills = n.fills();
                match fills.as_slice() {
                    [p] if simple_image(p).is_some_and(|h| self.use_image(&h)) => {
                        let mut pn = Self::node_at(Kind::Image, n.name(), bounds, origin, parent);
                        pn.image = simple_image(p);
                        pn.radius = n.uniform_radius().map(|r| r * w.scale_factor());
                        self.push(pn);
                    }
                    [] => self.stats.skipped += 1,
                    _ => self.raster(n, &w, parent, origin, n.name()),
                }
            }
            _ if is_shape(t) => self.raster(n, &w, parent, origin, n.name()),
            _ => {
                self.scene.warnings.insert(format!("unsupported Figma node type {t} (skipped)"));
                self.stats.skipped += 1;
            }
        }
    }

    fn container(&mut self, n: &RNode, w: &Mat, bounds: [f64; 4], parent: Option<usize>, origin: (f64, f64), root: bool) {
        let t = n.ty();
        let fills = n.fills();
        let strokes = n.strokes();
        let is_group = t == "GROUP" || n.d.b("resizeToFit") == Some(true);
        let painted = !fills.is_empty() || !strokes.is_empty();
        let kind = if is_group || (!root && !painted && t != "SECTION") { Kind::Group } else { Kind::Frame };
        let mut pn = Self::node_at(kind, n.name(), bounds, origin, parent);
        let scale = w.scale_factor();
        let mut rest: &[&Value] = &fills;
        if kind == Kind::Frame {
            if let Some(first) = fills.first().filter(|p| is_solid_normal(p))
                && let Some(c) = self.vars.color(first)
            {
                pn.fill = Some(hex_color(&c, first.f("opacity").unwrap_or(1.0)));
                rest = &fills[1..];
            }
            pn.radius = n.uniform_radius().map(|r| r * scale);
        }
        let id = self.push(pn);
        let me = (bounds[0], bounds[1]);
        // Background paints that aren't the frame's fill: image layers and rasters.
        let mut pending: Vec<&Value> = Vec::new();
        for p in rest {
            if w.axis_aligned()
                && let Some(h) = simple_image(p)
                && self.use_image(&h)
            {
                self.paint_layer(n, w, &pending, id, me);
                pending.clear();
                let mut img = Self::node_at(Kind::Image, format!("{} image", n.name()), bounds, me, Some(id));
                img.image = Some(h);
                img.radius = n.uniform_radius().map(|r| r * scale);
                self.push(img);
            } else {
                pending.push(p);
            }
        }
        self.paint_layer(n, w, &pending, id, me);
        for k in &n.kids {
            self.node(k, w.mul(&k.transform()), Some(id), me, false);
        }
        if !strokes.is_empty() {
            let mut ops = Vec::new();
            let mut b = Bounds::EMPTY;
            self.stroke_ops(n, w, &mut ops, &mut b);
            self.emit_raster(format!("{} stroke", n.name()), ops, b, Some(id), me);
        }
    }

    /// A raster of some of `n`'s fill paints over its own shape.
    fn paint_layer(&mut self, n: &RNode, w: &Mat, paints: &[&Value], parent: usize, origin: (f64, f64)) {
        if paints.is_empty() {
            return;
        }
        let mut ops = Vec::new();
        let mut b = Bounds::EMPTY;
        let (sw, sh) = n.size();
        b.add(w.bounds(sw, sh), 0.0);
        for (path, evenodd) in self.fill_geometry(n) {
            for p in paints {
                if let Some(paint) = self.paint(p, n.size()) {
                    ops.push(Op::Fill { path: path.clone(), evenodd, m: w.0, paint });
                }
            }
        }
        self.emit_raster(format!("{} fill", n.name()), ops, b, Some(parent), origin);
    }

    fn text(&mut self, n: &RNode, w: &Mat, bounds: [f64; 4], parent: Option<usize>, origin: (f64, f64)) {
        let mut chars = n.d.s("textData.characters").unwrap_or("").to_string();
        if chars.trim().is_empty() {
            self.stats.skipped += 1;
            return;
        }
        match n.d.s("textCase") {
            Some("UPPER") => chars = chars.to_uppercase(),
            Some("LOWER") => chars = chars.to_lowercase(),
            _ => {}
        }
        let s = w.scale_factor();
        let font_size = n.d.f("fontSize").unwrap_or(12.0) * s;
        let line_height = n.d.get("lineHeight").and_then(|l| {
            let v = l.f("value")?;
            match l.s("units")? {
                "PIXELS" => Some(v * s),
                "PERCENT" => Some(v / 100.0 * font_size),
                "RAW" => Some(v * font_size),
                _ => None,
            }
        });
        let letter_spacing = n
            .d
            .get("letterSpacing")
            .and_then(|l| {
                let v = l.f("value")?;
                Some(match l.s("units")? {
                    "PERCENT" => v / 100.0 * font_size,
                    _ => v * s,
                })
            })
            .unwrap_or(0.0);
        let style = n.d.s("fontName.style").unwrap_or("Regular").to_ascii_lowercase().replace([' ', '-'], "");
        let font_weight = [
            ("thin", 100),
            ("hairline", 100),
            ("extralight", 200),
            ("ultralight", 200),
            ("light", 300),
            ("medium", 500),
            ("semibold", 600),
            ("demibold", 600),
            ("extrabold", 800),
            ("ultrabold", 800),
            ("bold", 700),
            ("heavy", 900),
            ("black", 900),
        ]
        .iter()
        .find(|(k, _)| style.contains(k))
        .map_or(400, |(_, w)| *w);
        let color = n.fills().iter().find(|p| p.s("type") == Some("SOLID")).and_then(|p| Some(hex_color(&self.vars.color(p)?, p.f("opacity").unwrap_or(1.0) * n.opacity())));
        let lower = |s: Option<&str>, d: &str| s.unwrap_or(d).to_ascii_lowercase();
        let mut align = lower(n.d.s("textAlignHorizontal"), "left");
        if align == "justified" {
            align = "justify".into();
        }
        let style = TextStyle {
            characters: chars,
            font_size: (font_size * 100.0).round() / 100.0,
            color,
            font_family: n.d.s("fontName.family").unwrap_or("Inter").to_string(),
            font_weight,
            italic: style.contains("italic") || style.contains("oblique"),
            line_height: line_height.map(|v| (v * 100.0).round() / 100.0),
            letter_spacing: (letter_spacing * 1000.0).round() / 1000.0,
            align,
            valign: lower(n.d.s("textAlignVertical"), "top"),
            // Auto-width text never wraps; neither does text Figma laid out on one line (a
            // fallback font that runs wider must not push words onto a clipped second line).
            wrap: n.d.s("textAutoResize") != Some("WIDTH_AND_HEIGHT") && bounds[3] >= 1.5 * line_height.unwrap_or(font_size * 1.2),
        };
        let mut pn = Self::node_at(Kind::Text, n.name(), bounds, origin, parent);
        pn.text = Some(style);
        self.push(pn);
    }

    fn sticky(&mut self, n: &RNode, bounds: [f64; 4], parent: Option<usize>, origin: (f64, f64)) {
        let mut pn = Self::node_at(Kind::Note, n.name(), bounds, origin, parent);
        pn.fill = n.fills().first().and_then(|p| Some(hex_color(&self.vars.color(p)?, 1.0)));
        pn.text = Some(TextStyle {
            characters: n.d.s("textData.characters").unwrap_or("").to_string(),
            font_size: n.d.f("fontSize").unwrap_or(16.0),
            color: None,
            font_family: String::new(),
            font_weight: 400,
            italic: false,
            line_height: None,
            letter_spacing: 0.0,
            align: "left".into(),
            valign: "top".into(),
            wrap: true,
        });
        self.push(pn);
    }

    /// True when the subtree draws only vector art: shapes and containers, no text and no
    /// images. Such containers become one raster instead of hundreds of tiny nodes.
    fn vector_art(&self, n: &RNode) -> bool {
        fn walk(n: &RNode, shapes: &mut usize) -> bool {
            if !n.visible() {
                return true;
            }
            let t = n.ty();
            let images = n.fills().iter().any(|p| matches!(p.s("type"), Some("IMAGE" | "VIDEO")));
            if images || n.is_mask() {
                return false;
            }
            if is_shape(t) {
                *shapes += 1;
                return true;
            }
            if !is_container(t) {
                return false;
            }
            n.kids.iter().all(|k| walk(k, shapes))
        }
        let mut shapes = 0;
        walk(n, &mut shapes) && shapes > 0
    }

    // ---- rasters ------------------------------------------------------------------------

    fn raster(&mut self, n: &RNode, w: &Mat, parent: Option<usize>, origin: (f64, f64), name: String) {
        let mut ops = Vec::new();
        let mut b = Bounds::EMPTY;
        self.draw(n, w, &mut ops, &mut b);
        if ops.is_empty() {
            self.stats.skipped += 1;
            return;
        }
        self.emit_raster(name, ops, b, parent, origin);
    }

    fn emit_raster(&mut self, name: String, mut ops: Vec<Op>, b: Bounds, parent: Option<usize>, origin: (f64, f64)) {
        if ops.is_empty() || b.is_empty() {
            return;
        }
        // Whole canvas units: rasters stay pixel aligned at 100 %.
        let x0 = b.0[0].floor();
        let y0 = b.0[1].floor();
        let r = [x0, y0, (b.0[2].ceil() - x0).max(1.0), (b.0[3].ceil() - y0).max(1.0)];
        let shift = Mat::translate(-x0, -y0);
        let mut images = BTreeSet::new();
        for op in &mut ops {
            let mut paints: Vec<&Paint> = vec![];
            match op {
                Op::Fill { m, paint, .. } | Op::Stroke { m, paint, .. } => {
                    *m = shift.mul(&Mat(*m)).0;
                    paints.push(paint);
                }
                Op::Shadow { m, .. } => *m = shift.mul(&Mat(*m)).0,
                Op::Push { clip_m, .. } => *clip_m = shift.mul(&Mat(*clip_m)).0,
                Op::Bool { parts, paints: ps, m, .. } => {
                    for p in parts.iter_mut() {
                        p.m = shift.mul(&Mat(p.m)).0;
                    }
                    *m = shift.mul(&Mat(*m)).0;
                    paints.extend(ps.iter());
                }
                Op::Pop => {}
            }
            for p in paints {
                if let Paint::Image { hash, .. } = p {
                    images.insert(hash.clone());
                }
            }
        }
        let mut pn = Self::node_at(Kind::Raster, name, r, origin, parent);
        pn.raster = Some(Raster { ops, images: images.into_iter().collect() });
        self.push(pn);
    }

    /// Draw ops for `n` and its subtree in world space; grows `b`.
    fn draw(&mut self, n: &RNode, w: &Mat, ops: &mut Vec<Op>, b: &mut Bounds) {
        if !n.visible() || n.is_mask() {
            return;
        }
        let t = n.ty();
        if t == "TEXT" {
            self.scene.warnings.insert("text inside vector art is not rasterized".into());
            return;
        }
        let (sw, sh) = n.size();
        let opacity = n.opacity();
        let clip = n.clips() && !n.kids.is_empty();
        let layer = opacity < 0.999 || clip;
        if layer {
            ops.push(Op::Push {
                opacity,
                clip: clip.then(|| geom::rect_path(sw, sh, n.radii())),
                clip_m: w.0,
            });
        }
        let start = ops.len();
        let geometry = self.fill_geometry(n);
        // Shadows (behind everything the node draws).
        for e in n.drop_shadows() {
            let s = w.scale_factor();
            let (dx, dy) = (e.f("offset.x").unwrap_or(0.0) * s, e.f("offset.y").unwrap_or(0.0) * s);
            let blur = e.f("radius").unwrap_or(0.0) * s;
            let color = e.get("color").map_or("#00000040".into(), |c| hex_color(c, 1.0));
            b.add(w.bounds(sw, sh), blur + dx.abs().max(dy.abs()) + e.f("spread").unwrap_or(0.0).max(0.0));
            for (path, evenodd) in &geometry {
                ops.push(Op::Shadow { path: path.clone(), evenodd: *evenodd, m: w.0, color: color.clone(), dx, dy, blur });
            }
        }
        if t == "BOOLEAN_OPERATION" && n.d.get("fillGeometry").is_none() {
            let mut parts = Vec::new();
            for k in &n.kids {
                self.bool_parts(k, &w.mul(&k.transform()), &mut parts, b);
            }
            let paints: Vec<Paint> = n.fills().iter().filter_map(|p| self.paint(p, n.size())).collect();
            if !parts.is_empty() && !paints.is_empty() {
                b.add(w.bounds(sw, sh), 0.0);
                let mode = n.d.s("booleanOperation").unwrap_or("UNION").to_ascii_lowercase();
                ops.push(Op::Bool { mode, parts, paints, size: [sw, sh], m: w.0 });
            }
        } else {
            let fills = n.fills();
            if !fills.is_empty() && !geometry.is_empty() {
                b.add(w.bounds(sw, sh), 0.0);
            }
            for p in fills {
                let Some(paint) = self.paint(p, n.size()) else { continue };
                for (path, evenodd) in &geometry {
                    ops.push(Op::Fill { path: path.clone(), evenodd: *evenodd, m: w.0, paint: paint.clone() });
                }
            }
            let container = is_container(t);
            if !container {
                self.stroke_ops(n, w, ops, b);
            }
            if t != "BOOLEAN_OPERATION" {
                for k in &n.kids {
                    self.draw(k, &w.mul(&k.transform()), ops, b);
                }
            }
            if container {
                self.stroke_ops(n, w, ops, b);
            }
        }
        if layer {
            if ops.len() == start {
                ops.pop();
            } else {
                ops.push(Op::Pop);
            }
        }
    }

    fn bool_parts(&mut self, n: &RNode, w: &Mat, parts: &mut Vec<Part>, b: &mut Bounds) {
        if !n.visible() {
            return;
        }
        let geometry = self.fill_geometry(n);
        if !geometry.is_empty() {
            let (sw, sh) = n.size();
            b.add(w.bounds(sw, sh), 0.0);
            parts.extend(geometry.into_iter().map(|(path, evenodd)| Part { path, evenodd, m: w.0 }));
            return;
        }
        if n.ty() == "BOOLEAN_OPERATION" {
            self.scene.warnings.insert("nested boolean operations are approximated as unions".into());
        }
        for k in &n.kids {
            self.bool_parts(k, &w.mul(&k.transform()), parts, b);
        }
    }

    fn stroke_ops(&mut self, n: &RNode, w: &Mat, ops: &mut Vec<Op>, b: &mut Bounds) {
        let strokes = n.strokes();
        if strokes.is_empty() {
            return;
        }
        let (sw, sh) = n.size();
        let width = n.d.f("strokeWeight").unwrap_or(1.0);
        let align = n.d.s("strokeAlign").unwrap_or("CENTER").to_ascii_lowercase();
        let pad = match align.as_str() {
            "inside" => 0.0,
            "outside" => width,
            _ => width / 2.0,
        } * w.scale_factor()
            + if n.ty() == "VECTOR" || n.ty() == "LINE" { width * w.scale_factor() } else { 0.0 };
        b.add(w.bounds(sw, sh), pad);
        // A precomputed outline (instances): fill it.
        if let Some(g) = n.d.get("strokeGeometry").filter(|g| !g.items().is_empty()) {
            let paths: Vec<(String, bool)> = g
                .items()
                .iter()
                .filter_map(|e| Some((geom::commands_to_svg(self.blob(e.f("commandsBlob"))?)?, e.s("windingRule") == Some("ODD"))))
                .collect();
            for p in &strokes {
                let Some(paint) = self.paint(p, n.size()) else { continue };
                for (path, evenodd) in &paths {
                    ops.push(Op::Fill { path: path.clone(), evenodd: *evenodd, m: w.0, paint: paint.clone() });
                }
            }
            return;
        }
        let path = match n.ty() {
            "LINE" => geom::line_path(sw),
            "VECTOR" => match self.network(n) {
                Some((net, m)) => net.stroke_path(&m),
                None => return,
            },
            _ => match self.fill_geometry(n).into_iter().next() {
                Some((p, _)) => p,
                None => return,
            },
        };
        let lower = |k: &str, d: &str| n.d.s(k).unwrap_or(d).to_ascii_lowercase();
        let cap = match lower("strokeCap", "none").as_str() {
            "round" => "round",
            "square" => "square",
            _ => "butt",
        }
        .to_string();
        let join = match lower("strokeJoin", "miter").as_str() {
            "round" => "round",
            "bevel" => "bevel",
            _ => "miter",
        }
        .to_string();
        let dash: Vec<f64> = n.d.get("dashPattern").map(|d| d.items().iter().filter_map(Value::num).collect()).unwrap_or_default();
        for p in &strokes {
            let Some(paint) = self.paint(p, n.size()) else { continue };
            ops.push(Op::Stroke { path: path.clone(), m: w.0, width, align: align.clone(), cap: cap.clone(), join: join.clone(), dash: dash.clone(), paint });
        }
    }

    fn network(&self, n: &RNode) -> Option<(geom::Network, Mat)> {
        let vd = n.d.get("vectorData")?;
        let net = geom::decode_network(self.blob(vd.f("vectorNetworkBlob"))?)?;
        let (sw, sh) = n.size();
        let nx = vd.f("normalizedSize.x").filter(|v| *v > 0.0).unwrap_or(sw.max(1e-6));
        let ny = vd.f("normalizedSize.y").filter(|v| *v > 0.0).unwrap_or(sh.max(1e-6));
        Some((net, Mat::scale(if nx > 0.0 { sw / nx } else { 1.0 }, if ny > 0.0 { sh / ny } else { 1.0 })))
    }

    /// Fill outlines in node-local coordinates: precomputed geometry when present, else from
    /// the vector network or the shape type.
    fn fill_geometry(&self, n: &RNode) -> Vec<(String, bool)> {
        if let Some(g) = n.d.get("fillGeometry").filter(|g| !g.items().is_empty()) {
            return g
                .items()
                .iter()
                .filter_map(|e| Some((geom::commands_to_svg(self.blob(e.f("commandsBlob"))?)?, e.s("windingRule") == Some("ODD"))))
                .collect();
        }
        let (w, h) = n.size();
        match n.ty() {
            "VECTOR" => self.network(n).map(|(net, m)| net.fill_paths(&m).into_iter().map(|(p, nz)| (p, !nz)).collect()).unwrap_or_default(),
            "ELLIPSE" => vec![(geom::ellipse_path(w, h), false)],
            "REGULAR_POLYGON" => vec![(geom::polygon_path(w, h, n.d.f("count").unwrap_or(3.0) as usize, None), false)],
            "STAR" => vec![(geom::polygon_path(w, h, n.d.f("count").unwrap_or(5.0) as usize, Some(n.d.f("starInnerScale").unwrap_or(0.38))), false)],
            "LINE" | "BOOLEAN_OPERATION" | "GROUP" => vec![],
            _ => vec![(geom::rect_path(w, h, n.radii()), false)],
        }
    }

    fn paint(&mut self, p: &Value, (w, h): (f64, f64)) -> Option<Paint> {
        let opacity = p.f("opacity").unwrap_or(1.0);
        let blend = blend_of(p);
        let t = p.s("type")?;
        Some(match t {
            "SOLID" => Paint::Solid { color: hex_color(&self.vars.color(p)?, opacity), blend },
            "GRADIENT_LINEAR" | "GRADIENT_RADIAL" | "GRADIENT_ANGULAR" | "GRADIENT_DIAMOND" => {
                let inv = mat_of(p.get("transform")).invert()?;
                let m = Mat::scale(w, h).mul(&inv);
                let stops = p
                    .get("stops")?
                    .items()
                    .iter()
                    .filter_map(|s| Some((s.f("position")?.clamp(0.0, 1.0), hex_color(&self.vars.color(s)?, 1.0))))
                    .collect();
                let kind = t.trim_start_matches("GRADIENT_").to_ascii_lowercase();
                Paint::Gradient { kind, stops, m: m.0, opacity, blend }
            }
            "IMAGE" | "VIDEO" => {
                let hash = image_hash(p)?;
                if !self.use_image(&hash) {
                    return None;
                }
                let iw = p.f("originalImageWidth").filter(|v| *v > 0.0).unwrap_or(w.max(1.0));
                let ih = p.f("originalImageHeight").filter(|v| *v > 0.0).unwrap_or(h.max(1.0));
                let m = match p.s("imageScaleMode").unwrap_or("FILL") {
                    "STRETCH" | "CROP" => Mat::scale(w, h).mul(&mat_of(p.get("transform")).invert()?),
                    "FIT" => {
                        let s = (w / iw).min(h / ih);
                        Mat::translate((w - iw * s) / 2.0, (h - ih * s) / 2.0).mul(&Mat::scale(iw * s, ih * s))
                    }
                    mode => {
                        if mode == "TILE" {
                            self.scene.warnings.insert("tiled image fills are drawn as cover".into());
                        }
                        let s = (w / iw).max(h / ih);
                        Mat::translate((w - iw * s) / 2.0, (h - ih * s) / 2.0).mul(&Mat::scale(iw * s, ih * s))
                    }
                };
                Paint::Image { hash, m: m.0, opacity, blend }
            }
            _ => {
                self.scene.warnings.insert(format!("{t} paints are not supported (skipped)"));
                return None;
            }
        })
    }
}

/// Rasterizing a multi-thousand-pixel container would lose its structure for little gain.
fn root_is_huge(b: [f64; 4]) -> bool {
    b[2] * b[3] > 4096.0 * 4096.0
}

/// The selection after instance expansion (for tests and debugging).
pub fn resolve(clip: &Clip) -> Vec<RNode> {
    let mut scene = Scene::new(&clip.message);
    scene.roots().into_iter().filter_map(|g| scene.build(g, 0)).collect()
}

/// One line per resolved node: type, name, size, visibility, paint counts.
pub fn outline(nodes: &[RNode]) -> String {
    fn walk(n: &RNode, depth: usize, out: &mut String) {
        let (w, h) = n.size();
        out.push_str(&format!(
            "{}{} {:?} {w}x{h}{} fills={} strokes={}\n",
            "  ".repeat(depth),
            n.ty(),
            n.name(),
            if n.visible() { "" } else { " HIDDEN" },
            n.fills().len(),
            n.strokes().len()
        ));
        for k in &n.kids {
            walk(k, depth + 1, out);
        }
    }
    let mut s = String::new();
    for n in nodes {
        walk(n, 0, &mut s);
    }
    s
}

/// Builds the import plan from a decoded clipboard.
pub fn plan(clip: &Clip) -> Plan {
    let empty = serde_json::Map::new();
    let urls = clip.meta.get("imageHashes").and_then(|v| v.as_object()).unwrap_or(&empty);
    let blobs: Vec<&[u8]> = clip
        .message
        .get("blobs")
        .map(|b| b.items().iter().map(|b| b.get("bytes").and_then(Value::bytes).unwrap_or(&[])).collect())
        .unwrap_or_default();
    let mut scene = Scene::new(&clip.message);
    let figma_nodes = scene.nodes.len();
    let roots: Vec<RNode> = scene.roots().into_iter().filter_map(|g| scene.build(g, 0)).collect();
    // Selection bounds from the visible roots' boxes.
    let mut sel = Bounds::EMPTY;
    for r in roots.iter().filter(|r| r.visible()) {
        let (w, h) = r.size();
        sel.add(r.transform().bounds(w, h), 0.0);
    }
    if sel.is_empty() {
        sel = Bounds([0.0, 0.0, 0.0, 0.0]);
    }
    let origin = (sel.0[0], sel.0[1]);
    let vars = Vars::new(&clip.message);
    let mut e = Emit { scene: &mut scene, vars, blobs, urls, out: Vec::new(), images: BTreeMap::new(), stats: Stats::default() };
    for r in &roots {
        e.node(r, r.transform(), None, origin, true);
    }
    let Emit { out, images, mut stats, .. } = e;
    stats.figma_nodes = figma_nodes;
    stats.resolved = scene.resolved;
    Plan {
        file_key: clip.meta.get("fileKey").and_then(|v| v.as_str()).map(String::from),
        size: [(sel.0[2] - sel.0[0]).max(0.0), (sel.0[3] - sel.0[1]).max(0.0)],
        nodes: out,
        images,
        warnings: scene.warnings.into_iter().collect(),
        stats,
    }
}
