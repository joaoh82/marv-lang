//! End-to-end interpreter coverage for the MARV-23 prefix unary operators: a
//! program using `-e` and `not e` parses, lowers, *checks*, and runs (the
//! interpreter is the semantics oracle, `spec/03`). `&e` is also exercised — a
//! second-class reference evaluates to its referent's value (`spec/01` §4).

use marv_core::lower_module;
use marv_interp::{Program, Value};
use marv_types::{check_module, Severity, World};

/// Parse + lower + assert the checker is clean, then wrap in a runnable program.
fn checked_program(src: &str) -> Program {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let errors: Vec<_> = check_module(&lowered)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "program must check clean: {errors:?}");
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new(&module_path, defs, world)
}

#[test]
fn runs_neg_and_not() {
    // `not (n == 0)` selects the `-n` branch for non-zero inputs and `0`
    // otherwise — exercising both unary `Prim`s end to end.
    let prog = checked_program(
        "mod demo\n\npure fn classify(n: i64) -> i64 {\n    if not (n == 0) {\n        -n\n    } else {\n        0\n    }\n}\n",
    );
    assert_eq!(
        prog.run("classify", &[], &["5".to_string()])
            .expect("run")
            .value,
        Value::Int(-5),
        "classify(5) = -5"
    );
    assert_eq!(
        prog.run("classify", &[], &["0".to_string()])
            .expect("run")
            .value,
        Value::Int(0),
        "classify(0) = 0"
    );
}

#[test]
fn runs_not_to_a_bool_result() {
    let prog = checked_program("mod demo\n\npure fn t() -> bool {\n    not false\n}\n");
    assert_eq!(
        prog.run("t", &[], &[]).expect("run").value,
        Value::Bool(true)
    );
}

#[test]
fn passes_a_reference_into_a_function() {
    // The canonical reference-passing style (`total(&sales)` in `report.mv`):
    // `give` builds a value and hands `take` a `&S`; reading through it yields the
    // referent's value, so `give()` is `9`.
    let prog = checked_program(
        "mod demo\n\nstruct S { v: i64 }\n\npure fn take(r: &S) -> i64 {\n    r.v\n}\n\npure fn give() -> i64 {\n    let s = S { v: 9 }\n    take(&s)\n}\n",
    );
    assert_eq!(
        prog.run("give", &[], &[]).expect("run").value,
        Value::Int(9)
    );
}
