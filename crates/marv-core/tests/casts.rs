//! Lowering of the MARV-7 scalar/collection surface to the Core IR
//! (`spec/02-grammar-and-core-ir.md` §§C–D, `spec/01` §3.1):
//!
//! - `e as T` → `Core::Cast { value, to }` carrying the lowered target type;
//! - a `char` literal → `Atom::Lit(Literal::Char(_))`;
//! - the `len(x)` builtin → `Prim { op: Len, .. }` (not a function call);
//! - identity hashing: alpha-equivalent functions using casts hash identically
//!   (the M1 gate, `spec/02` §F).

use marv_core::ir::*;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
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

/// Find the first `Cast` node anywhere in a Core term.
fn find_cast(c: &Core) -> Option<(Atom, Type)> {
    match c {
        Core::Cast { value, to } => Some((value.clone(), to.clone())),
        Core::Let { value, body } => find_cast(value).or_else(|| find_cast(body)),
        Core::Match { branches, .. } => branches.iter().find_map(|b| find_cast(&b.body)),
        Core::Lam { body, .. } => find_cast(body),
        Core::Loop { cond, body, .. } => find_cast(cond).or_else(|| find_cast(body)),
        _ => None,
    }
}

fn has_prim(c: &Core, op: PrimOp) -> bool {
    match c {
        Core::Prim { op: o, .. } => *o == op,
        Core::Let { value, body } => has_prim(value, op) || has_prim(body, op),
        Core::Match { branches, .. } => branches.iter().any(|b| has_prim(&b.body, op)),
        Core::Lam { body, .. } => has_prim(body, op),
        Core::Loop { cond, body, .. } => has_prim(cond, op) || has_prim(body, op),
        _ => false,
    }
}

#[test]
fn cast_lowers_to_cast_node_with_target_type() {
    let m = lower("mod m\n\nfn f(n: i64) -> u8 {\n    (n as u8)\n}\n");
    let (_, to) = find_cast(&fn_body(&m, "f")).expect("a Cast node");
    assert_eq!(to, Type::Int(IntTy::U8));
}

#[test]
fn char_literal_lowers_to_char_lit() {
    let m = lower("mod m\n\nfn f() -> char {\n    'A'\n}\n");
    match fn_body(&m, "f") {
        Core::Atom(Atom::Lit(Literal::Char(c))) => assert_eq!(c, 'A'),
        other => panic!("expected a char literal, got {other:?}"),
    }
}

#[test]
fn len_builtin_lowers_to_prim_len() {
    let m = lower("mod m\n\nfn f(s: str) -> usize {\n    len(s)\n}\n");
    assert!(
        has_prim(&fn_body(&m, "f"), PrimOp::Len),
        "len(x) must lower to Prim::Len"
    );
}

#[test]
fn a_local_named_len_shadows_the_builtin() {
    // When `len` is a parameter, `len(s)` is an ordinary call, not `Prim::Len`.
    let m = lower("mod m\n\nfn f(len: i64, s: i64) -> i64 {\n    s\n}\n\nfn g(s: str) -> usize {\n    len(s)\n}\n");
    // `g` still uses the builtin (no local `len`).
    assert!(has_prim(&fn_body(&m, "g"), PrimOp::Len));
}

#[test]
fn alpha_equivalent_casts_hash_identically() {
    let a = lower("mod m\n\nfn f(x: i64) -> u8 {\n    (x as u8)\n}\n");
    let b = lower("mod m\n\nfn f(y: i64) -> u8 {\n    (y as u8)\n}\n");
    assert_eq!(
        def(&a, "f").hash,
        def(&b, "f").hash,
        "renaming the parameter must not change the content hash"
    );
}

#[test]
fn casts_to_different_widths_hash_differently() {
    let a = lower("mod m\n\nfn f(x: i64) -> u8 {\n    (x as u8)\n}\n");
    let b = lower("mod m\n\nfn f(x: i64) -> u16 {\n    (x as u16)\n}\n");
    assert_ne!(
        def(&a, "f").hash,
        def(&b, "f").hash,
        "the cast target type is part of identity"
    );
}
