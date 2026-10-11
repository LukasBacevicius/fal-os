//! fal sync. fal owns identity and metadata (favorites, tags, collections) of assets on fal;
//! local files are a cache.
//!
//! - **Pull:** newest first until a known asset (incremental), plus a slow walk of the whole
//!   library (3 pages per tick) that restarts every 30 min as the reconcile; assets not seen
//!   by a finished walk were deleted on fal.
//! - **Match:** a fal asset is linked to a local row by `fal_id`, then by request id (our
//!   generations), then by URL; otherwise it becomes a remote-only row `fal_<id>`.
//! - **Outbox:** writes (upload, favorite, tags, collect) queue in SQLite and retry with
//!   backoff while offline; 401/403 mark writes unavailable and stop them.
//! - **Projects:** a manual collection per project under a "fal-os" parent
//!   (`FALOS_FAL_COLLECTION_ROOT`), attached to the project's fal Agent project.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value as Json;

use super::{Library, db, fal};
use crate::host::now_ms;

const WALK_PAGES_PER_TICK: usize = 3;
const RECONCILE_EVERY_MS: i64 = 30 * 60_000;

/// `2026-10-11T13:38:09.770Z` → ms since the epoch.
pub fn parse_ts(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+']).next()?;
    let mut t = time.split(':');
    let (hh, mm) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + (sec * 1000.0).round() as i64)
}

fn kind_of_fal(t: &str) -> &'static str {
    match t {
        "video" => "video",
        "audio" => "audio",
        "3d" => "model3d",
        "image" => "image",
        _ => "other",
    }
}

/// Merge one fal asset into the index; returns its key.
pub fn merge(c: &Connection, a: &Json, epoch: i64) -> Result<Option<String>> {
    let Some(id) = a["asset_id"].as_str() else { return Ok(None) };
    let req = a["request_id"].as_str().filter(|r| !r.is_empty());
    let url = a["url"].as_str();
    let upload = a["source"].as_str() == Some("upload");
    let mut key: Option<String> = c.query_row("SELECT sha FROM assets WHERE fal_id=?", [id], |r| r.get(0)).optional()?;
    if key.is_none()
        && let Some(r) = req.filter(|r| !r.starts_with("upload:"))
    {
        key = c.query_row("SELECT sha FROM assets WHERE fal_request_id=? AND fal_id IS NULL ORDER BY sha LIKE 'fal_%' LIMIT 1", [r], |r| r.get(0)).optional()?;
    }
    if key.is_none()
        && let Some(u) = url
    {
        key = c.query_row("SELECT sha FROM assets WHERE fal_url=? AND fal_id IS NULL LIMIT 1", [u], |r| r.get(0)).optional()?;
    }
    let created = a["created_at"].as_str().and_then(parse_ts).unwrap_or_else(now_ms);
    let prompt = a["prompt"].as_str().filter(|p| !p.is_empty());
    let (p_prompt, p_caption) = if upload { (None, prompt) } else { (prompt, None) };
    let key = match key {
        Some(k) => k,
        None => {
            let k = format!("fal_{id}");
            c.execute(
                "INSERT OR IGNORE INTO assets(sha,kind,mime,w,h,source,prompt,caption,model,name,created,updated,fal_id,fal_url,fal_request_id,fal_synced,fal_source,fal_seen)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11,?12,?13,?14,1,?15,?16)",
                params![
                    k,
                    kind_of_fal(a["type"].as_str().unwrap_or("")),
                    a["content_type"].as_str(),
                    a["width"].as_i64(),
                    a["height"].as_i64(),
                    if upload { "import" } else { "generate" },
                    p_prompt,
                    p_caption,
                    a["endpoint"].as_str(),
                    a["title"].as_str(),
                    created,
                    id,
                    url,
                    req,
                    a["source"].as_str(),
                    epoch
                ],
            )?;
            k
        }
    };
    c.execute(
        "UPDATE assets SET fal_id=?1, fal_url=coalesce(fal_url,?2), fal_request_id=coalesce(fal_request_id,?3), fal_synced=1, fal_source=?4, fal_seen=?5,
           model=coalesce(model,?6), prompt=coalesce(prompt,?7), caption=coalesce(caption,?8), w=coalesce(w,?9), h=coalesce(h,?10),
           mime=coalesce(mime,?11), name=coalesce(name,?12) WHERE sha=?13",
        params![id, url, req, a["source"].as_str(), epoch, a["endpoint"].as_str(), p_prompt, p_caption, a["width"].as_i64(), a["height"].as_i64(), a["content_type"].as_str(), a["title"].as_str(), key],
    )?;
    let pending: i64 = c.query_row("SELECT count(*) FROM outbox WHERE sha=? AND attempts < 99", [&key], |r| r.get(0))?;
    if pending == 0 {
        c.execute("UPDATE assets SET favorite=? WHERE sha=?", params![a["is_favorited"].as_bool().unwrap_or(false) as i64, key])?;
        if let Some(tags) = a["tags"].as_array() {
            c.execute("DELETE FROM tags WHERE sha=?", [&key])?;
            for t in tags {
                if let Some(n) = t["name"].as_str().or(t.as_str()) {
                    c.execute("INSERT OR IGNORE INTO tags(sha,tag) VALUES(?,?)", params![key, n.to_lowercase()])?;
                }
            }
        }
        if let Some(cols) = a["collection_ids"].as_array() {
            c.execute("DELETE FROM collection_items WHERE sha=?1 AND collection IN (SELECT id FROM collections WHERE fal_id IS NOT NULL)", [&key])?;
            for cid in cols.iter().filter_map(Json::as_str) {
                c.execute("INSERT OR IGNORE INTO collections(id,name,fal_id) VALUES(?1,?1,?1)", [cid])?;
                let local: String = c.query_row("SELECT id FROM collections WHERE fal_id=?1 OR id=?1 LIMIT 1", [cid], |r| r.get(0))?;
                c.execute("INSERT OR IGNORE INTO collection_items(collection,sha) VALUES(?,?)", params![local, key])?;
            }
        }
    }
    db::reindex(c, &key)?;
    Ok(Some(key))
}

