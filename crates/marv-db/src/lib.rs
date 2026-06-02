//! # marv-db — incremental query database (milestone M3)
//!
//! The salsa-backed, demand-driven incremental query engine that is the backbone
//! of the agent protocol. Each compiler phase (parse → lower → typecheck →
//! effects/errors → verify) is a query, cached and invalidated by edit. See
//! `spec/03-compiler-protocol.md` §1 (architecture).
//!
//! Acceptance gate (M3): wire the query database under `marv-server`.
//!
//! This crate is currently a compiling stub — no query database is wired yet.

/// Placeholder until the query engine lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M3"
}
