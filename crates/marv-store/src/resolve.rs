//! Content-hash resolution: the Merkle DAG of code (`spec/01` §8, `spec/02` §F).
//!
//! M1 lowering references other definitions by `symbol_hash(name)` — a *name*,
//! not the callee's identity — and deferred true content linking to M7 (see
//! `marv_core::lower`). This module performs that linking: it rewrites every
//! in-module `Global(symbol_hash(name))` to the callee's **dag hash** (its
//! content hash with *its* references resolved, transitively), so a definition's
//! hash commits to its entire dependency graph and depends on *no names at all*.
//!
//! Two consequences fall out, exactly as `spec/01` §8 promises:
//!
//! - **Free renames.** Names never enter a dag hash: cross-references resolve to
//!   the callee's (name-independent) dag hash, and recursive / mutually-recursive
//!   references resolve to a *positional* placeholder within their cycle. Renaming
//!   any definition — even a recursive one, even a callee — changes no hashes.
//! - **Transitive dedup.** Structurally identical definitions (and their
//!   identical dependencies) collapse to one dag hash regardless of naming.
//!
//! Recursion would make naive content hashing cyclic, so — following Unison —
//! strongly-connected components are hashed as a unit: within a component, a
//! reference to a member is a positional placeholder, the component is hashed
//! whole, and each member's hash is derived from the component hash plus its
//! position. Acyclic definitions are just singleton components.

use std::collections::HashMap;

use marv_core::ir::*;
use marv_core::reach::collect_global_syms;
use marv_core::{content_hash, symbol_hash};

/// Domain separators (versioned) for the three derived hashes below.
const REC_DOMAIN: &[u8] = b"marv-dag-rec-v0";
const COMP_DOMAIN: &[u8] = b"marv-dag-comp-v0";
const MEMBER_DOMAIN: &[u8] = b"marv-dag-member-v0";

/// The outcome of resolving a module: a dag hash per input definition (aligned
/// with the input order) and the `symbol_hash → dag_hash` bindings other modules
/// use to link against these definitions.
pub struct Resolved {
    /// Dag hash of each definition, in the input order.
    pub dag_hashes: Vec<Hash>,
    /// Each definition with its in-module/known references rewritten to dag
    /// hashes — the name-free Merkle node to store, in the input order.
    pub resolved_defs: Vec<Def>,
    /// The dag hashes each definition references (its Merkle-DAG out-edges),
    /// deduplicated, in the input order.
    pub deps: Vec<Vec<Hash>>,
    /// `symbol_hash("<module>.<name>") → dag_hash`, so a later module's
    /// references resolve to these definitions' content identities.
    pub symbol_to_dag: HashMap<Hash, Hash>,
}

/// Resolve a module's definitions to dag hashes. `module_path` qualifies their
/// names; `external` maps already-known `symbol_hash`es (from the store /
/// lockfile) to their dag hashes so cross-module references resolve too.
pub fn resolve(
    module_path: &str,
    defs: &[(String, Def)],
    external: &HashMap<Hash, Hash>,
) -> Resolved {
    let n = defs.len();

    // symbol_hash(qualified) → local index, to recognize in-module references.
    let mut sym_to_idx: HashMap<Hash, usize> = HashMap::new();
    for (i, (name, _)) in defs.iter().enumerate() {
        sym_to_idx.insert(symbol_hash(&qualify(module_path, name)), i);
    }

    // Call graph over in-module references (edge i → j: def i mentions def j).
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, (_, def)) in defs.iter().enumerate() {
        let mut seen = Vec::new();
        collect_global_syms(def, &mut seen);
        for s in seen {
            if let Some(&j) = sym_to_idx.get(&s) {
                if !adj[i].contains(&j) {
                    adj[i].push(j);
                }
            }
        }
    }

    // Strongly-connected components, in reverse-topological (callees-first) order.
    let components = tarjan_scc(&adj);

    let mut dag_hashes: Vec<Option<Hash>> = vec![None; n];
    for comp in &components {
        // A stable, name-independent order within the component: by source index.
        let mut members = comp.clone();
        members.sort_unstable();
        // Position of each member within the component (for placeholders).
        let pos: HashMap<usize, u32> = members
            .iter()
            .enumerate()
            .map(|(p, &node)| (node, p as u32))
            .collect();

        // Substitution for references appearing in this component's members.
        let subst = |sym: Hash| -> Option<Hash> {
            if let Some(&j) = sym_to_idx.get(&sym) {
                if let Some(&p) = pos.get(&j) {
                    Some(rec_hash(p)) // intra-component: positional placeholder
                } else {
                    dag_hashes[j] // earlier component: already resolved
                }
            } else {
                external.get(&sym).copied() // cross-module, else leave as-is
            }
        };

        // Hash each member's substituted Core, then fold into a component hash.
        let mut member_content: Vec<Hash> = Vec::with_capacity(members.len());
        for &node in &members {
            let resolved_def = subst_def(&defs[node].1, &subst);
            member_content.push(content_hash(&resolved_def));
        }
        let comp_hash = component_hash(&member_content);
        for (&node, p) in members.iter().zip(0u32..) {
            dag_hashes[node] = Some(member_hash(&comp_hash, p));
        }
    }

    let dag_hashes: Vec<Hash> = dag_hashes.into_iter().map(|h| h.unwrap()).collect();

    // Final pass (all dag hashes now known): produce the name-free resolved
    // definitions to store and each definition's dependency edges.
    let mut resolved_defs = Vec::with_capacity(n);
    let mut deps = Vec::with_capacity(n);
    let mut symbol_to_dag = HashMap::new();
    for (i, (name, def)) in defs.iter().enumerate() {
        let final_subst = |sym: Hash| -> Option<Hash> {
            sym_to_idx
                .get(&sym)
                .map(|&j| dag_hashes[j])
                .or_else(|| external.get(&sym).copied())
        };
        resolved_defs.push(subst_def(def, &final_subst));

        let mut syms = Vec::new();
        collect_global_syms(def, &mut syms);
        let mut dd = Vec::new();
        for s in syms {
            if let Some(h) = final_subst(s) {
                if !dd.contains(&h) {
                    dd.push(h);
                }
            }
        }
        deps.push(dd);

        symbol_to_dag.insert(symbol_hash(&qualify(module_path, name)), dag_hashes[i]);
    }

    Resolved {
        dag_hashes,
        resolved_defs,
        deps,
        symbol_to_dag,
    }
}

