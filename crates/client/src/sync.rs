//! The seam between the client and a sync host (`crates/host`, local or cloud room).
//!
//! The client owns a full Loro replica of the canvas. A [`SyncTransport`] moves three kinds of
//! traffic; the transport decides the wire format (WebSocket frames, Tauri IPC, …):
//!
//! - **doc updates** (Loro update bytes) both ways. After every local transaction the client
//!   sends `export_updates(last_sent_vv)`; incoming updates are imported and flow through the
//!   same incremental [`crate::scene::Scene`] path as local edits. A snapshot can bootstrap.
//! - **presence**: cursors, viewport, selection and a status line per actor (ephemeral, not in
//!   the doc). Agents' cursors render in the engine overlay with DOM name labels.
//! - **claims / jobs**: host-owned ephemeral state (soft leases on nodes, generation jobs with
//!   progress and cost), passed through to the UI as JSON until their schema settles.
//!
//! [`LocalOnly`] is the transport used today: the doc lives in the browser (persisted by the
//! web app to IndexedDB, exportable as a file) and nothing leaves the page. In the browser a
//! transport can also be implemented in JS and attached with `CanvasApp.setTransport`.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// One actor's ephemeral presence on a canvas. Coordinates are world units.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    /// Actor id (`claude`, `codex`, `fal-agent`, `lukas`, …).
    pub actor: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    /// CSS color for the cursor and label, e.g. `#d97757`.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub cursor: Option<[f32; 2]>,
    /// Viewport `[x, y, w, h]` in world units.
    #[serde(default)]
    pub viewport: Option<[f32; 4]>,
    #[serde(default)]
    pub selection: Vec<String>,
    /// Status line, e.g. "generating 4 variations…".
    #[serde(default)]
    pub status: Option<String>,
}

/// Client → host.
#[derive(Clone, Debug, PartialEq)]
pub enum Outgoing {
    /// Loro update bytes for changes the host has not seen.
    Update(Vec<u8>),
    Presence(Presence),
}

/// Host → client.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// Loro update bytes (or a snapshot: Loro imports both).
    Update(Vec<u8>),
    /// Full presence list (replaces the previous one).
    Presence(Vec<Presence>),
    Claims(Json),
    Jobs(Json),
}

/// Moves doc updates, presence, claims and jobs between the client and a host.
pub trait SyncTransport {
    fn send(&mut self, msg: Outgoing);
    /// Messages received since the last poll (called once per tick).
    fn poll(&mut self) -> Vec<Incoming>;
    fn connected(&self) -> bool;
}

/// No host: the canvas is local to this page.
#[derive(Default)]
pub struct LocalOnly;

impl SyncTransport for LocalOnly {
    fn send(&mut self, _msg: Outgoing) {}
    fn poll(&mut self) -> Vec<Incoming> {
        Vec::new()
    }
    fn connected(&self) -> bool {
        false
    }
}
