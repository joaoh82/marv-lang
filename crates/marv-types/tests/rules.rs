//! M2 acceptance gate, negative half: a table of (program → expected
//! diagnostic-with-fix), one entry per rule in `spec/02` §E / `spec/01` §§3–7.
//!
//! Each case is the *smallest* program that trips exactly one rule, and asserts
//! the stable [`Code`] plus — for the five mechanically-derivable cases
//! `spec/03` §2 names — the presence and shape of the carried [`Fix`].
//!
//! Type, struct-field-reference, and returned-reference rules are reached from
//! real `.mv` source (parsed + lowered through `marv-core`). The capability,
//! error-set, exhaustiveness, and linearity rules are driven over hand-written
//! Core, because the M0 front end emits no `perform`/`raise`/enum/`linear`
//! surface forms yet (see `marv_types::check` scope notes).

mod common;

use common::*;
use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash};
use marv_syntax::parse;
use marv_types::{check_def, check_module, Code, Diagnostic, World, WorldBuilder};

// ============================ harness ====================================

/// Parse + lower a single-definition module and check it, returning its
/// diagnostics.
fn check_src(src: &str) -> Vec<Diagnostic> {
    let module = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower failed: {e}\n{src}"));
    check_module(&lowered)
}

/// Assert exactly one diagnostic, with the given code, and return it.
#[track_caller]
fn one(diags: Vec<Diagnostic>, code: Code) -> Diagnostic {
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic ({}), got: {:#?}",
        code.as_str(),
        diags
    );
    let d = diags.into_iter().next().unwrap();
    assert_eq!(d.code, code, "wrong code; full diagnostic: {d:#?}");
    d
}

/// Assert the diagnostic carries a best-first fix whose title contains `needle`.
#[track_caller]
fn has_fix_titled(d: &Diagnostic, needle: &str) {
    let fix = d
        .fixes
        .first()
        .unwrap_or_else(|| panic!("expected a fix on {}: {d:#?}", d.code.as_str()));
    assert!(
        fix.title.contains(needle),
        "fix title {:?} does not contain {needle:?}",
        fix.title
    );
}

// ============================ types ======================================

#[test]
fn e0101_return_type_mismatch() {
    // Body is `bool`, signature says `i32`.
    let d = one(
        check_src("mod m\n\npure fn f() -> i32 {\n    true\n}\n"),
        Code::TypeMismatch,
    );
    assert!(d.message.contains("i32"), "{}", d.message);
}

#[test]
fn e0103_bad_prim_operand() {
    // `<` needs numeric operands of the same type; `true` is `bool`.
    let d = one(
        check_src("mod m\n\npure fn f(a: i32) -> bool {\n    (a < true)\n}\n"),
        Code::BadPrimOperand,
    );
    assert!(d.message.contains('<'), "{}", d.message);
}

#[test]
fn e0102_calling_a_non_function() {
    // Apply an integer literal as if it were a function.
    let body = Core::App {
        func: int(5),
        arg: Atom::Lit(Literal::Unit),
    };
    let def = fn_def(&[Type::Unit], Type::Unit, row(&[], &[]), body);
    let d = one(
        check_def(&World::new(), &def, Some("f")),
        Code::NotAFunction,
    );
    assert!(d.fixes.is_empty(), "no mechanical fix expected for E0102");
}

// ============================ capabilities ===============================

#[test]
fn e0110_missing_capability_in_effect_row() {
    // Receives `fs: Fs` and performs an Fs op, but the declared row is empty.
    let world = WorldBuilder::new()
        .cap(
            "Fs",
            vec![marv_types::OpSig {
                params: vec![Type::Str],
                ret: Type::Unit,
                errors: vec![],
            }],
        )
        .build();
    let body = Core::Perform {
        cap: var_at(1, 0), // the `fs` parameter
        op: OpId(0),
        args: vec![Atom::Lit(Literal::Str("path".into()))],
    };
    let def = fn_def(&[nominal("Fs")], Type::Unit, row(&[], &[]), body);
    let d = one(check_def(&world, &def, Some("f")), Code::MissingCapability);
    has_fix_titled(&d, "add capability parameter `fs: Fs`");
}

