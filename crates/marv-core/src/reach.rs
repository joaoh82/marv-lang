//! Entry-reachability over a module's definitions (MARV-8).
//!
//! `marv build` compiles only the definitions **reachable from the entry
//! point**, so a module that mixes backend-supported functions with
//! not-yet-supported ones still builds when the entry never references the
//! unsupported ones. The edges walked here are the same dependency edges
//! `marv-store::resolve` links into the Merkle DAG: every `Global` /
//! constructor / `Raise` / `Nominal` symbol a definition mentions, in its body
//! and in its signature ([`collect_global_syms`]).
//!
//! Whole-module compilation remains the default everywhere else (`commit` /
//! audit flows, the differential corpus): pruning happens only when a caller
//! asks for it by naming an entry, and falls back to "everything" when no
//! entry can be resolved.

use std::collections::HashMap;

use crate::ir::*;
use crate::symbol_hash;

/// Which definitions a backend must compile to build `entry`: the entry's
/// transitive dependency closure over [`collect_global_syms`] edges, as a mask
/// aligned with `defs`.
///
/// The entry resolves the same way the backends resolve one at call time: an
/// explicit name (bare or `module.`-qualified), else `main`, else the sole
/// monomorphic function. When no entry resolves this way — no `main` among
/// several functions, or a name that matches nothing — every definition is
/// marked reachable (whole-module compilation, the pre-MARV-8 behavior), so
/// the backend reports the same `NoSuchEntry` / unsupported-construct errors
/// it always did.
pub fn reachable_mask(module_path: &str, defs: &[(String, Def)], entry: &str) -> Vec<bool> {
    let n = defs.len();
    let Some(start) = resolve_entry(module_path, defs, entry) else {
        return vec![true; n];
    };

    // symbol_hash(qualified name) → def index, to recognize in-module edges.
    let mut sym_to_idx: HashMap<Hash, usize> = HashMap::new();
    for (i, (name, _)) in defs.iter().enumerate() {
        sym_to_idx.insert(symbol_hash(&qualify(module_path, name)), i);
    }

    let mut mask = vec![false; n];
    mask[start] = true;
    let mut queue = vec![start];
    while let Some(i) = queue.pop() {
        let mut syms = Vec::new();
        collect_global_syms(&defs[i].1, &mut syms);
        for s in syms {
            if let Some(&j) = sym_to_idx.get(&s) {
                if !mask[j] {
                    mask[j] = true;
                    queue.push(j);
                }
            }
        }
    }
    mask
}

