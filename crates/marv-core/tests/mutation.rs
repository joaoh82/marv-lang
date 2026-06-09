//! Lowering of the MARV-4 surface — struct literals, index expressions, and
//! assignment statements — to the Core IR (`spec/02-grammar-and-core-ir.md`
//! §§C–D):
//!
//! - a struct literal → `Ctor { tag: 0, fields }` with fields in *declaration*
//!   order (so the field write-order in source does not change identity);
//! - `a[i]` → `Prim { op: Index, .. }`;
//! - `var` reassignment → ANF rebinding (no mutable cell in Core); a field
//!   update `p.x = e` → a `Ctor` rebuilding the aggregate from the other fields'
//!   `Proj`ections (mutable value semantics, `spec/01` §4);
//! - the lowering-time errors that guard these (assign to a `let`, deferred
//!   index store, unknown/incomplete struct literal).

use marv_core::ir::*;
use marv_core::lower::LowerError;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
}

fn try_lower(src: &str) -> Result<LoweredModule, LowerError> {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m)
}

fn def<'a>(m: &'a LoweredModule, name: &str) -> &'a DefEntry {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}`"))
}

/// The innermost (non-lambda) body of a function definition.
fn fn_body(m: &LoweredModule, name: &str) -> Core {
    let mut body = def(m, name).def.body.clone().expect("fn body");
    while let Core::Lam { body: inner, .. } = body {
        body = *inner;
    }
    body
}

/// Count `Ctor` and `Proj` nodes anywhere in a Core term (for structural
/// assertions about an aggregate rebuild).
fn count(c: &Core, ctors: &mut usize, projs: &mut usize) {
    match c {
        Core::Ctor { fields, .. } => {
            *ctors += 1;
            let _ = fields;
        }
        Core::Proj { .. } => *projs += 1,
        _ => {}
    }
    match c {
        Core::Let { value, body } => {
            count(value, ctors, projs);
            count(body, ctors, projs);
        }
        Core::Match { branches, .. } => {
            for b in branches {
                count(&b.body, ctors, projs);
            }
        }
        Core::Lam { body, .. } | Core::Loop { body, .. } => count(body, ctors, projs),
        _ => {}
    }
}

const POINT: &str = "struct Point { x: i64, y: i64 }";

#[test]
fn struct_literal_lowers_to_ctor_in_declaration_order() {
    // Field initializers are written out of declaration order; lowering must put
    // them back into declaration order (x, y) for the `Ctor`.
    let src = format!(
        "mod demo\n\n{POINT}\n\npure fn make() -> Point {{\n    Point {{ y: 2, x: 1 }}\n}}\n"
    );
    let m = lower(&src);
    match fn_body(&m, "make") {
        Core::Ctor { tag, fields, .. } => {
            assert_eq!(tag, 0, "products use tag 0");
            assert_eq!(
                fields,
                vec![Atom::Lit(Literal::Int(1)), Atom::Lit(Literal::Int(2))],
                "fields are in declaration order x=1, y=2"
            );
        }
        other => panic!("expected a Ctor, got {other:?}"),
    }
}

#[test]
fn struct_literal_field_write_order_is_canonical() {
    // Two literals differing only in the order fields are written lower to the
    // identical Core, hence the identical content hash (`spec/02` §F).
    let in_order = lower(&format!(
        "mod demo\n\n{POINT}\n\npure fn make() -> Point {{\n    Point {{ x: 1, y: 2 }}\n}}\n"
    ));
    let reordered = lower(&format!(
        "mod demo\n\n{POINT}\n\npure fn make() -> Point {{\n    Point {{ y: 2, x: 1 }}\n}}\n"
    ));
    assert_eq!(
        def(&in_order, "make").hash,
        def(&reordered, "make").hash,
        "field write-order must not affect identity"
    );
}

#[test]
fn index_lowers_to_prim_index() {
    let src = "mod demo\n\npure fn first(xs: []i64) -> i64 {\n    xs[0]\n}\n";
    match fn_body(&lower(src), "first") {
        Core::Prim { op, args } => {
            assert_eq!(op, PrimOp::Index);
            assert_eq!(args.len(), 2);
            // base is the (only) parameter, index is the literal 0.
            assert_eq!(args[0], Atom::Var(0));
            assert_eq!(args[1], Atom::Lit(Literal::Int(0)));
        }
        other => panic!("expected a Prim Index, got {other:?}"),
    }
}

