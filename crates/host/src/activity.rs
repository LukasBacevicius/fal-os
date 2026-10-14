//! Agent activity on the canvas (UX v1 §4, after Paper): every agent session has a working
//! set of nodes it read or changed. Tool calls emit `activity` events (`read` / `write` /
//! `edit` / `delete`); entries expire after 30 s or when the agent calls `finish_working`,
//! which emits `activity.clear`. Also: presence state (thinking / writing / idle), the
//! humans' selections (`get_selection`), and free-space placement for new media.

use std::time::Duration;

use falos_doc::Canvas;
use serde_json::{Value as Json, json};

use crate::host::{Event, Host, now_ms};

/// Working-set entries expire after this long without activity.
pub const ACTIVITY_TTL: Duration = Duration::from_secs(30);
/// Presence state drops to `idle` after this long without activity.
pub const IDLE_AFTER: Duration = Duration::from_secs(10);
/// Max node ids per `activity` event.
const MAX_NODES: usize = 50;
/// Gap between placed nodes and their neighbours (canvas units).
pub const GAP: f64 = 40.0;
/// A generating tile with no live job for this long turns `failed` (interrupted).
pub const ORPHAN_GRACE: Duration = Duration::from_secs(60);
/// How often generating tiles are checked for a live job.
const GEN_SWEEP_EVERY: Duration = Duration::from_secs(10);
/// The error an interrupted generating tile shows.
pub const INTERRUPTED: &str = "interrupted: its job stopped (host restart or crash) · rerun it";

/// Normalize an MCP `clientInfo.name` to the connect-flow client id.
pub fn normalize_client(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let known = [
        ("claude", "claude"),
        ("codex", "codex"),
        ("cursor", "cursor"),
        ("gemini", "gemini"),
        ("opencode", "opencode"),
    ];
    for (needle, id) in known {
        if n.starts_with(needle) {
            return id.into();
        }
    }
    let mut out = String::new();
    for c in n.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(24).collect();
    if out.is_empty() { "agent".into() } else { out }
}

impl Host {
    /// Record that `session` did `op` (`read` | `write` | `edit` | `delete`) on `nodes` and
    /// emit an `activity` event. A no-op for unknown sessions or empty node lists.
    pub fn activity(&self, session: &str, canvas: &str, op: &str, nodes: &[String]) {
        if nodes.is_empty() {
            return;
        }
        let Ok(canvas) = self.resolve_canvas(canvas) else {
            return;
        };
        let now = now_ms();
        let nodes: Vec<String> = nodes.iter().take(MAX_NODES).cloned().collect();
        let out = self.with_sessions(|ss| {
            let s = ss.get_mut(session)?;
            for n in &nodes {
                if op == "delete" {
                    s.working.remove(&(canvas.clone(), n.clone()));
                } else {
                    s.working.insert((canvas.clone(), n.clone()), now);
                }
            }
            let state = if op == "read" { "thinking" } else { "writing" };
            let changed = s.state != state;
            s.state = state.into();
            s.presence.last_activity = now;
            let ev = Event::Activity {
                canvas: canvas.clone(),
                session: session.into(),
                agent: s.agent_label(),
                chat: s.chat.clone(),
                color: s.display_color(),
                op: op.into(),
                nodes: nodes.clone(),
                at: now,
            };
            Some((ev, changed.then(|| s.info())))
        });
        if let Some((ev, info)) = out {
            self.emit(ev);
            if let Some(info) = info {
                self.emit(Event::Presence { session: info });
            }
        }
    }

    /// `finish_working`: drop `ids` (all when `None`) from the session's working set.
    pub fn finish_working(&self, session: &str, ids: Option<&[String]>) -> anyhow::Result<usize> {
        let n = self.with_session(session, |s| {
            let before = s.working.len();
            match ids {
                Some(ids) => s.working.retain(|(_, n), _| !ids.contains(n)),
                None => s.working.clear(),
            }
            before - s.working.len()
        })?;
        self.emit(Event::ActivityClear {
            session: session.into(),
            nodes: ids.map(|i| i.to_vec()),
        });
        Ok(n)
    }

