pub mod agent;
pub mod backup;
pub mod claude;
pub mod codex;
pub mod context;
#[cfg(feature = "desktop")]
pub mod desktop;
pub mod images;
pub mod mcp;
pub mod memory;
pub mod providers;
pub mod state;
pub mod transcription;
pub mod voice;
pub mod web;
#[cfg(feature = "desktop")]
mod workspace_bridge;
// Parsing of the workspace reply envelope is pure text handling, so it stays
// compiled (and tested) without the desktop feature.
mod workspace_relay;
