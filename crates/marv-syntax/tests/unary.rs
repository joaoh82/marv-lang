//! Round-trip and canonical-form tests for the MARV-23 surface: the prefix
//! unary expression operators `-e`, `not e`, `&e`, and `&mut e`
//! (`spec/02` §B `unary`).
//!
//! These are additionally fuzzed in `roundtrip.rs` (the structural generator
//! emits `Unary` over a postfix operand); here they are pinned with hand-written
//! canonical modules and a few shape/spacing checks.

use marv_syntax::ast::*;
use marv_syntax::{format, format_module, parse};

/// Each entry is already in canonical form: it must reprint to itself
/// (`format(src) == src`), round-trip through the AST (`parse(format(ast)) ==
/// ast`), and format idempotently.
const CANONICAL: &[&str] = &[
    // All four operators as `let` values; `&mut`/`not` take a separating space,
    // `&`/`-` abut their operand.
    "mod demo\n\npure fn run(n: i64) -> i64 {\n    let a = &n\n    let b = &mut n\n    let c = -n\n    let d = not true\n    c\n}\n",
    // `&e` as a call argument — the canonical reference-passing style.
    "mod demo\n\npure fn f(x: &i64) -> i64 {\n    0\n}\n\npure fn run(n: i64) -> i64 {\n    f(&n)\n}\n",
    // Unary binds tighter than a binary operator: `(-n + 1)` groups as
    // `((-n) + 1)`, printed with the unary abutting its operand inside the
    // fully-parenthesized binary node.
    "mod demo\n\npure fn run(n: i64) -> i64 {\n    (-n + 1)\n}\n",
    // `not` over a (parenthesized) comparison.
    "mod demo\n\npure fn run(a: i64, b: i64) -> bool {\n    not (a == b)\n}\n",
    // Stacked unaries round-trip (the parser is right-recursive): `not not p`
    // and `--n`.
    "mod demo\n\npure fn run(done: bool) -> bool {\n    not not done\n}\n",
    "mod demo\n\npure fn run(n: i64) -> i64 {\n    --n\n}\n",
    // A `&` *expression* alongside a `&[]T` *type* (different grammar positions).
    "mod demo\n\nstruct Sale { amount: i64 }\n\npure fn total(xs: &[]Sale) -> i64 {\n    xs[0].amount\n}\n\npure fn run(xs: []Sale) -> i64 {\n    total(&xs)\n}\n",
];

#[test]
fn canonical_unary_modules_round_trip() {
    for (i, src) in CANONICAL.iter().enumerate() {
        assert_eq!(
            format(src),
            *src,
            "case {i}: source is not in canonical form"
        );
        let ast = parse(src).unwrap_or_else(|e| panic!("case {i}: parse failed: {e}"));
        let printed = format_module(&ast);
        let reparsed = parse(&printed).unwrap_or_else(|e| panic!("case {i}: reparse failed: {e}"));
        assert_eq!(reparsed, ast, "case {i}: parse(format(ast)) != ast");
        assert_eq!(format(&printed), printed, "case {i}: format not idempotent");
    }
}

/// The four operators produce the expected AST shape, and unary binds tighter
/// than a following binary operator.
#[test]
fn unary_parses_to_expected_shape() {
    let body = |src: &str| -> Expr {
        let m = parse(src).expect("parses");
        let Item::Fn(f) = &m.items[0] else {
            panic!("expected a fn item")
        };
        let body = f.body.as_ref().expect("function has a body");
        match body.tail.clone().expect("a tail expression") {
            Tail::Expr(e) => e,
            other => panic!("expected a tail expression, got {other:?}"),
        }
    };

    let n = || Box::new(Expr::Var("n".to_string()));
    assert_eq!(
        body("mod m\npure fn f(n: i64) -> i64 {\n-n\n}\n"),
        Expr::Unary(UnOp::Neg, n())
    );
    assert_eq!(
        body("mod m\npure fn f(n: bool) -> bool {\nnot n\n}\n"),
        Expr::Unary(UnOp::Not, n())
    );
    assert_eq!(
        body("mod m\npure fn f(n: i64) -> i64 {\n&n\n}\n"),
        Expr::Unary(UnOp::Ref, n())
    );
    assert_eq!(
        body("mod m\npure fn f(n: i64) -> i64 {\n&mut n\n}\n"),
        Expr::Unary(UnOp::RefMut, n())
    );

    // `-n + 1` → `((-n) + 1)`: the unary is the left operand of `+`, proving it
    // binds tighter than the binary operator (grammar `bin_expr = unary , {...}`).
    assert_eq!(
        body("mod m\npure fn f(n: i64) -> i64 {\n-n + 1\n}\n"),
        Expr::Binary(
            Box::new(Expr::Unary(UnOp::Neg, n())),
            BinOp::Add,
            Box::new(Expr::Int(1)),
        )
    );
}

/// `not` is a reserved word (like `and`/`or`): it can no longer be used as an
/// ordinary identifier.
#[test]
fn not_is_reserved() {
    assert!(
        parse("mod m\npure fn f() -> i64 {\nlet not = 1\nnot\n}\n").is_err(),
        "`not` must be reserved and rejected as a binding name"
    );
}
