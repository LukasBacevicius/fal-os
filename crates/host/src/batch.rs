//! Batch generation (`generate_batch`): N fal jobs, fast.
//!
//! - All N placeholders appear in ONE transaction, in a compact grid in free space (inside a
//!   new frame `"<label> ×N"` when N ≥ 8), so the batch materializes at once.
//! - Every job is submitted to the fal queue right away (up to [`SUBMIT_CONCURRENCY`] HTTP
//!   submits in flight); fal runs them in parallel server-side.
//! - One shared poller checks the in-flight jobs (~500 ms cadence, backing off to 2 s while
//!   nothing finishes), instead of one polling task per job.
//! - Results download in parallel ([`DOWNLOAD_CONCURRENCY`]) and are converted in coalesced
//!   transactions (one flush every [`FLUSH_EVERY`]), so 200 completions are a handful of
//!   syncs, not 200.
//! - Progress: `{"kind":"batch","id","model","total","done","failed","running"}` chat events
//!   (throttled) in the calling session's chat, plus a canvas-scoped `batch` host event.
//!   The tool returns at once; `await_batch` waits for results when an agent needs them.
//! - Packing: identical requests (same model + input, seeds aside) are merged into fewer
//!   requests via the model's `num_images` when its schema has one (up to its maximum).
//! - Per-item pipelines (`then`): each output flows into the next step (`pipeline.rs`) the
//!   moment it lands; the batch finishes when every chain has.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use falos_doc::{Actor, Command, Lineage, NodeKind, Parent, Props};
use futures_util::StreamExt;
use serde_json::{Value as Json, json};
use tokio::sync::{Semaphore, watch};

use crate::fal::{self, Backend, Job};
use crate::pipeline::Step;
use crate::host::{Event, Host, now_ms};

pub const SUBMIT_CONCURRENCY: usize = 64;
pub const DOWNLOAD_CONCURRENCY: usize = 16;
pub const FLUSH_EVERY: Duration = Duration::from_millis(150);
const POLL_MIN: Duration = Duration::from_millis(500);
const POLL_MAX: Duration = Duration::from_secs(2);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_JOBS: usize = 500;
/// Batches of this size or more get their own frame.
pub const FRAME_AT: usize = 8;
const CELL_GAP: f64 = 16.0;
const FRAME_PAD: f64 = 32.0;
const FRAME_TOP: f64 = 72.0;

/// One fal request of a batch; `nodes` are its placeholders (several with `num_images`).
#[derive(Clone)]
pub struct Item {
    pub model: String,
    pub input: Json,
    pub name: String,
    pub nodes: Vec<String>,
    /// Media inputs of the request (lineage parents).
    pub parents: Vec<Parent>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub running: usize,
    pub finished: bool,
    /// Per-item pipelines (`then`): chains running / done / failed.
    pub chains_running: usize,
    pub chains_done: usize,
    pub chains_failed: usize,
}

pub struct Batch {
    pub id: String,
    pub canvas: String,
    pub model: String,
    pub nodes: Vec<String>,
    pub frame: Option<String>,
    pub started: i64,
    progress: watch::Sender<Progress>,
    errors: Mutex<Vec<(String, String)>>,
    /// `then` steps per output, and each chain's outcome (`pipeline::run_chain`).
    pub then: Vec<Step>,
    chains: Mutex<Vec<Json>>,
}

impl Batch {
    pub fn progress(&self) -> Progress {
        self.progress.borrow().clone()
    }

    pub fn summary(&self) -> Json {
        let p = self.progress();
        json!({
            "batch": self.id, "model": self.model, "frame": self.frame,
            "total": p.total, "done": p.done, "failed": p.failed, "running": p.running,
            "finished": p.finished, "nodes": self.nodes,
            "then": (!self.then.is_empty()).then(|| json!({
                "running": p.chains_running, "done": p.chains_done, "failed": p.chains_failed,
                "chains": *self.chains.lock().unwrap(),
            })),
            "errors": self.errors.lock().unwrap().iter().take(10)
                .map(|(n, e)| json!({"node": n, "error": e})).collect::<Vec<_>>(),
            "seconds": ((now_ms() - self.started) as f64 / 1000.0 * 10.0).round() / 10.0,
        })
    }

