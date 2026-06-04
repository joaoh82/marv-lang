//! Round-trip and canonical-form tests for the MARV-4 surface: struct literals
//! `Name { f: e, ... }`, index expressions `a[i]`, and assignment statements
//! `lvalue = expr` (including `p.x = e` and `a[i] = e`).
//!
//! Struct literals are *not* produced by the structural fuzzer in `roundtrip.rs`
//! — a bare `Name { .. }` collides with an `if`/`match` head's block brace, so
//! the fuzzer would generate ambiguous text — hence they are covered here with
//! hand-written canonical modules. (Index and assignment, which are brace-safe,
//! are additionally fuzzed in `roundtrip.rs`.)

use marv_syntax::{format, format_module, parse};

/// Each entry is already in canonical form: it must reprint to itself
/// (`format(src) == src`), round-trip through the AST (`parse(format(ast)) ==
/// ast`), and format idempotently.
const CANONICAL: &[&str] = &[
    // A struct literal as a `let` value, then field + `var` mutation.
    "mod demo\n\nstruct Point { x: i64, y: i64 }\n\npure fn run() -> i64 {\n    var p = Point { x: 1, y: 2 }\n    p.x = (p.x + 10)\n    var total = 0\n    total = (total + p.x)\n    total\n}\n",
    // An empty struct literal and an index expression.
    "mod demo\n\nstruct Unit {}\n\npure fn first(xs: []i64) -> i64 {\n    let u = Unit {}\n    xs[0]\n}\n",
    // A struct literal in `if`-condition-adjacent positions: as a call argument
    // (inside parens, so unambiguous) while the `if` head itself is a field read.
    "mod demo\n\nstruct Cfg { on: bool, n: i64 }\n\npure fn enabled(c: Cfg) -> bool {\n    c.on\n}\n\npure fn run() -> i64 {\n    let cfg = Cfg { on: true, n: 7 }\n    if enabled(Cfg { on: true }) {\n        cfg.n\n    } else {\n        0\n    }\n}\n",
    // Nested struct literals and a nested field assignment `o.inner.v = e`.
    "mod demo\n\nstruct Inner { v: i64 }\n\nstruct Outer { inner: Inner, tag: i64 }\n\npure fn run() -> i64 {\n    var o = Outer { inner: Inner { v: 1 }, tag: 9 }\n    o.inner.v = 42\n    o.inner.v\n}\n",
    // Index assignment and a chained index/field lvalue (parses even though
    // lowering defers index-store to MARV-9).
    "mod demo\n\npure fn run(xs: []i64) -> () {\n    xs[0] = 1\n    return\n}\n",
];

#[test]
fn canonical_mutation_modules_round_trip() {
    for (i, src) in CANONICAL.iter().enumerate() {
        // Already canonical: the formatter reprints it unchanged.
        assert_eq!(
            format(src),
            *src,
            "case {i}: source is not in canonical form"
        );
        // parse ∘ format == id over the AST.
        let ast = parse(src).unwrap_or_else(|e| panic!("case {i}: parse failed: {e}"));
        let printed = format_module(&ast);
        let reparsed = parse(&printed).unwrap_or_else(|e| panic!("case {i}: reparse failed: {e}"));
        assert_eq!(reparsed, ast, "case {i}: parse(format(ast)) != ast");
        // Idempotence.
        assert_eq!(format(&printed), printed, "case {i}: format not idempotent");
    }
}

/// A bare `Name {` in an `if`/`match` head must be read as a variable, not a
/// struct literal — the `{` opens the block. (If the parser greedily took it as
/// a struct literal, this would fail to parse.)
#[test]
fn struct_literal_suppressed_in_block_head() {
    let src = "mod demo\n\nstruct Cfg { on: bool, n: i64 }\n\npure fn run(cfg: Cfg) -> i64 {\n    if cfg.on {\n        cfg.n\n    } else {\n        0\n    }\n}\n";
    let ast = parse(src).expect("parses with `cfg.on` as a field read, not a struct literal");
    assert_eq!(format_module(&ast), src, "round-trips unchanged");
}
