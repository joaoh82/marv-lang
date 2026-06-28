//! M6 acceptance gate (Tier 2): prove a correct `clamp`; return a concrete
//! counterexample for a buggy one; report `unsupported` (→ runtime fallback)
//! for a function outside the verified subset. Mirrors `spec/03` §4.3.
//!
//! These tests need a z3 binary on `PATH`. When none is available the SMT layer
//! reports `SolverUnavailable` (the same fallback path as `unsupported`), and
//! the test skips rather than fails — so a solver-less CI stays green while a
//! solver-equipped run exercises the real prover.

use marv_core::{ir::Def, lower_module, lower_modules};
use marv_types::World;
use marv_verify::{verify_def, VerifyOutcome};

/// Lower a source module and return its first *function* `Def`, the function's
/// parameter names, and the module's `World` (struct/enum declarations, for
/// havocking ADT-typed parameters).
fn lower_one(src: &str) -> (Def, Vec<String>, World) {
    let module = marv_syntax::parse(src).expect("parse");
    let f = module
        .items
        .iter()
        .find_map(|i| match i {
            marv_syntax::Item::Fn(f) => Some(f),
            _ => None,
        })
        .expect("expected a function");
    let names: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let fn_name = f.name.clone();
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let def = lowered
        .defs
        .iter()
        .find(|e| e.name == fn_name)
        .expect("lowered fn")
        .def
        .clone();
    (def, names, world)
}

/// Lower a source module together with the minimal std modules needed by the
/// List/string tests. This mirrors the CLI's std import resolution without
/// making `marv-verify` depend on `marv-cli`.
fn lower_one_with_std(src: &str) -> (Def, Vec<String>, World) {
    let module = marv_syntax::parse(src).expect("parse");
    let f = module
        .items
        .iter()
        .find_map(|i| match i {
            marv_syntax::Item::Fn(f) => Some(f),
            _ => None,
        })
        .expect("expected a function");
    let names: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
    let fn_name = f.name.clone();

    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf();
    let std_src = |file: &str| -> marv_syntax::Module {
        let path = repo.join("std").join(file);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        marv_syntax::parse(&src).expect("parse std module")
    };

    let modules = vec![
        module,
        std_src("capabilities.mv"),
        std_src("collections.mv"),
        std_src("str.mv"),
    ];
    let lowered = lower_modules(&modules).expect("lower with std");
    let world = World::from_modules(&lowered);
    let def = lowered[0]
        .defs
        .iter()
        .find(|e| e.name == fn_name)
        .expect("lowered fn")
        .def
        .clone();
    (def, names, world)
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
    let (def, names, world) = lower_one(CLAMP_CORRECT);
    let outcome = verify_def(&def, &names, &world);
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
    let (def, names, world) = lower_one(CLAMP_BUGGY);
    let outcome = verify_def(&def, &names, &world);
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

const LIST_PUSH_LEN: &str = "\
mod lists
import std.io (Alloc)
import std.collections (List, push)

fn append_one(alloc: Alloc, xs: List[i64], v: i64) -> List[i64]
    ensures len(result) == (len(xs) + 1)
{
    push(alloc, xs, v)
}
";

#[test]
fn proves_list_push_increments_len() {
    let (def, names, world) = lower_one_with_std(LIST_PUSH_LEN);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "push should prove len(result) == len(xs) + 1, got {outcome:?}"
    );
}

const LIST_GET_AFTER_PUSH: &str = "\
mod lists
import std.io (Alloc)
import std.collections (List, push, get)

fn pushed_value(alloc: Alloc, xs: List[i64], v: i64) -> i64
    ensures result == v
{
    let ys: List[i64] = push(alloc, xs, v)
    get(ys, len(xs))
}
";

#[test]
fn proves_get_after_push_returns_pushed_value() {
    let (def, names, world) = lower_one_with_std(LIST_GET_AFTER_PUSH);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "get(push(xs, v), len(xs)) should prove equal to v, got {outcome:?}"
    );
}