#[test]
fn var_reassignment_rebinds_in_anf() {
    // `x = (x + 1)` introduces a fresh binding (a `Let` over a `Prim`); there is
    // no mutable cell in Core.
    let src = "mod demo\n\npure fn run() -> i64 {\n    var x = 1\n    x = (x + 1)\n    x\n}\n";
    let body = fn_body(&lower(src), "run");
    match &body {
        Core::Let { value, .. } => match value.as_ref() {
            Core::Prim { op, .. } => assert_eq!(*op, PrimOp::Add),
            other => panic!("expected the reassignment's `+` Prim, got {other:?}"),
        },
        other => panic!("expected a Let spine, got {other:?}"),
    }
}

#[test]
fn field_assignment_rebuilds_the_aggregate() {
    // `p.x = 9` rebuilds `p` as a new `Ctor` over the replaced field plus a
    // `Proj` of the untouched one — so the body has two Ctors (the literal and
    // the rebuild) and one Proj.
    let src = format!(
        "mod demo\n\n{POINT}\n\npure fn run() -> Point {{\n    var p = Point {{ x: 1, y: 2 }}\n    p.x = 9\n    p\n}}\n"
    );
    let body = fn_body(&lower(&src), "run");
    let (mut ctors, mut projs) = (0, 0);
    count(&body, &mut ctors, &mut projs);
    assert_eq!(ctors, 2, "the literal and the field-update rebuild");
    assert_eq!(
        projs, 1,
        "the untouched field is projected from the old value"
    );
}

#[test]
fn assignment_to_let_is_rejected() {
    let src = "mod demo\n\npure fn run() -> i64 {\n    let x = 1\n    x = 2\n    x\n}\n";
    assert!(matches!(
        try_lower(src),
        Err(LowerError::AssignToImmutable { .. })
    ));
}

#[test]
fn slice_index_store_lowers_to_index_set() {
    // A store into a runtime-length slice cannot use the array's static unroll, so
    // it lowers to a `Core::IndexSet` over the slice, the index, and the new value
    // (MARV-33). The store then rebinds the root `var`.
    let src = "mod demo\n\npure fn run(xs: []i64) -> () {\n    var ys = xs\n    ys[0] = 1\n    return\n}\n";
    let body = fn_body(&lower(src), "run");
    assert!(
        has_index_set(&body),
        "a slice element store must lower to a `Core::IndexSet`"
    );
}

/// Whether a `Core::IndexSet` appears anywhere in a Core term.
fn has_index_set(c: &Core) -> bool {
    match c {
        Core::IndexSet { .. } => true,
        Core::Let { value, body } => has_index_set(value) || has_index_set(body),
        Core::Match { branches, .. } => branches.iter().any(|b| has_index_set(&b.body)),
        Core::Lam { body, .. } | Core::Loop { body, .. } => has_index_set(body),
        _ => false,
    }
}

#[test]
fn unknown_struct_literal_is_rejected() {
    let src = "mod demo\n\npure fn run() -> i64 {\n    let p = Nope { x: 1 }\n    0\n}\n";
    assert!(matches!(
        try_lower(src),
        Err(LowerError::UnknownStruct { .. })
    ));
}

#[test]
fn struct_literal_missing_field_is_rejected() {
    let src =
        format!("mod demo\n\n{POINT}\n\npure fn run() -> Point {{\n    Point {{ x: 1 }}\n}}\n");
    assert!(matches!(
        try_lower(&src),
        Err(LowerError::MissingStructField { .. })
    ));
}

#[test]
fn struct_literal_unknown_field_is_rejected() {
    let src = format!(
        "mod demo\n\n{POINT}\n\npure fn run() -> Point {{\n    Point {{ x: 1, y: 2, z: 3 }}\n}}\n"
    );
    assert!(matches!(
        try_lower(&src),
        Err(LowerError::UnknownField { .. })
    ));
}
