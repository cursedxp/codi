//! codi-core — reusable library for the codi local-first coding agent.
//!
//! Modules:
//! - [`config`]: the `codi.toml` schema, loading and layered merge.
//! - [`routing`]: local/cloud routing policy and the heuristic classifier.
//! - [`engine`]: maps config to a session-scoped Goose config and launches it.
//! - [`review`]: self-review of the agent's own diff.

pub mod config;
pub mod engine;
pub mod mcp;
pub mod ollama;
pub mod review;
pub mod routing;
pub mod setup;
pub mod signals;
pub mod standards;
