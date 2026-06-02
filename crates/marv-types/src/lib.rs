//! # marv-types — type / effect / capability checker (milestone M2)
//!
//! Type + effect + capability checking, error-set inference, second-class
//! reference and linearity checks, and the contracts frontend. Diagnostics emit
//! machine-actionable fixes. See `spec/01-design-spec.md` §§3–7 and
//! `spec/02-grammar-and-core-ir.md` §E (typing/effect judgments).
//!
//! Acceptance gate (M2): type + effect + capability checking with fix-carrying
//! diagnostics.
//!
//! This crate is currently a compiling stub — no checker is implemented yet.

/// Placeholder until the checker lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M2"
}
