//! fal-os document model: canvases as Loro documents, actor-tagged transactions with
//! per-actor undo, first-class annotations, and an LLM-friendly text projection
//! (see `FORMAT.md`).

mod canvas;
mod model;
mod project;
mod provenance;
mod text;

pub use canvas::{
    ActorChanges, Canvas, Command, EditResult, Props, StaleReport, TxResult, Version,
};
pub use model::*;
pub use project::{CanvasEntry, Project};
pub use provenance::*;
pub use text::{ReadOpts, anchor_to_text, parse_anchor_text};

pub use loro;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown node `{0}`")]
    UnknownNode(String),
    #[error("unknown annotation `{0}`")]
    UnknownAnnotation(String),
    #[error("unknown version `{0}`")]
    UnknownVersion(String),
    #[error("{0}")]
    Invalid(String),
    #[error("line {line}: {source}")]
    Line { line: usize, source: Box<Error> },
    #[error(transparent)]
    Loro(#[from] loro::LoroError),
    #[error(transparent)]
    Encode(#[from] loro::LoroEncodeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