    /// Once a second: expire working sets, idle presence, ext chats.
    pub(crate) fn sweep(&self) {
        let now = now_ms();
        let ttl = ACTIVITY_TTL.as_millis() as i64;
        let idle = IDLE_AFTER.as_millis() as i64;
        let (clears, presences) = self.with_sessions(|ss| {
            let mut clears = vec![];
            let mut presences = vec![];
            for s in ss.values_mut() {
                let mut expired: Vec<String> = s
                    .working
                    .iter()
                    .filter(|(_, t)| now - **t >= ttl)
                    .map(|((_, n), _)| n.clone())
                    .collect();
                if !expired.is_empty() {
                    s.working.retain(|_, t| now - *t < ttl);
                    expired.sort();
                    expired.dedup();
                    clears.push(Event::ActivityClear {
                        session: s.id.clone(),
                        nodes: Some(expired),
                    });
                }
                if s.state != "idle" && now - s.presence.last_activity >= idle {
                    s.state = "idle".into();
                    presences.push(Event::Presence { session: s.info() });
                }
            }
            (clears, presences)
        });
        for e in clears.into_iter().chain(presences) {
            self.emit(e);
        }
        self.chat.sweep(self);
        let last = self.gen_swept.load(std::sync::atomic::Ordering::Relaxed);
        if now - last >= GEN_SWEEP_EVERY.as_millis() as i64 {
            self.gen_swept.store(now, std::sync::atomic::Ordering::Relaxed);
            self.sweep_generations(now, ORPHAN_GRACE.as_millis() as i64);
        }
    }

    /// The LED tile watchdog: a generation node that isn't `failed` must have an owner, a
    /// job in flight or a batch that will start one. One without for `grace` ms (the host
    /// restarted mid-job, a task died) turns `failed` with [`INTERRUPTED`], so no tile spins
    /// forever. Only canvases already loaded are checked. Returns the nodes it failed.
    pub fn sweep_generations(&self, now: i64, grace: i64) -> Vec<String> {
        let mut live: std::collections::HashSet<String> = self.jobs.lock().unwrap().keys().cloned().collect();
        live.extend(self.batches.live_nodes());
        let mut seen = std::collections::HashSet::new();
        let mut expired = Vec::new();
        for canvas in self.loaded_canvases() {
            let gens: Vec<String> = self
                .read(&canvas, |c| {
                    c.nodes()
                        .into_iter()
                        .filter(|n| n.kind == falos_doc::NodeKind::Generation)
                        .filter(|n| n.props.get("status").and_then(|s| s.as_str()) != Some("failed"))
                        .map(|n| n.id)
                        .collect()
                })
                .unwrap_or_default();
            let mut orphans = self.gen_orphans.lock().unwrap();
            for id in gens.into_iter().filter(|id| !live.contains(id)) {
                let key = (canvas.clone(), id);
                let first = *orphans.entry(key.clone()).or_insert(now);
                seen.insert(key.clone());
                if now - first >= grace {
                    expired.push(key);
                }
            }
        }
        self.gen_orphans.lock().unwrap().retain(|k, _| seen.contains(k) && !expired.contains(k));
        let actor = crate::fal::jobs_actor();
        let mut failed = Vec::new();
        for (canvas, node) in expired {
            let props: falos_doc::Props = serde_json::from_value(json!({"status": "failed", "error": INTERRUPTED})).unwrap_or_default();
            let ok = self
                .mutate(&canvas, &actor.id, None, |c| {
                    if c.node(&node).is_none_or(|n| n.kind != falos_doc::NodeKind::Generation) {
                        return Ok(false);
                    }
                    c.apply(&actor, None, &[falos_doc::Command::UpdateNode { id: node.clone(), props }])?;
                    Ok(true)
                })
                .unwrap_or(false);
            if ok {
                tracing::warn!("generation {node} had no live job; marked interrupted");
                self.emit(Event::Job {
                    canvas: canvas.clone(),
                    node: node.clone(),
                    model: String::new(),
                    status: "failed".into(),
                    queue_position: None,
                    error: Some(INTERRUPTED.into()),
                    cost: None,
                });
                failed.push(node);
            }
        }
        failed
    }

