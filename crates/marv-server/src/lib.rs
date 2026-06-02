//! # marv-server — JSON-RPC agent-protocol server (milestone M3)
//!
//! Wraps `marv-db` queries in the agent-facing JSON-RPC 2.0 protocol over stdio
//! or a local socket. Exposes `check`, `typeAt`, `errorSet`, `effects`,
//! `canonical`, `core`, `hash`, and the rest of the method catalog. See
//! `spec/03-compiler-protocol.md` §3.
//!
//! Acceptance gate (M3): expose the read-only query methods over JSON-RPC.
//!
//! This crate is currently a compiling stub — no server is implemented yet.

/// Placeholder until the protocol server lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M3"
}
