//! # marv-core — Core IR (milestone M1)
//!
//! The canonical **Core IR**: A-normal form + de Bruijn indices, ANF lowering
//! from the marv-syntax AST, de Bruijn conversion, and `blake3`-256 content
//! hashing. The Core IR is the unit of *identity* in marv — see
//! `spec/02-grammar-and-core-ir.md` §C (Core IR), §D (desugaring), and §F
//! (content hashing).
//!
//! ## Layout
//!
//! - [`ir`] — the Core IR data model ([`Type`], [`EffectRow`], [`Atom`],
//!   [`Core`], [`Branch`], [`Pred`], [`Def`], [`Hash`]), mirroring `spec/02` §C.
//! - [`hash`] — the canonical encoding and `blake3` content hash of a [`Def`]
//!   (`spec/02` §F), plus [`symbol_hash`] for cross-definition references.
//! - [`lower`] — AST → Core lowering: desugaring, ANF normalization, and de
//!   Bruijn conversion ([`lower_module`]).
//!
//! ## Acceptance gate (M1)
//!
//! Alpha-equivalent surface programs — same logic, different *local* names or
//! formatting — lower to *identical* Core hashes. The unit of identity erases
//! local names (de Bruijn), formatting, and a definition's own name; it commits
//! to the structure of the body and the identities of every definition it
//! references. See `tests/golden.rs`.
//!
//! ## Scope honesty
//!
//! M1 lowers the bounded AST the M0 parser actually produces. Two things the
//! spec describes are intentionally deferred and documented at their sites:
//! effect/error-row *inference* (every lowered arrow currently carries the empty
//! row; declared `pure` is already exact) is M2; and resolving [`symbol_hash`]
//! references to a callee's *own* content hash — so structurally identical code
//! deduplicates transitively, including recursive cycles — is content-store work
//! (M7). Both honour the §F encoding rules as written.

pub mod hash;
pub mod ir;
pub mod lower;
pub mod pred;

pub use hash::{content_hash, symbol_hash};
pub use ir::*;
pub use lower::{
    lower_module, lower_modules, DefEntry, ImplInfo, Instantiation, InterfaceInfo, InterfaceMethod,
    LowerError, LoweredModule, TypeArg, VariantInfo,
};
pub use pred::render_pred;