    /// Wait until the batch finishes or `timeout` passes.
    pub async fn wait(&self, timeout: Duration) -> Progress {
        let mut rx = self.progress.subscribe();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|p| p.finished)).await;
        self.progress()
    }
}

#[derive(Default)]
pub struct Batches(Mutex<HashMap<String, Arc<Batch>>>);

impl Batches {
    pub fn get(&self, id: &str) -> Option<Arc<Batch>> {
        self.0.lock().unwrap().get(id).cloned()
    }

    /// Unfinished batches' `(generating, queued)` requests (chains count as generating).
    pub fn in_flight(&self) -> (usize, usize) {
        self.0.lock().unwrap().values().map(|b| b.progress()).filter(|p| !p.finished).fold((0, 0), |(g, q), p| {
            let queued = p.total.saturating_sub(p.done + p.failed + p.running);
            (g + p.running + p.chains_running, q + queued)
        })
    }
}

/// Requested batch, before placement.
pub struct Request {
    /// `(model, input, name)` per fal request.
    pub jobs: Vec<(String, Json, Option<String>)>,
    pub parent: Option<String>,
    pub near: Vec<String>,
    /// Label for the frame (default: the first prompt).
    pub label: Option<String>,
    /// Cell size; default from the input's aspect (`image_size`, `aspect_ratio`).
    pub cell: Option<(f64, f64)>,
    /// Media inputs shared by every job (lineage parents).
    pub parents: Vec<Parent>,
    /// Per-job media inputs (lineage parents), by job index (may be shorter than `jobs`).
    pub job_parents: Vec<Vec<Parent>>,
    /// Per-item pipeline run on every output as soon as it lands.
    pub then: Vec<Step>,
}

/// Cell size from a fal input's aspect hints (long side 320).
pub fn cell_for(input: &Json) -> (f64, f64) {
    let ratio = match input["image_size"].as_str().or(input["aspect_ratio"].as_str()) {
        Some(s) if s.contains("landscape_16_9") || s == "16:9" => 16.0 / 9.0,
        Some(s) if s.contains("landscape_4_3") || s == "4:3" => 4.0 / 3.0,
        Some(s) if s.contains("portrait_16_9") || s == "9:16" => 9.0 / 16.0,
        Some(s) if s.contains("portrait_4_3") || s == "3:4" => 3.0 / 4.0,
        Some("21:9") => 21.0 / 9.0,
        Some("3:2") => 1.5,
        Some("2:3") => 2.0 / 3.0,
        _ => match (input["image_size"]["width"].as_f64(), input["image_size"]["height"].as_f64()) {
            (Some(w), Some(h)) if w > 0.0 && h > 0.0 => w / h,
            _ => 1.0,
        },
    };
    if ratio >= 1.0 { (320.0, (320.0 / ratio).round()) } else { ((320.0 * ratio).round(), 320.0) }
}

/// Columns for `n` cells of `w`×`h` so the grid is roughly 16:10 overall.
pub fn columns(n: usize, w: f64, h: f64) -> usize {
    let c = ((n as f64) * (h + CELL_GAP) / (w + CELL_GAP) * 1.6).sqrt().ceil() as usize;
    c.clamp(1, n.max(1))
}

fn random_id(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        (0..5)
            .map(|_| format!("{:02x}", rand::random::<u8>()))
            .collect::<String>()
    )
}

