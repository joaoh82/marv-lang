//! Tier-1 runtime contract checks for the MARV-11 contract-language extension:
//! bounded quantifiers (`forall`/`exists`), contract arithmetic, `old(e)`, and
//! quantified loop invariants are all *evaluated* in the debug runner, with a
//! violation aborting in a structured report.

use marv_core::lower_module;
use marv_interp::{Program, RunError, Value};
use marv_types::World;

/// Lower a source module and wrap it in a runnable program.
fn program_from_source(src: &str) -> Program {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new(&module_path, defs, world)
}

const ALL_BELOW: &str = "\
mod demo

pure fn all_below(n: i64, cap: i64) -> i64
    requires (forall i in 0..n: (i < cap))
    ensures (result >= 0)
{
    n
}
";

#[test]
fn quantified_requires_passes_when_true() {
    let prog = program_from_source(ALL_BELOW);
    let out = prog
        .run("all_below", &[], &["3".into(), "5".into()])
        .expect("3 < 5 everywhere on 0..3");
    assert_eq!(out.value, Value::Int(3));
}

#[test]
fn quantified_requires_violation_aborts() {
    let prog = program_from_source(ALL_BELOW);
    let err = prog
        .run("all_below", &[], &["3".into(), "2".into()])
        .expect_err("i = 2 violates i < 2");
    match err {
        RunError::PreconditionFailed(report) => {
            assert!(
                report.contains("forall i in"),
                "the report renders the quantifier: {report}"
            );
        }
        other => panic!("expected a precondition failure, got {other:?}"),
    }
}

const EXISTS_WITNESS: &str = "\
mod demo

pure fn zero_in_range(n: i64) -> i64
    ensures (exists i in 0..n: (i == result))
{
    0
}
";

#[test]
fn exists_finds_witness_at_runtime() {
    let prog = program_from_source(EXISTS_WITNESS);
    let out = prog
        .run("zero_in_range", &[], &["2".into()])
        .expect("0 is in 0..2");
    assert_eq!(out.value, Value::Int(0));
}

#[test]
fn exists_over_empty_range_is_violated() {
    let prog = program_from_source(EXISTS_WITNESS);
    let err = prog
        .run("zero_in_range", &[], &["0".into()])
        .expect_err("an empty range has no witness");
    assert!(
        matches!(err, RunError::PostconditionFailed(_)),
        "expected a postcondition failure, got {err:?}"
    );
}

// Contract arithmetic (`result == n + n`) is evaluated with the body's
// 64-bit wrapping semantics.
const ARITH_ENSURES: &str = "\
mod demo

pure fn double_ish(n: i64) -> i64
    ensures (result == (n + n))
{
    n + 1
}
";

#[test]
fn arithmetic_ensures_violation_aborts() {
    let prog = program_from_source(ARITH_ENSURES);
    let err = prog
        .run("double_ish", &[], &["5".into()])
        .expect_err("6 != 10");
    match err {
        RunError::PostconditionFailed(report) => {
            // The runtime labels parameters positionally (`arg0`).
            assert!(
                report.contains("arg0 + arg0"),
                "the report renders the clause: {report}"
            );
        }
        other => panic!("expected a postcondition failure, got {other:?}"),
    }
}

// `old(n)` is the parameter's entry value — with immutable value-semantics
// parameters, exactly `n`.
const OLD_ENSURES: &str = "\
mod demo

pure fn bump(n: i64) -> i64
    ensures (result == (old(n) + 1))
{
    n + 1
}
";

#[test]
fn old_evaluates_to_entry_value() {
    let prog = program_from_source(OLD_ENSURES);
    let out = prog.run("bump", &[], &["41".into()]).expect("41 + 1 == 42");
    assert_eq!(out.value, Value::Int(42));
}

// A quantified loop invariant is checked at the header on every iteration;
// here it breaks once `i` reaches 4 (k = 3 violates k < 3).
const QUANT_INVARIANT: &str = "\
mod demo

pure fn drift(n: i64) -> i64 {
    var i: i64 = 0
    while (i < n)
        invariant (forall k in 0..i: (k < 3))
    {
        i = (i + 1)
    }
    i
}
";

#[test]
fn quantified_loop_invariant_violation_aborts() {
    let prog = program_from_source(QUANT_INVARIANT);
    let err = prog
        .run("drift", &[], &["5".into()])
        .expect_err("k = 3 violates k < 3 once i reaches 4");
    match err {
        RunError::InvariantViolated(report) => {
            assert!(
                report.contains("forall"),
                "the report renders the quantifier: {report}"
            );
        }
        other => panic!("expected an invariant violation, got {other:?}"),
    }
}

#[test]
fn quantified_loop_invariant_passes_within_bound() {
    let prog = program_from_source(QUANT_INVARIANT);
    let out = prog.run("drift", &[], &["3".into()]).expect("0..3 < 3");
    assert_eq!(out.value, Value::Int(3));
}
