//! # marv-interp — tree-walking interpreter (milestone M4)
//!
//! A tree-walking interpreter over the Core IR, used as the semantics *oracle*
//! before native codegen lands (and kept afterward for differential testing).
//! Capabilities are injected explicitly at the entry point. See
//! `spec/03-compiler-protocol.md` §4.5 (`run`).
//!
//! Acceptance gate (M4): interpret Core IR as the reference semantics, ahead of
//! Cranelift codegen.
//!
//! This crate is currently a compiling stub — no interpreter is implemented yet.

/// Placeholder until the interpreter lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M4"
}