/// Place all placeholders (one transaction), register the batch and start running it.
pub fn start(host: &Arc<Host>, actor: &Actor, session: Option<&str>, canvas: &str, req: Request) -> Result<Arc<Batch>> {
    if req.jobs.is_empty() {
        bail!("generate_batch needs at least one job");
    }
    let backend = fal::backend(host)?;
    for (m, _, _) in &req.jobs {
        fal::check_model(m)?;
    }
    let canvas = host.resolve_canvas(canvas)?;
    // Outputs per request: `num_images` (when the model supports it), else 1.
    let per: Vec<usize> = req
        .jobs
        .iter()
        .map(|(_, i, _)| i["num_images"].as_u64().unwrap_or(1).clamp(1, 16) as usize)
        .collect();
    let total: usize = per.iter().sum();
    if total > MAX_JOBS {
        bail!("at most {MAX_JOBS} outputs per batch (asked for {total}); split it");
    }
    let (cw, ch) = req.cell.unwrap_or_else(|| cell_for(&req.jobs[0].1));
    let cols = columns(total, cw, ch);
    let rows = total.div_ceil(cols);
    let grid_w = cols as f64 * (cw + CELL_GAP) - CELL_GAP;
    let grid_h = rows as f64 * (ch + CELL_GAP) - CELL_GAP;
    let framed = total >= FRAME_AT;
    let (bw, bh) = if framed { (grid_w + 2.0 * FRAME_PAD, grid_h + FRAME_TOP + FRAME_PAD) } else { (grid_w, grid_h) };
    let (bx, by) = host.find_space(&canvas, req.parent.as_deref(), &req.near, bw, bh)?;
    let model = {
        let mut ms: Vec<&str> = req.jobs.iter().map(|(m, _, _)| m.as_str()).collect();
        ms.dedup();
        if ms.len() == 1 { ms[0].to_string() } else { format!("{} models", ms.len()) }
    };
    let label = req
        .label
        .clone()
        .or_else(|| req.jobs[0].1["prompt"].as_str().map(|p| p.split_whitespace().take(4).collect::<Vec<_>>().join(" ")))
        .unwrap_or_else(|| model.clone());
    let frame_id = framed.then(|| random_id("b"));
    let mut cmds = vec![];
    if let Some(fid) = &frame_id {
        cmds.push(Command::CreateNode {
            id: Some(fid.clone()),
            kind: NodeKind::Frame,
            parent: req.parent.clone(),
            index: None,
            name: format!("{} ×{total}", label.chars().take(40).collect::<String>()),
            props: serde_json::from_value(json!({"x": bx, "y": by, "w": bw, "h": bh}))?,
            text: None,
        });
    }
    let (ox, oy, parent) = match &frame_id {
        Some(f) => (FRAME_PAD, FRAME_TOP, Some(f.clone())),
        None => (bx, by, req.parent.clone()),
    };
    let mut items = vec![];
    let mut k = 0;
    for (ji, ((m, input, name), n)) in req.jobs.iter().zip(&per).enumerate() {
        let mut ids = vec![];
        for j in 0..*n {
            let id = random_id("g");
            let (col, row) = (k % cols, k / cols);
            k += 1;
            let nm = name.clone().unwrap_or_else(|| {
                input["prompt"].as_str().unwrap_or(m).chars().take(48).collect()
            });
            let props: Props = serde_json::from_value(json!({
                "x": ox + col as f64 * (cw + CELL_GAP), "y": oy + row as f64 * (ch + CELL_GAP),
                "w": cw, "h": ch, "status": "queued", "model": m, "params": input,
            }))?;
            cmds.push(Command::CreateNode {
                id: Some(id.clone()),
                kind: NodeKind::Generation,
                parent: parent.clone(),
                index: None,
                name: if *n > 1 { format!("{nm} {}", j + 1) } else { nm },
                props,
                text: None,
            });
            ids.push(id);
        }
        let mut parents = req.parents.clone();
        parents.extend(req.job_parents.get(ji).cloned().unwrap_or_default());
        items.push(Item { model: m.clone(), input: input.clone(), name: name.clone().unwrap_or_default(), nodes: ids, parents });
    }
    host.mutate(&canvas, &actor.id, None, |c| c.apply(actor, None, &cmds))?;
    let nodes: Vec<String> = items.iter().flat_map(|i| i.nodes.clone()).collect();
    let (tx, _) = watch::channel(Progress { total, ..Default::default() });
    let batch = Arc::new(Batch {
        id: random_id("batch_"),
        canvas: canvas.clone(),
        model,
        nodes: nodes.clone(),
        frame: frame_id.clone(),
        started: now_ms(),
        progress: tx,
        errors: Mutex::default(),
        then: req.then,
        chains: Mutex::default(),
    });
    host.batches.0.lock().unwrap().insert(batch.id.clone(), batch.clone());
    if let Some(s) = session {
        let mut shown = nodes.clone();
        shown.extend(frame_id);
        host.activity(s, &canvas, "write", &shown);
    }
    let chat = session.and_then(|s| host.with_sessions(|ss| ss.get(s).map(|x| (x.chat.clone(), x.agent_label()))));
    let runner = Runner {
        host: host.clone(),
        batch: batch.clone(),
        backend,
        requester: actor.id.clone(),
        actor: actor.clone(),
        session: session.map(String::from),
        chat: chat.clone().and_then(|(c, _)| c),
        agent: chat.map(|(_, a)| a).unwrap_or_else(|| actor.id.clone()),
        state: Mutex::default(),
    };
    tokio::spawn(Arc::new(runner).run(items));
    Ok(batch)
}