    /// After every doc mutation: connect-flow `agent.first_write` and chat turn tracking.
    pub(crate) fn after_mutation(
        &self,
        canvas: &str,
        actor: &str,
        created: &[String],
        touched: &[String],
    ) {
        let hits: Vec<(String, Option<String>, Option<String>, bool)> = self.with_sessions(|ss| {
            ss.values_mut()
                .filter(|s| s.actor.id == actor)
                .map(|s| {
                    let first = !s.wrote;
                    s.wrote = true;
                    (s.id.clone(), s.chat.clone(), s.client.clone(), first)
                })
                .collect()
        });
        for (session, chat, client, first) in hits {
            if first && let Some(client) = client {
                let nodes = if created.is_empty() { touched } else { created };
                self.emit(Event::AgentFirstWrite {
                    client,
                    session: session.clone(),
                    canvas: canvas.into(),
                    nodes: nodes.to_vec(),
                });
            }
            if let Some(chat) = chat.filter(|c| !c.starts_with("ext:")) {
                self.chat.track_mutation(self, &chat, canvas, actor, touched);
            }
        }
    }

    /// Attach a session to a chat (its color becomes the chat's).
    pub fn bind_session_chat(&self, session: &str, chat: &str, client: Option<String>) {
        let color = self.chat.color_of(self, chat);
        if let Ok(info) = self.with_session(session, |s| {
            s.chat = Some(chat.to_string());
            s.chat_color = color;
            if client.is_some() {
                s.client = client;
            }
            s.info()
        }) {
            self.emit(Event::Presence { session: info });
        }
    }

    /// The runtime told us which subagent a session belongs to.
    pub fn set_session_agent(&self, session: &str, agent: &str, parent: Option<&str>) {
        if let Ok(info) = self.with_session(session, |s| {
            s.agent = Some(agent.chars().take(40).collect());
            s.parent = parent.map(|p| p.chars().take(40).collect());
            s.info()
        }) {
            self.emit(Event::Presence { session: info });
        }
    }

    /// A human client's selection changed.
    pub fn set_selection(&self, session: &str, nodes: Vec<String>) {
        let nodes: Vec<String> = nodes.into_iter().take(200).collect();
        let changed = self
            .with_session(session, |s| {
                let changed = s.selection != nodes;
                s.selection = nodes.clone();
                changed
            })
            .unwrap_or(false);
        self.human_seen.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        if changed {
            crate::director::on_selection(self, session, &nodes);
        }
    }

    /// Node ids the humans on `canvas` have selected (all humans; deduplicated).
    pub fn selected_nodes(&self, canvas: &str) -> Vec<String> {
        let mut out: Vec<String> = self.with_sessions(|ss| {
            ss.values()
                .filter(|s| {
                    s.actor.kind == falos_doc::ActorKind::Human
                        && s.presence.canvas.as_deref() == Some(canvas)
                })
                .flat_map(|s| s.selection.clone())
                .collect()
        });
        out.dedup();
        out
    }