/// Resolve the entry definition: an explicit name (bare or qualified), else
/// `main`, else the sole monomorphic function. Generic templates are never
/// entries — only their monomorphizations are callable (`spec/01` §3.3).
fn resolve_entry(module_path: &str, defs: &[(String, Def)], entry: &str) -> Option<usize> {
    let concrete_fn = |def: &Def| -> bool { def.kind == DefKind::Fn && !def.ty.is_polymorphic() };
    if !entry.is_empty() {
        return defs.iter().position(|(name, def)| {
            concrete_fn(def) && (name == entry || qualify(module_path, name) == entry)
        });
    }
    if let Some(i) = defs
        .iter()
        .position(|(name, def)| name == "main" && concrete_fn(def))
    {
        return Some(i);
    }
    let mut fns = defs.iter().enumerate().filter(|(_, (_, d))| concrete_fn(d));
    match (fns.next(), fns.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() || name.contains('.') {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}

// ---- symbol collection ----------------------------------------------------

/// Collect every symbol hash a definition mentions (body + type): `Global`
/// atoms, `Ctor` nominals, `Raise` errors, and signature `Nominal`s. These are
/// a definition's module-graph out-edges — `marv-store::resolve` rewrites
/// exactly this set into dag hashes.
pub fn collect_global_syms(def: &Def, out: &mut Vec<Hash>) {
    collect_type_syms(&def.ty, out);
    if let Some(body) = &def.body {
        collect_core_syms(body, out);
    }
}

fn collect_type_syms(t: &Type, out: &mut Vec<Hash>) {
    match t {
        Type::Nominal { def, args } => {
            out.push(*def);
            args.iter().for_each(|a| collect_type_syms(a, out));
        }
        Type::Array(inner, _) | Type::Slice(inner) | Type::Linear(inner) => {
            collect_type_syms(inner, out)
        }
        Type::Ref { of, .. } => collect_type_syms(of, out),
        Type::Tuple(es) => es.iter().for_each(|e| collect_type_syms(e, out)),
        Type::Arrow { param, ret, .. } => {
            collect_type_syms(param, out);
            collect_type_syms(ret, out);
        }
        _ => {}
    }
}

fn collect_core_syms(c: &Core, out: &mut Vec<Hash>) {
    let atom = |a: &Atom, out: &mut Vec<Hash>| {
        if let Atom::Global(h) = a {
            out.push(*h);
        }
    };
    match c {
        Core::Atom(a) => atom(a, out),
        Core::Let { value, body } => {
            collect_core_syms(value, out);
            collect_core_syms(body, out);
        }
        Core::Lam { param, body, .. } => {
            collect_type_syms(param, out);
            collect_core_syms(body, out);
        }
        Core::App { func, arg } => {
            atom(func, out);
            atom(arg, out);
        }
        Core::Ctor { ty, fields, .. } => {
            out.push(*ty);
            fields.iter().for_each(|a| atom(a, out));
        }
        Core::Proj { base, .. } => atom(base, out),
        Core::Array { elem, items } => {
            collect_type_syms(elem, out);
            items.iter().for_each(|a| atom(a, out));
        }
        Core::IndexSet { base, index, value } => {
            atom(base, out);
            atom(index, out);
            atom(value, out);
        }
        Core::ListNew {
            elem,
            alloc,
            capacity,
        } => {
            collect_type_syms(elem, out);
            atom(alloc, out);
            atom(capacity, out);
        }
        Core::ListPush { alloc, list, value } => {
            atom(alloc, out);
            atom(list, out);
            atom(value, out);
        }
        Core::ListPop { list } => atom(list, out),
        Core::ListSet { list, index, value } => {
            atom(list, out);
            atom(index, out);
            atom(value, out);
        }
        Core::Match {
            scrutinee,
            branches,
        } => {
            atom(scrutinee, out);
            branches
                .iter()
                .for_each(|b| collect_core_syms(&b.body, out));
        }
        Core::Prim { args, .. } => args.iter().for_each(|a| atom(a, out)),
        Core::Cast { value, to } => {
            atom(value, out);
            collect_type_syms(to, out);
        }
        Core::Ref { of, .. } => atom(of, out),
        Core::Perform { cap, args, .. } => {
            atom(cap, out);
            args.iter().for_each(|a| atom(a, out));
        }
        Core::Raise { error, args } => {
            out.push(*error);
            args.iter().for_each(|a| atom(a, out));
        }
        Core::Loop { cond, body, .. } => {
            collect_core_syms(cond, out);
            collect_core_syms(body, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monomorphic `Fn` def whose body calls each named sibling (qualified
    /// under `mod_path`) once.
    fn fn_calling(mod_path: &str, callees: &[&str]) -> Def {
        let mut body = Core::Atom(Atom::Lit(Literal::Int(0)));
        for callee in callees {
            let g = symbol_hash(&qualify(mod_path, callee));
            body = Core::Let {
                value: Box::new(Core::Atom(Atom::Global(g))),
                body: Box::new(body),
            };
        }
        Def {
            kind: DefKind::Fn,
            ty: Type::Arrow {
                param: Box::new(Type::Int(IntTy::I64)),
                ret: Box::new(Type::Int(IntTy::I64)),
                effects: EffectRow::default(),
            },
            requires: Vec::new(),
            ensures: Vec::new(),
            body: Some(body),
        }
    }

    fn module(defs: &[(&str, &[&str])]) -> Vec<(String, Def)> {
        defs.iter()
            .map(|(name, callees)| (name.to_string(), fn_calling("m", callees)))
            .collect()
    }

    #[test]
    fn explicit_entry_prunes_to_its_transitive_closure() {
        // a → b → c, while d is unreferenced.
        let defs = module(&[("a", &["b"]), ("b", &["c"]), ("c", &[]), ("d", &[])]);
        assert_eq!(reachable_mask("m", &defs, "a"), [true, true, true, false]);
        // The qualified spelling resolves identically.
        assert_eq!(reachable_mask("m", &defs, "m.a"), [true, true, true, false]);
        assert_eq!(reachable_mask("m", &defs, "c"), [false, false, true, false]);
    }

    #[test]
    fn empty_entry_falls_back_to_main_then_sole_fn() {
        let defs = module(&[("main", &["helper"]), ("helper", &[]), ("stray", &[])]);
        assert_eq!(reachable_mask("m", &defs, ""), [true, true, false]);

        let sole = module(&[("only", &[])]);
        assert_eq!(reachable_mask("m", &sole, ""), [true]);
    }

    #[test]
    fn unresolvable_entry_keeps_whole_module() {
        // No `main`, several functions: whole-module compilation.
        let defs = module(&[("a", &[]), ("b", &[])]);
        assert_eq!(reachable_mask("m", &defs, ""), [true, true]);
        // A name that matches nothing: whole-module, so the backend still
        // reports its usual NoSuchEntry.
        assert_eq!(reachable_mask("m", &defs, "nope"), [true, true]);
    }

    #[test]
    fn recursion_and_mutual_recursion_terminate() {
        let defs = module(&[("even", &["odd", "even"]), ("odd", &["even"]), ("x", &[])]);
        assert_eq!(reachable_mask("m", &defs, "even"), [true, true, false]);
    }
}
