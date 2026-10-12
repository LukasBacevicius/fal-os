//! The asset library (docs/ASSETS-V1.md §2): a workspace-wide index of every asset in every
//! project plus the user's fal.ai library, with thumbnails, search and fal sync.
//!
//! - `db`: SQLite at `$FALOS_HOME/library.db`, keyed by sha256; placements separate.
//! - `ingest`: projects' canvases + `assets/` → index (startup + on change).
//! - `thumbs`: `/thumbs/<sha>-<256|1024|strip>.webp` under `$FALOS_HOME/thumbs`, generated in
//!   the background and on demand; a repair job fixes missing ones.
//! - `sync`: fal pull (newest first, incremental + periodic reconcile walk), outbox writes
//!   (upload, favorite, tags, collections), per-project collections under "fal-os".
//! - `query`: metadata FTS, color, similar, filters and smart rules.
//! - `ws` / `mcp` / `http`: the `/workspace` protocol, agent tools, thumbnail route.
//!
//! Enabled explicitly (`Library::enable`) by `falos-host serve`, so embedders and tests
//! never touch `~/.fal-os` unless they ask.

#![allow(clippy::type_complexity)] // row tuples from SQLite

pub mod db;
pub mod fal;
pub mod http;
pub mod ingest;
pub mod mcp;
pub mod query;
#[cfg(test)]
mod synth;
pub mod sync;
pub mod thumbs;
pub mod ws;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use falos_doc::{Actor, Asset, Command, NodeKind, Props};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore, broadcast};

use crate::host::now_ms;
use crate::workspace::Workspace;

pub struct Library {
    pub home: PathBuf,
    pub thumbs_dir: PathBuf,
    cache_dir: PathBuf,
    db: Mutex<Connection>,
    /// Serialized `library.changed` / `library.show` frames for `/workspace` clients.
    events: broadcast::Sender<String>,
    pending: Mutex<BTreeSet<String>>,
    pending_notify: Notify,
    pub fal: Option<fal::FalApi>,
    pub http: reqwest::Client,
    thumb_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    thumb_sem: Semaphore,
    thumb_notify: Notify,
    sigs: Mutex<HashMap<String, String>>,
    pub(crate) sync_notify: Notify,
    pub(crate) outbox_notify: Notify,
    ws: std::sync::OnceLock<Weak<Workspace>>,
    started: std::sync::Once,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

impl Library {
    /// Open (or create) the library under `home` (normally `$FALOS_HOME`). `fal_key` enables
    /// fal sync.
    pub fn open(home: &Path, fal_key: Option<String>) -> Result<Arc<Self>> {
        let http = reqwest::Client::new();
        Self::open_with(home, fal_key.map(|k| fal::FalApi::new(http, k)))
    }