/// `(status, queue position, error)` from the fal queue.
type JobStatus = (String, Option<u64>, Option<String>);

struct InFlight {
    item: Item,
    request: String,
    submit: Json,
    running: bool,
}

#[derive(Default)]
struct RunState {
    inflight: Vec<InFlight>,
    /// Converted / failed / running updates waiting for the next flush.
    pending: Vec<Command>,
    done: usize,
    failed: usize,
    running: usize,
    submitted_all: bool,
    downloads: usize,
    chains_running: usize,
    chains_done: usize,
    chains_failed: usize,
}

struct Runner {
    host: Arc<Host>,
    batch: Arc<Batch>,
    backend: Backend,
    requester: String,
    actor: Actor,
    session: Option<String>,
    chat: Option<String>,
    agent: String,
    state: Mutex<RunState>,
}

impl Runner {
    fn job(&self, item: &Item) -> Job {
        Job {
            host: self.host.clone(),
            canvas: self.batch.canvas.clone(),
            node: item.nodes[0].clone(),
            model: item.model.clone(),
            input: item.input.clone(),
            backend: self.backend.clone(),
            requester: self.requester.clone(),
            parents: item.parents.clone(),
        }
    }

    fn fail_item(&self, item: &Item, err: &str) {
        let mut st = self.state.lock().unwrap();
        for n in &item.nodes {
            st.failed += 1;
            st.pending.push(Command::UpdateNode {
                id: n.clone(),
                props: serde_json::from_value(json!({"status": "failed", "error": err.chars().take(300).collect::<String>()})).unwrap_or_default(),
            });
            self.batch.errors.lock().unwrap().push((n.clone(), err.chars().take(300).collect()));
        }
    }

