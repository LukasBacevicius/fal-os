//! Provenance: what's an input, what's generated (`docs/ASSETS-V1.md` §1).
//!
//! Two orthogonal fields: [`Asset::source`] (immutable: how the bytes came to exist) and the
//! node's `role` prop (mutable: what the human means by it). Old records (`parents: [sha]`,
//! `model`) deserialize into the new shape.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::model::{Node, PropType};

/// What the human means by a node (mutable, `role(...)` in the text format).
pub const ROLES: &[&str] = &["input", "candidate", "wip", "final", "rejected"];

/// Props every node kind may carry, in print order: intent (`role`), agent protection
/// (`protected`) and saved export settings (`export`: `[{preset|format, scale, suffix}]`).
pub const COMMON_PROPS: &[(&str, PropType)] = &[
    ("role", PropType::Enum(ROLES)),
    ("protected", PropType::Bool),
    ("export", PropType::Json),
];

impl Node {
    /// `asset` prop, if any.
    pub fn asset_id(&self) -> Option<&str> {
        self.props.get("asset").and_then(Json::as_str)
    }
    /// `role` prop (`input|candidate|wip|final|rejected`), if set.
    pub fn role(&self) -> Option<&str> {
        self.props.get("role").and_then(Json::as_str)
    }
    /// Agents can't delete, overwrite or move protected nodes (imports default to protected).
    pub fn is_protected(&self) -> bool {
        self.props.get("protected").and_then(Json::as_bool) == Some(true)
    }
}

/// How an asset's bytes came to exist. Immutable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A user file, paste, Figma, a fal library upload.
    #[default]
    Import,
    /// A fal model or the fal Agent.
    Generate,
    /// A local or fal op applied to other assets (crop, trim, upscale, …).
    Derive,
    /// Remotion, HyperFrames, a frame render, compose.
    Render,
    /// A screen / camera capture.
    Capture,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Import => "import",
            Source::Generate => "generate",
            Source::Derive => "derive",
            Source::Render => "render",
            Source::Capture => "capture",
        }
    }
    /// Short form used by the text projection: `src(import|gen|derive|render|capture)`.
    pub fn short(self) -> &'static str {
        match self {
            Source::Generate => "gen",
            s => s.as_str(),
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "import" => Source::Import,
            "generate" | "gen" => Source::Generate,
            "derive" => Source::Derive,
            "render" => Source::Render,
            "capture" => Source::Capture,
            _ => return None,
        })
    }
}

/// Where an imported asset came from.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Origin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub filename: String,
    /// The app it came from (`figma`, `finder`, `premiere`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Actor id.
    #[serde(default)]
    pub imported_by: String,
    /// Unix millis.
    #[serde(default)]
    pub at: i64,
}

/// Probed media facts.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Media {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<bool>,
    /// Color space / transfer, e.g. `bt709`, `srgb`, `bt2020/arib-std-b67`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// What a parent asset was used as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentRole {
    #[default]
    Input,
    Reference,
    Mask,
    Style,
    Audio,
    StartFrame,
    EndFrame,
}

impl ParentRole {
    pub const ALL: [ParentRole; 7] = [
        ParentRole::Input,
        ParentRole::Reference,
        ParentRole::Mask,
        ParentRole::Style,
        ParentRole::Audio,
        ParentRole::StartFrame,
        ParentRole::EndFrame,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            ParentRole::Input => "input",
            ParentRole::Reference => "reference",
            ParentRole::Mask => "mask",
            ParentRole::Style => "style",
            ParentRole::Audio => "audio",
            ParentRole::StartFrame => "start_frame",
            ParentRole::EndFrame => "end_frame",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// One input of a derivation / generation.
#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct Parent {
    /// Asset id (`sha256:…`).
    pub asset: String,
    /// The canvas node it was taken from, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub role: ParentRole,
}

impl Parent {
    pub fn new(asset: &str, node: Option<&str>, role: ParentRole) -> Self {
        Self {
            asset: asset.into(),
            node: node.map(String::from),
            role,
        }
    }
    pub fn input(asset: &str, node: Option<&str>) -> Self {
        Self::new(asset, node, ParentRole::Input)
    }
}

/// Old records stored parents as bare asset ids; new ones as objects. Unknown roles read as
/// `input` so one odd record never hides the whole asset.
impl<'de> Deserialize<'de> for Parent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Id(String),
            Full {
                asset: String,
                #[serde(default)]
                node: Option<String>,
                #[serde(default)]
                role: Option<String>,
            },
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Id(asset) => Parent::input(&asset, None),
            Raw::Full { asset, node, role } => Parent {
                asset,
                node,
                role: role
                    .as_deref()
                    .and_then(ParentRole::parse)
                    .unwrap_or_default(),
            },
        })
    }
}