/// The dag hash of a single definition (its index within `resolve`'s output).
/// Convenience for callers that resolved a whole module.
impl Resolved {
    pub fn hash_of(&self, idx: usize) -> Hash {
        self.dag_hashes[idx]
    }
}

// ---- hash derivations ---------------------------------------------------

fn rec_hash(pos: u32) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(REC_DOMAIN);
    h.update(&pos.to_le_bytes());
    Hash(*h.finalize().as_bytes())
}

fn component_hash(members: &[Hash]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(COMP_DOMAIN);
    h.update(&(members.len() as u64).to_le_bytes());
    for m in members {
        h.update(&m.0);
    }
    Hash(*h.finalize().as_bytes())
}

fn member_hash(comp: &Hash, pos: u32) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(MEMBER_DOMAIN);
    h.update(&comp.0);
    h.update(&pos.to_le_bytes());
    Hash(*h.finalize().as_bytes())
}

// ---- global-reference substitution -------------------------------------

// The symbol collector these passes share with entry-reachability pruning
// lives in `marv_core::reach` ([`collect_global_syms`], MARV-8).

/// Rewrite a definition's `Global`/`Nominal` references via `subst` (a symbol
/// hash → replacement hash map; `None` leaves the reference unchanged).
fn subst_def(def: &Def, subst: &dyn Fn(Hash) -> Option<Hash>) -> Def {
    Def {
        kind: def.kind,
        ty: subst_type(&def.ty, subst),
        requires: def.requires.clone(),
        ensures: def.ensures.clone(),
        body: def.body.as_ref().map(|b| subst_core(b, subst)),
    }
}

fn subst_hash(h: Hash, subst: &dyn Fn(Hash) -> Option<Hash>) -> Hash {
    subst(h).unwrap_or(h)
}

fn subst_atom(a: &Atom, subst: &dyn Fn(Hash) -> Option<Hash>) -> Atom {
    match a {
        Atom::Global(h) => Atom::Global(subst_hash(*h, subst)),
        other => other.clone(),
    }
}

fn subst_type(t: &Type, subst: &dyn Fn(Hash) -> Option<Hash>) -> Type {
    match t {
        Type::Nominal { def, args } => Type::Nominal {
            def: subst_hash(*def, subst),
            args: args.iter().map(|a| subst_type(a, subst)).collect(),
        },
        Type::Array(inner, n) => Type::Array(Box::new(subst_type(inner, subst)), *n),
        Type::Slice(inner) => Type::Slice(Box::new(subst_type(inner, subst))),
        Type::Linear(inner) => Type::Linear(Box::new(subst_type(inner, subst))),
        Type::Ref { mutable, of } => Type::Ref {
            mutable: *mutable,
            of: Box::new(subst_type(of, subst)),
        },
        Type::Tuple(es) => Type::Tuple(es.iter().map(|e| subst_type(e, subst)).collect()),
        Type::Arrow {
            param,
            ret,
            effects,
        } => Type::Arrow {
            param: Box::new(subst_type(param, subst)),
            ret: Box::new(subst_type(ret, subst)),
            effects: effects.clone(),
        },
        other => other.clone(),
    }
}