    /// `get_selection`: every human's current selection with node rows.
    pub fn selections(&self) -> Json {
        let humans: Vec<(String, String, Option<String>, Vec<String>)> = self.with_sessions(|ss| {
            let mut v: Vec<_> = ss
                .values()
                .filter(|s| s.actor.kind == falos_doc::ActorKind::Human)
                .map(|s| {
                    (
                        s.actor.id.clone(),
                        s.actor.name.clone(),
                        s.presence.canvas.clone(),
                        s.selection.clone(),
                    )
                })
                .collect();
            v.sort();
            v
        });
        let rows: Vec<Json> = humans
            .into_iter()
            .map(|(actor, name, canvas, sel)| {
                let nodes: Vec<Json> = canvas
                    .as_deref()
                    .and_then(|c| {
                        self.read(c, |cv| {
                            sel.iter()
                                .filter_map(|id| {
                                    let n = cv.node(id)?;
                                    let row = cv
                                        .read(&falos_doc::ReadOpts {
                                            root: Some(id.clone()),
                                            depth: Some(0),
                                            annotations: false,
                                            ..Default::default()
                                        })
                                        .unwrap_or_default();
                                    Some(json!({
                                        "id": n.id, "type": n.kind.to_string(), "name": n.name,
                                        "row": crate::context::untrusted(row.trim_end()),
                                    }))
                                })
                                .collect()
                        })
                        .ok()
                    })
                    .unwrap_or_default();
                json!({"actor": format!("@{actor}"), "name": name, "canvas": canvas, "nodes": nodes})
            })
            .collect();
        json!({ "selections": rows })
    }

    /// Free spot (`x, y` in `parent`'s coordinates) for a `w`×`h` node next to `near`
    /// (falls back to the humans' selection, then to the existing content).
    pub fn find_space(
        &self,
        canvas: &str,
        parent: Option<&str>,
        near: &[String],
        w: f64,
        h: f64,
    ) -> anyhow::Result<(f64, f64)> {
        let canvas = self.resolve_canvas(canvas)?;
        let selected = self.selected_nodes(&canvas);
        self.read(&canvas, |c| {
            let mut anchors: Vec<[f64; 4]> =
                near.iter().filter_map(|id| rect_in(c, id, parent)).collect();
            if anchors.is_empty() {
                anchors = selected.iter().filter_map(|id| rect_in(c, id, parent)).collect();
            }
            free_spot(&siblings(c, parent), &anchors, w, h)
        })
    }
}

/// Local rects of `parent`'s children (top-level nodes when `None`).
fn siblings(c: &Canvas, parent: Option<&str>) -> Vec<[f64; 4]> {
    match parent {
        Some(p) => c
            .node(p)
            .map(|n| n.children)
            .unwrap_or_default()
            .iter()
            .filter_map(|id| c.node(id))
            .map(|n| [n.x, n.y, n.w, n.h])
            .collect(),
        None => c
            .nodes()
            .into_iter()
            .filter(|n| n.parent.is_none())
            .map(|n| [n.x, n.y, n.w, n.h])
            .collect(),
    }
}

/// Rect of `id` (or of its ancestor that is a direct child of `parent`) in `parent`'s space.
fn rect_in(c: &Canvas, id: &str, parent: Option<&str>) -> Option<[f64; 4]> {
    let mut n = c.node(id)?;
    for _ in 0..64 {
        if n.parent.as_deref() == parent {
            return Some([n.x, n.y, n.w, n.h]);
        }
        n = c.node(n.parent.as_deref()?)?;
    }
    None
}

fn overlaps(a: [f64; 4], b: [f64; 4], gap: f64) -> bool {
    a[0] < b[0] + b[2] + gap && b[0] < a[0] + a[2] + gap && a[1] < b[1] + b[3] + gap && b[1] < a[1] + a[3] + gap
}

fn bbox(rs: &[[f64; 4]]) -> Option<[f64; 4]> {
    let x0 = rs.iter().map(|r| r[0]).fold(f64::INFINITY, f64::min);
    let y0 = rs.iter().map(|r| r[1]).fold(f64::INFINITY, f64::min);
    let x1 = rs.iter().map(|r| r[0] + r[2]).fold(f64::NEG_INFINITY, f64::max);
    let y1 = rs.iter().map(|r| r[1] + r[3]).fold(f64::NEG_INFINITY, f64::max);
    x0.is_finite().then_some([x0, y0, x1 - x0, y1 - y0])
}