/// What kind of process produced an asset (mirrors [`Source`] for non-imports).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    #[default]
    Generate,
    Derive,
    Render,
    /// Old `import_file` records (`model: "import"`).
    Import,
}

impl LineageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LineageKind::Generate => "generate",
            LineageKind::Derive => "derive",
            LineageKind::Render => "render",
            LineageKind::Import => "import",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "generate" | "gen" => LineageKind::Generate,
            "derive" => LineageKind::Derive,
            "render" => LineageKind::Render,
            "import" => LineageKind::Import,
            _ => return None,
        })
    }
    pub fn source(self) -> Source {
        match self {
            LineageKind::Generate => Source::Generate,
            LineageKind::Derive => Source::Derive,
            LineageKind::Render => Source::Render,
            LineageKind::Import => Source::Import,
        }
    }
}

/// How an asset was made: enough to re-run it.
#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct Lineage {
    pub kind: LineageKind,
    /// The op (`crop`, `reframe`, `ffmpeg`, …) or the fal endpoint (`fal-ai/flux/dev`).
    pub op: String,
    /// Canonical args (op args / the model's input), enough to re-run.
    pub recipe: Json,
    pub parents: Vec<Parent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fal_request_id: Option<String>,
    /// USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Actor id that triggered it.
    pub actor: String,
    /// Re-running gives the same bytes (local ops); fal models are not.
    pub deterministic: bool,
    /// Engine + version, e.g. `ffmpeg 8.0`, `falos-image 0.1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
}

impl<'de> Deserialize<'de> for Lineage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            kind: Option<String>,
            #[serde(default)]
            op: Option<String>,
            /// Old shape: the fal endpoint (or `import`).
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            recipe: Option<Json>,
            /// Old shape: the model input.
            #[serde(default)]
            params: Option<Json>,
            #[serde(default)]
            parents: Vec<Parent>,
            #[serde(default)]
            seed: Option<i64>,
            #[serde(default)]
            fal_request_id: Option<String>,
            #[serde(default)]
            cost: Option<f64>,
            #[serde(default)]
            actor: String,
            #[serde(default)]
            deterministic: Option<bool>,
            #[serde(default)]
            engine_version: Option<String>,
        }
        let r = Raw::deserialize(d)?;
        let op = r.op.or(r.model).unwrap_or_default();
        let kind = r
            .kind
            .as_deref()
            .and_then(LineageKind::parse)
            .unwrap_or(if op == "import" {
                LineageKind::Import
            } else {
                LineageKind::Generate
            });
        Ok(Lineage {
            kind,
            op,
            recipe: r.recipe.or(r.params).unwrap_or(Json::Null),
            parents: r.parents,
            seed: r.seed,
            fal_request_id: r.fal_request_id,
            cost: r.cost,
            actor: r.actor,
            deterministic: r.deterministic.unwrap_or(false),
            engine_version: r.engine_version,
        })
    }
}

impl Lineage {
    /// A generation by a fal model.
    pub fn generate(model: &str, input: Json, actor: &str) -> Self {
        Self {
            kind: LineageKind::Generate,
            op: model.into(),
            recipe: input,
            actor: actor.into(),
            ..Default::default()
        }
    }