#[test]
fn e0111_unauthorized_perform_no_capability() {
    // The "capability" is an integer literal — not a capability value in scope.
    let body = Core::Perform {
        cap: int(7),
        op: OpId(0),
        args: vec![],
    };
    let def = fn_def(&[Type::Unit], Type::Unit, row(&[], &[]), body);
    one(
        check_def(&World::new(), &def, Some("f")),
        Code::UnauthorizedPerform,
    );
}

#[test]
fn e0112_forged_capability() {
    // A capability *constructed* with `Ctor` (not received/narrowed), then
    // performed. The declared row includes `Fs`, isolating the forge error.
    let world = WorldBuilder::new()
        .cap(
            "Fs",
            vec![marv_types::OpSig {
                params: vec![Type::Str],
                ret: Type::Unit,
                errors: vec![],
            }],
        )
        .build();
    // depth 1: one synthetic unit param. `let c = Fs{} in perform(c, read, "p")`.
    let body = let_(
        Core::Ctor {
            ty: symbol_hash("Fs"),
            tag: 0,
            fields: vec![],
        },
        Core::Perform {
            cap: var_at(2, 1), // the let-bound, constructed capability
            op: OpId(0),
            args: vec![Atom::Lit(Literal::Str("p".into()))],
        },
    );
    let def = fn_def(&[Type::Unit], Type::Unit, row(&["Fs"], &[]), body);
    one(check_def(&world, &def, Some("f")), Code::ForgedCapability);
}

// ============================ error sets =================================

#[test]
fn e0120_missing_error_in_set() {
    // Raises `LoadError`, but the declared error set is empty.
    let world = WorldBuilder::new().error("LoadError", vec![]).build();
    let body = Core::Raise {
        error: symbol_hash("LoadError"),
        args: vec![],
    };
    let def = fn_def(&[Type::Unit], Type::Unit, row(&[], &[]), body);
    let d = one(check_def(&world, &def, Some("f")), Code::MissingError);
    has_fix_titled(&d, "add `LoadError` to the declared error set");
}

// ============================ exhaustiveness =============================

#[test]
fn e0130_non_exhaustive_match() {
    // `Color` has three variants; the match covers two.
    let world = WorldBuilder::new()
        .enum_decl(
            "Color",
            vec![("Red", vec![]), ("Green", vec![]), ("Blue", vec![])],
        )
        .build();
    let body = Core::Match {
        scrutinee: var_at(1, 0), // the `c: Color` parameter
        branches: vec![
            Branch {
                binds: 0,
                body: Core::Atom(int(0)),
            },
            Branch {
                binds: 0,
                body: Core::Atom(int(1)),
            },
        ],
    };
    let def = fn_def(
        &[nominal("Color")],
        Type::Int(IntTy::I32),
        row(&[], &[]),
        body,
    );
    let d = one(check_def(&world, &def, Some("f")), Code::NonExhaustiveMatch);
    has_fix_titled(&d, "missing match arm");
    assert!(
        d.message.contains("Blue"),
        "should name the missing variant: {}",
        d.message
    );
}

// ============================ linearity ==================================

/// A world with a `File` linear type and a `consume(File) -> ()` function.
fn linear_world() -> World {
    WorldBuilder::new()
        .struct_decl("File", vec![Type::Int(IntTy::I32)], true)
        .global(
            "m.consume",
            arrow(&[linear(nominal("File"))], Type::Unit, row(&[], &[])),
        )
        .build()
}

#[test]
fn e0140_linear_value_unused() {
    // A `linear` parameter that is never consumed.
    let def = fn_def(
        &[linear(nominal("File"))],
        Type::Unit,
        row(&[], &[]),
        unit(),
    );
    let d = one(
        check_def(&linear_world(), &def, Some("f")),
        Code::LinearUnused,
    );
    assert!(!d.fixes.is_empty(), "linear-unused should carry a fix");
}

