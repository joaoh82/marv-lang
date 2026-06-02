//! # marv-codegen-llvm — LLVM backend (milestone M4, release path)
//!
//! The LLVM backend (via `inkwell`) for optimized release builds. Consumes the
//! same Core IR as the Cranelift dev backend. See `spec/01-design-spec.md` §9
//! (compilation targets).
//!
//! Acceptance gate (M4): optimized native release builds via LLVM, alongside the
//! Cranelift dev backend.
//!
//! This crate is currently a compiling stub — no codegen is implemented yet.

/// Placeholder until the LLVM backend lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M4"
}
