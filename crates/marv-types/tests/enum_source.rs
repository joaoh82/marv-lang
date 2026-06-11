//! End-to-end checker coverage for enums + `match` driven from real `.mv`
//! source (parse → lower → [`World::from_module`] → [`check_def`]). The other
//! checker tests build enums by hand through [`WorldBuilder`]; these prove the
//! same rules now fire from source, which is the point of MARV-1.

use marv_core::lower_module;
use marv_syntax::parse;
use marv_types::{check_def, Code, Severity, World};

/// Lower a module and return every (def-name, diagnostic) the checker emits.
fn diagnostics(src: &str) -> Vec<(String, marv_types::Diagnostic)> {
    let module = parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let mut out = Vec::new();
    for entry in &lowered.defs {
        for d in check_def(&world, &entry.def, Some(&entry.name)) {
            out.push((entry.name.clone(), d));
        }
    }
    out
}

const EXHAUSTIVE: &str = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn rank(c: Color) -> i64 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}
";

#[test]
fn exhaustive_match_checks_clean() {
    let errs: Vec<_> = diagnostics(EXHAUSTIVE)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}

#[test]
fn missing_arm_fires_exhaustiveness() {
    let src = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn rank(c: Color) -> i64 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
    }
}
";
    let ds = diagnostics(src);
    let hit = ds
        .iter()
        .find(|(_, d)| d.code == Code::NonExhaustiveMatch)
        .unwrap_or_else(|| panic!("expected NonExhaustiveMatch, got: {ds:?}"));
    assert!(
        hit.1.message.contains("Blue"),
        "diagnostic should name the missing variant: {}",
        hit.1.message
    );
    // A mechanical fix is attached (`spec/03` §2).
    assert!(
        !hit.1.fixes.is_empty(),
        "exhaustiveness fix should be offered"
    );
}

#[test]
fn wildcard_arm_makes_match_exhaustive() {
    let src = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn is_red(c: Color) -> bool {
    match c {
        Color.Red => true,
        _ => false,
    }
}
";
    let errs: Vec<_> = diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "`_` arm should satisfy exhaustiveness: {errs:?}"
    );
}

#[test]
fn generic_enum_ctor_satisfies_declared_generic_return() {
    // A `Ctor` result carries no type arguments (`Nominal { args: [] }` — the
    // names-erased Core records only the nominal hash and tag), so it must
    // satisfy a declared parameterized return of the *same* enum, both at a
    // concrete instantiation (`Box[i64]`) and inside a generic body
    // (`Box[T]`). This is what `std/result.mv`'s `ok` relies on (MARV-18).
    let src = "\
mod demo

enum Box[T] {
    Empty,
    Full(T),
}

pure fn fill[T](x: T) -> Box[T] {
    Box.Full(x)
}

pure fn empty_i64() -> Box[i64] {
    Box.Empty
}
";
    let errs: Vec<_> = diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}

#[test]
fn ctor_of_a_different_enum_still_mismatches_generic_return() {
    // The unparameterized-Ctor compatibility is per-nominal: constructing some
    // *other* enum where `Box[i64]` is declared still fails E0101.
    let src = "\
mod demo

enum Box[T] {
    Empty,
    Full(T),
}

enum Hue {
    A,
    B,
}

pure fn wrong() -> Box[i64] {
    Hue.A
}
";
    let errs: Vec<_> = diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.code == Code::TypeMismatch)
        .collect();
    assert!(
        !errs.is_empty(),
        "constructing a different enum must still be a type error"
    );
}

#[test]
fn concrete_use_of_generic_enum_checks_clean() {
    // A *non-generic* function constructing and matching a generic enum at a
    // concrete instantiation: the declaration's field types are unresolved
    // parameters (`T`), so the constructed field (`i64`) and the arm-bound
    // payload check against a wildcard, not a mismatch (MARV-18 — the
    // `examples/optionals.mv` shape).
    let src = "\
mod demo

enum Box[T] {
    Empty,
    Full(T),
}

pure fn wrap(n: i64) -> Box[i64] {
    Box.Full(n)
}

pure fn or_zero(b: Box[i64]) -> i64 {
    match b {
        Box.Empty => 0,
        Box.Full(x) => x,
    }
}
";
    let errs: Vec<_> = diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}
