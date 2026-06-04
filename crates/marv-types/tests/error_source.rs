//! End-to-end checker coverage for error handling driven from real `.mv` source
//! (parse → lower → [`World::from_module`] → [`check_def`]), MARV-3. The error-
//! set *values* and cross-call propagation are asserted in marv-db's
//! `error_sets.rs` (which runs the fixpoint pass); here we assert the checker's
//! diagnostics: `!T` opens the error set, a plain return does not, and an
//! exhaustive `match` over a caught error value is enforced.

use marv_core::lower_module;
use marv_syntax::parse;
use marv_types::{check_def, Code, Severity, World};

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

fn errors(src: &str) -> Vec<(String, marv_types::Diagnostic)> {
    diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect()
}

/// A `!T` function that raises an error directly checks clean: declaring `!`
/// opens the error set, so the inferred error need not be listed (`spec/01` §6).
#[test]
fn error_union_fn_raising_checks_clean() {
    let src = "\
mod m

error ParseError { Empty, Overflow }

fn digit(b: i64) -> !i64 {
    if (b < 0) {
        ParseError.Empty
    } else {
        b
    }
}
";
    let errs = errors(src);
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}

/// A plain (non-`!`) function that raises an error it does not declare is
/// reported — `!`-ness is what opens the set.
#[test]
fn plain_fn_raising_fires_missing_error() {
    let src = "\
mod m

error ParseError { Empty }

fn bad(b: i64) -> i64 {
    ParseError.Empty
}
";
    let errs = errors(src);
    assert!(
        errs.iter().any(|(_, d)| d.code == Code::MissingError),
        "expected MissingError, got: {errs:?}"
    );
}

/// A `match` over a caught error value is exhaustiveness-checked: covering only
/// some of an error type's variants fires `NonExhaustiveMatch`.
#[test]
fn non_exhaustive_match_over_error_fires() {
    let src = "\
mod m

error ParseError { Empty, Overflow }

pure fn describe(e: ParseError) -> i64 {
    match e {
        ParseError.Empty => 1,
    }
}
";
    let errs = errors(src);
    assert!(
        errs.iter().any(|(_, d)| d.code == Code::NonExhaustiveMatch),
        "expected NonExhaustiveMatch over the error type, got: {errs:?}"
    );
}

/// The complementary positive: matching *all* of an error type's variants
/// checks clean.
#[test]
fn exhaustive_match_over_error_checks_clean() {
    let src = "\
mod m

error ParseError { Empty, Overflow }

pure fn describe(e: ParseError) -> i64 {
    match e {
        ParseError.Empty => 1,
        ParseError.Overflow => 2,
    }
}
";
    let errs = errors(src);
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}