    /// [`Library::open`] with an explicit fal client (tests point it at a mock).
    pub fn open_with(home: &Path, fal: Option<fal::FalApi>) -> Result<Arc<Self>> {
        let db = db::open(&home.join("library.db"))?;
        let http = reqwest::Client::new();
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            home: home.to_path_buf(),
            thumbs_dir: home.join("thumbs"),
            cache_dir: home.join("cache").join("originals"),
            db: Mutex::new(db),
            events,
            pending: Mutex::default(),
            pending_notify: Notify::new(),
            fal,
            http,
            thumb_locks: Mutex::default(),
            thumb_sem: Semaphore::new(4),
            thumb_notify: Notify::new(),
            sigs: Mutex::default(),
            sync_notify: Notify::new(),
            outbox_notify: Notify::new(),
            ws: std::sync::OnceLock::new(),
            started: std::sync::Once::new(),
        }))
    }

    /// Open the library for a workspace and start its background tasks (needs a tokio
    /// runtime). Idempotent per workspace.
    pub fn enable(ws: &Arc<Workspace>, home: &Path, fal_key: Option<String>) -> Result<Arc<Self>> {
        if let Some(l) = ws.library.get() {
            return Ok(l.clone());
        }
        Ok(Self::attach(ws, Self::open(home, fal_key)?))
    }

    /// Attach an opened library to a workspace and start its background tasks.
    pub fn attach(ws: &Arc<Workspace>, lib: Arc<Self>) -> Arc<Self> {
        let _ = ws.library.set(lib.clone());
        let lib = ws.library.get().cloned().unwrap_or(lib);
        let _ = lib.ws.set(Arc::downgrade(ws));
        lib.start();
        lib
    }

    pub fn workspace(&self) -> Option<Arc<Workspace>> {
        self.ws.get()?.upgrade()
    }

    pub(crate) fn with_db<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut c = self.db.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut c)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.events.subscribe()
    }

    /// Queue `library.changed` for these keys (flushed every 250 ms).
    pub fn changed<I: IntoIterator<Item = String>>(&self, keys: I) {
        let mut p = self.pending.lock().unwrap();
        let before = p.len();
        p.extend(keys);
        if p.len() != before {
            self.pending_notify.notify_one();
        }
    }

    /// `library.show`: open the user's library view with these assets.
    pub fn show(&self, keys: &[String]) {
        let _ = self.events.send(json!({"type": "library.show", "shas": keys}).to_string());
    }

    pub fn status(&self) -> Json {
        let writes = self.with_db(|c| Ok(db::meta(c, "fal.writes"))).ok().flatten();
        let pending: i64 = self.with_db(|c| Ok(c.query_row("SELECT count(*) FROM outbox WHERE attempts < 99", [], |r| r.get(0))?)).unwrap_or(0);
        json!({
            "type": "library.status",
            "fal": if self.fal.is_some() { "connected" } else { "off" },
            "writes": writes.as_deref().map_or("ok", |w| if w.starts_with("unavailable") { "unavailable" } else { "ok" }),
            "outbox": pending,
        })
    }

    fn start(self: &Arc<Self>) {
        self.started.call_once(|| {
            if tokio::runtime::Handle::try_current().is_err() {
                return;
            }
            let me = Arc::downgrade(self);
            // Changed-event flusher.
            tokio::spawn({
                let me = me.clone();
                async move {
                    loop {
                        let Some(l) = me.upgrade() else { return };
                        l.pending_notify.notified().await;
                        drop(l);
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let Some(l) = me.upgrade() else { return };
                        let keys: Vec<String> = std::mem::take(&mut *l.pending.lock().unwrap()).into_iter().collect();
                        if !keys.is_empty() {
                            for chunk in keys.chunks(2000) {
                                let _ = l.events.send(json!({"type": "library.changed", "shas": chunk}).to_string());
                            }
                        }
                    }
                }
            });
            // Ingest: everything now, then projects whose signature moved.
            tokio::spawn({
                let me = me.clone();
                async move {
                    loop {
                        let Some(l) = me.upgrade() else { return };
                        let _ = tokio::task::spawn_blocking(move || l.ingest_changed()).await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            });
            // Thumbnails: background generation + repair.
            tokio::spawn({
                let me = me.clone();
                async move {
                    let mut last_repair = 0i64;
                    loop {
                        let Some(l) = me.upgrade() else { return };
                        if now_ms() - last_repair > 30 * 60_000 {
                            last_repair = now_ms();
                            let _ = l.repair_thumbs();
                        }
                        let n = l.thumb_pass().await;
                        if n == 0 {
                            let wait = l.thumb_notify.notified();
                            let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
                        }
                    }
                }
            });
            if self.fal.is_some() {
                sync::start(self);
            }
        });
    }

    // ---- ingest -------------------------------------------------------------------------------

    /// Re-ingest projects whose signature changed; forget projects that are gone.
    pub fn ingest_changed(&self) -> Result<()> {
        let Some(ws) = self.workspace() else { return Ok(()) };
        let ids = ws.ids();
        for id in &ids {
            let Some(sig) = ingest::signature(&ws, id) else { continue };
            if self.sigs.lock().unwrap().get(id) == Some(&sig) {
                continue;
            }
            match self.ingest_project(&ws, id) {
                Ok(_) => {
                    self.sigs.lock().unwrap().insert(id.clone(), sig);
                }
                Err(e) => tracing::warn!("library: ingesting {id}: {e:#}"),
            }
        }
        let gone: Vec<String> = self.with_db(|c| {
            let mut st = c.prepare("SELECT DISTINCT project FROM asset_projects UNION SELECT DISTINCT project FROM placements")?;
            let all: Vec<String> = st.query_map([], |r| r.get(0))?.flatten().collect();
            Ok(all.into_iter().filter(|p| !ids.contains(p)).collect())
        })?;
        for p in gone {
            let keys = self.with_db(|c| {
                let mut st = c.prepare("SELECT sha FROM asset_projects WHERE project=?1 UNION SELECT sha FROM placements WHERE project=?1")?;
                let keys: Vec<String> = st.query_map([&p], |r| r.get(0))?.flatten().collect();
                c.execute("DELETE FROM asset_projects WHERE project=?", [&p])?;
                c.execute("DELETE FROM placements WHERE project=?", [&p])?;
                Ok(keys)
            })?;
            self.sigs.lock().unwrap().remove(&p);
            self.changed(keys);
        }
        Ok(())
    }

    /// Index one project; returns the keys that changed.
    pub fn ingest_project(&self, ws: &Workspace, id: &str) -> Result<Vec<String>> {
        let scan = ingest::scan_project(ws, id)?;
        let now = now_ms();
        let mut changed: BTreeSet<String> = BTreeSet::new();
        let mut generated = false;
        self.with_db(|c| {
            let tx = c.transaction()?;
            let mut seen = BTreeSet::new();
            for a in &scan.assets {
                let mut a = a.clone();
                if a.created.is_none() {
                    a.created = Some(now);
                }
                if db::upsert_ingested(&tx, &a, now)? {
                    changed.insert(a.sha.clone());
                    generated |= a.fal_request_id.is_some();
                }
                seen.insert(a.sha.clone());
            }
            let before: BTreeSet<String> = {
                let mut st = tx.prepare("SELECT sha FROM asset_projects WHERE project=?")?;
                st.query_map([id], |r| r.get(0))?.flatten().collect()
            };
            for s in seen.difference(&before) {
                tx.execute("INSERT OR IGNORE INTO asset_projects(sha,project) VALUES(?,?)", params![s, id])?;
                changed.insert(s.clone());
            }
            for s in before.difference(&seen) {
                tx.execute("DELETE FROM asset_projects WHERE sha=? AND project=?", params![s, id])?;
                changed.insert(s.clone());
            }
            let old: BTreeSet<(String, String, String, String, String)> = {
                let mut st = tx.prepare("SELECT canvas,node,sha,coalesce(role,''),coalesce(name,'') FROM placements WHERE project=?")?;
                st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.flatten().collect()
            };
            let new: BTreeSet<(String, String, String, String, String)> = scan
                .placements
                .iter()
                .map(|p| (p.canvas.clone(), p.node.clone(), p.sha.clone(), p.role.clone().unwrap_or_default(), p.name.clone().unwrap_or_default()))
                .collect();
            if old != new {
                for p in old.symmetric_difference(&new) {
                    changed.insert(p.2.clone());
                }
                tx.execute("DELETE FROM placements WHERE project=?", [id])?;
                for p in &scan.placements {
                    // A placement of an asset with no record yet (e.g. a broken ref) still counts.
                    tx.execute("INSERT OR IGNORE INTO assets(sha,created,updated) VALUES(?,?,?)", params![p.sha, now, now])?;
                    tx.execute(
                        "INSERT OR REPLACE INTO placements(project,canvas,node,sha,role,name) VALUES(?,?,?,?,?,?)",
                        params![id, p.canvas, p.node, p.sha, p.role, p.name],
                    )?;
                }
                for k in &changed {
                    db::reindex(&tx, k)?;
                }
            }
            tx.commit()?;
            // A fal pull may have seen this generation first (as a remote-only row): merge it.
            let linkable: Vec<&db::Ingested> = scan.assets.iter().filter(|a| changed.contains(&a.sha) && (a.fal_request_id.is_some() || a.fal_url.is_some())).collect();
            for a in linkable {
                let remote: Option<String> = c
                    .query_row(
                        "SELECT sha FROM assets WHERE sha LIKE 'fal\\_%' ESCAPE '\\' AND (fal_request_id=?1 OR fal_url=?2) LIMIT 1",
                        params![a.fal_request_id, a.fal_url],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(r) = remote {
                    db::rekey(c, &r, &a.sha)?;
                    changed.insert(r);
                }
            }
            Ok(())
        })?;
        if !changed.is_empty() {
            self.thumb_notify.notify_one();
            if generated {
                self.sync_notify.notify_one();
            }
        }
        let out: Vec<String> = changed.into_iter().collect();
        self.changed(out.clone());
        Ok(out)
    }

    /// Ingest every project right now (tests, CLI).
    pub fn ingest_all(&self) -> Result<()> {
        self.sigs.lock().unwrap().clear();
        self.ingest_changed()
    }

    // ---- thumbnails ---------------------------------------------------------------------------

    /// Reset rows whose thumbnails went missing, and retry failures once in a while.
    pub fn repair_thumbs(&self) -> Result<usize> {
        let dir = self.thumbs_dir.clone();
        self.with_db(|c| {
            let mut st = c.prepare("SELECT sha FROM assets WHERE thumb=1")?;
            let missing: Vec<String> = st.query_map([], |r| r.get::<_, String>(0))?.flatten().filter(|k| !thumbs::thumbs_exist(&dir, k)).collect();
            for k in &missing {
                c.execute("UPDATE assets SET thumb=0 WHERE sha=?", [k])?;
            }
            c.execute("UPDATE assets SET thumb=0 WHERE thumb=2 AND (thumb_err IS NULL OR thumb_err NOT LIKE 'permanent:%')", [])?;
            Ok(missing.len())
        })
    }

    /// One background pass: up to 8 assets still missing thumbnails (local ones first; only
    /// the newest remote fal assets, the rest are made when the grid asks for them).
    async fn thumb_pass(self: &Arc<Self>) -> usize {
        let keys: Vec<String> = self
            .with_db(|c| {
                let mut st = c.prepare(
                    "SELECT sha FROM assets WHERE thumb=0 AND (path IS NOT NULL OR (fal_url IS NOT NULL AND sha IN (SELECT sha FROM assets WHERE fal_url IS NOT NULL AND path IS NULL ORDER BY created DESC LIMIT 200)))
                     ORDER BY (path IS NULL), created DESC LIMIT 8",
                )?;
                Ok(st.query_map([], |r| r.get(0))?.flatten().collect())
            })
            .unwrap_or_default();
        let n = keys.len();
        let jobs: Vec<_> = keys.into_iter().map(|k| { let me = self.clone(); tokio::spawn(async move { let _ = me.ensure_thumb(&k).await; }) }).collect();
        for j in jobs {
            let _ = j.await;
        }
        n
    }

    /// Make sure the thumbnails of `key` exist (generating them if needed). Returns the key
    /// the asset has afterwards (a remote asset whose bytes were fetched is re-keyed by sha).
    pub async fn ensure_thumb(self: &Arc<Self>, key: &str) -> Result<String> {
        if thumbs::thumbs_exist(&self.thumbs_dir, key) {
            return Ok(key.to_string());
        }
        let lock = self.thumb_locks.lock().unwrap().entry(key.to_string()).or_default().clone();
        let _g = lock.lock().await;
        if thumbs::thumbs_exist(&self.thumbs_dir, key) {
            return Ok(key.to_string());
        }
        let _permit = self.thumb_sem.acquire().await?;
        let res = self.make_thumb(key).await;
        self.thumb_locks.lock().unwrap().remove(key);
        if let Err(e) = &res {
            let msg = format!("{e:#}");
            let _ = self.with_db(|c| Ok(c.execute("UPDATE assets SET thumb=2, thumb_err=? WHERE sha=?", params![msg, key])?));
        }
        res
    }

    async fn make_thumb(self: &Arc<Self>, key: &str) -> Result<String> {
        let row: Option<(String, Option<String>, Option<String>, Option<String>, Option<i64>)> = self.with_db(|c| {
            Ok(c.query_row("SELECT kind, path, fal_url, mime, w FROM assets WHERE sha=?", [key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?)
        })?;
        let Some((kind, path, url, mime, w0)) = row else { bail!("permanent: no such asset") };
        let path = path.filter(|p| Path::new(p).is_file());
        let src = match (&path, &url) {
            (Some(p), _) => p.clone(),
            (None, Some(u)) => u.clone(),
            _ => {
                self.with_db(|c| Ok(c.execute("UPDATE assets SET thumb=3 WHERE sha=?", [key])?))?;
                bail!("permanent: no local file or remote url")
            }
        };
        let dir = self.thumbs_dir.clone();
        let mut final_key = key.to_string();
        let made = match kind.as_str() {
            "image" => {
                let bytes = match &path {
                    Some(p) => tokio::fs::read(p).await?,
                    None => self.fetch(&src).await?,
                };
                if path.is_none() {
                    // Remote: keep the original in the cache and switch to its content key.
                    let sha = hex(&bytes);
                    let ext = crate::assets::ext_of_mime(mime.as_deref().unwrap_or("image/png"));
                    let file = self.cache_dir.join(format!("{sha}.{ext}"));
                    tokio::fs::create_dir_all(&self.cache_dir).await?;
                    tokio::fs::write(&file, &bytes).await?;
                    let fp = file.display().to_string();
                    self.with_db(|c| Ok(c.execute("UPDATE assets SET path=?, bytes=? WHERE sha=?", params![fp, bytes.len() as i64, key])?))?;
                    final_key = sha;
                }
                let k = final_key.clone();
                let local = path.clone();
                let mime = mime.clone().unwrap_or_default();
                tokio::task::spawn_blocking(move || -> Result<thumbs::Made> {
                    match thumbs::decode_image(&bytes, None) {
                        Ok(img) => thumbs::image_thumbs(&img, &dir, &k),
                        // SVG and friends: the OS renders them.
                        Err(e) => match local.as_deref().map(Path::new).filter(|p| thumbs::doc_thumbable(p, &mime)) {
                            Some(p) => thumbs::doc_thumbs(p, &mime, &dir, &k),
                            None => Err(e),
                        },
                    }
                })
                .await??
            }
            "video" => {
                let k = key.to_string();
                tokio::task::spawn_blocking(move || thumbs::video_thumbs(&src, &dir, &k)).await??
            }
            "audio" => {
                let k = key.to_string();
                tokio::task::spawn_blocking(move || thumbs::audio_thumbs(&src, &dir, &k)).await??
            }
            _ if path.as_deref().is_some_and(|p| thumbs::doc_thumbable(Path::new(p), mime.as_deref().unwrap_or(""))) => {
                // PDF first page, svg, fonts.
                let (k, p, m) = (key.to_string(), PathBuf::from(src.clone()), mime.clone().unwrap_or_default());
                tokio::task::spawn_blocking(move || thumbs::doc_thumbs(&p, &m, &dir, &k)).await??
            }
            _ => {
                self.with_db(|c| Ok(c.execute("UPDATE assets SET thumb=3 WHERE sha=?", [key])?))?;
                bail!("permanent: no thumbnail for `{kind}`")
            }
        };
        if final_key != key {
            // Old thumbnail names keep working for in-flight requests.
            for size in ["256", "1024", "strip"] {
                let (from, to) = (thumbs::path_of(&self.thumbs_dir, &final_key, size), thumbs::path_of(&self.thumbs_dir, key, size));
                if from.is_file() && !to.exists() {
                    let _ = std::fs::hard_link(&from, &to);
                }
            }
            let (old, new) = (key.to_string(), final_key.clone());
            self.with_db(|c| db::rekey(c, &old, &new))?;
            self.changed([old, new]);
        }
        let pal = json!(made.palette.iter().map(|(h, w)| json!({"hex": h, "w": w})).collect::<Vec<_>>()).to_string();
        let fk = final_key.clone();
        self.with_db(|c| {
            c.execute(
                "UPDATE assets SET thumb=1, thumb_err=NULL, w=coalesce(?1,w), h=coalesce(?2,h), duration=coalesce(?3,duration), palette=CASE WHEN ?4='[]' THEN palette ELSE ?4 END, tiny=coalesce(?5,tiny) WHERE sha=?6",
                params![made.w, made.h, made.duration, pal, made.tiny, fk],
            )?;
            Ok(())
        })?;
        if w0.is_none() && made.w.is_some() {
            self.changed([final_key.clone()]);
        }
        Ok(final_key)
    }

    /// Download a remote original (fal CDN files are public; no key is sent).
    async fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        let mut r = self.http.get(url).timeout(Duration::from_secs(300)).send().await?;
        if !r.status().is_success() {
            bail!("download {url}: {}", r.status());
        }
        let mut out = vec![];
        while let Some(c) = r.chunk().await? {
            out.extend_from_slice(&c);
            if out.len() as u64 > crate::assets::MAX_IMPORT_BYTES {
                bail!("permanent: original too large");
            }
        }
        Ok(out)
    }

    // ---- writes -------------------------------------------------------------------------------

    fn resolve_all(&self, keys: &[String]) -> Result<Vec<String>> {
        self.with_db(|c| {
            keys.iter().map(|k| query::resolve_key(c, k).ok_or_else(|| anyhow!("no asset `{k}` in the library"))).collect()
        })
    }

    fn enqueue(c: &Connection, op: &str, sha: &str, payload: Json) -> Result<()> {
        // Collapse repeated favorite/tags writes for the same asset.
        if op == "favorite" || op == "tags" {
            c.execute("DELETE FROM outbox WHERE op=? AND sha=? AND attempts=0", params![op, sha])?;
        }
        c.execute("INSERT INTO outbox(op,sha,payload,created) VALUES(?,?,?,?)", params![op, sha, payload.to_string(), now_ms()])?;
        Ok(())
    }

    pub fn tag(&self, keys: &[String], add: &[String], remove: &[String]) -> Result<Vec<String>> {
        let keys = self.resolve_all(keys)?;
        let norm = |t: &String| t.trim().trim_start_matches('#').to_lowercase().chars().take(50).collect::<String>();
        let add: Vec<String> = add.iter().map(norm).filter(|t| !t.is_empty()).collect();
        let remove: Vec<String> = remove.iter().map(norm).collect();
        self.with_db(|c| {
            let tx = c.transaction()?;
            for k in &keys {
                for t in &add {
                    tx.execute("INSERT OR IGNORE INTO tags(sha,tag) VALUES(?,?)", params![k, t])?;
                }
                for t in &remove {
                    tx.execute("DELETE FROM tags WHERE sha=? AND tag=?", params![k, t])?;
                }
                db::reindex(&tx, k)?;
                if Self::synced(&tx, k) {
                    Self::enqueue(&tx, "tags", k, json!({}))?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        self.outbox_notify.notify_one();
        self.changed(keys.clone());
        Ok(keys)
    }

    fn synced(c: &Connection, k: &str) -> bool {
        c.query_row("SELECT fal_id IS NOT NULL FROM assets WHERE sha=?", [k], |r| r.get(0)).unwrap_or(false)
    }

    pub fn favorite(&self, keys: &[String], value: bool) -> Result<Vec<String>> {
        let keys = self.resolve_all(keys)?;
        self.with_db(|c| {
            let tx = c.transaction()?;
            for k in &keys {
                tx.execute("UPDATE assets SET favorite=? WHERE sha=?", params![value as i64, k])?;
                if Self::synced(&tx, k) {
                    Self::enqueue(&tx, "favorite", k, json!({"value": value}))?;
                }
            }
            tx.commit()?;
            Ok(())
        })?;
        self.outbox_notify.notify_one();
        self.changed(keys.clone());
        Ok(keys)
    }

    pub fn local_only(&self, keys: &[String], value: bool) -> Result<Vec<String>> {
        let keys = self.resolve_all(keys)?;
        self.with_db(|c| {
            for k in &keys {
                c.execute("UPDATE assets SET local_only=? WHERE sha=?", params![value as i64, k])?;
                if value {
                    c.execute("DELETE FROM outbox WHERE op='upload' AND sha=?", [k])?;
                }
            }
            Ok(())
        })?;
        self.changed(keys.clone());
        Ok(keys)
    }

    /// Add to a collection (by id or name; a new name makes a local collection that syncs to
    /// fal for assets on fal). Returns the collection id.
    pub fn collect(&self, keys: &[String], collection: &str) -> Result<String> {
        let keys = self.resolve_all(keys)?;
        let name = collection.trim();
        if name.is_empty() {
            bail!("collection needs a name or id");
        }
        let id = self.with_db(|c| {
            let tx = c.transaction()?;
            let id: Option<String> = tx
                .query_row("SELECT id FROM collections WHERE id=?1 OR fal_id=?1 OR lower(name)=lower(?1) ORDER BY (id=?1) DESC LIMIT 1", [name], |r| r.get(0))
                .optional()?;
            let id = match id {
                Some(id) => id,
                None => {
                    let id = format!("c_{:08x}", rand::random::<u32>());
                    tx.execute("INSERT INTO collections(id,name) VALUES(?,?)", params![id, name])?;
                    id
                }
            };
            for k in &keys {
                tx.execute("INSERT OR IGNORE INTO collection_items(collection,sha) VALUES(?,?)", params![id, k])?;
                if Self::synced(&tx, k) {
                    Self::enqueue(&tx, "collect", k, json!({"collection": id}))?;
                }
            }
            tx.commit()?;
            Ok(id)
        })?;
        self.outbox_notify.notify_one();
        self.changed(keys);
        Ok(id)
    }

    /// Queue uploads to fal (explicit, visible). Returns (queued, skipped with reasons).
    pub fn upload(&self, keys: &[String], project: Option<&str>) -> Result<(Vec<String>, Vec<Json>)> {
        let keys = self.resolve_all(keys)?;
        if self.fal.is_none() {
            bail!("fal isn't connected (no FAL_KEY): uploads aren't available");
        }
        let mut queued = vec![];
        let mut skipped = vec![];
        self.with_db(|c| {
            for k in &keys {
                let (path, local_only, fal_id): (Option<String>, i64, Option<String>) =
                    c.query_row("SELECT path, local_only, fal_id FROM assets WHERE sha=?", [k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                let why = if fal_id.is_some() {
                    Some("already on fal")
                } else if local_only != 0 {
                    Some("local-only")
                } else if !path.as_deref().is_some_and(|p| Path::new(p).is_file()) {
                    Some("no local file")
                } else {
                    None
                };
                match why {
                    Some(w) => skipped.push(json!({"sha": k, "reason": w})),
                    None => {
                        c.execute("DELETE FROM outbox WHERE op='upload' AND sha=?", [k])?;
                        Self::enqueue(c, "upload", k, json!({"project": project}))?;
                        queued.push(k.clone());
                    }
                }
            }
            Ok(())
        })?;
        self.outbox_notify.notify_one();
        Ok((queued, skipped))
    }

    /// Another part of the host put these bytes on fal (e.g. a model input upload): record
    /// the URL (and the library record, if one was made) so the badge shows it.
    pub fn note_fal_upload(&self, sha: &str, url: &str, asset_record_id: Option<&str>) -> Result<()> {
        let Some(k) = crate::assets::parse_id(sha) else { bail!("bad sha") };
        self.with_db(|c| {
            c.execute(
                "UPDATE assets SET fal_url=coalesce(fal_url,?1), fal_id=coalesce(fal_id,?2), fal_synced=CASE WHEN ?2 IS NULL THEN fal_synced ELSE 1 END WHERE sha=?3",
                params![url, asset_record_id, k],
            )?;
            Ok(())
        })?;
        self.changed([k]);
        Ok(())
    }

    /// One asset with everything known about it (`get_asset`).
    pub fn detail(&self, key: &str, project: Option<&str>) -> Result<Json> {
        self.with_db(|c| {
            let k = query::resolve_key(c, key).or_else(|| query::key_of_node(c, key, project)).ok_or_else(|| anyhow!("no asset `{key}` in the library"))?;
            let mut item = query::items(c, std::slice::from_ref(&k), None)?.into_iter().next().context("asset vanished")?;
            let (lineage, origin, media, palette, err): (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>) =
                c.query_row("SELECT lineage, origin, media, palette, thumb_err FROM assets WHERE sha=?", [&k], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Json>(&s).ok()).unwrap_or(Json::Null);
            item["lineage"] = parse(lineage);
            item["origin"] = parse(origin);
            item["media"] = parse(media);
            item["palette_weights"] = parse(palette);
            item["placements"] = json!(db::placements_of(c, &k));
            item["collections"] = json!(db::collections_of(c, &k));
            if let Some(e) = err {
                item["thumb_error"] = json!(e);
            }
            Ok(item)
        })
    }

    // ---- placing ------------------------------------------------------------------------------

    /// A local file with the asset's bytes (downloading a remote original into the cache).
    /// Returns (key after any re-key, path).
    pub async fn original(self: &Arc<Self>, key: &str) -> Result<(String, PathBuf)> {
        let (path, url, mime): (Option<String>, Option<String>, Option<String>) =
            self.with_db(|c| Ok(c.query_row("SELECT path, fal_url, mime FROM assets WHERE sha=?", [key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?))?;
        if let Some(p) = path.filter(|p| Path::new(p).is_file()) {
            return Ok((key.to_string(), PathBuf::from(p)));
        }
        // Another project may have the file.
        if key.len() == 64
            && let Some(ws) = self.workspace()
            && let Some(p) = ws.ids().iter().find_map(|id| ingest::project_dir(&ws, id).and_then(|d| crate::assets::find(&d.join("assets"), key)))
        {
            return Ok((key.to_string(), p));
        }
        let url = url.ok_or_else(|| anyhow!("asset `{key}` has no local file and no fal url"))?;
        let bytes = self.fetch(&url).await?;
        let sha = hex(&bytes);
        let ext = crate::assets::ext_of_mime(mime.as_deref().unwrap_or(""));
        let ext = if ext == "bin" { url.rsplit('.').next().filter(|e| e.len() <= 4).unwrap_or("bin") } else { ext };
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        let file = self.cache_dir.join(format!("{sha}.{ext}"));
        tokio::fs::write(&file, &bytes).await?;
        let fp = file.display().to_string();
        self.with_db(|c| {
            c.execute("UPDATE assets SET path=?, bytes=? WHERE sha=?", params![fp, bytes.len() as i64, key])?;
            if sha != key {
                db::rekey(c, key, &sha)?;
            }
            Ok(())
        })?;
        if sha != key {
            for size in ["256", "1024", "strip"] {
                let (from, to) = (thumbs::path_of(&self.thumbs_dir, key, size), thumbs::path_of(&self.thumbs_dir, &sha, size));
                if from.is_file() && !to.exists() {
                    let _ = std::fs::hard_link(&from, &to);
                }
            }
            self.changed([key.to_string(), sha.clone()]);
        }
        Ok((sha, file))
    }

    /// Place an asset on a project's canvas: a new node (new placement, same asset).
    pub async fn place(self: &Arc<Self>, key: &str, project: &str, canvas: Option<&str>, at: (Option<f64>, Option<f64>), size: Option<f64>, actor: &Actor) -> Result<Json> {
        let ws = self.workspace().context("library has no workspace")?;
        let key = self.resolve_all(&[key.to_string()])?.remove(0);
        let host = ws.host_for_frame(Some(project))?;
        let (key, src) = self.original(&key).await?;
        let (kind, mime, w, h, record, name, url, prompt): (String, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>) = self.with_db(|c| {
            Ok(c.query_row("SELECT kind, mime, w, h, record, name, fal_url, prompt FROM assets WHERE sha=?", [&key], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))
            })?)
        })?;
        let node_kind = match kind.as_str() {
            "image" => NodeKind::Image,
            "video" => NodeKind::Video,
            "audio" => NodeKind::Audio,
            "model3d" => NodeKind::Model3d,
            k => bail!("can't place a `{k}` asset"),
        };
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_string();
        let dir = host.dir().join("assets");
        tokio::fs::create_dir_all(&dir).await?;
        if crate::assets::find(&dir, &key).is_none() {
            let dst = dir.join(format!("{key}.{ext}"));
            let tmp = dir.join(format!("{key}.part{}", rand::random::<u32>()));
            tokio::fs::copy(&src, &tmp).await.with_context(|| format!("copying {}", src.display()))?;
            tokio::fs::rename(&tmp, &dst).await?;
        }
        let asset: Asset = record
            .and_then(|r| serde_json::from_str::<Asset>(&r).ok())
            .unwrap_or_else(|| Asset { id: format!("sha256:{key}"), mime: mime.clone(), uri: url.clone(), ..Default::default() });
        let canvas = match canvas {
            Some(c) => host.resolve_canvas(c)?,
            None => host.project().canvases.first().map(|c| c.id.clone()).unwrap_or_else(|| "main".into()),
        };
        let (nw, nh) = match (node_kind, w, h) {
            (NodeKind::Audio, _, _) => (480.0, 96.0),
            (_, Some(iw), Some(ih)) if iw > 0 && ih > 0 => {
                let w = size.unwrap_or_else(|| (iw as f64).min(960.0));
                (w, w * ih as f64 / iw as f64)
            }
            _ => (size.unwrap_or(512.0), size.unwrap_or(512.0)),
        };
        let (x, y) = match at {
            (Some(x), Some(y)) => (x, y),
            _ => host.find_space(&canvas, None, &[], nw, nh)?,
        };
        let props: Props = serde_json::from_value(json!({"x": x.round(), "y": y.round(), "w": nw.round(), "h": nh.round(), "asset": format!("sha256:{key}")}))?;
        let label: String = name.or(prompt).unwrap_or_else(|| kind.clone()).chars().take(60).collect();
        let res = host.mutate(&canvas, &actor.id, None, |c| {
            c.apply(
                actor,
                None,
                &[Command::PutAsset(asset.clone()), Command::CreateNode { id: None, kind: node_kind, parent: None, index: None, name: label.clone(), props: props.clone(), text: None }],
            )
        })?;
        let node = res.created.iter().find(|id| host.read(&canvas, |c| c.node(id).is_some()).unwrap_or(false)).cloned().context("place created no node")?;
        Ok(json!({"node": node, "canvas": canvas, "project": host.id, "sha": key}))
    }
}

/// `$FALOS_HOME`-aware default location used by `serve`.
pub fn default_home() -> PathBuf {
    crate::server::home_dir()
}
