//! Plain data types: actors, node kinds + prop schema, annotations, assets.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::Error;

/// Who performed a mutation. Every transaction is tagged with one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// Stable handle, rendered as `@id` (e.g. `claude`, `codex`, `lukas`).
    pub id: String,
    pub name: String,
    pub kind: ActorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
}

impl Actor {
    pub fn human(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: ActorKind::Human,
        }
    }
    pub fn agent(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind: ActorKind::Agent,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StyleKind {
    Palette,
    Lut,
    Font,
    Character,
    StyleRef,
    Lora,
}

/// Every canvas node type. `Style` covers palette/lut/font/character/style_ref/lora.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Image,
    Video,
    Audio,
    Text,
    Model3d,
    Vector,
    Document,
    Embed,
    /// A live web app (a folder of plain web files in `<project>/apps/<app>/`).
    App,
    Timeline,
    Stream,
    Generation,
    Style(StyleKind),
    Frame,
    Group,
    Note,
    Connector,
}

const STYLES: [(StyleKind, &str); 6] = [
    (StyleKind::Palette, "palette"),
    (StyleKind::Lut, "lut"),
    (StyleKind::Font, "font"),
    (StyleKind::Character, "character"),
    (StyleKind::StyleRef, "style_ref"),
    (StyleKind::Lora, "lora"),
];

const KINDS: [(NodeKind, &str); 16] = [
    (NodeKind::Image, "image"),
    (NodeKind::Video, "video"),
    (NodeKind::Audio, "audio"),
    (NodeKind::Text, "text"),
    (NodeKind::Model3d, "model3d"),
    (NodeKind::Vector, "vector"),
    (NodeKind::Document, "document"),
    (NodeKind::Embed, "embed"),
    (NodeKind::App, "app"),
    (NodeKind::Timeline, "timeline"),
    (NodeKind::Stream, "stream"),
    (NodeKind::Generation, "generation"),
    (NodeKind::Frame, "frame"),
    (NodeKind::Group, "group"),
    (NodeKind::Note, "note"),
    (NodeKind::Connector, "connector"),
];

/// Value shape of a type-specific prop. Used to validate and coerce input.
#[derive(Clone, Copy, Debug)]
pub enum PropType {
    Num,
    Bool,
    Str,
    Enum(&'static [&'static str]),
    NumList(usize),
    StrList,
    Json,
}

use PropType::*;

pub const GEN_STATUS: &[&str] = &["queued", "running", "done", "failed", "canceled"];
pub const CONNECTOR_KINDS: &[&str] = &["lineage", "reference", "flow"];
pub const TEXT_ALIGN: &[&str] = &["left", "center", "right", "justify"];
pub const TEXT_VALIGN: &[&str] = &["top", "center", "bottom"];

/// Text: font size, and the "styled" look (a `color` turns the card into bare text in the
/// given font, e.g. text imported from Figma).
const TEXT: &[(&str, PropType)] = &[
    ("fontSize", Num),
    ("color", Str),
    ("font", Str),
    ("weight", Num),
    ("italic", Bool),
    ("lineHeight", Num),
    ("letterSpacing", Num),
    ("align", Enum(TEXT_ALIGN)),
    ("valign", Enum(TEXT_VALIGN)),
    ("nowrap", Bool),
];

const VIDEO: &[(&str, PropType)] = &[
    ("asset", Str),
    ("crop", NumList(4)),
    ("in", Num),
    ("out", Num),
    ("loop", Bool),
    ("muted", Bool),
];
const AUDIO: &[(&str, PropType)] = &[
    ("asset", Str),
    ("in", Num),
    ("out", Num),
    ("loop", Bool),
    ("muted", Bool),
];

impl NodeKind {
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(sub) = s.strip_prefix("style:") {
            return STYLES
                .iter()
                .find(|(_, n)| *n == sub)
                .map(|(k, _)| NodeKind::Style(*k));
        }
        KINDS.iter().find(|(_, n)| *n == s).map(|(k, _)| *k)
    }

    /// Whether the node carries collaborative rich text (`text(...)`).
    pub fn has_text(self) -> bool {
        matches!(self, NodeKind::Text | NodeKind::Note)
    }

    /// Type-specific props in canonical (print) order.
    pub fn props(self) -> &'static [(&'static str, PropType)] {
        match self {
            NodeKind::Image => &[("asset", Str), ("crop", NumList(4)), ("radius", Num), ("transparent", Bool), ("error", Str)],
            NodeKind::Video => VIDEO,
            NodeKind::Audio => AUDIO,
            NodeKind::Model3d | NodeKind::Vector => &[("asset", Str)],
            NodeKind::Document => &[("asset", Str), ("page", Num)],
            NodeKind::Embed => &[("url", Str), ("asset", Str)],
            // `app`: folder id under `<project>/apps/`; `version`: the file snapshot the node
            // shows (reverting it restores the files); `asset`: a still of the running app;
            // `chat`: the chat that wrote it (provenance).
            NodeKind::App => &[("app", Str), ("entry", Str), ("version", Num), ("asset", Str), ("chat", Str)],
            NodeKind::Timeline => &[("duration", Num)],
            NodeKind::Stream => &[("url", Str), ("model", Str), ("muted", Bool)],
            NodeKind::Generation => &[
                ("status", Enum(GEN_STATUS)),
                ("model", Str),
                ("job", Str),
                ("progress", Num),
                ("params", Json),
                ("asset", Str),
                ("cost", Num),
                ("error", Str),
            ],
            NodeKind::Style(_) => &[("asset", Str), ("data", Json)],
            NodeKind::Note => &[("color", Str), ("fontSize", Num)],
            NodeKind::Text => TEXT,
            NodeKind::Frame => &[("fill", Str), ("radius", Num)],
            NodeKind::Connector => &[
                ("from", Str),
                ("to", Str),
                ("kind", Enum(CONNECTOR_KINDS)),
                ("label", Str),
            ],
            NodeKind::Group => &[],
        }
    }

    pub fn prop_type(self, key: &str) -> Option<PropType> {
        self.props()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, t)| *t)
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let NodeKind::Style(sub) = self {
            let name = STYLES.iter().find(|(k, _)| k == sub).unwrap().1;
            return write!(f, "style:{name}");
        }
        f.write_str(KINDS.iter().find(|(k, _)| k == self).unwrap().1)
    }
}

