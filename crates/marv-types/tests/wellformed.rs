//! M2 acceptance gate, positive half: well-typed programs check clean.
//!
//! Real `.mv` source covers what the M0 front end can express (arithmetic,
//! `if`, calls, field access through references); hand-written Core covers the
//! capability / error-set / exhaustiveness / linearity rules in their
//! *well-formed* shape, proving the checker accepts correct programs and not
//! merely that it rejects broken ones.

mod common;

use common::*;
use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash};
use marv_syntax::parse;
use marv_types::{check_def, check_module, Code, OpSig, World, WorldBuilder};

#[track_caller]
fn check_src_clean(src: &str) {
    let module = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"));
    let diags = check_module(&lowered);
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:#?}");
}

#[track_caller]
fn check_def_clean(world: &World, def: &Def) {
    let diags = check_def(world, def, Some("f"));
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:#?}");
}

// ============================ real source ================================

#[test]
fn arithmetic_function_checks_clean() {
    check_src_clean("mod m\n\npure fn add(a: i32, b: i32) -> i32 {\n    (a + b)\n}\n");
}

#[test]
fn if_expression_checks_clean() {
    check_src_clean(
        "mod m\n\npure fn max(a: i32, b: i32) -> i32 {\n    if (a < b) {\n        b\n    } else {\n        a\n    }\n}\n",
    );
}

#[test]
fn struct_field_access_through_reference_checks_clean() {
    // A `&Point` parameter (passing a reference down is allowed), reading its
    // fields, returning a value (not a reference).
    check_src_clean(
        "mod m\n\nstruct Point {\n    x: i32,\n    y: i32,\n}\n\n\
         pure fn sum_coords(p: &Point) -> i32 {\n    (p.x + p.y)\n}\n",
    );
}

#[test]
fn calls_between_local_functions_check_clean() {
    check_src_clean(
        "mod m\n\npure fn dbl(n: i32) -> i32 {\n    (n * 2)\n}\n\n\
         pure fn quad(n: i32) -> i32 {\n    dbl(dbl(n))\n}\n",
    );
}

#[test]
fn calling_an_unknown_import_is_permissive() {
    // `io.write(...)` resolves to an opaque global the world does not know; it
    // must not produce a spurious diagnostic.
    check_src_clean("mod m\n\nfn main(io: Io) {\n    io.write(io)\n}\n");
}

// ============================ capabilities ===============================

#[test]
fn perform_with_declared_capability_checks_clean() {
    let world = WorldBuilder::new()
        .cap(
            "Fs",
            vec![OpSig {
                params: vec![Type::Str],
                ret: Type::Unit,
                errors: vec![],
            }],
        )
        .build();
    // `fn read_it(fs: Fs) -{Fs}-> () { perform(fs, read, "p") }`
    let body = Core::Perform {
        cap: var_at(1, 0),
        op: OpId(0),
        args: vec![Atom::Lit(Literal::Str("p".into()))],
    };
    let def = fn_def(&[nominal("Fs")], Type::Unit, row(&["Fs"], &[]), body);
    check_def_clean(&world, &def);
}

// ============================ error sets =================================

#[test]
fn raise_within_declared_error_set_checks_clean() {
    let world = WorldBuilder::new().error("LoadError", vec![]).build();
    let body = Core::Raise {
        error: symbol_hash("LoadError"),
        args: vec![],
    };
    let def = fn_def(&[Type::Unit], Type::Unit, row(&[], &["LoadError"]), body);
    check_def_clean(&world, &def);
}

// ============================ exhaustiveness =============================

#[test]
fn exhaustive_match_checks_clean() {
    let world = WorldBuilder::new()
        .enum_decl(
            "Color",
            vec![("Red", vec![]), ("Green", vec![]), ("Blue", vec![])],
        )
        .build();
    let arm = |n: i64| Branch {
        binds: 0,
        body: Core::Atom(int(n)),
    };
    let body = Core::Match {
        scrutinee: var_at(1, 0),
        branches: vec![arm(0), arm(1), arm(2)],
    };
    let def = fn_def(
        &[nominal("Color")],
        Type::Int(IntTy::I32),
        row(&[], &[]),
        body,
    );
    check_def_clean(&world, &def);
}

// ============================ linearity ==================================

#[test]
fn linear_value_consumed_exactly_once_checks_clean() {
    let world = WorldBuilder::new()
        .struct_decl("File", vec![Type::Int(IntTy::I32)], true)
        .global(
            "m.consume",
            arrow(&[linear(nominal("File"))], Type::Unit, row(&[], &[])),
        )
        .build();
    // `fn close_it(f: File) { consume(f) }` — consumed once on the only path.
    let body = Core::App {
        func: global("m.consume"),
        arg: var_at(1, 0),
    };
    let def = fn_def(&[linear(nominal("File"))], Type::Unit, row(&[], &[]), body);
    check_def_clean(&world, &def);
}

#[test]
fn linear_value_consumed_once_on_every_branch_checks_clean() {
    let world = WorldBuilder::new()
        .struct_decl("File", vec![Type::Int(IntTy::I32)], true)
        .global(
            "m.consume",
            arrow(&[linear(nominal("File"))], Type::Unit, row(&[], &[])),
        )
        .build();
    // Consume `f` in *both* branches: exactly once on every path.
    let consume_f = Core::App {
        func: global("m.consume"),
        arg: var_at(2, 0),
    };
    let body = Core::Match {
        scrutinee: var_at(2, 1), // b: bool
        branches: vec![
            Branch {
                binds: 0,
                body: consume_f.clone(),
            },
            Branch {
                binds: 0,
                body: consume_f,
            },
        ],
    };
    let def = fn_def(
        &[linear(nominal("File")), Type::Bool],
        Type::Unit,
        row(&[], &[]),
        body,
    );
    check_def_clean(&world, &def);
}

// ============================ catalog ====================================

#[test]
fn error_code_strings_are_unique_and_stable() {
    use std::collections::BTreeSet;
    let codes = Code::catalog();
    let strings: BTreeSet<&str> = codes.iter().map(|c| c.as_str()).collect();
    assert_eq!(strings.len(), codes.len(), "duplicate error-code strings");
    // Every code is in the E0xxx family and has a non-empty summary.
    for c in codes {
        assert!(c.as_str().starts_with('E'), "{}", c.as_str());
        assert!(!c.summary().is_empty());
    }
}