const LIST_PUSH_FALSE_LEN: &str = "\
mod lists
import std.io (Alloc)
import std.collections (List, push)

fn append_one(alloc: Alloc, xs: List[i64], v: i64) -> List[i64]
    ensures len(result) == len(xs)
{
    push(alloc, xs, v)
}
";

#[test]
fn counterexample_for_false_list_length_claim() {
    let (def, names, world) = lower_one_with_std(LIST_PUSH_FALSE_LEN);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed {
            obligation,
            counterexample,
            ..
        } => {
            let val = |k: &str| -> i64 {
                counterexample
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse::<i64>().ok())
                    .unwrap_or_else(|| panic!("missing/non-int {k} in {counterexample:?}"))
            };
            assert_eq!(
                val("len(result)"),
                val("len(xs)") + 1,
                "counterexample should reflect the push length effect: {counterexample:?}"
            );
            assert!(
                obligation.contains("len(result)") && obligation.contains("len(xs)"),
                "obligation should mention the false length claim: {obligation}"
            );
        }
        other => panic!("false push length claim should fail, got {other:?}"),
    }
}

const STRING_LEN_INDEX: &str = "\
mod strings

pure fn first_preserves(s: str) -> char
    requires len(s) >= 1
    ensures result == s[0]
{
    s[0]
}
";

#[test]
fn proves_string_index_contract() {
    let (def, names, world) = lower_one(STRING_LEN_INDEX);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "string indexing in body and contract should prove, got {outcome:?}"
    );
}

const STRING_LEN_BODY: &str = "\
mod strings

pure fn count(s: str) -> usize
    ensures result == len(s)
{
    len(s)
}
";

#[test]
fn proves_string_len_contract() {
    let (def, names, world) = lower_one(STRING_LEN_BODY);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "string len in body and contract should prove, got {outcome:?}"
    );
}

const STRING_SLICE_UNSUPPORTED: &str = "\
mod strings

pure fn head_slice(s: str) -> str
    requires len(s) >= 1
    ensures len(result) == 1
{
    s[0..1]
}
";

#[test]
fn unsupported_string_slice_reports_honest_fallback() {
    let (def, names, world) = lower_one(STRING_SLICE_UNSUPPORTED);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Unsupported { reason } => {
            assert!(
                reason.contains("string operations") || reason.contains("slice"),
                "unsupported reason should name the out-of-subset op, got {reason}"
            );
        }
        other => panic!("string slicing should stay unsupported, got {other:?}"),
    }
}

// ---- truncating division/remainder (MARV-11) ------------------------------
//
// SMT `div`/`mod` are Euclidean while marv truncates toward zero; the encoding
// corrects for that, and these tests pin the *direction* of the difference:
// claims true under Euclidean-but-false-under-truncating semantics must be
// refuted, and vice versa.

// `result <= x` for `x / 2` holds under floor/Euclidean division (the wrong
// semantics) but is violated by truncation at any negative x: trunc(-1 / 2) is
// 0, and 0 ≤ -1 fails. A sound encoding must produce that counterexample —
// `unsat` here would mean we encoded Euclidean division.
const TRUNC_REFUTES: &str = "\
mod math

pure fn half(x: i64) -> i64
    ensures result <= x
{
    x / 2
}
";

#[test]
fn truncating_division_refutes_floor_only_claim() {
    let (def, names, world) = lower_one(TRUNC_REFUTES);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed { counterexample, .. } => {
            let val = |k: &str| -> i64 {
                counterexample
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse::<i64>().ok())
                    .unwrap_or_else(|| panic!("missing/non-int {k} in {counterexample:?}"))
            };
            assert!(
                val("x") < 0 && val("result") > val("x"),
                "the violation comes from truncation toward zero at negative x: \
                 {counterexample:?}"
            );
        }
        other => panic!("`result <= x` for x / 2 must be refuted (x = -1), got {other:?}"),
    }
}

// The exact truncating quotient/remainder values at a negative dividend:
// -7 / 2 == -3 (Euclidean div would give -4) and -7 % 2 == -1 (Euclidean mod
// would give 1).
const TRUNC_EXACT: &str = "\
mod math

