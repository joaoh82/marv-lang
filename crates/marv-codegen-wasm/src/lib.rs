//! # marv-codegen-wasm — WASM + component-model backend (milestone M5)
//!
//! The WebAssembly / component-model backend for both browser and server. The
//! capability model composes cleanly with WASM imports, which is what makes
//! running untrusted agent-generated marv in a browser safe. See
//! `spec/01-design-spec.md` §9.
//!
//! Acceptance gate (M5): a browser demo proving capability-gated sandboxing.
//!
//! This crate is currently a compiling stub — no codegen is implemented yet.

/// Placeholder until the WASM backend lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M5"
}
