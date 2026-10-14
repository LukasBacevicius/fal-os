//! Project folder layout:
//!
//! ```text
//! <project>/project.json          Project { format, id, name, canvases: [{ id, name }] }
//! <project>/canvases/<id>.loro    Loro snapshot (full history) of one canvas
//! ```
//!
//! Every write is durable: temp file → fsync (F_FULLFSYNC on macOS) → rename → fsync of the
//! folder, so a crash or power loss leaves either the old or the new file, never a torn one.
//! `format` ([`FORMAT`]) versions the on-disk layout; the host gates and migrates on it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Canvas, Error, Result};

/// On-disk format of `project.json` and of a canvas's root `meta.format`. Bump it when a
/// change needs a migration; files with a higher number come from a newer fal OS.
pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// [`FORMAT`] of the file (0 = written before formats existed).
    #[serde(default)]
    pub format: u32,
    pub id: String,
    pub name: String,
    pub canvases: Vec<CanvasEntry>,
    /// Other fields (e.g. the host's `fal_project`, `touched`), kept as they are.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasEntry {
    pub id: String,
    pub name: String,
}

/// Flush a file's data to stable storage. On macOS `fsync` only reaches the drive's cache;
/// `F_FULLFSYNC` asks the drive to flush it (falls back to `fsync` where unsupported).
pub fn sync_file(f: &fs::File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid open fd; F_FULLFSYNC takes no argument.
        if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    f.sync_all()
}

/// fsync a folder, so a rename inside it is durable.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Durable atomic write: a uniquely named temp file next to `path` → fsync → rename over
/// `path` → fsync the folder. A crash leaves the old file or the new one, never a torn one;
/// a failure leaves the old file as it was (the temp file is removed).
pub fn write_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let r = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        sync_file(&f)?;
        drop(f);
        fs::rename(&tmp, path)?;
        sync_dir(dir)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(r?)
}

/// `<dir>/canvases/<id>.loro` (ids are checked: no path tricks).
pub fn canvas_path(dir: &Path, id: &str) -> Result<PathBuf> {
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
            format: FORMAT,
            id: id.into(),
            name: name.into(),
            canvases: vec![],
            extra: Default::default(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&fs::read(
            dir.join("project.json"),
        )?)?)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_durable(&dir.join("project.json"), &serde_json::to_vec_pretty(self)?)
    }

    /// Snapshot `canvas` to `canvases/<id>.loro` and record it in `project.json`.
    pub fn save_canvas(&mut self, dir: &Path, canvas: &Canvas) -> Result<()> {
        let id = canvas.id();
        write_durable(&canvas_path(dir, &id)?, &canvas.export_snapshot()?)?;
        self.record_canvas(dir, canvas)
    }

    /// Record `canvas` (id, name) in `project.json`; writes it only when that changed it.
    pub fn record_canvas(&mut self, dir: &Path, canvas: &Canvas) -> Result<()> {
        let id = canvas.id();
        let before = self.canvases.clone();
        match self.canvases.iter_mut().find(|c| c.id == id) {
            Some(e) => e.name = canvas.name(),
            None => self.canvases.push(CanvasEntry {
                id,
                name: canvas.name(),
            }),
        }
        if self.canvases != before || !dir.join("project.json").is_file() {
            self.save(dir)?;
        }
        Ok(())
    }

    pub fn open_canvas(&self, dir: &Path, id: &str) -> Result<Canvas> {
        Canvas::from_snapshot(&fs::read(canvas_path(dir, id)?)?)
    }
}