pure fn neg_div(x: i64, y: i64) -> i64
    requires x == -7 and y == 2
    ensures result == -3
{
    x / y
}
";

#[test]
fn proves_exact_truncating_quotient() {
    let (def, names, world) = lower_one(TRUNC_EXACT);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "-7 / 2 must prove as the truncating -3, got {outcome:?}"
    );
}

const TRUNC_REM: &str = "\
mod math

pure fn neg_rem(x: i64, y: i64) -> i64
    requires x == -7 and y == 2
    ensures result == -1
{
    x % y
}
";

#[test]
fn proves_exact_truncating_remainder() {
    let (def, names, world) = lower_one(TRUNC_REM);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "-7 % 2 must prove as the truncating -1, got {outcome:?}"
    );
}

// The defining identity `x == y * (x / y) + (x % y)` over all non-zero
// divisors. Division and remainder also appear in the *contract* here
// (MARV-11 contract expressions).
const DIV_IDENTITY: &str = "\
mod math

pure fn tdiv(x: i64, y: i64) -> i64
    requires y != 0
    ensures x == (y * result) + (x % y)
{
    x / y
}
";

#[test]
fn proves_division_identity() {
    let (def, names, world) = lower_one(DIV_IDENTITY);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "the truncating division identity should prove, got {outcome:?}"
    );
}

// ---- fixed-width wraparound (MARV-38) -------------------------------------
//
// Tier-2 arithmetic computes the runtime's 64-bit *wrapping* values, not
// mathematical integers. A contract that holds only for unbounded integers
// must now be refuted with the overflowing counterexample, while a contract
// that holds for every i64 (because it wraps identically, or stays in range)
// must still prove.

// `result > x` for `x + 1` is true over the mathematical integers but FALSE in
// i64: at `x = i64::MAX` the add wraps to `i64::MIN`. A faithful encoding must
// produce that counterexample — a `proved` here would be the old soundness bug.
const INC_WRAPS: &str = "\
mod wrap

pure fn inc(x: i64) -> i64
    ensures result > x
{
    x + 1
}
";

#[test]
fn increment_overflow_is_refuted_at_max() {
    let (def, names, world) = lower_one(INC_WRAPS);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    match outcome {
        VerifyOutcome::Failed { counterexample, .. } => {
            let val = |k: &str| -> i64 {
                counterexample
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse::<i64>().ok())
                    .unwrap_or_else(|| panic!("missing/non-int {k} in {counterexample:?}"))
            };
            // The only x where `x + 1` overflows is i64::MAX, wrapping to MIN.
            assert_eq!(
                val("x"),
                i64::MAX,
                "the overflow witness is x = i64::MAX: {counterexample:?}"
            );
            assert_eq!(
                val("result"),
                i64::MIN,
                "x + 1 wraps to i64::MIN: {counterexample:?}"
            );
        }
        other => panic!("`result > x` for x + 1 must be refuted (x = i64::MAX), got {other:?}"),
    }
}

// The same body proves once the precondition rules out the overflowing input:
// `x < i64::MAX` ⇒ `x + 1` does not wrap ⇒ `result > x`. Guards against an
// over-eager wrap that would refute even non-overflowing arithmetic.
const INC_BOUNDED: &str = "\
mod wrap

pure fn inc(x: i64) -> i64
    requires x < 9223372036854775807
    ensures result > x
{
    x + 1
}
";

#[test]
fn bounded_increment_still_proves() {
    let (def, names, world) = lower_one(INC_BOUNDED);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "x + 1 with x < i64::MAX cannot overflow, so result > x holds: {outcome:?}"
    );
}

// Multiplication wraps too: `result >= x` for `x * 2` with `x >= 0` fails once
// `2 * x` exceeds i64::MAX (e.g. x = 2^62, where `x * 2 = i64::MIN`).
const DOUBLE_WRAPS: &str = "\
mod wrap

