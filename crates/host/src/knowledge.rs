//! Small reference docs agents load on demand instead of giant tool descriptions.

use anyhow::{Result, bail};

const DOCS: &[(&str, &str)] = &[
    ("format", include_str!("../knowledge/format.md")),
    ("annotations", include_str!("../knowledge/annotations.md")),
    ("generation", include_str!("../knowledge/generation.md")),
    (
        "collaboration",
        include_str!("../knowledge/collaboration.md"),
    ),
    ("assets", include_str!("../knowledge/assets.md")),
    ("export", include_str!("../knowledge/export.md")),
    ("apps", include_str!("../knowledge/apps.md")),
];

pub const KEYS: &[&str] = &["format", "annotations", "generation", "collaboration", "assets", "export", "apps"];

pub fn get(keys: &[String]) -> Result<String> {
    let mut out = vec![];
    for k in keys {
        match DOCS.iter().find(|(key, _)| key == k) {
            Some((_, doc)) => out.push(*doc),
            None => bail!(
                "unknown knowledge key `{k}`; available: {}",
                KEYS.join(", ")
            ),
        }
    }
    Ok(out.join("\n---\n\n"))
}
