//! Round-trip and canonical-form tests for the MARV-2 loop surface: `while`
//! loops (with and without `invariant` clauses) and `for` loops.
//!
//! Loop heads contain a block brace immediately after the condition/iterator, so
//! a bare `Name {` there would collide with a struct literal — like `if`/`match`
//! heads, these are covered with hand-written canonical modules rather than the
//! structural fuzzer in `roundtrip.rs`.

use marv_syntax::{format, format_module, parse};

/// Each entry is already in canonical form: it must reprint to itself
/// (`format(src) == src`), round-trip through the AST, and format idempotently.
const CANONICAL: &[&str] = &[
    // A `while` with no invariant: the body brace shares the head line.
    "mod demo\n\npure fn count_down(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0) {\n        i = (i - 1)\n    }\n    i\n}\n",
    // A `while` with a single `invariant`: clause on its own line, body brace on
    // a fresh line (mirroring `fn` contract clauses).
    "mod demo\n\npure fn sum_to(n: i64) -> i64 {\n    var sum: i64 = 0\n    var i: i64 = n\n    while (i > 0)\n        invariant (i >= 0)\n    {\n        sum = (sum + i)\n        i = (i - 1)\n    }\n    sum\n}\n",
    // Multiple `invariant` clauses, in source order.
    "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = 0\n    while (i < n)\n        invariant (i >= 0)\n        invariant (i <= n)\n    {\n        i = (i + 1)\n    }\n    i\n}\n",
    // A `for` loop over a slice; the body brace shares the head line.
    "mod demo\n\npure fn total(xs: []i64) -> i64 {\n    var sum: i64 = 0\n    for x in xs {\n        sum = (sum + x)\n    }\n    sum\n}\n",
    // A loop is a statement, so ordinary code may follow it in the same block.
    "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = 0\n    while (i < n) {\n        i = (i + 1)\n    }\n    let done = i\n    done\n}\n",
    // Nested loops.
    "mod demo\n\npure fn grid(n: i64) -> i64 {\n    var total: i64 = 0\n    var i: i64 = 0\n    while (i < n) {\n        var j: i64 = 0\n        while (j < n) {\n            total = (total + 1)\n            j = (j + 1)\n        }\n        i = (i + 1)\n    }\n    total\n}\n",
];

#[test]
fn canonical_loop_modules_round_trip() {
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

/// A bare `Name {` in a `while` condition must be read as a variable, not a
/// struct literal — the `{` opens the loop body. (If the parser greedily took it
/// as a struct literal, this would fail to parse.)
#[test]
fn struct_literal_suppressed_in_while_head() {
    let src = "mod demo\n\nstruct Cfg { on: bool }\n\npure fn run(cfg: Cfg) -> i64 {\n    var i: i64 = 0\n    while cfg.on {\n        i = (i + 1)\n    }\n    i\n}\n";
    // `cfg.on` is the condition; `{` opens the body. Parses without error.
    assert!(parse(src).is_ok());
}

/// A non-canonical draft (unparenthesized condition, sloppy spacing) normalizes
/// to the canonical form.
#[test]
fn normalizes_a_loose_while() {
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = 0\n    while i<n {\n        i = i+1\n    }\n    i\n}\n";
    let expected = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = 0\n    while (i < n) {\n        i = (i + 1)\n    }\n    i\n}\n";
    assert_eq!(format(src), expected);
}