pure fn double(x: i64) -> i64
    requires x >= 0
    ensures result >= x
{
    x * 2
}
";

#[test]
fn multiplication_overflow_is_refuted() {
    let (def, names, world) = lower_one(DOUBLE_WRAPS);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert!(
        matches!(outcome, VerifyOutcome::Failed { .. }),
        "`result >= x` for x * 2 must be refuted by overflow, got {outcome:?}"
    );
}

// `result == old(n) + 1` for body `n + 1` proves *because* both sides wrap
// identically — an equality between the same wrapping expression holds for
// every i64, including i64::MAX. (Modeling wrap must not break this.)
const WRAP_EQ: &str = "\
mod wrap

pure fn bump(n: i64) -> i64
    ensures result == old(n) + 1
{
    n + 1
}
";

#[test]
fn equal_wrapping_expressions_still_prove() {
    let (def, names, world) = lower_one(WRAP_EQ);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "result == old(n) + 1 for n + 1 wraps identically on both sides: {outcome:?}"
    );
}

// ---- loop invariants (MARV-22) ------------------------------------------
//
// Hoare-style discharge of `while` invariants: initiation (holds on entry),
// consecution (preserved by the body), use (invariant ∧ ¬cond is all that is
// known after the loop). Counterexamples label carried slots positionally
// (`s0`, `s1`, …; primed for post-iteration values) — Core erases names.

// `sum_to` accumulates `sum + i` under the invariant `sum >= 0` — true over the
// mathematical integers, but the running sum overflows i64 for large `n`. Under
// fixed-width wrapping (MARV-38) consecution now refutes it: a carried `sum`
// near i64::MAX plus a positive `i` wraps to a *negative* `sum'`, so `sum >= 0`
// is not preserved. A `proved` here would resurrect the soundness gap MARV-38
// closes — `examples/loops.mv` keeps this as a documented caught overflow, and
// `count_down_sum` there is the bounded form that proves.
const SUM_TO_OVERFLOWS: &str = "\
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
fn sum_to_overflow_is_refuted() {
    let (def, names, world) = lower_one(SUM_TO_OVERFLOWS);
    let outcome = verify_def(&def, &names, &world);
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
                "the running sum overflows, breaking consecution: {message}"
            );
            // A post-iteration carried slot (`s{j}'`) wraps to a negative value
            // — the overflow that refutes `sum >= 0`.
            let post_wraps_negative = counterexample.iter().any(|(name, v)| {
                name.ends_with('\'') && v.parse::<i64>().map(|x| x < 0).unwrap_or(false)
            });
            assert!(
                post_wraps_negative,
                "a carried slot wraps negative on overflow: {counterexample:?}"
            );
        }
        other => panic!("sum_to's unbounded `sum + i` must be refuted by overflow, got {other:?}"),
    }
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
    let (def, names, world) = lower_one(BAD_ENTRY);
    let outcome = verify_def(&def, &names, &world);
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
    let (def, names, world) = lower_one(BAD_PRESERVE);
    let outcome = verify_def(&def, &names, &world);
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
    let (def, names, world) = lower_one(WEAK_INVARIANT);
    let outcome = verify_def(&def, &names, &world);
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
    let (def, names, world) = lower_one(INVARIANT_ONLY);
    assert!(
        marv_verify::has_loop_invariant(&def),
        "spin carries a loop invariant"
    );
    let outcome = verify_def(&def, &names, &world);
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
// yields the next-state tuple; consecution merges them componentwise. The
// `acc <= 10 * (n - i)` bound (and `n <= 1e8`) keeps the accumulator inside
// i64 so the `acc + 10` step cannot wrap — without it, faithful wrapping
// (MARV-38) refutes `acc >= 0`.
const BRANCH_JOIN: &str = "\
mod loops

