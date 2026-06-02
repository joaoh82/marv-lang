//! # marv-codegen-cl — Cranelift backend (milestone M4)
//!
//! The Cranelift backend for fast dev/debug and native builds. Consumes the
//! Core IR shared with every other backend. See `spec/01-design-spec.md` §9
//! (compilation targets).
//!
//! Acceptance gate (M4): native codegen via Cranelift, after the interpreter
//! oracle is in place.
//!
//! This crate is currently a compiling stub — no codegen is implemented yet.

/// Placeholder until the Cranelift backend lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M4"
}