fn subst_core(c: &Core, subst: &dyn Fn(Hash) -> Option<Hash>) -> Core {
    match c {
        Core::Atom(a) => Core::Atom(subst_atom(a, subst)),
        Core::Let { value, body } => Core::Let {
            value: Box::new(subst_core(value, subst)),
            body: Box::new(subst_core(body, subst)),
        },
        Core::Lam {
            param,
            effects,
            body,
        } => Core::Lam {
            param: subst_type(param, subst),
            effects: effects.clone(),
            body: Box::new(subst_core(body, subst)),
        },
        Core::App { func, arg } => Core::App {
            func: subst_atom(func, subst),
            arg: subst_atom(arg, subst),
        },
        Core::Ctor { ty, tag, fields } => Core::Ctor {
            ty: subst_hash(*ty, subst),
            tag: *tag,
            fields: fields.iter().map(|a| subst_atom(a, subst)).collect(),
        },
        Core::Proj { base, idx } => Core::Proj {
            base: subst_atom(base, subst),
            idx: *idx,
        },
        Core::Array { elem, items } => Core::Array {
            elem: subst_type(elem, subst),
            items: items.iter().map(|a| subst_atom(a, subst)).collect(),
        },
        Core::IndexSet { base, index, value } => Core::IndexSet {
            base: subst_atom(base, subst),
            index: subst_atom(index, subst),
            value: subst_atom(value, subst),
        },
        Core::ListNew {
            elem,
            alloc,
            capacity,
        } => Core::ListNew {
            elem: subst_type(elem, subst),
            alloc: subst_atom(alloc, subst),
            capacity: subst_atom(capacity, subst),
        },
        Core::ListPush { alloc, list, value } => Core::ListPush {
            alloc: subst_atom(alloc, subst),
            list: subst_atom(list, subst),
            value: subst_atom(value, subst),
        },
        Core::ListPop { list } => Core::ListPop {
            list: subst_atom(list, subst),
        },
        Core::ListSet { list, index, value } => Core::ListSet {
            list: subst_atom(list, subst),
            index: subst_atom(index, subst),
            value: subst_atom(value, subst),
        },
        Core::Match {
            scrutinee,
            branches,
        } => Core::Match {
            scrutinee: subst_atom(scrutinee, subst),
            branches: branches
                .iter()
                .map(|b| Branch {
                    binds: b.binds,
                    body: subst_core(&b.body, subst),
                })
                .collect(),
        },
        Core::Prim { op, args } => Core::Prim {
            op: *op,
            args: args.iter().map(|a| subst_atom(a, subst)).collect(),
        },
        Core::Cast { value, to } => Core::Cast {
            value: subst_atom(value, subst),
            to: subst_type(to, subst),
        },
        Core::Ref { mutable, of } => Core::Ref {
            mutable: *mutable,
            of: subst_atom(of, subst),
        },
        Core::Perform { cap, op, args } => Core::Perform {
            cap: subst_atom(cap, subst),
            op: *op,
            args: args.iter().map(|a| subst_atom(a, subst)).collect(),
        },
        Core::Raise { error, args } => Core::Raise {
            error: subst_hash(*error, subst),
            args: args.iter().map(|a| subst_atom(a, subst)).collect(),
        },
        Core::Return { value } => Core::Return {
            value: subst_atom(value, subst),
        },
        Core::Loop {
            state,
            invariant,
            cond,
            body,
        } => Core::Loop {
            state: state.iter().map(|a| subst_atom(a, subst)).collect(),
            invariant: invariant.clone(),
            cond: Box::new(subst_core(cond, subst)),
            body: Box::new(subst_core(body, subst)),
        },
    }
}

// ---- Tarjan SCC ---------------------------------------------------------

/// Strongly-connected components of a directed graph, returned in
/// reverse-topological order (a component appears before the components that
/// depend on it) — which is what `resolve` needs (callees hashed first).
fn tarjan_scc(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct State<'a> {
        adj: &'a [Vec<usize>],
        index: u32,
        indices: Vec<Option<u32>>,
        lowlink: Vec<u32>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        out: Vec<Vec<usize>>,
    }
    let n = adj.len();
    let mut st = State {
        adj,
        index: 0,
        indices: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        out: Vec::new(),
    };
    // Iterative DFS to avoid stack overflow on large modules.
    fn strongconnect(st: &mut State, v: usize) {
        st.indices[v] = Some(st.index);
        st.lowlink[v] = st.index;
        st.index += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        for &w in &st.adj[v] {
            match st.indices[w] {
                None => {
                    strongconnect(st, w);
                    st.lowlink[v] = st.lowlink[v].min(st.lowlink[w]);
                }
                Some(idx) if st.on_stack[w] => {
                    st.lowlink[v] = st.lowlink[v].min(idx);
                }
                Some(_) => {}
            }
        }
        if st.lowlink[v] == st.indices[v].unwrap() {
            let mut comp = Vec::new();
            loop {
                let w = st.stack.pop().unwrap();
                st.on_stack[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            st.out.push(comp);
        }
    }
    for v in 0..n {
        if st.indices[v].is_none() {
            strongconnect(&mut st, v);
        }
    }
    st.out
}

fn qualify(module_path: &str, name: &str) -> String {
    if module_path.is_empty() || name.contains('.') {
        name.to_string()
    } else {
        format!("{module_path}.{name}")
    }
}
