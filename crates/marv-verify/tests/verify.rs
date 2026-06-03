//! M6 acceptance gate (Tier 2): prove a correct `clamp`; return a concrete
//! counterexample for a buggy one; report `unsupported` (→ runtime fallback)
//! for a function outside the verified subset. Mirrors `spec/03` §4.3.
//!
//! These tests need a z3 binary on `PATH`. When none is available the SMT layer
//! reports `SolverUnavailable` (the same fallback path as `unsupported`), and
//! the test skips rather than fails — so a solver-less CI stays green while a
//! solver-equipped run exercises the real prover.

use marv_core::{ir::Def, lower_module};
use marv_verify::{verify_def, VerifyOutcome};

/// Lower a single-function source module and return its `Def` + parameter names.
fn lower_one(src: &str) -> (Def, Vec<String>) {
    let module = marv_syntax::parse(src).expect("parse");
    let names: Vec<String> = match &module.items[0] {
        marv_syntax::Item::Fn(f) => f.params.iter().map(|p| p.name.clone()).collect(),
        _ => panic!("expected a function"),
    };
    let lowered = lower_module(&module).expect("lower");
    (lowered.defs[0].def.clone(), names)
}

/// Skip (don't fail) when no solver is present, so CI without z3 stays green.
fn skip_if_no_solver(o: &VerifyOutcome) -> bool {
    if let VerifyOutcome::SolverUnavailable { reason } = o {
        eprintln!("skipping: {reason}");
        return true;
    }
    false
}

const CLAMP_CORRECT: &str = "\
mod math

pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{
    if x < lo {
        lo
    } else if x > hi {
        hi
    } else {
        x
    }
}
";

// Buggy: the low bound is never applied (no `x < lo` arm), but the contract
// still claims `result >= lo`. This is the `spec/03` §4.3 scenario.
const CLAMP_BUGGY: &str = "\
mod math

pure fn clamp(x: i32, lo: i32, hi: i32) -> i32
    requires lo <= hi
    ensures result >= lo and result <= hi
{
    if x > hi {
        hi
    } else {
        x
    }
}
";

#[test]
fn proves_correct_clamp() {
    let (def, names) = lower_one(CLAMP_CORRECT);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "a correct clamp should be proved, got {outcome:?}"
    );
}

#[test]
fn counterexample_for_buggy_clamp() {
    let (def, names) = lower_one(CLAMP_BUGGY);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed {
            obligation,
            counterexample,
            ..
        } => {
            // The counterexample must assign every parameter and the result.
            let names: Vec<&str> = counterexample.iter().map(|(n, _)| n.as_str()).collect();
            assert!(
                names.contains(&"x"),
                "counterexample names x: {counterexample:?}"
            );
            assert!(names.contains(&"lo"));
            assert!(names.contains(&"hi"));
            assert!(names.contains(&"result"));

            // It must be a genuine violation of `result >= lo`: a model where
            // x < lo, so the else-branch returns x = result < lo.
            let val = |k: &str| -> i64 {
                counterexample
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse::<i64>().ok())
                    .unwrap_or_else(|| panic!("missing/non-int {k} in {counterexample:?}"))
            };
            assert!(
                val("result") < val("lo"),
                "counterexample should violate result >= lo: {counterexample:?}"
            );
            assert!(
                obligation.contains("result") && obligation.contains("lo"),
                "obligation should mention the violated clause: {obligation}"
            );
        }
        other => panic!("buggy clamp should fail with a counterexample, got {other:?}"),
    }
}

// Integer division is not (yet) in the verified subset, so a function using it
// must report `unsupported` — the honest fallback to Tier-1 runtime checks.
const OUT_OF_SUBSET: &str = "\
mod math

pure fn half(x: i64) -> i64
    ensures result <= x
{
    x / 2
}
";

#[test]
fn out_of_subset_is_unsupported() {
    let (def, names) = lower_one(OUT_OF_SUBSET);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Unsupported { reason } => {
            assert!(
                reason.contains("division") || reason.contains("subset"),
                "reason should explain the boundary: {reason}"
            );
        }
        other => panic!("a division-using function should be unsupported, got {other:?}"),
    }
}
