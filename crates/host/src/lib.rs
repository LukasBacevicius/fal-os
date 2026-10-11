//! fal-os host: the canvas authority. Owns a project on disk, open canvases, sessions
//! (actors with presence), soft claims and an event bus; serves the canvas MCP server and
//! the client sync WebSocket on one loopback HTTP server.

pub mod activity;
pub mod assets;
pub mod auth;
pub mod batch;
pub mod chat;
pub mod context;
pub mod fal;
pub mod fal_relay;
pub mod host;
pub mod knowledge;
pub mod mcp;
pub mod presence;
pub mod register;
pub mod relay;
pub mod runtime;
pub mod server;
pub mod sync;
pub mod transcript;
pub mod workspace;

pub use host::{Event, Host, HostOptions};
