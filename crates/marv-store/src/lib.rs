//! # marv-store — content-addressed code store (milestone M7)
//!
//! The content-addressed code store and lockfile resolution: defs keyed by the
//! blake3 hash of their Core IR, forming a Merkle DAG of code. Enables
//! reproducible builds, automatic dedup, and "has this hash been audited
//! before?" queries. See `spec/01-design-spec.md` §8.
//!
//! Acceptance gate (M7): content-addressed store + lockfile, enabling Stage-1
//! self-hosting.
//!
//! This crate is currently a compiling stub — no store is implemented yet.

/// Placeholder until the store lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M7"
}
