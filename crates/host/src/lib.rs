//! fal-os host: the canvas authority. Owns a project on disk, open canvases, sessions
//! (actors with presence), soft claims and an event bus; serves the canvas MCP server and
//! the client sync WebSocket on one loopback HTTP server.

pub mod assets;
pub mod context;
pub mod fal;
pub mod host;
pub mod knowledge;
pub mod mcp;
pub mod presence;
pub mod register;
pub mod server;
pub mod sync;

pub use host::{Event, Host, HostOptions};
