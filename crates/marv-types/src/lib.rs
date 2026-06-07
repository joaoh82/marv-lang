//! # marv-types — type / effect / capability checker (milestone M2)
//!
//! Over the canonical **Core IR** (`marv-core`), this crate performs the six
//! families of static check that constitute marv's always-on Tier-0 guarantee
//! (`spec/01-design-spec.md` §§3–7, `spec/02-grammar-and-core-ir.md` §E):
//!
//! 1. **Type checking** — bidirectional synthesis following the §E judgments.
//!    No global inference: signatures are fully annotated; inference happens
//!    only *inside* a body (`spec/01` §1).
//! 2. **Effect-row inference** — the capabilities a body may exercise, unioned
//!    up the ANF spine and checked against the declared signature row.
//! 3. **Capability checking** — `perform` needs a capability *value in scope*,
//!    and capabilities are unforgeable: never produced by construction
//!    (`spec/01` §5).
//! 4. **Error-set inference** — the errors a body may raise, checked against
//!    the declared set (`spec/01` §6).
//! 5. **Second-class-reference checking** — a reference may be passed down but
//!    never stored, returned, or held in a struct field (`spec/01` §4).
//! 6. **Linearity** — a `linear` value is consumed exactly once on every
//!    control path (`spec/01` §4.1).
//!
//! Every diagnostic follows `spec/03-compiler-protocol.md` §2 ([`Diagnostic`]:
//! stable [`Code`], severity, span, message, related, fixes) and carries a
//! machine-actionable [`Fix`] for each of the mechanically-derivable cases the
//! protocol names. See [`diagnostic`] for how real (definition-granular) source
//! spans are stamped one layer up in `marv-db`, and [`check`] for the
//! rule-by-rule details and which rules the current front end can reach from
//! real `.mv` source.
//!
//! ## Entry points
//!
//! - [`check_module`] — check a whole [`LoweredModule`], building the [`World`]
//!   from its own definitions. This is the path real `.mv` source takes.
//! - [`check_def`] — check one [`Def`] against an explicit [`World`]. Tests use
//!   this with a [`WorldBuilder`]-assembled world to exercise the capability /
//!   error-set / exhaustiveness / linearity rules that have no M0 surface form
//!   yet.

pub mod bounds;
pub mod check;
pub mod diagnostic;
pub mod world;

pub use bounds::{check_bounds, resolve_impl_for, resolve_impls, ImplResolution, ImplSelection};
pub use check::{check_def, effect_row};
pub use diagnostic::{Code, Diagnostic, Edit, Fix, Position, Related, Severity, Span};
pub use world::{
    CapDecl, EnumDecl, ErrorDecl, OpSig, StructDecl, VariantDecl, World, WorldBuilder,
};

use marv_core::LoweredModule;

/// Check every definition in a lowered module, returning all diagnostics in a
/// deterministic order (definitions in source order; within a definition, in
/// traversal order).
///
/// The [`World`] is built from the module itself ([`World::from_module`]), so
/// in-module calls and nominal types resolve. References to imports or builtins
/// the world does not know are treated as opaque (`Unknown`) and never produce
/// spurious diagnostics — see [`check`].
pub fn check_module(m: &LoweredModule) -> Vec<Diagnostic> {
    let world = World::from_module(m);
    let mut diags = Vec::new();
    for entry in &m.defs {
        diags.extend(check_def(&world, &entry.def, Some(&entry.name)));
    }
    // Interface-bound and coherence checks run over the module's generics/impl
    // metadata, not its Core (`spec/01` §§3.3–3.4).
    diags.extend(check_bounds(std::slice::from_ref(m)).into_iter().map(|(_, d)| d));
    diags
}
