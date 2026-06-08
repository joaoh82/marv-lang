//! # Runtime layout & a lightweight type oracle for the backends (MARV-9)
//!
//! The interpreter is type-erased (it carries a tagged `Value`), but the
//! Cranelift and WASM backends are not: every marv value lives in a single
//! machine word, and an *aggregate* (a `struct`/tuple product or an `enum`
//! variant) is a **pointer** to a contiguous block of words laid out as
//! `[tag, field_0, …, field_{n-1}]` (`spec/02` §C `Ctor`/`Proj`/`Match`). To
//! lower `Match` correctly the backend must know whether a scrutinee is a scalar
//! `bool` (the `if`/`else` desugaring — the value *is* the tag) or a boxed sum
//! (the tag lives at word 0), and to bind a variant's fields it must know how
//! many words to load and what they are.
//!
//! The M2 checker ([`crate::check`]) already computes the type of every Core
//! node, but it is fused with diagnostic emission and linear-use tracking. This
//! module exposes the *pure* part the backends need: a small, allocation-light
//! [`type_of`] that synthesizes a Core term's type from the types of the binders
//! in scope (by de Bruijn **level**), plus the layout queries
//! ([`is_boxed`], [`variant_fields`], [`tuple_field_types`]) those backends share
//! so the two stay in lockstep by construction.
//!
//! It is deliberately *partial*: it returns [`None`] for a term whose type it
//! cannot pin down (e.g. an unresolved global, or a construct no front end
//! emits). A backend treats `None` conservatively — it never silently
//! miscompiles, it reports `unsupported` — so a gap here surfaces as a clean
//! backend error and a failing differential test, never as wrong code.

use marv_core::ir::*;

use crate::World;

/// Whether a value of this type is represented as a **boxed aggregate** (a
/// pointer to a `[tag, fields…]` block) rather than a scalar machine word.
///
/// Structs, enums (including the enum form of a caught `error`), tuples, and
/// fixed arrays are boxed; scalars (`int`/`bool`/`char`/`float`), references
/// (mutable value semantics carries no cell, `spec/01` §4), and capabilities
/// (no runtime value) are not. A `linear` wrapper is transparent to layout.
pub fn is_boxed(world: &World, ty: &Type) -> bool {
    match ty {
        Type::Tuple(_) | Type::Array(_, _) => true,
        Type::Nominal { def, .. } => {
            world.struct_decl(def).is_some() || world.enum_decl(def).is_some()
        }
        Type::Linear(inner) => is_boxed(world, inner),
        _ => false,
    }
}

/// The field types of variant `tag` of a boxed aggregate of type `ty`, in the
/// order `Ctor`/`Proj` index into (word `i+1` holds field `i`). A struct or
/// tuple is the single product variant (`tag` 0); an enum selects the variant.
/// Returns `None` if `ty` is not a known aggregate or `tag` is out of range.
pub fn variant_fields(world: &World, ty: &Type, tag: u32) -> Option<Vec<Type>> {
    match ty {
        Type::Linear(inner) => variant_fields(world, inner, tag),
        Type::Tuple(elems) if tag == 0 => Some(elems.clone()),
        Type::Nominal { def, .. } => {
            if let Some(s) = world.struct_decl(def) {
                if tag == 0 {
                    return Some(s.fields.clone());
                }
                None
            } else {
                let e = world.enum_decl(def)?;
                e.variants.get(tag as usize).map(|v| v.fields.clone())
            }
        }
        _ => None,
    }
}

/// The element types of a product (`struct`/tuple) — its tag-0 field list. A
/// convenience over [`variant_fields`] for the `Proj`/box paths that only deal
/// with products.
pub fn tuple_field_types(world: &World, ty: &Type) -> Option<Vec<Type>> {
    variant_fields(world, ty, 0)
}

/// The static type of an atom, given the types of the binders in scope by de
/// Bruijn **level** (`tys[0]` is the outermost binder). [`None`] for an
/// out-of-scope index or an unresolved global.
pub fn atom_type(world: &World, a: &Atom, tys: &[Option<Type>]) -> Option<Type> {
    match a {
        Atom::Lit(l) => Some(lit_type(l)),
        Atom::Var(idx) => {
            let d = tys.len();
            let i = (*idx as usize) + 1;
            if i > d {
                return None;
            }
            tys[d - i].clone()
        }
        Atom::Global(h) => world.global(h).cloned(),
    }
}

/// The type a literal denotes. An integer literal defaults to `i64` and a char
/// to `char`; only the *shape* (scalar vs. aggregate, and `bool` vs. the rest)
/// matters to the backends, so the exact integer width is immaterial here.
fn lit_type(l: &Literal) -> Type {
    match l {
        Literal::Unit => Type::Unit,
        Literal::Bool(_) => Type::Bool,
        Literal::Int(_) => Type::Int(IntTy::I64),
        Literal::Float(_) => Type::Float(FloatTy::F64),
        Literal::Str(_) => Type::Str,
        Literal::Char(_) => Type::Char,
    }
}

