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

// ---- loop invariants (MARV-22) ------------------------------------------
//
// Hoare-style discharge of `while` invariants: initiation (holds on entry),
// consecution (preserved by the body), use (invariant ∧ ¬cond is all that is
// known after the loop). Counterexamples label carried slots positionally
// (`s0`, `s1`, …; primed for post-iteration values) — Core erases names.

/// The MARV-22 acceptance gate: `sum_to`'s loop proves once the invariant is
/// strong enough to carry `result >= 0` past the loop (mirrors
/// `examples/loops.mv`).
const SUM_TO: &str = "\
mod loops

pure fn sum_to(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
{
    var sum: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
        invariant (sum >= 0)
    {
        sum = (sum + i)
        i = (i - 1)
    }
    sum
}
";

#[test]
fn proves_sum_to_loop() {
    let (def, names) = lower_one(SUM_TO);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "sum_to with a strong enough invariant should be proved, got {outcome:?}"
    );
}

// `requires n >= 0` admits n = 0, and the initial sum is 0 — the invariant
// `sum > 0` cannot be established on entry (initiation fails).
const BAD_ENTRY: &str = "\
mod loops

pure fn bad_entry(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
{
    var sum: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (sum > 0)
    {
        sum = (sum + i)
        i = (i - 1)
    }
    sum
}
";

#[test]
fn wrong_invariant_fails_initiation_with_counterexample() {
    let (def, names) = lower_one(BAD_ENTRY);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed {
            message,
            counterexample,
            ..
        } => {
            assert!(
                message.contains("can fail on entry"),
                "initiation failure should say so: {message}"
            );
            let names: Vec<&str> = counterexample.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"n"), "counterexample names n: {names:?}");
            assert!(
                names.iter().any(|n| n.starts_with('s')),
                "counterexample includes the carried state: {names:?}"
            );
        }
        other => panic!("a wrong invariant should fail with a counterexample, got {other:?}"),
    }
}

// `i <= 0` holds on entry (i starts at 0) but `i = i + 1` breaks it — the
// consecution obligation must yield a counterexample, not a false `proved`.
const BAD_PRESERVE: &str = "\
mod loops

pure fn bad_preserve(n: i64) -> i64
    requires (n >= 0)
{
    var i: i64 = 0
    while (i < n)
        invariant (i <= 0)
    {
        i = (i + 1)
    }
    i
}
";

#[test]
fn wrong_invariant_fails_consecution_with_counterexample() {
    let (def, names) = lower_one(BAD_PRESERVE);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed {
            message,
            counterexample,
            ..
        } => {
            assert!(
                message.contains("not preserved"),
                "consecution failure should say so: {message}"
            );
            // Pre- and post-iteration values of the carried slot.
            let names: Vec<&str> = counterexample.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"s0"), "pre-state s0: {names:?}");
            assert!(names.contains(&"s0'"), "post-state s0': {names:?}");
        }
        other => panic!("an unpreserved invariant should fail, got {other:?}"),
    }
}

// An invariant that holds but is too weak to imply the postcondition: after
// the loop only `i >= 0 ∧ ¬(i > 0)` is known, so `result = sum` is
// unconstrained. The honest answer is a counterexample for the `ensures`
// (the agent's cue to strengthen the invariant), never a false `proved`.
const WEAK_INVARIANT: &str = "\
mod loops

pure fn weak_inv(n: i64) -> i64
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
fn weak_invariant_yields_postcondition_counterexample() {
    let (def, names) = lower_one(WEAK_INVARIANT);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed { obligation, .. } => {
            assert!(
                obligation.contains("result"),
                "the unprovable clause is the postcondition: {obligation}"
            );
        }
        other => panic!("a too-weak invariant must not prove the ensures, got {other:?}"),
    }
}

// A loop invariant is an obligation even when the function has no `ensures`
// (here the result type is unit, which `ensures` could not even mention).
const INVARIANT_ONLY: &str = "\
mod loops

pure fn spin(n: i64)
    requires (n >= 0)
{
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
    {
        i = (i - 1)
    }
}
";

#[test]
fn invariant_only_function_is_discharged() {
    let (def, names) = lower_one(INVARIANT_ONLY);
    assert!(
        marv_verify::has_loop_invariant(&def),
        "spin carries a loop invariant"
    );
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "an invariant-only function should discharge its loop, got {outcome:?}"
    );
}

// A loop body whose tail is an `if`/`else` (MARV-21 branch join): each branch
// yields the next-state tuple; consecution merges them componentwise.
const BRANCH_JOIN: &str = "\
mod loops

pure fn weighted(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
{
    var i: i64 = n
    var acc: i64 = 0
    while (i > 0)
        invariant (i >= 0)
        invariant (acc >= 0)
    {
        i = (i - 1)
        if (i > 2) {
            acc = (acc + 10)
        } else {
            acc = (acc + 1)
        }
    }
    acc
}
";

#[test]
fn proves_branch_join_loop() {
    let (def, names) = lower_one(BRANCH_JOIN);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "the branch-join loop should be proved, got {outcome:?}"
    );
}

// Nested loops: the inner loop's obligations discharge under the outer
// iteration's assumptions, and its exit state feeds the outer consecution.
const NESTED: &str = "\
mod loops

pure fn grid(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
{
    var total: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
        invariant (total >= 0)
    {
        var j: i64 = i
        while (j > 0)
            invariant (j >= 0)
            invariant (total >= 0)
        {
            total = (total + j)
            j = (j - 1)
        }
        i = (i - 1)
    }
    total
}
";

#[test]
fn proves_nested_loops() {
    let (def, names) = lower_one(NESTED);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "nested loops should be proved, got {outcome:?}"
    );
}

// Division inside a loop body keeps the whole function out-of-subset — the
// same honest boundary as straight-line division.
const DIV_IN_LOOP: &str = "\
mod loops

pure fn halve_down(n: i64) -> i64
    requires (n >= 0)
{
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
    {
        i = (i / 2)
    }
    i
}
";

#[test]
fn division_in_loop_is_unsupported() {
    let (def, names) = lower_one(DIV_IN_LOOP);
    let outcome = verify_def(&def, &names);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Unsupported { reason } => {
            assert!(
                reason.contains("division"),
                "reason should explain the boundary: {reason}"
            );
        }
        other => panic!("division in a loop should be unsupported, got {other:?}"),
    }
}
