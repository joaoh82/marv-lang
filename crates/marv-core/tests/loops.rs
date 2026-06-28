//! Lowering of the MARV-2 loop surface to the Core IR
//! (`spec/02-grammar-and-core-ir.md` §§C–D):
//!
//! - `while cond { invariant e }* body` → `Core::Loop { state, invariant, cond,
//!   body }`, with the loop-carried `var`s threaded as `state` and rebound from
//!   the loop's final-state tuple afterward;
//! - `for x in xs { body }` → an index-driven `Loop` (`spec/02` §D);
//! - alpha-equivalent loops lower to identical content hashes (the M1 gate);
//! - a loop body whose tail is an `if`/`match` threads the carried `var`s through
//!   the branch join — every branch yields the next-state tuple (MARV-21);
//! - a loop body ending in `return` lowers to early function exit (MARV-58).

use marv_core::ir::*;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
}

fn def<'a>(m: &'a LoweredModule, name: &str) -> &'a DefEntry {
    m.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def `{name}`"))
}

fn fn_body(m: &LoweredModule, name: &str) -> Core {
    let mut body = def(m, name).def.body.clone().expect("fn body");
    while let Core::Lam { body: inner, .. } = body {
        body = *inner;
    }
    body
}

/// Find the first `Core::Loop` anywhere in a term.
fn find_loop(c: &Core) -> Option<&Core> {
    fn walk<'a>(c: &'a Core, found: &mut Option<&'a Core>) {
        if found.is_some() {
            return;
        }
        if matches!(c, Core::Loop { .. }) {
            *found = Some(c);
            return;
        }
        match c {
            Core::Let { value, body } => {
                walk(value, found);
                walk(body, found);
            }
            Core::Loop { cond, body, .. } => {
                walk(cond, found);
                walk(body, found);
            }
            Core::Match { branches, .. } => branches.iter().for_each(|b| walk(&b.body, found)),
            Core::Lam { body, .. } => walk(body, found),
            _ => {}
        }
    }
    let mut found = None;
    walk(c, &mut found);
    found
}

const SUM_TO: &str = "\
mod demo

pure fn sum_to(n: i64) -> i64 {
    var sum: i64 = 0
    var i: i64 = n
    while (i > 0) {
        sum = (sum + i)
        i = (i - 1)
    }
    sum
}
";

#[test]
fn while_lowers_to_a_loop_with_two_carried_vars() {
    let m = lower(SUM_TO);
    let body = fn_body(&m, "sum_to");
    let lp = find_loop(&body).expect("a `while` lowers to a Core::Loop");
    match lp {
        Core::Loop {
            state,
            invariant,
            body,
            ..
        } => {
            // Two carried variables: `sum` and `i`.
            assert_eq!(state.len(), 2, "two `var`s are carried");
            assert!(invariant.is_none(), "no invariant clause here");
            // The body evaluates to the next-state tuple (its two carried values).
            let terminal = innermost(body);
            match terminal {
                Core::Ctor { tag, fields, .. } => {
                    assert_eq!(*tag, 0);
                    assert_eq!(fields.len(), 2, "body yields the two next values");
                }
                other => panic!("loop body should end in the carried-state tuple, got {other:?}"),
            }
        }
        other => panic!("expected a Loop, got {other:?}"),
    }
}

/// The innermost body of a right-nested `Let` spine.
fn innermost(c: &Core) -> &Core {
    let mut cur = c;
    while let Core::Let { body, .. } = cur {
        cur = body;
    }
    cur
}

