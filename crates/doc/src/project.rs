//! Project folder layout:
//!
//! ```text
//! <project>/project.json          Project { id, name, canvases: [{ id, name }] }
//! <project>/canvases/<id>.loro    Loro snapshot (full history) of one canvas
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Canvas, Error, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub canvases: Vec<CanvasEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasEntry {
    pub id: String,
    pub name: String,
}

/// Write via temp file + rename so a crash never leaves a truncated file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn canvas_path(dir: &Path, id: &str) -> Result<PathBuf> {
    let safe = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !safe {
        return Err(Error::Invalid(format!("bad canvas id `{id}`")));
    }
    Ok(dir.join("canvases").join(format!("{id}.loro")))
}

impl Project {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            canvases: vec![],
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&fs::read(
            dir.join("project.json"),
        )?)?)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join("project.json"), &serde_json::to_vec_pretty(self)?)
    }

    /// Snapshot `canvas` to `canvases/<id>.loro` and record it in `project.json`.
    pub fn save_canvas(&mut self, dir: &Path, canvas: &Canvas) -> Result<()> {
        let id = canvas.id();
        write_atomic(&canvas_path(dir, &id)?, &canvas.export_snapshot()?)?;
        match self.canvases.iter_mut().find(|c| c.id == id) {
            Some(e) => e.name = canvas.name(),
            None => self.canvases.push(CanvasEntry {
                id,
                name: canvas.name(),
            }),
        }
        self.save(dir)
    }

    pub fn open_canvas(&self, dir: &Path, id: &str) -> Result<Canvas> {
        Canvas::from_snapshot(&fs::read(canvas_path(dir, id)?)?)
    }
}