impl PropType {
    /// Validate `v` for this type, coercing the forgiving cases (scalar → 1-elem list,
    /// number → string).
    pub fn coerce(self, key: &str, v: Json) -> Result<Json, Error> {
        let bad = |want: &str| Error::Invalid(format!("prop `{key}` expects {want}, got {v}"));
        Ok(match (self, &v) {
            (Json, _) => v,
            (Num, Json::Number(_)) | (Bool, Json::Bool(_)) | (Str, Json::String(_)) => v,
            (Str, Json::Number(n)) => Json::String(n.to_string()),
            (Enum(opts), Json::String(s)) if opts.contains(&s.as_str()) => v,
            (Enum(opts), _) => return Err(bad(&opts.join("|"))),
            (NumList(n), Json::Array(a)) if a.len() == n && a.iter().all(Json::is_number) => v,
            (NumList(n), _) => return Err(bad(&format!("{n} numbers"))),
            (StrList, Json::String(_)) => Json::Array(vec![v]),
            (StrList, Json::Array(a)) if a.iter().all(Json::is_string) => v,
            (Num, _) => return Err(bad("a number")),
            (Bool, _) => return Err(bad("true|false")),
            (Str, _) => return Err(bad("a string")),
            (StrList, _) => return Err(bad("a list of strings")),
        })
    }
}

/// Read-only snapshot of a canvas node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub rot: f64,
    pub locked: bool,
    pub created_by: String,
    pub updated_by: String,
    pub parent: Option<String>,
    pub children: Vec<String>,
    /// Content of the node's LoroText (text / note nodes).
    pub text: Option<String>,
    /// Type-specific and unknown props.
    pub props: BTreeMap<String, Json>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Resolved,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Resolved => "resolved",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Status::Open, Status::InProgress, Status::Resolved]
            .into_iter()
            .find(|x| x.as_str() == s)
    }
}

/// A 2D region inside a frame or page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Region {
    Point { x: f64, y: f64 },
    Box { x: f64, y: f64, w: f64, h: f64 },
    Polygon { points: Vec<[f64; 2]> },
    Mask { asset: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub pos: [f64; 3],
    pub target: [f64; 3],
    pub fov: f64,
}

/// Where on a node an annotation points. 2D coordinates are node-local and normalized
/// (0..1 of the node's width/height),
/// times are seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    Point { x: f64, y: f64 },
    Box { x: f64, y: f64, w: f64, h: f64 },
    Polygon { points: Vec<[f64; 2]> },
    Mask { asset: String },
    Time { t: f64 },
    Range { t0: f64, t1: f64 },
    Frame { t: f64, region: Option<Region> },
    TextRange { start: u32, end: u32 },
    Page { n: u32, region: Option<Region> },
    Point3d { p: [f64; 3], camera: Camera },
}

impl Region {
    /// The 2D coordinates the region spans (empty for masks).
    pub fn coords(&self) -> Vec<f64> {
        match self {
            Region::Point { x, y } => vec![*x, *y],
            Region::Box { x, y, w, h } => vec![*x, *y, *x + *w, *y + *h],
            Region::Polygon { points } => points.concat(),
            Region::Mask { .. } => vec![],
        }
    }
}

impl Anchor {
    /// The 2D region of the anchor, if it has one (`frame(t) box(..)` → the box).
    pub fn region(&self) -> Option<Region> {
        match self {
            Anchor::Point { x, y } => Some(Region::Point { x: *x, y: *y }),
            Anchor::Box { x, y, w, h } => Some(Region::Box {
                x: *x,
                y: *y,
                w: *w,
                h: *h,
            }),
            Anchor::Polygon { points } => Some(Region::Polygon {
                points: points.clone(),
            }),
            Anchor::Mask { asset } => Some(Region::Mask {
                asset: asset.clone(),
            }),
            Anchor::Frame { region, .. } | Anchor::Page { region, .. } => region.clone(),
            _ => None,
        }
    }

    /// True when every 2D coordinate is node-normalized (0..1, with a little slack). Pixel
    /// coordinates such as `box(10,20,200,100)` are the common mistake this catches.
    pub fn is_normalized(&self) -> bool {
        self.region()
            .is_none_or(|r| r.coords().iter().all(|v| (-0.001..=1.001).contains(v)))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub author: String,
    pub text: String,
    /// Unix millis.
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: String,
    pub node_id: String,
    pub author: String,
    pub status: Status,
    pub anchor: Anchor,
    pub thread: Vec<Message>,
    pub assignee: Option<String>,
    /// Nodes produced in response.
    pub outcome: Vec<String>,
    pub created_at: i64,
}

