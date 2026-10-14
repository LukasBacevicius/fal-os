//! Crash-safe persistence helpers for a project folder (see "Reliability" in
//! `docs/ARCHITECTURE.md`):
//!
//! - every write is durable (`falos_doc::write_durable`: temp → fsync → rename → fsync dir);
//! - a small ring of last-good copies per canvas, `canvases/.bak/<id>.<ms>.loro`: the newest
//!   slot follows the latest good save, a new slot starts every [`BACKUP_EVERY`], and the
//!   newest [`BACKUPS_KEPT`] are kept ([`track_save`]);
//! - originals are archived to `.archive/` before a format migration rewrites them;
//! - a `project.json` that doesn't parse is rebuilt from `canvases/*.loro` (the original is
//!   kept next to it as `project.json.broken-<ms>`);
//! - a save whose node count collapses without matching deletes is refused
//!   ([`collapse_refused`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use falos_doc::{Canvas, CanvasEntry, Project};

use crate::host::now_ms;

/// Last-good copies kept per canvas.
pub const BACKUPS_KEPT: usize = 5;
/// At most one backup per canvas this often (a ring rotated by time, not by save).
pub const BACKUP_EVERY: Duration = Duration::from_secs(10 * 60);
/// Canvases smaller than this never trip the collapse guard.
pub const COLLAPSE_MIN_NODES: usize = 10;

/// `FALOS_BACKUP_EVERY_SECS` overrides [`BACKUP_EVERY`] (tests, manual checks).
pub fn backup_every() -> Duration {
    std::env::var("FALOS_BACKUP_EVERY_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(BACKUP_EVERY)
}

pub fn bak_dir(dir: &Path) -> PathBuf {
    dir.join("canvases").join(".bak")
}

/// Backups of canvas `id`, newest first: `(ms, path)`.
pub fn backups(dir: &Path, id: &str) -> Vec<(i64, PathBuf)> {
    let prefix = format!("{id}.");
    let mut v: Vec<(i64, PathBuf)> = std::fs::read_dir(bak_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let ms = name.strip_prefix(&prefix)?.strip_suffix(".loro")?.parse().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.0));
    v
}

/// Write `bytes` as a new backup of canvas `id` and prune the ring to [`BACKUPS_KEPT`].
pub fn write_backup(dir: &Path, id: &str, bytes: &[u8]) -> Result<PathBuf> {
    let mut ms = now_ms();
    // Never two backups with one name (two saves within a millisecond).
    while backups(dir, id).iter().any(|(m, _)| *m >= ms) {
        ms += 1;
    }
    let path = bak_dir(dir).join(format!("{id}.{ms}.loro"));
    falos_doc::write_durable(&path, bytes)?;
    for (_, old) in backups(dir, id).into_iter().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(old);
    }
    Ok(path)
}

/// Copy the canvas file on disk (a good one: it loaded, or we wrote it) into the backup ring
/// when the newest backup is older than `every`. Returns whether a backup was written.
pub fn maybe_backup(dir: &Path, id: &str, every: Duration) -> Result<bool> {
    let newest = backups(dir, id).first().map(|(ms, _)| *ms);
    if newest.is_some_and(|ms| now_ms() - ms < every.as_millis() as i64) {
        return Ok(false);
    }
    let file = falos_doc::canvas_path(dir, id)?;
    let Ok(bytes) = std::fs::read(&file) else { return Ok(false) };
    write_backup(dir, id, &bytes)?;
    Ok(true)
}

/// After a good save: the ring's newest slot follows the latest save, and a new slot starts
/// when the newest one is older than `every`. So a restore gets the last good state, and the
/// older slots are at least `every` apart (a bad state saved over and over can't push out
/// every good one).
pub fn track_save(dir: &Path, id: &str, bytes: &[u8], every: Duration) -> Result<()> {
    match backups(dir, id).first() {
        Some((ms, path)) if now_ms() - ms < every.as_millis() as i64 => falos_doc::write_durable(path, bytes)?,
        _ => {
            write_backup(dir, id, bytes)?;
        }
    }
    Ok(())
}

/// Keep a copy of `file` in `<dir>/.archive/<name>.v<format>.<ms>` before a migration
/// rewrites it.
pub fn archive(dir: &Path, file: &Path, format: u32) -> Result<PathBuf> {
    let name = file.file_name().context("no file name")?.to_string_lossy().into_owned();
    let to = dir.join(".archive").join(format!("{name}.v{format}.{}", now_ms()));
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    falos_doc::write_durable(&to, &bytes)?;
    Ok(to)
}

/// Move a file aside as `<file>.broken-<ms>` (its bytes are never changed).
pub fn set_aside(file: &Path) -> Result<PathBuf> {
    let to = PathBuf::from(format!("{}.broken-{}", file.display(), now_ms()));
    std::fs::rename(file, &to).with_context(|| format!("moving {} aside", file.display()))?;
    if let Some(d) = file.parent() {
        let _ = falos_doc::sync_dir(d);
    }
    Ok(to)
}

/// A `project.json` rebuilt from the canvas files: every `canvases/<id>.loro`, named from the
/// canvas's own metadata when it loads (the id otherwise).
pub fn rebuild_project(dir: &Path, id: &str, name: &str) -> Project {
    let mut p = Project::new(id, name);
    let mut entries: Vec<CanvasEntry> = std::fs::read_dir(dir.join("canvases"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_str()?.to_string();
            let cid = file.strip_suffix(".loro")?.to_string();
            falos_doc::canvas_path(dir, &cid).ok()?;
            let name = std::fs::read(e.path())
                .ok()
                .and_then(|b| Canvas::from_snapshot(&b).ok())
                .map(|c| c.name())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| cid.clone());
            Some(CanvasEntry { id: cid, name })
        })
        .collect();
    // `main` first (the project's default canvas), then by id.
    entries.sort_by(|a, b| (a.id != "main", &a.id).cmp(&(b.id != "main", &b.id)));
    p.canvases = entries;
    p
}

/// Whether a save of a canvas that had `saved` nodes on disk and now has `now` must be
/// refused: more than 80 % of the nodes vanished, and deletes recorded since the last save
/// (`deleted`) explain less than half of the loss.
pub fn collapse_refused(saved: usize, now: usize, deleted: usize) -> bool {
    if saved < COLLAPSE_MIN_NODES || now * 5 >= saved {
        return false;
    }
    let lost = saved - now;
    deleted * 2 < lost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_rule() {
        assert!(!collapse_refused(5, 0, 0), "small canvases never trip it");
        assert!(!collapse_refused(100, 30, 0), "70 % gone is allowed");
        assert!(collapse_refused(100, 10, 0), "90 % gone without deletes");
        assert!(!collapse_refused(100, 10, 90), "explained by deletes");
        assert!(collapse_refused(100, 0, 20), "deletes explain too little");
    }
}