#[test]
fn e0141_linear_value_duplicated() {
    // Consume the same `linear` parameter twice along one path.
    let world = linear_world();
    // depth 1: param `f`. `let _ = consume(f) in (let _ = consume(f) in ())`.
    let inner = let_(
        Core::App {
            func: global("m.consume"),
            arg: var_at(2, 0), // f, at depth 2
        },
        unit(),
    );
    let body = let_(
        Core::App {
            func: global("m.consume"),
            arg: var_at(1, 0), // f, at depth 1
        },
        inner,
    );
    let def = fn_def(&[linear(nominal("File"))], Type::Unit, row(&[], &[]), body);
    one(check_def(&world, &def, Some("f")), Code::LinearDuplicated);
}

#[test]
fn e0142_linear_value_not_on_all_paths() {
    // Consume `f` in one match branch but not the other.
    let world = linear_world();
    // params: f: File (level 0), b: bool (level 1). depth 2 in branches.
    let body = Core::Match {
        scrutinee: var_at(2, 1), // b
        branches: vec![
            Branch {
                binds: 0,
                body: Core::App {
                    func: global("m.consume"),
                    arg: var_at(2, 0), // f
                },
            },
            Branch {
                binds: 0,
                body: unit(),
            },
        ],
    };
    let def = fn_def(
        &[linear(nominal("File")), Type::Bool],
        Type::Unit,
        row(&[], &[]),
        body,
    );
    one(check_def(&world, &def, Some("f")), Code::LinearNotAllPaths);
}

// ============================ references =================================

#[test]
fn e0150_reference_returned() {
    // A second-class reference may not be returned.
    let d = one(
        check_src("mod m\n\nfn f(p: &i32) -> &i32 {\n    p\n}\n"),
        Code::EscapingReference,
    );
    has_fix_titled(&d, "by value");
}

#[test]
fn e0150_reference_in_struct_field() {
    // A struct may not store a reference in a field.
    let d = one(
        check_src("mod m\n\nstruct Bad {\n    r: &i32,\n}\n"),
        Code::EscapingReference,
    );
    assert!(d.message.contains("reference"), "{}", d.message);
}

#[test]
fn e0150_reference_in_ctor_field() {
    // Constructing an aggregate with a reference field (hand-written Core, since
    // the front end has no struct-literal expression yet).
    let world = WorldBuilder::new()
        .struct_decl("Wrap", vec![ref_to(false, Type::Int(IntTy::I32))], false)
        .build();
    // param `r: &i32` (level 0); construct `Wrap { r }`.
    let body = Core::Ctor {
        ty: symbol_hash("Wrap"),
        tag: 0,
        fields: vec![var_at(1, 0)],
    };
    // Return type is the nominal so the only error is the escaping field.
    let def = fn_def(
        &[ref_to(false, Type::Int(IntTy::I32))],
        nominal("Wrap"),
        row(&[], &[]),
        body,
    );
    one(check_def(&world, &def, Some("f")), Code::EscapingReference);
}

#[test]
fn array_coerces_to_slice_argument() {
    // A fixed-length array `[3]i64` is accepted where a `[]i64` slice is expected
    // (MARV-33): the two share the boxed layout, so the call type-checks clean.
    let src = "mod demo\n\npure fn take(xs: []i64) -> i64 {\n    (len(xs) as i64)\n}\n\npure fn run() -> i64 {\n    let a: [3]i64 = [1, 2, 3]\n    take(a)\n}\n";
    let diags = check_src(src);
    assert!(
        diags.is_empty(),
        "array→slice coercion should type-check clean, got: {diags:#?}"
    );
}

#[test]
fn slice_does_not_coerce_to_array_argument() {
    // The coercion is one-way: a runtime-length slice has no static length, so it
    // may not be passed where a fixed-length array is expected (MARV-33).
    let src = "mod demo\n\npure fn take(xs: [3]i64) -> i64 {\n    (len(xs) as i64)\n}\n\npure fn run(s: []i64) -> i64 {\n    take(s)\n}\n";
    one(check_src(src), Code::TypeMismatch);
}
