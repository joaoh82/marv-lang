//! Round-trip and canonical-form tests for `enum` declarations and `match`
//! expressions (and the generics they ride in on). These constructs are not
//! produced by the structural fuzzer in `roundtrip.rs` — building a *valid*
//! exhaustive `match` requires the enum in scope — so they are covered here with
//! hand-written modules plus the real `std/` prelude files.

use std::fs;
use std::path::{Path, PathBuf};

use marv_syntax::{format, format_module, parse};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root is two levels above crates/marv-syntax")
        .to_path_buf()
}

/// Each of these is already in canonical form: it must parse, reprint to itself
/// (`format(src) == src`), and round-trip through the AST (`parse(format(ast))
/// == ast`).
const CANONICAL: &[&str] = &[
    // A monomorphic enum + an exhaustive `match` at a function tail.
    "mod demo\n\nenum Color {\n    Red,\n    Green,\n    Blue,\n}\n\npure fn rank(c: Color) -> i64 {\n    match c {\n        Color.Red => 1,\n        Color.Green => 2,\n        Color.Blue => 3,\n    }\n}\n",
    // A payload-carrying variant, a wildcard field pattern, and a `_` arm.
    "mod demo\n\nenum Shape {\n    Circle(i64),\n    Rect(i64, i64),\n}\n\npure fn area_kind(s: Shape) -> i64 {\n    match s {\n        Shape.Circle(_) => 1,\n        Shape.Rect(w, h) => 2,\n    }\n}\n",
    // Generics on both the enum and the functions, plus a bound field binder.
    "mod demo\n\nenum Option[T] {\n    None,\n    Some(T),\n}\n\npure fn unwrap_or[T](opt: Option[T], fallback: T) -> T {\n    match opt {\n        Option.None => fallback,\n        Option.Some(x) => x,\n    }\n}\n",
];

#[test]
fn canonical_enum_match_modules_round_trip() {
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

/// The `std/` prelude files that use enums + generics + `match` must parse and
/// reprint to themselves — i.e. they are canonical and fully inside the parsed
/// subset (they are no longer "reference-only").
#[test]
fn std_prelude_enum_files_are_canonical() {
    for rel in ["std/option.mv", "std/result.mv"] {
        let path = repo_root().join(rel);
        let src = fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {rel}"));
        parse(&src).unwrap_or_else(|e| panic!("{rel} did not parse: {e}"));
        assert_eq!(
            format(&src),
            src,
            "{rel} is not in canonical form; run `marv fmt {rel}`"
        );
    }
}
