//! Lowering of error handling to the Core IR (MARV-3, `spec/02` §§C–D): an
//! `error` declaration → a `DefKind::Error` def; referencing an error variant →
//! `Core::Raise`; the `!T` return type → `Result[T, error-union]`; and `?` as a
//! success-value pass-through (errors propagate as an effect by unwinding).

use marv_core::ir::*;
use marv_core::{lower_module, symbol_hash, DefEntry, LoweredModule};
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

const SRC: &str = "\
mod m

error ParseError { Empty, Overflow }

fn digit(b: i64) -> !i64 {
    if (b < 0) {
        ParseError.Empty
    } else {
        b
    }
}

fn sum(x: i64) -> !i64 {
    let a = digit(x)?
    a
}
";

#[test]
fn error_decl_lowers_to_error_def_with_variants() {
    let m = lower(SRC);
    let e = def(&m, "ParseError");
    assert_eq!(e.def.kind, DefKind::Error);
    let variants = e.enum_variants.as_ref().expect("error variant metadata");
    assert_eq!(
        variants.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        vec!["Empty", "Overflow"]
    );
    // Variants are nullary: each payload tuple is empty.
    assert!(variants.iter().all(|v| v.fields.is_empty()));
}

#[test]
fn referencing_an_error_variant_lowers_to_raise() {
    let m = lower(SRC);
    let digit = def(&m, "digit");
    assert!(
        contains_raise(
            digit.def.body.as_ref().unwrap(),
            symbol_hash("m.ParseError")
        ),
        "`ParseError.Empty` should lower to a `Raise` of `m.ParseError`"
    );
}

#[test]
fn error_union_return_lowers_to_result_nominal() {
    let m = lower(SRC);
    let digit = def(&m, "digit");
    // The arrow's return type is `Result[i64, @error-union]`.
    let ret = arrow_ret(&digit.def.ty);
    match ret {
        Type::Nominal { def, args } => {
            assert_eq!(*def, symbol_hash("Result"));
            assert_eq!(args.len(), 2, "Result[success, error-union]");
            assert_eq!(args[0], Type::Int(IntTy::I64), "success type is i64");
            assert_eq!(
                args[1],
                Type::Nominal {
                    def: symbol_hash("@error-union"),
                    args: vec![]
                },
                "second arg is the inferred-error-set marker"
            );
        }
        other => panic!("expected Result nominal return, got {other:?}"),
    }
}

#[test]
fn try_is_a_value_passthrough() {
    // `let a = digit(x)?` lowers identically to `let a = digit(x)` — `?` adds no
    // node; the error propagates as an effect (a `Raise` unwinds at runtime).
    let with_try = lower(SRC);
    let without_try = lower(&SRC.replace("digit(x)?", "digit(x)"));
    assert_eq!(
        def(&with_try, "sum").def.body,
        def(&without_try, "sum").def.body,
        "`e?` and `e` lower to the same Core (success-value pass-through)"
    );
}

/// Walk a Core term looking for a `Raise` of `err`.
fn contains_raise(c: &Core, err: Hash) -> bool {
    match c {
        Core::Raise { error, .. } => *error == err,
        Core::Let { value, body } => contains_raise(value, err) || contains_raise(body, err),
        Core::Lam { body, .. } => contains_raise(body, err),
        Core::Match { branches, .. } => branches.iter().any(|b| contains_raise(&b.body, err)),
        Core::Loop { cond, body, .. } => contains_raise(cond, err) || contains_raise(body, err),
        _ => false,
    }
}

/// The innermost return type of a (curried) arrow.
fn arrow_ret(t: &Type) -> &Type {
    match t {
        Type::Arrow { ret, .. } if matches!(**ret, Type::Arrow { .. }) => arrow_ret(ret),
        Type::Arrow { ret, .. } => ret,
        other => other,
    }
}