/// Pick the free position closest to the anchors (right of / below / left of / above them
/// or of any obstacle), never overlapping `obstacles` (with [`GAP`]). Without anchors, the
/// spot goes right of the existing content (`(0, 0)` on an empty canvas).
pub fn free_spot(obstacles: &[[f64; 4]], anchors: &[[f64; 4]], w: f64, h: f64) -> (f64, f64) {
    let Some(content) = bbox(obstacles) else {
        return bbox(anchors).map_or((0.0, 0.0), |a| (a[0] + a[2] + GAP, a[1]));
    };
    let anchor = bbox(anchors).unwrap_or([content[0] + content[2], content[1], 0.0, 0.0]);
    let mut cands: Vec<(f64, f64)> = vec![];
    for r in obstacles.iter().chain(anchors).chain(std::iter::once(&anchor)) {
        cands.push((r[0] + r[2] + GAP, r[1])); // right
        cands.push((r[0], r[1] + r[3] + GAP)); // below
        cands.push((r[0] - GAP - w, r[1])); // left
        cands.push((r[0], r[1] - GAP - h)); // above
    }
    let (ax, ay) = (anchor[0] + anchor[2] * 0.5, anchor[1] + anchor[3] * 0.5);
    let free = |p: &(f64, f64)| !obstacles.iter().any(|o| overlaps([p.0, p.1, w, h], *o, GAP - 0.01));
    let score = |p: &(f64, f64)| {
        let (cx, cy) = (p.0 + w * 0.5, p.1 + h * 0.5);
        // Prefer reading order (right, then below) over left/above at equal distance.
        let penalty = if p.0 + w <= anchor[0] || p.1 + h <= anchor[1] { 1.25 } else { 1.0 };
        ((cx - ax).powi(2) + (cy - ay).powi(2)).sqrt() * penalty
    };
    cands
        .into_iter()
        .filter(free)
        .min_by(|a, b| score(a).total_cmp(&score(b)))
        .unwrap_or((content[0] + content[2] + GAP, content[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_normalize() {
        assert_eq!(normalize_client("claude-code"), "claude");
        assert_eq!(normalize_client("codex-mcp-client"), "codex");
        assert_eq!(normalize_client("cursor-vscode"), "cursor");
        assert_eq!(normalize_client("gemini-cli-mcp-client"), "gemini");
        assert_eq!(normalize_client("opencode"), "opencode");
        assert_eq!(normalize_client("My Tool 2"), "my-tool-2");
        assert_eq!(normalize_client(""), "agent");
    }

    #[test]
    fn free_spot_never_overlaps_and_hugs_the_anchor() {
        assert_eq!(free_spot(&[], &[], 100.0, 100.0), (0.0, 0.0));
        let a = [0.0, 0.0, 200.0, 200.0];
        let b = [240.0, 0.0, 200.0, 200.0];
        // Next to `a`, but its right side is taken by `b`: below `a`.
        let p = free_spot(&[a, b], &[a], 200.0, 200.0);
        assert_eq!(p, (0.0, 240.0));
        // Without anchors: right of everything.
        let p = free_spot(&[a, b], &[], 100.0, 100.0);
        assert!(p.0 >= 480.0, "{p:?}");
        // Dense field: still no overlap.
        let obstacles: Vec<[f64; 4]> = (0..25)
            .map(|i| [(i % 5) as f64 * 150.0, (i / 5) as f64 * 150.0, 120.0, 120.0])
            .collect();
        let p = free_spot(&obstacles, &[obstacles[12]], 300.0, 300.0);
        assert!(obstacles.iter().all(|o| !overlaps([p.0, p.1, 300.0, 300.0], *o, 0.0)), "{p:?}");
    }
}
