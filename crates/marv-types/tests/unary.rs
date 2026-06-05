//! Checker coverage for the MARV-23 prefix unary operators, driven from real
//! `.mv` source (parse → lower → [`World::from_module`] → [`check_module`]):
//!
//! - `-e` requires a numeric operand and preserves its type;
//! - `not e` requires a `bool` operand and yields `bool`;
//! - `&e` / `&mut e` produce a [`Type::Ref`] the checker recognizes — a value
//!   passed where a reference is expected is rejected, and a returned reference
//!   is an escaping-reference error.

use marv_core::lower_module;
use marv_syntax::parse;
use marv_types::{check_module, Code, Diagnostic, Severity};

fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let module = parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    check_module(&lowered)
}

fn errors(src: &str) -> Vec<Diagnostic> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect()
}

#[test]
fn neg_and_not_check_clean() {
    let src = "\
mod demo

pure fn classify(n: i64) -> i64 {
    if not (n == 0) {
        -n
    } else {
        0
    }
}
";
    assert!(
        errors(src).is_empty(),
        "`-n` and `not (n == 0)` are well typed: {:?}",
        errors(src)
    );
}

#[test]
fn not_on_int_is_rejected() {
    let errs = errors("mod m\n\npure fn f(n: i64) -> bool {\n    not n\n}\n");
    assert!(
        errs.iter().any(|d| d.code == Code::BadPrimOperand),
        "`not n` on an integer must be a BadPrimOperand: {errs:?}"
    );
}

#[test]
fn neg_on_bool_is_rejected() {
    let errs = errors("mod m\n\npure fn f(p: bool) -> bool {\n    -p\n}\n");
    assert!(
        errs.iter().any(|d| d.code == Code::BadPrimOperand),
        "`-p` on a bool must be a BadPrimOperand: {errs:?}"
    );
}

#[test]
fn reference_argument_checks_clean() {
    // `&s` must produce `&S` to match `take`'s `&S` parameter; projecting `r.v`
    // reads through the reference.
    let src = "\
mod demo

struct S { v: i64 }

pure fn take(r: &S) -> i64 {
    r.v
}

pure fn give(s: S) -> i64 {
    take(&s)
}
";
    assert!(
        errors(src).is_empty(),
        "passing `&s` to a `&S` parameter is well typed: {:?}",
        errors(src)
    );
}

#[test]
fn passing_value_where_reference_expected_is_rejected() {
    // Without the `&`, a bare `S` value does not satisfy a `&S` parameter — there
    // is no implicit reference-taking.
    let src = "\
mod demo

struct S { v: i64 }

pure fn take(r: &S) -> i64 {
    r.v
}

pure fn give(s: S) -> i64 {
    take(s)
}
";
    let errs = errors(src);
    assert!(
        errs.iter().any(|d| d.code == Code::TypeMismatch),
        "a value where a reference is expected must be a TypeMismatch: {errs:?}"
    );
}

#[test]
fn returning_a_reference_is_an_escape() {
    let errs = errors("mod m\n\npure fn leak(n: i64) -> &i64 {\n    &n\n}\n");
    assert!(
        errs.iter().any(|d| d.code == Code::EscapingReference),
        "a returned `&n` must be an EscapingReference: {errs:?}"
    );
}