    /// Short args of the recipe for summaries: scalar values of `recipe.args` (or of the
    /// recipe itself), e.g. `{9:16,pad-blur}`.
    pub fn recipe_brief(&self) -> String {
        let src = self.recipe.get("args").unwrap_or(&self.recipe);
        let Json::Object(m) = src else {
            return String::new();
        };
        let mut keys: Vec<&String> = m.keys().collect();
        if self.recipe.get("args").is_none() {
            // A model input: keep it short and stable.
            keys.sort();
        }
        let vals: Vec<String> = keys
            .into_iter()
            .filter(|k| !matches!(k.as_str(), "prompt" | "negative_prompt" | "inputs" | "chip"))
            .filter_map(|k| match &m[k] {
                Json::String(s)
                    if s.chars().count() <= 24
                        && !s.starts_with("http")
                        && !s.contains(char::is_whitespace) =>
                {
                    Some(s.clone())
                }
                Json::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .take(4)
            .collect();
        if vals.is_empty() {
            String::new()
        } else {
            format!("{{{}}}", vals.join(","))
        }
    }

    /// One-line summary, e.g. `← op:reframe{9:16,pad-blur} of 881s9r by @motion`;
    /// `label` names a parent (its node id, else a short asset id).
    pub fn summary(&self, label: impl Fn(&Parent) -> String) -> String {
        let head = match self.kind {
            LineageKind::Generate => format!("gen:{}", self.op),
            LineageKind::Derive => format!("op:{}", self.op),
            LineageKind::Render => format!("render:{}", self.op),
            LineageKind::Import => "import".to_string(),
        };
        let mut s = format!("← {head}{}", self.recipe_brief());
        if !self.parents.is_empty() {
            let ps: Vec<String> = self
                .parents
                .iter()
                .take(4)
                .map(|p| match p.role {
                    ParentRole::Input => label(p),
                    r => format!("{}:{}", r.as_str(), label(p)),
                })
                .collect();
            s.push_str(" of ");
            s.push_str(&ps.join(","));
            if self.parents.len() > 4 {
                s.push_str(&format!("+{}", self.parents.len() - 4));
            }
        }
        if !self.actor.is_empty() {
            s.push_str(&format!(" by @{}", self.actor));
        }
        s
    }
}

/// Content-addressed asset record with provenance.
#[derive(Clone, Debug, PartialEq, Default, Serialize)]
pub struct Asset {
    /// Content hash, e.g. `sha256:9f86d0…`.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// How the bytes came to exist (immutable).
    pub source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Media>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Lineage>,
}

/// Records without `source` predate provenance: a lineage means a generation (or an import
/// when its model was `import`), no lineage means an import (upload, paste).
impl<'de> Deserialize<'de> for Asset {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            id: String,
            #[serde(default)]
            mime: Option<String>,
            #[serde(default)]
            uri: Option<String>,
            #[serde(default)]
            source: Option<String>,
            #[serde(default)]
            origin: Option<Origin>,
            #[serde(default)]
            media: Option<Media>,
            #[serde(default)]
            lineage: Option<Lineage>,
        }
        let r = Raw::deserialize(d)?;
        let source = r
            .source
            .as_deref()
            .and_then(Source::parse)
            .unwrap_or_else(|| {
                r.lineage
                    .as_ref()
                    .map_or(Source::Import, |l| l.kind.source())
            });
        Ok(Asset {
            id: r.id,
            mime: r.mime,
            uri: r.uri,
            source,
            origin: r.origin,
            media: r.media,
            lineage: r.lineage,
        })
    }
}

impl Asset {
    /// Short form of an asset id (6 hex chars, no `sha256:`), used in summaries.
    pub fn short_id(id: &str) -> String {
        id.strip_prefix("sha256:")
            .unwrap_or(id)
            .chars()
            .take(6)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_records_migrate() {
        let a: Asset = serde_json::from_value(json!({
            "id": "sha256:aa", "mime": "image/png",
            "lineage": {"model": "fal-ai/flux/dev", "params": {"prompt": "x", "steps": 4},
                        "parents": ["sha256:bb"], "seed": 3, "actor": "claude"}
        }))
        .unwrap();
        assert_eq!(a.source, Source::Generate);
        let l = a.lineage.unwrap();
        assert_eq!((l.kind, l.op.as_str()), (LineageKind::Generate, "fal-ai/flux/dev"));
        assert_eq!(l.recipe["steps"], 4);
        assert_eq!(l.parents, vec![Parent::input("sha256:bb", None)]);
        let imp: Asset = serde_json::from_value(json!({
            "id": "sha256:cc", "lineage": {"model": "import", "params": {}, "parents": [], "actor": "x"}
        }))
        .unwrap();
        assert_eq!(imp.source, Source::Import);
        let bare: Asset = serde_json::from_value(json!({"id": "sha256:dd"})).unwrap();
        assert_eq!(bare.source, Source::Import);
        // New shape round-trips; unknown parent roles read as input.
        let new: Asset = serde_json::from_value(json!({
            "id": "sha256:ee", "source": "derive",
            "lineage": {"kind": "derive", "op": "crop", "recipe": {"args": {"aspect": "9:16"}},
                        "parents": [{"asset": "sha256:ff", "node": "n1", "role": "mask"},
                                    {"asset": "sha256:00", "role": "weird"}],
                        "actor": "motion", "deterministic": true}
        }))
        .unwrap();
        assert_eq!(new.source, Source::Derive);
        let l = new.lineage.clone().unwrap();
        assert_eq!(l.parents[0].role, ParentRole::Mask);
        assert_eq!(l.parents[1].role, ParentRole::Input);
        let back: Asset = serde_json::from_value(serde_json::to_value(&new).unwrap()).unwrap();
        assert_eq!(back.lineage.unwrap().parents[1].role, ParentRole::Input);
        assert_eq!(
            l.summary(|p| p.node.clone().unwrap_or_else(|| Asset::short_id(&p.asset))),
            "← op:crop{9:16} of mask:n1,00 by @motion"
        );
    }
}
