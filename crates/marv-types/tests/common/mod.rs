//! Shared builders for the M2 checker tests.
//!
//! The capability / error-set / exhaustiveness / linearity rules have no M0
//! surface syntax yet (see `marv_types::check` scope notes), so the rule table
//! drives them over hand-written Core. These helpers keep that construction
//! readable: `fn_def` builds the curried arrow/lambda spine the checker peels,
//! matching `marv_core::lower` exactly (effect row on the innermost layer).

#![allow(dead_code)]

use marv_core::ir::*;
use marv_core::symbol_hash;

/// A nominal type referenced by name (the same `symbol_hash` key the `World`
/// builder registers under).
pub fn nominal(name: &str) -> Type {
    Type::Nominal {
        def: symbol_hash(name),
        args: Vec::new(),
    }
}

/// An effect row from capability and error *names*.
pub fn row(caps: &[&str], errors: &[&str]) -> EffectRow {
    EffectRow {
        caps: caps.iter().map(|c| symbol_hash(c)).collect(),
        errors: errors.iter().map(|e| symbol_hash(e)).collect(),
    }
}

/// A `linear` wrapper.
pub fn linear(t: Type) -> Type {
    Type::Linear(Box::new(t))
}

/// A shared/`&mut` reference.
pub fn ref_to(mutable: bool, t: Type) -> Type {
    Type::Ref {
        mutable,
        of: Box::new(t),
    }
}

/// The curried arrow type for `params -> ret`, with `eff` on the innermost
/// arrow (outer, partial-application arrows are pure) — exactly the shape
/// `marv_core::lower` produces and `check_fn` peels.
pub fn arrow(params: &[Type], ret: Type, eff: EffectRow) -> Type {
    let params = ensure_params(params);
    let last = params.len() - 1;
    let mut t = ret;
    for (i, p) in params.iter().enumerate().rev() {
        let e = if i == last {
            eff.clone()
        } else {
            EffectRow::empty()
        };
        t = Type::Arrow {
            param: Box::new(p.clone()),
            ret: Box::new(t),
            effects: e,
        };
    }
    t
}

/// The curried lambda wrapping `body`, mirroring [`arrow`].
pub fn lam(params: &[Type], eff: EffectRow, body: Core) -> Core {
    let params = ensure_params(params);
    let last = params.len() - 1;
    let mut c = body;
    for (i, p) in params.iter().enumerate().rev() {
        let e = if i == last {
            eff.clone()
        } else {
            EffectRow::empty()
        };
        c = Core::Lam {
            param: p.clone(),
            effects: e,
            body: Box::new(c),
        };
    }
    c
}

/// A `fn` definition with the given parameter types, return type, declared
/// effect row, and Core body (written at binder-depth `params.len()`).
pub fn fn_def(params: &[Type], ret: Type, eff: EffectRow, body: Core) -> Def {
    Def {
        kind: DefKind::Fn,
        ty: arrow(params, ret.clone(), eff.clone()),
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(lam(params, eff, body)),
    }
}

/// A `struct` definition with the given field types (M1 lowers a struct to a
/// `Tuple`, `Linear`-wrapped when declared `linear`).
pub fn struct_def(fields: &[Type], is_linear: bool) -> Def {
    let prod = Type::Tuple(fields.to_vec());
    let ty = if is_linear {
        Type::Linear(Box::new(prod))
    } else {
        prod
    };
    Def {
        kind: DefKind::Struct,
        ty,
        requires: Vec::new(),
        ensures: Vec::new(),
        body: None,
    }
}

/// A `let value in body` Core node.
pub fn let_(value: Core, body: Core) -> Core {
    Core::Let {
        value: Box::new(value),
        body: Box::new(body),
    }
}

/// A reference to the binder at de Bruijn *level* `level`, when the current
/// binder depth is `depth`. Keeps the index arithmetic in one readable place.
pub fn var_at(depth: u32, level: u32) -> Atom {
    Atom::Var(depth - 1 - level)
}

/// `Core::Atom` shorthands.
pub fn unit() -> Core {
    Core::Atom(Atom::Lit(Literal::Unit))
}
pub fn int(n: i64) -> Atom {
    Atom::Lit(Literal::Int(n))
}
pub fn global(name: &str) -> Atom {
    Atom::Global(symbol_hash(name))
}

fn ensure_params(params: &[Type]) -> Vec<Type> {
    if params.is_empty() {
        vec![Type::Unit]
    } else {
        params.to_vec()
    }
}