    async fn run(self: Arc<Self>, items: Vec<Item>) {
        let started = tokio::time::Instant::now();
        let items = pack_items(&self.host.http, items).await;
        // Submit everything now; fal parallelizes server-side.
        let submitter = {
            let me = self.clone();
            tokio::spawn(async move {
                futures_util::stream::iter(items)
                    .for_each_concurrent(SUBMIT_CONCURRENCY, |item| {
                        let me = me.clone();
                        async move {
                            match me.job(&item).submit().await {
                                Ok((request, submit)) => me.state.lock().unwrap().inflight.push(InFlight {
                                    item,
                                    request,
                                    submit,
                                    running: false,
                                }),
                                Err(e) => me.fail_item(&item, &format!("submit: {e:#}")),
                            }
                        }
                    })
                    .await;
                me.state.lock().unwrap().submitted_all = true;
            })
        };
        let flusher = {
            let me = self.clone();
            tokio::spawn(async move { me.flush_loop().await })
        };
        let downloads = Arc::new(Semaphore::new(DOWNLOAD_CONCURRENCY));
        let mut interval = POLL_MIN;
        loop {
            tokio::time::sleep(interval).await;
            if started.elapsed() > TIMEOUT {
                let left: Vec<Item> = self.state.lock().unwrap().inflight.drain(..).map(|f| f.item).collect();
                for it in left {
                    self.fail_item(&it, "timed out");
                }
                break;
            }
            let snapshot: Vec<(String, Json, Item)> = self
                .state
                .lock()
                .unwrap()
                .inflight
                .iter()
                .map(|f| (f.request.clone(), f.submit.clone(), f.item.clone()))
                .collect();
            let statuses: Vec<(String, Item, Json, Result<JobStatus>)> =
                futures_util::stream::iter(snapshot)
                    .map(|(req, submit, item)| {
                        let job = self.job(&item);
                        async move {
                            let s = job.status(&req, &submit).await;
                            (req, item, submit, s)
                        }
                    })
                    .buffer_unordered(SUBMIT_CONCURRENCY)
                    .collect()
                    .await;
            let mut progressed = false;
            for (req, item, submit, s) in statuses {
                match s {
                    Ok((st, _, err)) => match st.as_str() {
                        "COMPLETED" | "OK" | "SUCCEEDED" => {
                            progressed = true;
                            self.take(&req);
                            if let Some(e) = err {
                                self.fail_item(&item, &e);
                                continue;
                            }
                            self.state.lock().unwrap().downloads += 1;
                            let me = self.clone();
                            let permits = downloads.clone();
                            tokio::spawn(async move {
                                let _permit = permits.acquire().await;
                                me.complete(&req, &submit, &item).await;
                                me.state.lock().unwrap().downloads -= 1;
                            });
                        }
                        "FAILED" | "ERROR" | "CANCELLED" => {
                            progressed = true;
                            self.take(&req);
                            self.fail_item(&item, &err.unwrap_or(st));
                        }
                        "IN_PROGRESS" | "RUNNING" => self.mark_running(&req),
                        _ => {}
                    },
                    // Transient status errors: keep polling.
                    Err(e) => tracing::debug!("batch status {req}: {e:#}"),
                }
            }
            interval = if progressed { POLL_MIN } else { (interval * 3 / 2).min(POLL_MAX) };
            let st = self.state.lock().unwrap();
            if st.submitted_all && st.inflight.is_empty() && st.downloads == 0 {
                break;
            }
        }
        let _ = submitter.await;
        // Let the flusher apply the last conversions and mark the batch finished.
        self.state.lock().unwrap().submitted_all = true;
        let _ = flusher.await;
    }

    fn take(&self, req: &str) {
        let mut st = self.state.lock().unwrap();
        if let Some(i) = st.inflight.iter().position(|f| f.request == req) {
            let f = st.inflight.remove(i);
            if f.running {
                st.running = st.running.saturating_sub(f.item.nodes.len());
            }
        }
    }

    fn mark_running(&self, req: &str) {
        let mut st = self.state.lock().unwrap();
        let Some(f) = st.inflight.iter_mut().find(|f| f.request == req && !f.running) else { return };
        f.running = true;
        let nodes = f.item.nodes.clone();
        st.running += nodes.len();
        for n in nodes {
            st.pending.push(Command::UpdateNode {
                id: n,
                props: serde_json::from_value(json!({"status": "running", "progress": 0.5})).unwrap_or_default(),
            });
        }
    }

    /// Start `then` on a landed output (after its conversion is flushed).
    fn start_chain(self: &Arc<Self>, node: String) {
        if self.batch.then.is_empty() {
            return;
        }
        self.state.lock().unwrap().chains_running += 1;
        let me = self.clone();
        tokio::spawn(async move {
            let canvas = me.batch.canvas.clone();
            let r = match crate::pipeline::wait_media(&me.host, &canvas, &node, Duration::from_secs(60)).await {
                Ok(()) => crate::pipeline::run_chain(&me.host, &me.actor, me.session.as_deref(), &canvas, &node, &me.batch.then).await,
                Err(e) => json!({"source": node, "nodes": [], "status": "failed", "error": format!("{e:#}")}),
            };
            let ok = r["status"] == "done";
            me.batch.chains.lock().unwrap().push(r);
            let mut st = me.state.lock().unwrap();
            st.chains_running -= 1;
            if ok { st.chains_done += 1 } else { st.chains_failed += 1 }
        });
    }

