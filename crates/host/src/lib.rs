//! fal-os host: the canvas authority. Owns a project on disk, open canvases, sessions
//! (actors with presence), soft claims and an event bus; serves the canvas MCP server and
//! the client sync WebSocket on one loopback HTTP server.

pub mod auth;
pub mod chat;
pub mod context;
pub mod fal;
pub mod fal_relay;
pub mod host;
pub mod knowledge;
pub mod mcp;
pub mod register;
pub mod runtime;
pub mod server;
pub mod sync;

pub use host::{Event, Host, HostOptions};
