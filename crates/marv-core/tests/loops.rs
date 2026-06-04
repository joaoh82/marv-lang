//! Lowering of the MARV-2 loop surface to the Core IR
//! (`spec/02-grammar-and-core-ir.md` §§C–D):
//!
//! - `while cond { invariant e }* body` → `Core::Loop { state, invariant, cond,
//!   body }`, with the loop-carried `var`s threaded as `state` and rebound from
//!   the loop's final-state tuple afterward;
//! - `for x in xs { body }` → an index-driven `Loop` (`spec/02` §D);
//! - alpha-equivalent loops lower to identical content hashes (the M1 gate);
//! - a loop body ending in `if`/`match`/`return` is rejected (deferred join
//!   lowering).

use marv_core::ir::*;
use marv_core::lower::LowerError;
use marv_core::{lower_module, DefEntry, LoweredModule};
use marv_syntax::parse;

fn lower(src: &str) -> LoweredModule {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"))
}

fn try_lower(src: &str) -> Result<LoweredModule, LowerError> {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m)
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
fn loop_body_with_a_branch_tail_is_rejected() {
    // Threading carried `var`s through a branch join is not lowered yet (MARV-2
    // handles straight-line bodies); an `if` as the body's tail is an error.
    let src = "mod demo\n\npure fn run(n: i64) -> i64 {\n    var i: i64 = n\n    while (i > 0) {\n        if (i > 5) {\n            i = (i - 1)\n        } else {\n            i = (i - 2)\n        }\n    }\n    i\n}\n";
    assert_eq!(try_lower(src), Err(LowerError::LoopBodyControlFlow));
}