    async fn complete(self: &Arc<Self>, req: &str, submit: &Json, item: &Item) {
        let r: Result<()> = async {
            let result = self.job(item).result(req, submit).await?;
            let media = fal::find_all_media(&result);
            if media.is_empty() {
                bail!("no media in fal result");
            }
            let seed = result.get("seed").and_then(Json::as_i64);
            for (i, node) in item.nodes.iter().enumerate() {
                let Some(m) = media.get(i) else {
                    self.fail_item(&Item { nodes: vec![node.clone()], ..item.clone() }, "the model returned fewer outputs");
                    continue;
                };
                let asset = fal::download(
                    &self.host,
                    m,
                    Lineage {
                        seed,
                        fal_request_id: Some(req.to_string()),
                        parents: item.parents.clone(),
                        ..Lineage::generate(&item.model, item.input.clone(), &self.requester)
                    },
                )
                .await?;
                // Conversion commands are built at flush time (they read the node's current
                // spot); queue the asset + media.
                let conv = self.host.read(&self.batch.canvas, |c| fal::convert_cmds(c, node, m, &asset))??;
                let mut st = self.state.lock().unwrap();
                if !conv.is_empty() {
                    st.pending.push(Command::PutAsset(asset.clone()));
                    // Skip the PutAsset inside conv (already pushed) — keep delete+create.
                    st.pending.extend(conv.into_iter().skip(1));
                }
                st.done += 1;
                drop(st);
                self.start_chain(node.clone());
            }
            Ok(())
        }
        .await;
        if let Err(e) = r {
            self.fail_item(item, &format!("{e:#}"));
        }
    }

    /// Apply queued updates/conversions in one transaction every [`FLUSH_EVERY`]; emit
    /// throttled progress; finish when everything is resolved.
    async fn flush_loop(&self) {
        let mut last_progress = tokio::time::Instant::now() - PROGRESS_EVERY;
        let mut last_sent = (usize::MAX, 0, 0, 0, 0, 0);
        loop {
            tokio::time::sleep(FLUSH_EVERY).await;
            let (cmds, mut p) = {
                let mut st = self.state.lock().unwrap();
                let finished = st.submitted_all
                    && st.inflight.is_empty()
                    && st.downloads == 0
                    && st.chains_running == 0
                    && st.pending.is_empty()
                    && st.done + st.failed >= self.batch.progress().total;
                let p = Progress {
                    total: self.batch.progress().total,
                    done: st.done,
                    failed: st.failed,
                    running: st.running,
                    finished,
                    chains_running: st.chains_running,
                    chains_done: st.chains_done,
                    chains_failed: st.chains_failed,
                };
                (std::mem::take(&mut st.pending), p)
            };
            if !cmds.is_empty() {
                self.apply(cmds);
                // Report "finished" only after the last conversions are applied.
                p.finished = false;
            }
            let key = (p.done, p.failed, p.running, p.chains_running, p.chains_done, p.chains_failed);
            let changed = last_sent != key;
            let finished = p.finished;
            if finished || (changed && last_progress.elapsed() >= PROGRESS_EVERY) {
                last_progress = tokio::time::Instant::now();
                last_sent = key;
                self.report(&p);
            }
            self.batch.progress.send_replace(p);
            if finished {
                return;
            }
        }
    }

    /// Apply as the jobs actor, skipping commands whose node vanished (user deleted it).
    fn apply(&self, cmds: Vec<Command>) {
        let actor = fal::jobs_actor();
        let r = self.host.mutate(&self.batch.canvas, &actor.id, None, |c| {
            let live: Vec<Command> = cmds
                .into_iter()
                .filter(|cmd| match cmd {
                    Command::UpdateNode { id, .. } | Command::DeleteNode { id } => {
                        c.node(id).is_some_and(|n| n.kind == NodeKind::Generation)
                    }
                    _ => true,
                })
                .collect();
            // A CreateNode whose DeleteNode was dropped would clash: drop it too.
            let deleted: std::collections::HashSet<String> = live
                .iter()
                .filter_map(|c| match c {
                    Command::DeleteNode { id } => Some(id.clone()),
                    _ => None,
                })
                .collect();
            let live: Vec<Command> = live
                .into_iter()
                .filter(|cmd| match cmd {
                    Command::CreateNode { id: Some(id), .. } => deleted.contains(id),
                    _ => true,
                })
                .collect();
            if live.is_empty() {
                return Ok(());
            }
            c.apply(&actor, None, &live).map(|_| ())
        });
        if let Err(e) = r {
            tracing::warn!("batch {} flush failed: {e:#}", self.batch.id);
        }
    }

