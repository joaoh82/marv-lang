//! # marv-verify — SMT contract discharge (milestone M6)
//!
//! Tier 2 verification: statically discharge `requires`/`ensures`/`invariant`
//! contracts for the verified subset via an SMT backend (z3 / easy-smt),
//! returning a counterexample on failure or honestly reporting `unsupported`.
//! See `spec/01-design-spec.md` §7 and `spec/03-compiler-protocol.md` §4.3.
//!
//! Acceptance gate (M6): SMT discharge for the decidable-ish verified subset.
//!
//! This crate is currently a compiling stub — no verifier is implemented yet.

/// Placeholder until the SMT verifier lands. Names the milestone this crate serves.
pub fn milestone() -> &'static str {
    "M6"
}
