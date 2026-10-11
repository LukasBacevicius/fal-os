//! The library index: SQLite at `$FALOS_HOME/library.db`, one row per asset keyed by its
//! sha256 (remote-only fal assets use `fal_<assetRecordId>` until their bytes are fetched).
//! Placements (asset ↔ project/canvas/node) are separate from assets.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value as Json, json};

pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS assets(
  sha TEXT PRIMARY KEY,
  kind TEXT NOT NULL DEFAULT 'other',
  mime TEXT,
  w INTEGER, h INTEGER, duration REAL, bytes INTEGER,
  source TEXT NOT NULL DEFAULT 'import',
  origin TEXT, lineage TEXT, media TEXT, record TEXT,
  prompt TEXT, caption TEXT, model TEXT, name TEXT,
  palette TEXT, tiny BLOB,
  favorite INTEGER NOT NULL DEFAULT 0,
  local_only INTEGER NOT NULL DEFAULT 0,
  created INTEGER NOT NULL DEFAULT 0,
  updated INTEGER NOT NULL DEFAULT 0,
  path TEXT,
  fal_id TEXT, fal_url TEXT, fal_request_id TEXT,
  fal_synced INTEGER NOT NULL DEFAULT 0,
  fal_source TEXT, fal_seen INTEGER NOT NULL DEFAULT 0,
  thumb INTEGER NOT NULL DEFAULT 0,
  thumb_err TEXT
);
CREATE INDEX IF NOT EXISTS assets_created ON assets(created DESC);
CREATE UNIQUE INDEX IF NOT EXISTS assets_fal_id ON assets(fal_id) WHERE fal_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS assets_fal_req ON assets(fal_request_id);
CREATE INDEX IF NOT EXISTS assets_fal_url ON assets(fal_url);
CREATE TABLE IF NOT EXISTS tags(sha TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY(sha, tag)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS tags_tag ON tags(tag);
CREATE TABLE IF NOT EXISTS asset_projects(sha TEXT NOT NULL, project TEXT NOT NULL, PRIMARY KEY(sha, project)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS asset_projects_project ON asset_projects(project);
CREATE TABLE IF NOT EXISTS placements(
  project TEXT NOT NULL, canvas TEXT NOT NULL, node TEXT NOT NULL,
  sha TEXT NOT NULL, role TEXT, name TEXT,
  PRIMARY KEY(project, canvas, node)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS placements_sha ON placements(sha);
CREATE TABLE IF NOT EXISTS collections(id TEXT PRIMARY KEY, name TEXT NOT NULL, fal_id TEXT, parent TEXT, project TEXT);
CREATE TABLE IF NOT EXISTS collection_items(collection TEXT NOT NULL, sha TEXT NOT NULL, PRIMARY KEY(collection, sha)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS collection_items_sha ON collection_items(sha);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(prompt, caption, tags, name, model, tokenize='unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS outbox(id INTEGER PRIMARY KEY, op TEXT NOT NULL, sha TEXT, payload TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL DEFAULT 0, error TEXT, created INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
"#;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let c = Connection::open(path)?;
    init(&c)?;
    Ok(c)
}

pub fn init(c: &Connection) -> Result<()> {
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.execute_batch(SCHEMA)?;
    set_meta(c, "schema", &SCHEMA_VERSION.to_string())?;
    Ok(())
}

pub fn meta(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional().ok().flatten()
}

pub fn set_meta(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute("INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
    Ok(())
}

/// What ingest knows about an asset (from a canvas record and/or the file).
#[derive(Default, Debug, Clone)]
pub struct Ingested {
    pub sha: String,
    pub kind: String,
    pub mime: Option<String>,
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub duration: Option<f64>,
    pub bytes: Option<i64>,
    pub source: Option<String>,
    pub origin: Option<Json>,
    pub lineage: Option<Json>,
    pub media: Option<Json>,
    /// The raw canvas asset record (whatever fields the doc model has).
    pub record: Option<Json>,
    pub prompt: Option<String>,
    pub caption: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub created: Option<i64>,
    pub path: Option<String>,
    pub fal_url: Option<String>,
    pub fal_request_id: Option<String>,
}

fn s(v: &Option<Json>) -> Option<String> {
    v.as_ref().map(|j| j.to_string())
}

/// Insert or refresh an asset from local ingest. Never touches user/fal fields (favorite,
/// tags, local_only, fal_id, …); fills metadata that is missing and refreshes provenance.
/// Returns true when the row is new or changed.
pub fn upsert_ingested(c: &Connection, a: &Ingested, now: i64) -> Result<bool> {
    let before: Option<String> = c
        .query_row(
            "SELECT coalesce(mime,'')||'|'||coalesce(w,'')||'|'||coalesce(h,'')||'|'||source||'|'||coalesce(lineage,'')||'|'||coalesce(record,'')||'|'||coalesce(prompt,'')||'|'||coalesce(name,'')||'|'||coalesce(path,'')||'|'||coalesce(model,'') FROM assets WHERE sha=?",
            [&a.sha],
            |r| r.get(0),
        )
        .optional()?;
    c.execute(
        "INSERT INTO assets(sha,kind,mime,w,h,duration,bytes,source,origin,lineage,media,record,prompt,caption,model,name,created,updated,path,fal_url,fal_request_id)
         VALUES(?1,?2,?3,?4,?5,?6,?7,coalesce(?8,'import'),?9,?10,?11,?12,?13,?14,?15,?16,coalesce(?17,?18),?18,?19,?20,?21)
         ON CONFLICT(sha) DO UPDATE SET
           kind=CASE WHEN excluded.kind!='other' THEN excluded.kind ELSE assets.kind END,
           mime=coalesce(excluded.mime,assets.mime),
           w=coalesce(assets.w,excluded.w), h=coalesce(assets.h,excluded.h),
           duration=coalesce(assets.duration,excluded.duration),
           bytes=coalesce(excluded.bytes,assets.bytes),
           source=coalesce(?8,assets.source),
           origin=coalesce(excluded.origin,assets.origin),
           lineage=coalesce(excluded.lineage,assets.lineage),
           media=coalesce(excluded.media,assets.media),
           record=coalesce(excluded.record,assets.record),
           prompt=coalesce(excluded.prompt,assets.prompt),
           caption=coalesce(assets.caption,excluded.caption),
           model=coalesce(excluded.model,assets.model),
           name=coalesce(excluded.name,assets.name),
           created=CASE WHEN assets.created=0 THEN excluded.created ELSE min(assets.created, excluded.created) END,
           path=coalesce(excluded.path,assets.path),
           fal_url=coalesce(assets.fal_url,excluded.fal_url),
           fal_request_id=coalesce(assets.fal_request_id,excluded.fal_request_id)",
        params![
            a.sha,
            if a.kind.is_empty() { "other" } else { &a.kind },
            a.mime,
            a.w,
            a.h,
            a.duration,
            a.bytes,
            a.source,
            s(&a.origin),
            s(&a.lineage),
            s(&a.media),
            s(&a.record),
            a.prompt,
            a.caption,
            a.model,
            a.name,
            a.created,
            now,
            a.path,
            a.fal_url,
            a.fal_request_id,
        ],
    )?;
    let after: Option<String> = c
        .query_row(
            "SELECT coalesce(mime,'')||'|'||coalesce(w,'')||'|'||coalesce(h,'')||'|'||source||'|'||coalesce(lineage,'')||'|'||coalesce(record,'')||'|'||coalesce(prompt,'')||'|'||coalesce(name,'')||'|'||coalesce(path,'')||'|'||coalesce(model,'') FROM assets WHERE sha=?",
            [&a.sha],
            |r| r.get(0),
        )
        .optional()?;
    let changed = before != after;
    if changed {
        c.execute("UPDATE assets SET updated=? WHERE sha=?", params![now, a.sha])?;
        reindex(c, &a.sha)?;
    }
    Ok(changed)
}

/// Refresh the full-text row of an asset (fts rowid = assets rowid).
pub fn reindex(c: &Connection, sha: &str) -> Result<()> {
    let row: Option<(i64, Option<String>, Option<String>, Option<String>, Option<String>)> = c
        .query_row("SELECT rowid,prompt,caption,name,model FROM assets WHERE sha=?", [sha], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((rowid, prompt, caption, name, model)) = row else { return Ok(()) };
    let mut st = c.prepare_cached("SELECT tag FROM tags WHERE sha=? ORDER BY tag")?;
    let tags: Vec<String> = st.query_map([sha], |r| r.get(0))?.filter_map(|t| t.ok()).collect();
    let mut names: Vec<String> = name.into_iter().collect();
    let mut st = c.prepare_cached("SELECT DISTINCT name FROM placements WHERE sha=? AND name IS NOT NULL AND name!=''")?;
    names.extend(st.query_map([sha], |r| r.get::<_, String>(0))?.filter_map(|t| t.ok()));
    names.dedup();
    c.execute("DELETE FROM fts WHERE rowid=?", [rowid])?;
    c.execute(
        "INSERT INTO fts(rowid,prompt,caption,tags,name,model) VALUES(?,?,?,?,?,?)",
        params![rowid, prompt.unwrap_or_default(), caption.unwrap_or_default(), tags.join(" "), names.join(" "), model.map(|m| m.replace(['/', '-'], " ") + " " + &m).unwrap_or_default()],
    )?;
    Ok(())
}

/// Move every reference of `old` to `new` (a remote-only `fal_…` row whose bytes are now
/// known). If `new` already exists, fal metadata, tags and favorites merge into it.
pub fn rekey(c: &mut Connection, old: &str, new: &str) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let tx = c.transaction()?;
    let exists: bool = tx.query_row("SELECT count(*) FROM assets WHERE sha=?", [new], |r| r.get::<_, i64>(0))? > 0;
    if exists {
        let (fid, furl, freq, fsrc, fsync, fav, prompt, caption): (Option<String>, Option<String>, Option<String>, Option<String>, i64, i64, Option<String>, Option<String>) = tx.query_row(
            "SELECT fal_id,fal_url,fal_request_id,fal_source,fal_synced,favorite,prompt,caption FROM assets WHERE sha=?",
            [old],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )?;
        let rowid: i64 = tx.query_row("SELECT rowid FROM assets WHERE sha=?", [old], |r| r.get(0))?;
        tx.execute("DELETE FROM fts WHERE rowid=?", [rowid])?;
        tx.execute("DELETE FROM assets WHERE sha=?", [old])?;
        tx.execute(
            "UPDATE assets SET fal_id=coalesce(fal_id,?1), fal_url=coalesce(fal_url,?2), fal_request_id=coalesce(fal_request_id,?3),
             fal_source=coalesce(fal_source,?4), fal_synced=max(fal_synced,?5), favorite=max(favorite,?6),
             prompt=coalesce(prompt,?7), caption=coalesce(caption,?8) WHERE sha=?9",
            params![fid, furl, freq, fsrc, fsync, fav, prompt, caption, new],
        )?;
    } else {
        tx.execute("UPDATE assets SET sha=? WHERE sha=?", params![new, old])?;
    }
    for t in ["tags", "asset_projects", "collection_items"] {
        tx.execute(&format!("UPDATE OR IGNORE {t} SET sha=? WHERE sha=?"), params![new, old])?;
        tx.execute(&format!("DELETE FROM {t} WHERE sha=?"), [old])?;
    }
    tx.execute("UPDATE placements SET sha=? WHERE sha=?", params![new, old])?;
    tx.execute("UPDATE outbox SET sha=? WHERE sha=?", params![new, old])?;
    reindex(&tx, new)?;
    tx.commit()?;
    Ok(())
}

pub fn tags_of(c: &Connection, sha: &str) -> Vec<String> {
    let Ok(mut st) = c.prepare_cached("SELECT tag FROM tags WHERE sha=? ORDER BY tag") else { return vec![] };
    st.query_map([sha], |r| r.get(0)).map(|it| it.filter_map(|t| t.ok()).collect()).unwrap_or_default()
}

pub fn projects_of(c: &Connection, sha: &str) -> Vec<String> {
    let Ok(mut st) = c.prepare_cached("SELECT project FROM asset_projects WHERE sha=? UNION SELECT project FROM placements WHERE sha=? ORDER BY 1") else {
        return vec![];
    };
    st.query_map([sha, sha], |r| r.get(0)).map(|it| it.filter_map(|t| t.ok()).collect()).unwrap_or_default()
}

pub fn placements_of(c: &Connection, sha: &str) -> Vec<Json> {
    let Ok(mut st) = c.prepare_cached("SELECT project,canvas,node,role,name FROM placements WHERE sha=? ORDER BY project,canvas,node") else {
        return vec![];
    };
    st.query_map([sha], |r| {
        Ok(json!({"project": r.get::<_, String>(0)?, "canvas": r.get::<_, String>(1)?, "node": r.get::<_, String>(2)?, "role": r.get::<_, Option<String>>(3)?, "name": r.get::<_, Option<String>>(4)?}))
    })
    .map(|it| it.filter_map(|t| t.ok()).collect())
    .unwrap_or_default()
}

pub fn collections_of(c: &Connection, sha: &str) -> Vec<Json> {
    let Ok(mut st) = c.prepare_cached("SELECT c.id,c.name,c.fal_id FROM collection_items i JOIN collections c ON c.id=i.collection WHERE i.sha=? ORDER BY c.name") else {
        return vec![];
    };
    st.query_map([sha], |r| Ok(json!({"id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "fal_id": r.get::<_, Option<String>>(2)?})))
        .map(|it| it.filter_map(|t| t.ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_rekey_fts() {
        let mut c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        let a = Ingested { sha: "a".repeat(64), kind: "image".into(), prompt: Some("a red fox in snow".into()), source: Some("generate".into()), ..Default::default() };
        assert!(upsert_ingested(&c, &a, 1).unwrap());
        assert!(!upsert_ingested(&c, &a, 2).unwrap());
        let n: i64 = c.query_row("SELECT count(*) FROM fts WHERE fts MATCH 'fox'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        c.execute("INSERT INTO assets(sha,kind,fal_id,favorite,created) VALUES('fal_x','image','x',1,5)", []).unwrap();
        c.execute("INSERT INTO tags VALUES('fal_x','mood')", []).unwrap();
        rekey(&mut c, "fal_x", &"a".repeat(64)).unwrap();
        let (fid, fav): (String, i64) = c.query_row("SELECT fal_id,favorite FROM assets WHERE sha=?", [&"a".repeat(64)], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((fid.as_str(), fav), ("x", 1));
        assert_eq!(tags_of(&c, &"a".repeat(64)), vec!["mood".to_string()]);
        let n: i64 = c.query_row("SELECT count(*) FROM assets", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