    fn report(&self, p: &Progress) {
        let ev = json!({
            "kind": "batch", "agent": self.agent, "id": self.batch.id, "model": self.batch.model,
            "total": p.total, "done": p.done, "failed": p.failed, "running": p.running,
            "finished": p.finished, "frame": self.batch.frame,
            "then": (!self.batch.then.is_empty()).then(|| json!({"running": p.chains_running, "done": p.chains_done, "failed": p.chains_failed})),
            "nodes": if p.finished { json!(self.batch.nodes) } else { Json::Null },
        });
        self.host.emit(Event::Batch {
            canvas: self.batch.canvas.clone(),
            batch: ev.clone(),
        });
        if let Some(chat) = &self.chat {
            let _ = self.host.chat.append(&self.host, chat, None, ev);
        }
    }
}

/// The model's `num_images` maximum from its fal input schema (None: no such param, or the
/// schema couldn't be read). Cached per process.
async fn num_images_max(http: &reqwest::Client, model: &str) -> Option<usize> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, Option<usize>>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(v) = cache.lock().unwrap().get(model) {
        return *v;
    }
    let base = std::env::var("FALOS_FAL_SCHEMA_URL").unwrap_or_else(|_| "https://fal.ai/api/openapi/queue/openapi.json".into());
    let fetched: Option<Json> = async {
        let url = format!("{base}?endpoint_id={}", model.replace('&', "%26").replace('?', "%3F"));
        let r = http.get(url).timeout(Duration::from_secs(4)).send().await.ok()?;
        r.error_for_status().ok()?.json().await.ok()
    }
    .await;
    let doc = fetched?; // not cached: retry next batch
    let max = doc["components"]["schemas"]
        .as_object()
        .and_then(|s| s.iter().find(|(k, _)| k.ends_with("Input")).map(|(_, v)| v.clone()))
        .and_then(|input| input["properties"].get("num_images").cloned())
        .map(|p| p["maximum"].as_u64().unwrap_or(4).clamp(1, 16) as usize);
    cache.lock().unwrap().insert(model.to_string(), max);
    max
}

/// Fill `num_images` slots: merge identical single-output requests (same model, input and
/// parents; seeds aside) into requests of up to `max_of(model)` outputs each.
pub(crate) fn pack(items: Vec<Item>, max_of: &dyn Fn(&str) -> Option<usize>) -> Vec<Item> {
    let key = |it: &Item| {
        let mut i = it.input.clone();
        if let Some(o) = i.as_object_mut() {
            o.remove("seed");
            o.remove("num_images");
        }
        (it.model.clone(), i.to_string())
    };
    let mut out: Vec<Item> = vec![];
    // Open packed request per key: index into `out`.
    let mut open: HashMap<(String, String), usize> = HashMap::new();
    for it in items {
        let max = max_of(&it.model).unwrap_or(1);
        if max < 2 || it.nodes.len() != 1 {
            out.push(it);
            continue;
        }
        let k = key(&it);
        match open.get(&k).copied() {
            Some(i) if out[i].nodes.len() < max && out[i].parents == it.parents => {
                out[i].nodes.extend(it.nodes);
                let n = out[i].nodes.len();
                out[i].input["num_images"] = json!(n);
            }
            _ => {
                open.insert(k, out.len());
                out.push(it);
            }
        }
    }
    out
}

async fn pack_items(http: &reqwest::Client, items: Vec<Item>) -> Vec<Item> {
    // Only worth a schema lookup when some model has identical requests to merge.
    let mut seen = std::collections::HashSet::new();
    let mut models = std::collections::HashSet::new();
    for it in &items {
        let mut i = it.input.clone();
        if let Some(o) = i.as_object_mut() {
            o.remove("seed");
        }
        if it.nodes.len() == 1 && !seen.insert((it.model.clone(), i.to_string())) {
            models.insert(it.model.clone());
        }
    }
    if models.is_empty() {
        return items;
    }
    let mut max = HashMap::new();
    for m in models {
        let v = num_images_max(http, &m).await;
        max.insert(m, v);
    }
    let before = items.len();
    let packed = pack(items, &|m| max.get(m).copied().flatten());
    if packed.len() < before {
        tracing::info!("batch: packed {before} requests into {} via num_images", packed.len());
    }
    packed
}

