//! Lowering of the MARV-23 prefix unary operators to the Core IR
//! (`spec/02-grammar-and-core-ir.md` §B `unary`, §§C–D):
//!
//! - `-e`   → `Prim { op: Neg, .. }`
//! - `not e` → `Prim { op: Not, .. }`
//! - `&e`   → `Core::Ref { mutable: false, .. }`
//! - `&mut e` → `Core::Ref { mutable: true, .. }`
//! - identity hashing: alpha-equivalent functions using unaries hash identically,
//!   and `&` vs `&mut` are distinct identities (`spec/02` §F).

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

/// Find the first `Ref` node anywhere in a Core term, returning its mutability.
fn find_ref(c: &Core) -> Option<bool> {
    match c {
        Core::Ref { mutable, .. } => Some(*mutable),
        Core::Let { value, body } => find_ref(value).or_else(|| find_ref(body)),
        Core::Match { branches, .. } => branches.iter().find_map(|b| find_ref(&b.body)),
        Core::Lam { body, .. } => find_ref(body),
        Core::Loop { cond, body, .. } => find_ref(cond).or_else(|| find_ref(body)),
        _ => None,
    }
}

#[test]
fn neg_lowers_to_prim_neg() {
    let m = lower("mod m\n\nfn f(n: i64) -> i64 {\n    -n\n}\n");
    assert!(
        has_prim(&fn_body(&m, "f"), PrimOp::Neg),
        "-e must lower to Prim::Neg"
    );
}

#[test]
fn not_lowers_to_prim_not() {
    let m = lower("mod m\n\nfn f(p: bool) -> bool {\n    not p\n}\n");
    assert!(
        has_prim(&fn_body(&m, "f"), PrimOp::Not),
        "not e must lower to Prim::Not"
    );
}

#[test]
fn ref_lowers_to_ref_node() {
    let m = lower("mod m\n\nfn f(n: i64) -> i64 {\n    let r = &n\n    n\n}\n");
    assert_eq!(
        find_ref(&fn_body(&m, "f")),
        Some(false),
        "&e must lower to a shared Core::Ref"
    );
}

#[test]
fn ref_mut_lowers_to_mutable_ref_node() {
    let m = lower("mod m\n\nfn f(n: i64) -> i64 {\n    let r = &mut n\n    n\n}\n");
    assert_eq!(
        find_ref(&fn_body(&m, "f")),
        Some(true),
        "&mut e must lower to a mutable Core::Ref"
    );
}

#[test]
fn alpha_equivalent_unaries_hash_identically() {
    let a = lower("mod m\n\nfn f(x: i64) -> i64 {\n    -x\n}\n");
    let b = lower("mod m\n\nfn f(y: i64) -> i64 {\n    -y\n}\n");
    assert_eq!(
        def(&a, "f").hash,
        def(&b, "f").hash,
        "renaming the parameter must not change the content hash"
    );
}

#[test]
fn shared_and_mutable_refs_hash_differently() {
    let a = lower("mod m\n\nfn f(n: i64) -> i64 {\n    let r = &n\n    n\n}\n");
    let b = lower("mod m\n\nfn f(n: i64) -> i64 {\n    let r = &mut n\n    n\n}\n");
    assert_ne!(
        def(&a, "f").hash,
        def(&b, "f").hash,
        "&T and &mut T are distinct identities"
    );
}

#[test]
fn neg_and_sub_hash_differently() {
    // `-x` (unary Neg) is a distinct operation from `0 - x` (binary Sub), so the
    // two must not collapse to the same content hash.
    let neg = lower("mod m\n\nfn f(x: i64) -> i64 {\n    -x\n}\n");
    let sub = lower("mod m\n\nfn f(x: i64) -> i64 {\n    (0 - x)\n}\n");
    assert_ne!(
        def(&neg, "f").hash,
        def(&sub, "f").hash,
        "unary Neg and binary Sub are distinct primitives"
    );
}