pure fn weighted(n: i64) -> i64
    requires (n >= 0 and n <= 100000000)
    ensures (result >= 0)
{
    var i: i64 = n
    var acc: i64 = 0
    while (i > 0)
        invariant (i >= 0)
        invariant (i <= n)
        invariant (acc >= 0)
        invariant (acc <= (10 * (n - i)))
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
    let (def, names, world) = lower_one(BRANCH_JOIN);
    let outcome = verify_def(&def, &names, &world);
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
// iteration's assumptions, and its exit state feeds the outer consecution. The
// outer accumulator is bounded by the non-overflowing `total <= n - i` (a real
// bound — `total + i <= n` would be *vacuous* under wrapping, since the sum can
// itself wrap), so `total + 1` never overflows and the loop proves (MARV-38).
const NESTED: &str = "\
mod loops

pure fn grid(n: i64) -> i64
    requires (n >= 0 and n <= 1000000)
    ensures (result >= 0)
{
    var total: i64 = 0
    var i: i64 = n
    while (i > 0)
        invariant (i >= 0)
        invariant (i <= n)
        invariant (total >= 0)
        invariant (total <= (n - i))
    {
        var j: i64 = i
        while (j > 0)
            invariant (j >= 0)
        {
            j = (j - 1)
        }
        total = (total + 1)
        i = (i - 1)
    }
    total
}
";

#[test]
fn proves_nested_loops() {
    let (def, names, world) = lower_one(NESTED);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "nested loops should be proved, got {outcome:?}"
    );
}

// Division inside a loop body discharges through the loop's invariant
// (MARV-11): truncating `i / 2` of a non-negative `i` stays non-negative.
const DIV_IN_LOOP: &str = "\
mod loops

pure fn halve_down(n: i64) -> i64
    requires (n >= 0)
    ensures (result >= 0)
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
fn proves_division_in_loop() {
    let (def, names, world) = lower_one(DIV_IN_LOOP);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "halving in a loop should preserve its invariant, got {outcome:?}"
    );
}

// ---- arrays, quantifiers, old(e), ADTs (MARV-11) ---------------------------

// A bounded `forall` over an array parameter in `requires` discharges an
// `ensures` about an element the body reads — the MARV-11 acceptance shape.
const FORALL_ARRAY: &str = "\
mod arrays

pure fn pick(a: [4]i64, lo: i64) -> i64
    requires (forall i in 0..len(a): (a[i] >= lo))
    ensures (result >= lo)
{
    a[2]
}
";

#[test]
fn proves_forall_over_array() {
    let (def, names, world) = lower_one(FORALL_ARRAY);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "a bounded forall over an array should discharge, got {outcome:?}"
    );
}

// The same shape with a too-short domain must NOT prove: `a[3]` is read but
// only `0..3` is constrained.
const FORALL_TOO_SHORT: &str = "\
mod arrays

pure fn pick(a: [4]i64, lo: i64) -> i64
    requires (forall i in 0..3: (a[i] >= lo))
    ensures (result >= lo)
{
    a[3]
}
";

#[test]
fn short_forall_domain_yields_counterexample() {
    let (def, names, world) = lower_one(FORALL_TOO_SHORT);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert!(
        matches!(outcome, VerifyOutcome::Failed { .. }),
        "an unconstrained element must refute the ensures, got {outcome:?}"
    );
}

// A sortedness-style nested-index `forall` (binder arithmetic `a[i + 1]`),
// plus `exists` in the conclusion over a slice parameter.
const EXISTS_WITNESS: &str = "\
mod arrays

pure fn first(a: []i64) -> i64
    requires (len(a) >= 1)
    requires (forall i in 0..(len(a) - 1): (a[i] <= a[i + 1]))
    ensures (exists i in 0..len(a): (a[i] == result))
{
    a[0]
}
";

#[test]
fn proves_exists_witness_over_slice() {
    let (def, names, world) = lower_one(EXISTS_WITNESS);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "the read element is its own exists-witness, got {outcome:?}"
    );
}

// An array literal, element store, `len`, and indexing all encode; the
// final read proves through the store chain.
const ARRAY_OPS: &str = "\
mod arrays

pure fn build(x: i64) -> i64
    requires (x >= 1)
    ensures (result >= 1)
{
    let a: [3]i64 = [1, 2, 3]
    a[2]
}
";

