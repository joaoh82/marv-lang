//! Interface-bound checking and coherent impl resolution (`spec/01` §§3.3–3.4).
//!
//! Generics in marv are **explicit and monomorphized**, and interface
//! implementations are **coherent** (one impl per interface-per-type) with
//! **deterministic** resolution. Those are properties of the *surface* program,
//! not of any single names-erased [`Def`], so they are checked here over the
//! [`LoweredModule`] metadata the lowerer records ([`InterfaceInfo`],
//! [`ImplInfo`], [`Instantiation`]) rather than over the Core IR.
//!
//! Two checks and one report:
//!
//! - **Coherence** ([`Code::ConflictingImpl`]): no two `impl`s for the same
//!   interface and type across the module set.
//! - **Bound satisfaction** ([`Code::UnsatisfiedBound`]): every generic
//!   instantiation whose type parameter carries an interface bound resolves to an
//!   `impl` for the concrete type it is instantiated at.
//! - **`marv/resolveImpl`** ([`resolve_impls`]): for every instantiation, *which*
//!   impl (and which method definitions) each bounded type argument resolves to —
//!   the "which impl was selected" report the protocol exposes (`spec/03`).

use std::collections::HashMap;

use marv_core::{ImplInfo, LoweredModule};

use crate::diagnostic::{Code, Diagnostic};

/// The selected impl for one bounded type argument at an instantiation: the
/// parameter, the interface it satisfies, the concrete type key, and the
/// fully-qualified method definitions the call resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplSelection {
    pub param: String,
    pub interface: String,
    pub type_key: String,
    /// Method name → fully-qualified impl-method def name.
    pub methods: Vec<(String, String)>,
}

/// The `marv/resolveImpl` answer for one generic instantiation: which impl each
/// of its bounded type parameters selected (`spec/01` §3.4, `spec/03`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplResolution {
    /// Fully-qualified instance name, e.g. `"sort.max@i32"`.
    pub instance: String,
    /// The generic function's source name, e.g. `"max"`.
    pub generic: String,
    /// One selection per bounded type parameter (unbounded parameters are
    /// omitted — they require no impl).
    pub selections: Vec<ImplSelection>,
}

/// Index every impl in the module set by `(interface, type_key)`. The value is
/// the list of impls under that key (length > 1 means a coherence violation) and,
/// for the first one, its method definitions.
fn impl_index(modules: &[LoweredModule]) -> HashMap<(String, String), Vec<ImplInfo>> {
    let mut idx: HashMap<(String, String), Vec<ImplInfo>> = HashMap::new();
    for m in modules {
        for imp in &m.impls {
            idx.entry((imp.interface.clone(), imp.type_key.clone()))
                .or_default()
                .push(imp.clone());
        }
    }
    idx
}

/// Check coherence and bound satisfaction across a set of lowered modules,
/// returning each diagnostic paired with a name for context (the interface for a
/// coherence error, the qualified instance for a bound error), in a deterministic
/// order.
pub fn check_bounds(modules: &[LoweredModule]) -> Vec<(String, Diagnostic)> {
    let idx = impl_index(modules);
    let mut out = Vec::new();

    // Coherence: at most one impl per (interface, type). Report duplicates once,
    // in a stable order.
    let mut conflict_keys: Vec<&(String, String)> = idx
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, _)| k)
        .collect();
    conflict_keys.sort();
    for (interface, type_key) in conflict_keys {
        out.push((
            interface.clone(),
            Diagnostic::error(
                Code::ConflictingImpl,
                format!(
                    "conflicting `impl {interface}[{type_key}]`: an interface may be implemented \
                     for a type at most once (coherence)"
                ),
            )
            .with_related(format!(
                "{} impls of `{interface}` for `{type_key}` found",
                idx[&(interface.clone(), type_key.clone())].len()
            )),
        ));
    }

    // Bound satisfaction: every bounded type argument must resolve to an impl.
    for m in modules {
        let prefix = m.module.join(".");
        for inst in &m.instantiations {
            for arg in &inst.args {
                let Some(interface) = &arg.bound else {
                    continue;
                };
                let key = (interface.clone(), arg.type_key.clone());
                if !idx.contains_key(&key) {
                    let qualified = qualify(&prefix, &inst.instance);
                    out.push((
                        qualified,
                        Diagnostic::error(
                            Code::UnsatisfiedBound,
                            format!(
                                "`{}` requires `{}: {interface}`, but `{}` does not implement \
                                 `{interface}` (no `impl {interface}[{}]`)",
                                inst.generic, arg.param, arg.type_key, arg.type_key
                            ),
                        )
                        .with_related(format!(
                            "instantiated here as `{}`",
                            inst.instance
                        )),
                    ));
                }
            }
        }
    }

    out
}

/// Resolve, for every generic instantiation in the module set, which coherent
/// impl each of its bounded type parameters selects — the `marv/resolveImpl`
/// report (`spec/01` §3.4). Instantiations whose bounds are unsatisfied are
/// omitted (there is no impl to report; [`check_bounds`] flags them).
pub fn resolve_impls(modules: &[LoweredModule]) -> Vec<ImplResolution> {
    let idx = impl_index(modules);
    let mut out = Vec::new();
    for m in modules {
        let prefix = m.module.join(".");
        for inst in &m.instantiations {
            let mut selections = Vec::new();
            for arg in &inst.args {
                let Some(interface) = &arg.bound else {
                    continue;
                };
                if let Some(impls) = idx.get(&(interface.clone(), arg.type_key.clone())) {
                    if let Some(chosen) = impls.first() {
                        selections.push(ImplSelection {
                            param: arg.param.clone(),
                            interface: interface.clone(),
                            type_key: arg.type_key.clone(),
                            methods: chosen.methods.clone(),
                        });
                    }
                }
            }
            out.push(ImplResolution {
                instance: qualify(&prefix, &inst.instance),
                generic: inst.generic.clone(),
                selections,
            });
        }
    }
    out
}

/// One [`resolve_impls`] entry, for the single instance named `instance`
/// (qualified). `None` if there is no such instantiation.
pub fn resolve_impl_for(modules: &[LoweredModule], instance: &str) -> Option<ImplResolution> {
    resolve_impls(modules)
        .into_iter()
        .find(|r| r.instance == instance)
}

fn qualify(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}
