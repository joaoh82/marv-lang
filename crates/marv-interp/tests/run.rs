//! Interpreter behavior tests: pure evaluation through the real front end, and
//! the capability-injection sandbox (`spec/03` §4.5) over hand-built Core (which
//! the M0/M1 surface cannot yet express).

use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash};
use marv_interp::{Program, RunError, Value};
use marv_types::{OpSig, World, WorldBuilder};

/// Lower a source module and wrap it in a runnable program.
fn program_from_source(src: &str) -> Program {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    Program::new(&module_path, defs, world)
}

#[test]
fn interprets_recursive_factorial() {
    let prog = program_from_source(
        "mod demo\n\npure fn factorial(n: i64) -> i64 {\n    if n < 2 {\n        1\n    } else {\n        n * factorial(n - 1)\n    }\n}\n",
    );
    let out = prog.run("factorial", &[], &["6".to_string()]).expect("run");
    assert_eq!(out.value, Value::Int(720));
    assert!(
        out.effects.is_empty(),
        "a pure function performs no effects"
    );
}

#[test]
fn fills_value_parameters_from_args_in_order() {
    let prog =
        program_from_source("mod demo\n\npure fn sub(a: i64, b: i64) -> i64 {\n    a - b\n}\n");
    let out = prog
        .run("sub", &[], &["10".to_string(), "3".to_string()])
        .expect("run");
    assert_eq!(out.value, Value::Int(7));
}

/// A `touch(fs: Fs)` whose body performs `Fs` op 0. Hand-built because the
/// surface has no `perform` form yet.
fn touch_program() -> Program {
    let fs = symbol_hash("Fs");
    let fs_ty = Type::Nominal {
        def: fs,
        args: Vec::new(),
    };
    // touch : Fs -{Fs}-> ()   ;   body = \fs. perform fs.op0("/etc/passwd")
    let row = EffectRow {
        caps: vec![fs],
        errors: Vec::new(),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(fs_ty.clone()),
            ret: Box::new(Type::Unit),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: fs_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: vec![Atom::Lit(Literal::Str("/etc/passwd".to_string()))],
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Fs",
            vec![OpSig {
                params: vec![Type::Str],
                ret: Type::Unit,
                errors: Vec::new(),
            }],
        )
        .build();
    Program::new("sandbox", vec![("touch".to_string(), def)], world)
}

#[test]
fn granted_capability_is_injected_and_its_effect_recorded() {
    let prog = touch_program();
    let out = prog
        .run("touch", &["Fs".to_string()], &[])
        .expect("run with Fs granted");
    assert_eq!(out.value, Value::Unit);
    assert_eq!(out.effects.len(), 1, "the perform should be recorded");
    let eff = &out.effects[0];
    assert_eq!(eff.cap, "Fs");
    assert_eq!(eff.op, 0);
    assert_eq!(eff.args, vec![Value::Str("/etc/passwd".to_string())]);
}

#[test]
fn ungranted_capability_is_refused_at_the_boundary() {
    let prog = touch_program();
    // No grant: the capability value is never created, so the entry cannot run.
    let err = prog.run("touch", &[], &[]).unwrap_err();
    assert_eq!(err, RunError::UngrantedCapability("Fs".to_string()));
}

#[test]
fn unknown_entry_is_an_error() {
    let prog = touch_program();
    assert_eq!(
        prog.run("nope", &[], &[]).unwrap_err(),
        RunError::NoSuchEntry("nope".to_string())
    );
}
