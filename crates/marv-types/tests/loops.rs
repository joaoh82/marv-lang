//! Checker coverage for the MARV-2 loop surface, driven from real `.mv` source
//! (parse → lower → [`World::from_module`] → [`check_def`]).

use marv_core::lower_module;
use marv_syntax::parse;
use marv_types::{check_def, Code, Severity, World};

/// Lower a module and return every diagnostic the checker emits across its defs.
fn diagnostics(src: &str) -> Vec<marv_types::Diagnostic> {
    let module = parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let mut out = Vec::new();
    for entry in &lowered.defs {
        out.extend(check_def(&world, &entry.def, Some(&entry.name)));
    }
    out
}

const WELL_TYPED: &str = "\
mod demo

pure fn sum_to(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
{
    var sum: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
    {
        sum = (sum + i)
        i = (i - 1)
    }
    sum
}
";

#[test]
fn well_typed_loop_checks_clean() {
    let errors: Vec<_> = diagnostics(WELL_TYPED)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "a well-typed `while` loop should check clean, got: {errors:?}"
    );
}

const NON_BOOL_CONDITION: &str = "\
mod demo

pure fn run(n: i64) -> i64 {
    var i: i64 = n
    while (i + 1) {
        i = (i - 1)
    }
    i
}
";

#[test]
fn non_boolean_loop_condition_is_a_type_error() {
    let diags = diagnostics(NON_BOOL_CONDITION);
    assert!(
        diags
            .iter()
            .any(|d| d.code == Code::TypeMismatch && d.severity == Severity::Error),
        "an `i64` loop condition should be a type mismatch, got: {diags:?}"
    );
}