#[test]
fn proves_array_literal_read() {
    let (def, names, world) = lower_one(ARRAY_OPS);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "reading back an array literal element should prove, got {outcome:?}"
    );
}

// `old(e)` in `ensures`: parameters are immutable values, so `old(n)` is `n` —
// and the contract discharges like the plain spelling.
const OLD_PRESTATE: &str = "\
mod contracts

pure fn bump(n: i64) -> i64
    ensures result == old(n) + 1
{
    n + 1
}
";

#[test]
fn proves_old_in_ensures() {
    let (def, names, world) = lower_one(OLD_PRESTATE);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "`old(n)` is the parameter's (immutable) value, got {outcome:?}"
    );
}

// A struct parameter havocs from its declaration; field projections feed the
// contract through scalar arithmetic. The upper bounds keep `p.x + p.y` inside
// i64 — without them, faithful wrapping (MARV-38) refutes `result >= 0` at
// `p.x = p.y = i64::MAX`, where the sum wraps to -2.
const STRUCT_PARAM: &str = "\
mod adts

struct Point { x: i64, y: i64 }

pure fn norm1(p: Point) -> i64
    requires (p.x >= 0 and p.y >= 0 and p.x <= 1000000000 and p.y <= 1000000000)
    ensures (result >= 0)
{
    p.x + p.y
}
";

#[test]
fn proves_struct_param_contract() {
    let (def, names, world) = lower_one(STRUCT_PARAM);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "a struct parameter should havoc and prove, got {outcome:?}"
    );
}

// An enum parameter havocs as a tag in `[0, variants)` plus per-variant
// fields; `match` joins the branches. The `None` arm's 0 keeps the result
// non-negative only because the `Some` payload is constrained.
const ENUM_MATCH: &str = "\
mod adts

enum Opt {
    None,
    Some(i64),
}

pure fn get_or_zero(o: Opt) -> i64
    ensures (result >= 0)
{
    match o {
        Opt.None => 0,
        Opt.Some(v) => {
            if v >= 0 {
                v
            } else {
                0
            }
        },
    }
}
";

#[test]
fn proves_enum_match_contract() {
    let (def, names, world) = lower_one(ENUM_MATCH);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "an enum match should branch-join and prove, got {outcome:?}"
    );
}

// The unguarded version is refutable: a negative payload flows straight out.
const ENUM_MATCH_BUGGY: &str = "\
mod adts

enum Opt {
    None,
    Some(i64),
}

pure fn get_or_zero(o: Opt) -> i64
    ensures (result >= 0)
{
    match o {
        Opt.None => 0,
        Opt.Some(v) => v,
    }
}
";

#[test]
fn enum_match_counterexample() {
    let (def, names, world) = lower_one(ENUM_MATCH_BUGGY);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert!(
        matches!(outcome, VerifyOutcome::Failed { .. }),
        "a negative Some payload refutes the ensures, got {outcome:?}"
    );
}

// A bounded forall inside a loop *invariant*: every element written so far is
// the constant 7. Quantifiers in invariants use de Bruijn indices and bind
// index 0 in their body.
const QUANT_INVARIANT: &str = "\
mod loops

pure fn fill(a: []i64) -> i64
    requires (len(a) >= 0)
    ensures (result >= 0)
{
    var i: i64 = 0
    var out: []i64 = a
    while (i < len(out))
        invariant (i >= 0)
        invariant (forall k in 0..i: (out[k] == 7))
    {
        out[i] = 7
        i = (i + 1)
    }
    i
}
";

#[test]
fn proves_quantified_loop_invariant() {
    let (def, names, world) = lower_one(QUANT_INVARIANT);
    let outcome = verify_def(&def, &names, &world);
    if skip_if_no_solver(&outcome) {
        return;
    }
    assert_eq!(
        outcome,
        VerifyOutcome::Proved,
        "the fill loop's quantified invariant should prove, got {outcome:?}"
    );
}
