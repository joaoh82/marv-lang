//! # marv-core — Core IR (milestone M1)
//!
//! The canonical **Core IR**: A-normal form + de Bruijn indices, ANF lowering
//! from the AST, de Bruijn conversion, and blake3-256 content hashing. The Core
//! IR is the unit of *identity* in marv — see `spec/02-grammar-and-core-ir.md`
//! §C (Core IR) and §F (content hashing).
//!
//! Acceptance gate (M1): alpha-equivalent surface programs lower to *identical*
//! Core hashes (golden tests).
//!
//! This crate is currently a compiling stub — the IR data model is not defined
//! yet.

/// Placeholder until the Core IR lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M1"
}