#[test]
fn loop_invariant_lowers_to_a_pred() {
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0)\n        invariant (i >= 0)\n    {\n        i = (i - 1)\n    }\n    i\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "run");
    let lp = find_loop(&body).expect("loop");
    match lp {
        Core::Loop { invariant, .. } => {
            assert!(
                matches!(invariant.as_deref(), Some(Pred::Cmp(CmpOp::Ge, _, _))),
                "the `invariant (i >= 0)` lowers to a `Ge` comparison Pred"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn alpha_equivalent_loops_hash_identically() {
    // The same loop with every binder renamed must lower to the *same* content
    // hash — names are not part of identity (the M1 gate, `spec/02` §F).
    let a = lower(SUM_TO);
    let renamed = "\
mod demo

pure fn sum_to(total: i64) -> i64 {
    var acc: i64 = 0
    var k: i64 = total
    while (k > 0) {
        acc = (acc + k)
        k = (k - 1)
    }
    acc
}
";
    let b = lower(renamed);
    assert_eq!(
        def(&a, "sum_to").hash,
        def(&b, "sum_to").hash,
        "alpha-equivalent loops must have identical content hashes"
    );
}

#[test]
fn for_desugars_to_a_loop() {
    // `for x in xs` desugars to an index-driven `Loop` (`spec/02` §D). Execution
    // awaits slice/`len` support (MARV-7); lowering must still produce a Loop.
    let src = "mod demo\n\npure fn total(xs: []i64) -> i64 {\n    var sum: i64 = 0\n    for x in xs {\n        sum = (sum + x)\n    }\n    sum\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "total");
    assert!(
        find_loop(&body).is_some(),
        "a `for` loop desugars to a Core::Loop"
    );
}

#[test]
fn loop_body_with_an_if_tail_threads_carried_vars_through_the_join() {
    // MARV-21: a loop body whose tail is an `if`/`else` lowers — each branch
    // produces the next-state tuple, so the loop body's terminal is a `Match`
    // whose every branch ends in the carried-state `Ctor`.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0) {\n        if (i > 5) {\n            i = (i - 1)\n        } else {\n            i = (i - 2)\n        }\n    }\n    i\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "run");
    let lp = find_loop(&body).expect("the branch-tail `while` lowers to a Core::Loop");
    let Core::Loop {
        state, body: lbody, ..
    } = lp
    else {
        unreachable!()
    };
    // One carried variable: `i`.
    assert_eq!(state.len(), 1, "one `var` is carried");
    // The body's terminal (under any `let` spine) is the branch-join `Match`.
    match innermost(lbody) {
        Core::Match { branches, .. } => {
            assert_eq!(branches.len(), 2, "an `if`/`else` is a two-arm bool match");
            for br in branches {
                match innermost(&br.body) {
                    Core::Ctor { tag, fields, .. } => {
                        assert_eq!(*tag, 0);
                        assert_eq!(fields.len(), 1, "every branch yields the carried tuple");
                    }
                    other => panic!("branch should end in the carried-state tuple, got {other:?}"),
                }
            }
        }
        other => panic!("loop body terminal should be the branch-join Match, got {other:?}"),
    }
}

#[test]
fn loop_body_with_a_match_tail_lowers() {
    // A loop body whose tail is a `match` over an enum threads the carried `acc`
    // through every arm (MARV-21): each arm yields the next-state tuple.
    let src = "mod demo\n\nenum Bit {\n    Lo,\n    Hi,\n}\n\npure fn pick(i: i64) -> Bit {\n    if (i > 0) {\n        Bit.Hi\n    } else {\n        Bit.Lo\n    }\n}\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    var acc: i64 = 0\n    while (i > 0) {\n        i = (i - 1)\n        let b: Bit = pick(i)\n        match b {\n            Bit.Lo => { acc = (acc + 1) }\n            Bit.Hi => { acc = (acc + 2) }\n        }\n    }\n    acc\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "run");
    let lp = find_loop(&body).expect("the match-tail `while` lowers to a Core::Loop");
    let Core::Loop {
        state, body: lbody, ..
    } = lp
    else {
        unreachable!()
    };
    // Two carried variables: `i` and `acc`.
    assert_eq!(state.len(), 2, "two `var`s are carried");
    match innermost(lbody) {
        Core::Match { branches, .. } => {
            assert_eq!(branches.len(), 2, "the enum has two variants");
            for br in branches {
                match innermost(&br.body) {
                    Core::Ctor { fields, .. } => {
                        assert_eq!(fields.len(), 2, "every arm yields the two carried values");
                    }
                    other => panic!("arm should end in the carried-state tuple, got {other:?}"),
                }
            }
        }
        other => panic!("loop body terminal should be the branch-join Match, got {other:?}"),
    }
}

#[test]
fn outer_carried_var_shadowed_in_one_branch_is_still_carried() {
    // MARV-21 regression: `x` is reassigned only in the `then` branch; the `else`
    // branch declares a body-local `let x` that shadows it. The carried set must
    // still include `x` (the shadow is scoped to the else branch), so the loop
    // threads two `var`s — `i` and `x`.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    var x: i64 = 0\n    while (i > 0) {\n        i = (i - 1)\n        if (i > 2) {\n            x = (x + 10)\n        } else {\n            let x: i64 = 99\n            i = ((i - x) + x)\n        }\n    }\n    x\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "run");
    let lp = find_loop(&body).expect("the shadowing branch-join `while` lowers to a Core::Loop");
    let Core::Loop { state, .. } = lp else {
        unreachable!()
    };
    assert_eq!(
        state.len(),
        2,
        "`i` and the outer `x` are both carried despite the else-branch `let x` shadow"
    );
}

#[test]
fn loop_body_ending_in_return_lowers_to_core_return() {
    // MARV-58: `return` inside a loop body is early function exit, not the loop's
    // next carried-state tuple.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0) {\n        i = (i - 1)\n        if (i == 2) {\n            return i\n        }\n    }\n    0\n}\n";
    let m = lower(src);
    let body = fn_body(&m, "run");
    let lp = find_loop(&body).expect("the early-return `while` lowers to a Core::Loop");
    let Core::Loop { body: lbody, .. } = lp else {
        unreachable!()
    };
    match innermost(lbody) {
        Core::Match { branches, .. } => {
            assert!(
                branches
                    .iter()
                    .any(|br| matches!(innermost(&br.body), Core::Return { .. })),
                "one loop-body branch should lower to Core::Return"
            );
        }
        other => panic!("loop body terminal should be a branch join, got {other:?}"),
    }
}
