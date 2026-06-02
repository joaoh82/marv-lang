//! M1 acceptance gate: alpha-equivalent surface programs lower to *identical*
//! Core hashes, plus hand-written lowering goldens that pin down the exact ANF +
//! de Bruijn + desugaring shape (`spec/02-grammar-and-core-ir.md` §§C–F).

use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash, DefEntry, LowerError, LoweredModule};
use marv_syntax::parse;

/// Parse + lower a module, panicking with context on failure.
fn lower(src: &str) -> LoweredModule {
    let module = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n--- source ---\n{src}"));
    lower_module(&module).unwrap_or_else(|e| panic!("lower failed: {e}\n--- source ---\n{src}"))
}

/// The content hashes of every definition in a module, in source order.
fn hashes(src: &str) -> Vec<Hash> {
    lower(src).defs.iter().map(|d| d.hash).collect()
}

/// The single def's body Core, asserting the module has exactly one definition.
fn only_def(src: &str) -> Def {
    let m = lower(src);
    assert_eq!(m.defs.len(), 1, "expected exactly one definition");
    m.defs.into_iter().next().unwrap().def
}

// ===== alpha-equivalence (the gate) =====================================

#[test]
fn alpha_equivalent_renamed_locals_hash_identically() {
    // Same logic; parameters and the `let` binding are renamed, and the second
    // form is written non-canonically (missing parens, tight spacing). Local
    // names and formatting are *not* part of identity, so the hashes must match.
    let a = "\
mod m

pure fn f(a: i32, b: i32) -> i32 {
    let s = (a + b)
    (s * a)
}
";
    let b = "\
mod m

pure fn f(first: i32, second: i32) -> i32 {
  let total = first+second
  total*first
}
";
    assert_eq!(hashes(a), hashes(b));
}

#[test]
fn alpha_equivalent_if_and_calls_hash_identically() {
    // `if`/`else` desugars to a Match; the called global `neg` is identical in
    // both, only the parameter name and formatting differ.
    let a = "\
mod demo

pure fn classify(n: i32) -> i32 {
    if (n < 0) {
        neg(n)
    } else {
        n
    }
}
";
    let b = "\
mod demo

pure fn classify(value: i32) -> i32 {
    if value < 0 {
        neg(value)
    } else {
        value
    }
}
";
    assert_eq!(hashes(a), hashes(b));
}

#[test]
fn renaming_a_called_global_changes_the_hash() {
    // The gate is about *local* renames. A called function's name is part of the
    // logic, so changing `neg` → `negate` must change the hash.
    let a = "\
mod demo

pure fn classify(n: i32) -> i32 {
    if (n < 0) {
        neg(n)
    } else {
        n
    }
}
";
    let b = "\
mod demo

pure fn classify(n: i32) -> i32 {
    if (n < 0) {
        negate(n)
    } else {
        n
    }
}
";
    assert_ne!(hashes(a), hashes(b));
}

#[test]
fn structurally_identical_structs_dedupe() {
    // Field names are erased; a struct's content is its ordered field types. Two
    // structs with identical field types therefore have the same content hash.
    let m = lower(
        "\
mod m

struct Point { x: i32, y: i32 }

struct Pair { first: i32, second: i32 }
",
    );
    assert_eq!(m.defs[0].hash, m.defs[1].hash);
}

#[test]
fn field_order_is_significant() {
    let a = lower("mod m\nstruct S { x: i32, y: bool }\n");
    let b = lower("mod m\nstruct S { x: bool, y: i32 }\n");
    assert_ne!(a.defs[0].hash, b.defs[0].hash);
}

#[test]
fn linear_struct_differs_from_plain() {
    let a = lower("mod m\nstruct S { x: i32 }\n");
    let b = lower("mod m\nlinear struct S { x: i32 }\n");
    assert_ne!(a.defs[0].hash, b.defs[0].hash);
}

#[test]
fn a_function_name_is_not_part_of_its_identity() {
    // Renaming the *defined* function (not a callee) does not change its hash.
    let a = only_def("mod m\npure fn add(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n");
    let b = only_def("mod m\npure fn plus(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n");
    assert_eq!(a.content_hash(), b.content_hash());
}

#[test]
fn lowering_is_deterministic() {
    let src = "mod m\npure fn f(a: i32) -> i32 {\n    (a + a)\n}\n";
    assert_eq!(hashes(src), hashes(src));
}

// ===== hand-written lowering goldens ====================================

#[test]
fn golden_curried_arithmetic() {
    // `(a + b)` ⇒ a single Prim tail; two params curry into nested Lams; de
    // Bruijn indices count from the innermost binder (b = 0, a = 1).
    let def = only_def("mod m\npure fn add(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n");

    let i32t = || Type::Int(IntTy::I32);
    let expected = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(i32t()),
            ret: Box::new(Type::Arrow {
                param: Box::new(i32t()),
                ret: Box::new(i32t()),
                effects: EffectRow::empty(),
            }),
            effects: EffectRow::empty(),
        },
        requires: vec![],
        ensures: vec![],
        body: Some(Core::Lam {
            param: i32t(),
            effects: EffectRow::empty(),
            body: Box::new(Core::Lam {
                param: i32t(),
                effects: EffectRow::empty(),
                body: Box::new(Core::Prim {
                    op: PrimOp::Add,
                    args: vec![Atom::Var(1), Atom::Var(0)],
                }),
            }),
        }),
    };
    assert_eq!(def, expected);
}