/// Synthesize the type of a Core term, threading binder types (by de Bruijn
/// **level**) through the binders it introduces so `tys` is restored on return.
///
/// This mirrors the checker's synthesis ([`crate::check`]) for the cases a
/// well-typed, front-end-produced body reaches, but emits no diagnostics and
/// allocates only the type it returns. It is the backends' source of truth for
/// "is this `Match` scrutinee a `bool` or a boxed sum, and of what type" — the
/// one fact the type-erased Core does not carry at the node.
pub fn type_of(world: &World, c: &Core, tys: &mut Vec<Option<Type>>) -> Option<Type> {
    match c {
        Core::Atom(a) => atom_type(world, a, tys),

        Core::Let { value, body } => {
            let vt = type_of(world, value, tys);
            tys.push(vt);
            let r = type_of(world, body, tys);
            tys.pop();
            r
        }

        Core::Lam {
            param,
            effects,
            body,
        } => {
            tys.push(Some(param.clone()));
            let ret = type_of(world, body, tys);
            tys.pop();
            Some(Type::Arrow {
                param: Box::new(param.clone()),
                ret: Box::new(ret.unwrap_or(Type::Unit)),
                effects: effects.clone(),
            })
        }

        // Application peels one arrow off the (curried) callee's type. In ANF a
        // saturated call `f a b` is `let t = App(f, a); App(t, b)`, so the func
        // atom's type is the partial arrow and one peel gives this node's type.
        Core::App { func, .. } => match atom_type(world, func, tys)? {
            Type::Arrow { ret, .. } => Some(*ret),
            _ => None,
        },

        // A constructor's type is its nominal (`spec/02` §C). Tuples have no
        // nominal hash; the front end only emits nominal `Ctor`s today.
        Core::Ctor { ty, .. } => Some(Type::Nominal {
            def: *ty,
            args: Vec::new(),
        }),

        // An array literal has the fixed-length array type `[N]elem`; the length
        // is the element count. This is what lets `len`/`index` over a bound array
        // recover its element type and length in the backends.
        Core::Array { elem, items } => {
            Some(Type::Array(Box::new(elem.clone()), items.len() as u64))
        }

        Core::Proj { base, idx } => {
            let bt = atom_type(world, base, tys)?;
            variant_fields(world, &bt, 0)?
                .into_iter()
                .nth(*idx as usize)
        }

        Core::Match { branches, .. } => {
            // Every arm has the same type; synthesize the first, binding its
            // fields as `Unknown` (their precise types are not needed to type the
            // body's *result*, only to bind names — and the backend supplies real
            // field types from the scrutinee at the binding site).
            let br = branches.first()?;
            for _ in 0..br.binds {
                tys.push(None);
            }
            let t = type_of(world, &br.body, tys);
            for _ in 0..br.binds {
                tys.pop();
            }
            t
        }

        Core::Prim { op, args } => prim_type(world, *op, args, tys),

        Core::Cast { to, .. } => Some(to.clone()),

        Core::Ref { mutable, of } => Some(Type::Ref {
            mutable: *mutable,
            of: Box::new(atom_type(world, of, tys).unwrap_or(Type::Unit)),
        }),

        Core::Perform { cap, op, .. } => {
            let Atom::Var(idx) = cap else { return None };
            let d = tys.len();
            let i = (*idx as usize) + 1;
            if i > d {
                return None;
            }
            let Some(Type::Nominal { def, .. }) = &tys[d - i] else {
                return None;
            };
            world
                .cap(def)
                .and_then(|c| c.ops.get(op.0 as usize))
                .map(|s| s.ret.clone())
        }

        // A loop evaluates to its loop-carried state as a product (`spec/02` §C).
        Core::Loop { state, .. } => {
            let elems: Vec<Type> = state
                .iter()
                .map(|a| atom_type(world, a, tys).unwrap_or(Type::Unit))
                .collect();
            Some(Type::Tuple(elems))
        }

        Core::Raise { .. } => None,
    }
}

/// The result type of a primitive. Comparisons and logical ops yield `bool`;
/// arithmetic and negation take the operand's type; `len` yields `usize`; `index`
/// yields the element type of the indexed aggregate.
fn prim_type(world: &World, op: PrimOp, args: &[Atom], tys: &[Option<Type>]) -> Option<Type> {
    use PrimOp::*;
    match op {
        Eq | Ne | Lt | Le | Gt | Ge | And | Or | Not => Some(Type::Bool),
        Add | Sub | Mul | Div | Rem | Neg => atom_type(world, args.first()?, tys),
        Len => Some(Type::Int(IntTy::Usize)),
        Index => {
            let base = atom_type(world, args.first()?, tys)?;
            match base {
                Type::Array(elem, _) | Type::Slice(elem) => Some(*elem),
                Type::Tuple(elems) => elems.into_iter().next(),
                _ => None,
            }
        }
    }
}
