//! fal-os canvas client: the wasm API the web UI talks to.
//!
//! ```text
//!   UI (React, apps/web) ──calls──▶ CanvasApp (wasm-bindgen, web/)
//!        ▲                              │ owns
//!        │ events (JSON)                ├─ falos_doc::Canvas  (Loro replica, actor transactions)
//!        │                              │     └─ ChangeFeed (Loro subscription → DocChanges)
//!        │                              ├─ Scene  (doc nodes ↔ engine handles, incremental)
//!        │                              ├─ Interaction (tools, drag, marquee, annotate)
//!        │                              ├─ falos_engine::Renderer (WebGPU, tiers, overlay)
//!        │                              ├─ media (asset blobs → ImageBitmaps / <video>)
//!        └──────────────────────────────└─ SyncTransport (LocalOnly now; host later)
//! ```
//!
//! Everything except `web` is platform-free and unit-tested natively.

pub mod api;
pub mod interact;
pub mod overlay;
pub mod scene;
pub mod snap;
pub mod sync;

#[cfg(target_arch = "wasm32")]
mod web;