/// Parse `generate_batch` arguments into jobs: explicit `jobs`, or `model` + `input` ×
/// `count` with prompt `variations` (cycled) and seeds varied per job when `input.seed` is set.
pub fn jobs_from(
    jobs: Option<Vec<Json>>,
    model: Option<String>,
    input: Option<Json>,
    count: Option<usize>,
    variations: Option<Vec<String>>,
) -> Result<Vec<(String, Json, Option<String>)>> {
    if let Some(js) = jobs.filter(|j| !j.is_empty()) {
        return js
            .into_iter()
            .map(|j| {
                let m = j["model"].as_str().map(String::from).or(model.clone()).ok_or_else(|| anyhow!("each job needs a model"))?;
                let mut inp = input.clone().unwrap_or_else(|| json!({}));
                if let (Some(base), Some(o)) = (inp.as_object_mut(), j["input"].as_object()) {
                    base.extend(o.clone());
                }
                Ok((m, inp, j["name"].as_str().map(String::from)))
            })
            .collect();
    }
    let model = model.ok_or_else(|| anyhow!("pass `jobs`, or `model` + `input` (+ `count`)"))?;
    let input = input.unwrap_or_else(|| json!({}));
    let vars = variations.unwrap_or_default();
    let n = count.unwrap_or(vars.len().max(1)).max(1);
    Ok((0..n)
        .map(|i| {
            let mut inp = input.clone();
            if !vars.is_empty() {
                inp["prompt"] = json!(vars[i % vars.len()]);
            }
            if let Some(s) = inp["seed"].as_i64() {
                inp["seed"] = json!(s + i as i64);
            }
            (model.clone(), inp, None)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_and_cells() {
        assert_eq!(cell_for(&json!({"image_size": "landscape_16_9"})), (320.0, 180.0));
        assert_eq!(cell_for(&json!({"aspect_ratio": "9:16"})), (180.0, 320.0));
        assert_eq!(cell_for(&json!({})), (320.0, 320.0));
        let c = columns(200, 320.0, 320.0);
        assert!((15..=20).contains(&c), "{c}");
        assert_eq!(columns(1, 320.0, 320.0), 1);
    }

    #[test]
    fn jobs_from_count_and_variations() {
        let j = jobs_from(None, Some("m".into()), Some(json!({"prompt": "p", "seed": 10})), Some(3), None).unwrap();
        assert_eq!(j.len(), 3);
        assert_eq!(j[2].1["seed"], 12);
        let j = jobs_from(None, Some("m".into()), Some(json!({})), Some(4), Some(vec!["a".into(), "b".into()])).unwrap();
        assert_eq!(j[3].1["prompt"], "b");
        let j = jobs_from(Some(vec![json!({"input": {"prompt": "x"}})]), Some("m".into()), None, None, None).unwrap();
        assert_eq!((j[0].0.as_str(), j[0].1["prompt"].as_str()), ("m", Some("x")));
        assert!(jobs_from(None, None, None, None, None).is_err());
    }

    #[test]
    fn packs_identical_requests_into_num_images() {
        let item = |prompt: &str, seed: i64, node: &str| Item {
            model: "m".into(),
            input: json!({"prompt": prompt, "seed": seed}),
            name: String::new(),
            nodes: vec![node.into()],
            parents: vec![],
        };
        let items: Vec<Item> = (0..10).map(|i| item("p", i, &format!("n{i}"))).chain([item("other", 0, "x")]).collect();
        let packed = pack(items.clone(), &|_| Some(4));
        // 10 identical → 4 + 4 + 2; the distinct prompt stays alone.
        assert_eq!(packed.iter().map(|i| i.nodes.len()).collect::<Vec<_>>(), vec![4, 4, 2, 1]);
        assert_eq!(packed[0].input["num_images"], 4);
        assert_eq!(packed[2].nodes, vec!["n8".to_string(), "n9".to_string()]);
        // No num_images on the model: untouched.
        assert_eq!(pack(items, &|_| None).len(), 11);
    }
}