pub fn start(lib: &Arc<Library>) {
    let me = Arc::downgrade(lib);
    tokio::spawn({
        let me = me.clone();
        async move {
            loop {
                let Some(l) = me.upgrade() else { return };
                if let Err(e) = l.pull_tick().await {
                    tracing::debug!("library: fal pull: {e:#}");
                }
                let walking = l.with_db(|c| Ok(db::meta(c, "fal.walk").as_deref() != Some("done"))).unwrap_or(false);
                let wait = l.sync_notify.notified();
                let _ = tokio::time::timeout(Duration::from_secs(if walking { 3 } else { 20 }), wait).await;
            }
        }
    });
    tokio::spawn(async move {
        loop {
            let Some(l) = me.upgrade() else { return };
            let n = l.outbox_tick().await.unwrap_or(0);
            let wait = l.outbox_notify.notified();
            if n == 0 {
                let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
            }
        }
    });
}

impl Library {
    fn fal_api(&self) -> Result<&fal::FalApi> {
        self.fal.as_ref().ok_or_else(|| anyhow!("fal isn't connected"))
    }

    fn merge_all(&self, items: &[Json], epoch: i64) -> Result<(Vec<String>, usize)> {
        let mut keys = vec![];
        let mut known = 0;
        self.with_db(|c| {
            let tx = c.transaction()?;
            for a in items {
                let had: bool = a["asset_id"].as_str().is_some_and(|id| tx.query_row("SELECT count(*) FROM assets WHERE fal_id=?", [id], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0);
                known += had as usize;
                if let Some(k) = merge(&tx, a, epoch)? {
                    keys.push(k);
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok((keys, known))
    }

    /// One pull round: new assets, then a few pages of the reconcile walk.
    pub async fn pull_tick(self: &Arc<Self>) -> Result<()> {
        let api = self.fal_api()?.clone();
        let (epoch, walk, walk_started) = self.with_db(|c| {
            let epoch: i64 = db::meta(c, "fal.epoch").and_then(|e| e.parse().ok()).unwrap_or(0);
            Ok((epoch, db::meta(c, "fal.walk"), db::meta(c, "fal.walk_started").and_then(|e| e.parse::<i64>().ok()).unwrap_or(0)))
        })?;
        let (epoch, walk) = if epoch == 0 || (walk.as_deref() == Some("done") && now_ms() - walk_started > RECONCILE_EVERY_MS) {
            let e = epoch + 1;
            self.with_db(|c| {
                db::set_meta(c, "fal.epoch", &e.to_string())?;
                db::set_meta(c, "fal.walk", "")?;
                db::set_meta(c, "fal.walk_started", &now_ms().to_string())?;
                Ok(())
            })?;
            (e, Some(String::new()))
        } else {
            (epoch, walk)
        };
        // New assets (newest first) until a page is all known.
        let mut cursor: Option<String> = None;
        let mut changed = vec![];
        for _ in 0..10 {
            let mut p = vec![("limit", "100".to_string())];
            if let Some(c) = &cursor {
                p.push(("cursor", c.clone()));
            }
            let (items, next) = api.list(&p).await?;
            let (keys, known) = self.merge_all(&items, epoch)?;
            changed.extend(keys);
            if known == items.len() || next.is_none() {
                break;
            }
            cursor = next;
        }
        // Reconcile walk.
        if let Some(mut cur) = walk.filter(|w| w != "done") {
            for _ in 0..WALK_PAGES_PER_TICK {
                let mut p = vec![("limit", "100".to_string())];
                if !cur.is_empty() {
                    p.push(("cursor", cur.clone()));
                }
                let (items, next) = api.list(&p).await?;
                let (keys, _) = self.merge_all(&items, epoch)?;
                changed.extend(keys);
                match next {
                    Some(n) => cur = n,
                    None => {
                        cur = "done".into();
                        break;
                    }
                }
            }
            let done = cur == "done";
            let gone = self.with_db(|c| {
                db::set_meta(c, "fal.walk", &cur)?;
                if !done {
                    return Ok(vec![]);
                }
                // Not seen by a full walk: deleted on fal.
                let mut st = c.prepare("SELECT sha FROM assets WHERE fal_id IS NOT NULL AND fal_seen < ?")?;
                let gone: Vec<String> = st.query_map([epoch], |r| r.get(0))?.flatten().collect();
                for k in &gone {
                    if k.starts_with("fal_") && !c.query_row("SELECT count(*) FROM placements WHERE sha=?", [k], |r| r.get::<_, i64>(0)).map(|n| n > 0).unwrap_or(true) {
                        c.execute("DELETE FROM fts WHERE rowid=(SELECT rowid FROM assets WHERE sha=?)", [k])?;
                        c.execute("DELETE FROM assets WHERE sha=?", [k])?;
                        c.execute("DELETE FROM tags WHERE sha=?", [k])?;
                        c.execute("DELETE FROM collection_items WHERE sha=?", [k])?;
                    } else {
                        c.execute("UPDATE assets SET fal_id=NULL, fal_synced=0 WHERE sha=?", [k])?;
                    }
                }
                Ok(gone)
            })?;
            changed.extend(gone);
        }
        if !changed.is_empty() {
            self.thumb_notify.notify_one();
            self.changed(changed);
        }
        Ok(())
    }

    /// fal semantic search (`q` or similar to a fal-hosted image/video): merged into the
    /// index; returns keys in fal's order.
    pub async fn fal_search(self: &Arc<Self>, params: Vec<(&'static str, String)>) -> Result<Vec<String>> {
        let api = self.fal_api()?.clone();
        let mut p = params;
        p.push(("limit", "100".into()));
        let (items, _) = api.list(&p).await?;
        let epoch: i64 = self.with_db(|c| Ok(db::meta(c, "fal.epoch").and_then(|e| e.parse().ok()).unwrap_or(0)))?;
        let (keys, _) = self.merge_all(&items, epoch)?;
        self.thumb_notify.notify_one();
        Ok(keys)
    }

    // ---- outbox -------------------------------------------------------------------------------

    async fn outbox_tick(self: &Arc<Self>) -> Result<usize> {
        let writes_off = self.with_db(|c| Ok(db::meta(c, "fal.writes").is_some_and(|w| w.starts_with("unavailable"))))?;
        if writes_off {
            return Ok(0);
        }
        let jobs: Vec<(i64, String, String, String, i64)> = self.with_db(|c| {
            let mut st = c.prepare("SELECT id, op, coalesce(sha,''), payload, attempts FROM outbox WHERE attempts < 99 AND next_at <= ? ORDER BY id LIMIT 8")?;
            Ok(st.query_map([now_ms()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.flatten().collect())
        })?;
        let n = jobs.len();
        for (id, op, sha, payload, attempts) in jobs {
            let payload: Json = serde_json::from_str(&payload).unwrap_or(Json::Null);
            match self.run_write(&op, &sha, &payload).await {
                Ok(()) => {
                    self.with_db(|c| Ok(c.execute("DELETE FROM outbox WHERE id=?", [id])?))?;
                    self.changed([sha]);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let auth = e.downcast_ref::<fal::FalError>().is_some_and(|f| matches!(f.status, Some(401) | Some(403)));
                    let retry = fal::transient(&e);
                    tracing::warn!("library: fal {op} {sha}: {msg}");
                    self.with_db(|c| {
                        if auth {
                            db::set_meta(c, "fal.writes", &format!("unavailable: {msg}"))?;
                        }
                        let next = now_ms() + (5_000i64 << attempts.min(8));
                        c.execute(
                            "UPDATE outbox SET attempts=?, next_at=?, error=? WHERE id=?",
                            params![if retry && !auth { attempts + 1 } else { 99 }, next, msg, id],
                        )?;
                        Ok(())
                    })?;
                    if auth {
                        return Ok(0);
                    }
                }
            }
        }
        Ok(n)
    }

    async fn run_write(self: &Arc<Self>, op: &str, sha: &str, payload: &Json) -> Result<()> {
        let api = self.fal_api()?.clone();
        let fal_id: Option<String> = self.with_db(|c| Ok(c.query_row("SELECT fal_id FROM assets WHERE sha=?", [sha], |r| r.get(0)).optional()?.flatten()))?;
        match op {
            "upload" => {
                if fal_id.is_some() {
                    return Ok(());
                }
                let (path, kind, mime, prompt, name, local_only, favorite): (Option<String>, String, Option<String>, Option<String>, Option<String>, i64, i64) = self.with_db(|c| {
                    Ok(c.query_row("SELECT path, kind, mime, prompt, name, local_only, favorite FROM assets WHERE sha=?", [sha], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                    })?)
                })?;
                if local_only != 0 {
                    return Ok(());
                }
                let path = path.context("no local file")?;
                let bytes = tokio::fs::read(&path).await?;
                let mime = mime.unwrap_or_else(|| "application/octet-stream".into());
                let file_name = std::path::Path::new(&path).file_name().and_then(|n| n.to_str()).unwrap_or("asset").to_string();
                let url = api.upload_file(bytes, &mime, &file_name).await?;
                let collection = match payload["project"].as_str() {
                    Some(p) => self.project_collection(p).await.map_err(|e| tracing::warn!("library: project collection: {e:#}")).ok(),
                    None => None,
                };
                let asset = api.register_upload(&url, fal::fal_type(&kind), prompt.as_deref().or(name.as_deref()), collection.as_deref(), sha).await?;
                let epoch: i64 = self.with_db(|c| Ok(db::meta(c, "fal.epoch").and_then(|e| e.parse().ok()).unwrap_or(0)))?;
                let aid = asset["asset_id"].as_str().context("no asset_id")?.to_string();
                self.with_db(|c| {
                    c.execute("UPDATE assets SET fal_id=?, fal_url=?, fal_synced=1, fal_source='upload', fal_seen=? WHERE sha=?", params![aid, asset["url"].as_str().unwrap_or(&url), epoch, sha])?;
                    if let Some(cid) = &collection {
                        c.execute("INSERT OR IGNORE INTO collection_items(collection,sha) SELECT id, ? FROM collections WHERE fal_id=?", params![sha, cid])?;
                    }
                    Ok(())
                })?;
                if favorite != 0 {
                    api.favorite(&aid, true).await?;
                }
                if !self.with_db(|c| Ok(db::tags_of(c, sha)))?.is_empty() {
                    self.push_tags(&aid, sha).await?;
                }
                Ok(())
            }
            "favorite" => {
                let Some(aid) = fal_id else { return Ok(()) };
                api.favorite(&aid, payload["value"].as_bool().unwrap_or(true)).await
            }
            "tags" => {
                let Some(aid) = fal_id else { return Ok(()) };
                self.push_tags(&aid, sha).await
            }
            "collect" => {
                let Some(aid) = fal_id else { return Ok(()) };
                let local = payload["collection"].as_str().context("no collection")?;
                let cid = self.fal_collection_of(local).await?;
                api.add_to_collection(&cid, &aid).await
            }
            other => Err(anyhow!("unknown outbox op `{other}`")),
        }
    }

    async fn push_tags(&self, aid: &str, sha: &str) -> Result<()> {
        let api = self.fal_api()?.clone();
        let names = self.with_db(|c| Ok(db::tags_of(c, sha)))?;
        let existing = api.tags().await?;
        let mut ids = vec![];
        for n in names {
            match existing.iter().find(|t| t["name"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(&n))).and_then(|t| t["id"].as_str()) {
                Some(id) => ids.push(id.to_string()),
                None => ids.push(api.create_tag(&n).await?),
            }
        }
        api.set_tags(aid, &ids).await
    }

    /// The fal id of a local collection (created on fal under the root when missing).
    async fn fal_collection_of(&self, local: &str) -> Result<String> {
        let (name, fal_id): (String, Option<String>) = self.with_db(|c| Ok(c.query_row("SELECT name, fal_id FROM collections WHERE id=?", [local], |r| Ok((r.get(0)?, r.get(1)?)))?))?;
        if let Some(f) = fal_id {
            return Ok(f);
        }
        let api = self.fal_api()?.clone();
        let root = self.root_collection().await?;
        let id = api.create_collection(&name, Some(&root)).await?;
        self.with_db(|c| Ok(c.execute("UPDATE collections SET fal_id=?, parent=? WHERE id=?", params![id, root, local])?))?;
        Ok(id)
    }

    async fn root_collection(&self) -> Result<String> {
        let root_name = std::env::var("FALOS_FAL_COLLECTION_ROOT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "fal-os".into());
        let meta_key = format!("fal.root.{root_name}");
        if let Some(id) = self.with_db(|c| Ok(db::meta(c, &meta_key)))? {
            return Ok(id);
        }
        let api = self.fal_api()?.clone();
        let existing = api.collections().await?;
        let id = match existing.iter().find(|c| c["name"].as_str() == Some(&root_name) && c["parent_collection_id"].is_null()).and_then(|c| c["id"].as_str()) {
            Some(id) => id.to_string(),
            None => api.create_collection(&root_name, None).await?,
        };
        self.with_db(|c| {
            db::set_meta(c, &meta_key, &id)?;
            c.execute("INSERT OR IGNORE INTO collections(id,name,fal_id) VALUES(?,?,?)", params![id, root_name, id])?;
            Ok(())
        })?;
        Ok(id)
    }

    /// The project's manual fal collection (under the root), attached to its fal Agent
    /// project when it has one.
    pub async fn project_collection(&self, project: &str) -> Result<String> {
        let meta_key = format!("fal.coll.{project}");
        if let Some(id) = self.with_db(|c| Ok(db::meta(c, &meta_key)))? {
            return Ok(id);
        }
        let api = self.fal_api()?.clone();
        let root = self.root_collection().await?;
        let ws = self.workspace().context("no workspace")?;
        let host = ws.host_for_frame(Some(project))?;
        let name = host.project().name;
        let existing = api.collections().await?;
        let id = match existing.iter().find(|c| c["name"].as_str() == Some(&name) && c["parent_collection_id"].as_str() == Some(&root)).and_then(|c| c["id"].as_str()) {
            Some(id) => id.to_string(),
            None => api.create_collection(&name, Some(&root)).await?,
        };
        self.with_db(|c| {
            db::set_meta(c, &meta_key, &id)?;
            c.execute("INSERT OR IGNORE INTO collections(id,name,fal_id,parent,project) VALUES(?,?,?,?,?)", params![id, name, id, root, project])?;
            Ok(())
        })?;
        if let Some(fp) = host.fal_project()
            && let Err(e) = api.attach_collection(&fp, &id).await
        {
            tracing::warn!("library: attaching collection to fal project {fp}: {e:#}");
        }
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ts_and_merge() {
        assert_eq!(parse_ts("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(parse_ts("2026-10-11T13:38:09.770Z"), Some(1791725889770));
        let c = Connection::open_in_memory().unwrap();
        db::init(&c).unwrap();
        // A local generation with a request id gets linked, not duplicated.
        c.execute("INSERT INTO assets(sha,kind,source,fal_request_id,created) VALUES(?, 'video','generate','req-1',1)", [&"e".repeat(64)]).unwrap();
        let a = json!({"asset_id": "abc", "request_id": "req-1", "url": "https://v3b.fal.media/x.mp4", "type": "video", "source": "response", "endpoint": "fal-ai/kling", "is_favorited": true, "tags": [{"id":"t","name":"Hero"}], "collection_ids": ["col1"], "created_at": "2026-10-11T13:38:09.770Z"});
        assert_eq!(merge(&c, &a, 1).unwrap(), Some("e".repeat(64)));
        let b = json!({"asset_id": "zzz", "request_id": "upload:1", "url": "https://v3b.fal.media/y.png", "type": "image", "source": "upload", "prompt": "a caption", "created_at": "2026-10-10T00:00:00Z"});
        assert_eq!(merge(&c, &b, 1).unwrap(), Some("fal_zzz".into()));
        let (fav, model): (i64, String) = c.query_row("SELECT favorite, model FROM assets WHERE sha=?", [&"e".repeat(64)], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((fav, model.as_str()), (1, "fal-ai/kling"));
        assert_eq!(db::tags_of(&c, &"e".repeat(64)), vec!["hero".to_string()]);
        let (src, cap): (String, String) = c.query_row("SELECT source, caption FROM assets WHERE sha='fal_zzz'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((src.as_str(), cap.as_str()), ("import", "a caption"));
    }
}