#[test]
fn golden_anf_hoists_nested_calls() {
    // `g(h(a))` must hoist the inner call into a `let`, leaving both applications
    // with atomic operands. Nullary-free `g`/`h` are globals.
    let def = only_def("mod m\npure fn f(a: i32) -> i32 {\n    g(h(a))\n}\n");

    // body (one param `a` at level 0):
    //   let t0 = App(h, a)   // value at depth 1: `a` is level 0 ⇒ index 0
    //   App(g, t0)           // body at depth 2: t0 is level 1 ⇒ index 0
    let expected_body = Core::Lam {
        param: Type::Int(IntTy::I32),
        effects: EffectRow::empty(),
        body: Box::new(Core::Let {
            value: Box::new(Core::App {
                func: Atom::Global(symbol_hash("h")),
                arg: Atom::Var(0),
            }),
            body: Box::new(Core::App {
                func: Atom::Global(symbol_hash("g")),
                arg: Atom::Var(0),
            }),
        }),
    };

    assert_eq!(def.body.as_ref().unwrap(), &expected_body);
}

#[test]
fn golden_if_lowers_to_bool_match() {
    // `if c { t } else { e }` ⇒ Match on the bool, branch order false-then-true.
    let def = only_def(
        "mod m\npure fn pick(c: bool) -> i32 {\n    if c {\n        1\n    } else {\n        0\n    }\n}\n",
    );

    let expected_body = Core::Lam {
        param: Type::Bool,
        effects: EffectRow::empty(),
        body: Box::new(Core::Match {
            scrutinee: Atom::Var(0), // `c`
            branches: vec![
                Branch {
                    binds: 0,
                    body: Core::Atom(Atom::Lit(Literal::Int(0))), // false ⇒ else
                },
                Branch {
                    binds: 0,
                    body: Core::Atom(Atom::Lit(Literal::Int(1))), // true ⇒ then
                },
            ],
        }),
    };
    assert_eq!(def.body.as_ref().unwrap(), &expected_body);
}

#[test]
fn golden_method_call_desugars_and_anf_normalizes() {
    // `io.write("hi")` ⇒ App(App(write, io), "hi"), the inner App hoisted.
    let def = only_def("mod m\nfn run(io: Io) -> () {\n    io.write(\"hi\")\n}\n");

    let write = Atom::Global(symbol_hash("write"));
    let io_ty = Type::Nominal {
        def: symbol_hash("Io"),
        args: vec![],
    };
    let expected_body = Core::Lam {
        param: io_ty,
        effects: EffectRow::empty(),
        body: Box::new(Core::Let {
            // let t0 = App(write, io)
            value: Box::new(Core::App {
                func: write,
                arg: Atom::Var(0), // io
            }),
            // App(t0, "hi")
            body: Box::new(Core::App {
                func: Atom::Var(0), // t0, freshest
                arg: Atom::Lit(Literal::Str("hi".to_string())),
            }),
        }),
    };
    assert_eq!(def.body.as_ref().unwrap(), &expected_body);
}

#[test]
fn golden_field_projection_resolves_index() {
    // `p.y` on a `&Point` resolves to Proj index 1 using the struct declaration.
    let m = lower(
        "\
mod g

struct Point { x: i32, y: i32 }

pure fn gety(p: &Point) -> i32 {
    p.y
}
",
    );
    let gety = &m.defs[1].def;
    let point_ty = Type::Ref {
        mutable: false,
        of: Box::new(Type::Nominal {
            def: symbol_hash("g.Point"),
            args: vec![],
        }),
    };
    let expected_body = Core::Lam {
        param: point_ty,
        effects: EffectRow::empty(),
        body: Box::new(Core::Proj {
            base: Atom::Var(0), // p
            idx: 1,             // field `y`
        }),
    };
    assert_eq!(gety.body.as_ref().unwrap(), &expected_body);
}

#[test]
fn unresolved_projection_is_an_honest_error() {
    // No annotation on the base, so M1 cannot resolve the field index.
    let module = parse("mod m\nfn f(p: Unknown) -> i32 {\n    p.field\n}\n").unwrap();
    match lower_module(&module) {
        Err(LowerError::UnresolvedProjection { field }) => assert_eq!(field, "field"),
        other => panic!("expected UnresolvedProjection, got {other:?}"),
    }
}

#[test]
fn def_entry_names_are_preserved() {
    let m = lower("mod m\npure fn answer() -> i32 {\n    42\n}\n");
    let entry: &DefEntry = &m.defs[0];
    assert_eq!(entry.name, "answer");
}
