//! Figma paste importer: decodes the scene Figma puts on the clipboard (⌘C in Figma) and
//! turns it into a fal-os [`Plan`]: frames, groups, text, image nodes (by Figma image SHA-1,
//! with signed download URLs from the clipboard metadata) and rasters (draw ops the client
//! renders) with exact sizes and parent-relative positions. No Figma API token involved.
//!
//! - [`kiwi`]: generic decoder for the kiwi binary format (schema shipped in the payload).
//! - [`clip`]: the HTML payload and fig archive (deflate / zstd chunks).
//! - [`geom`]: path blobs, vector networks, shapes, transforms.
//! - [`plan`]: instance expansion and the mapping to fal-os nodes.

pub mod clip;
pub mod geom;
pub mod kiwi;
pub mod plan;
#[cfg(test)]
mod synthetic;

pub use clip::{Clip, decode_html, is_figma_html};
pub use kiwi::{Error, Result, Value};
pub use plan::{Kind, Op, Paint, Plan, PlanNode, plan};

/// Decodes Figma clipboard HTML straight to a plan.
pub fn plan_html(html: &str) -> Result<Plan> {
    Ok(plan(&decode_html(html)?))
}
