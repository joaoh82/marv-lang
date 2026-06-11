//! MARV-3 error-set inference over real `.mv` source.
//!
//! Exercises the full source pipeline (`parse → lower → typecheck →
//! effects/errors`) for error handling: `error` declarations, the `!T` error
//! union, `?` propagation, and — the headline — *cross-call* error-set
//! inference, where a caller's inferred set includes the errors of the fallible
//! functions it calls (`spec/01` §6). The set is what `marv/errorSet` reports.

use marv_db::{analyze_text, DefInfo, FileAnalysis, SourceKind};

fn analyze(src: &str) -> FileAnalysis {
    analyze_text(SourceKind::Source, src)
}

fn def<'a>(a: &'a FileAnalysis, name: &str) -> &'a DefInfo {
    a.defs.iter().find(|d| d.name == name).unwrap_or_else(|| {
        panic!(
            "no def `{name}` in {:?}",
            a.defs.iter().map(|d| &d.name).collect::<Vec<_>>()
        )
    })
}

/// A function that raises an `error` variant directly has that error in its
/// inferred set, and declaring `!T` (not listing the error) is accepted — the
/// set is inferred, not declared, so no `MissingError` fires.
#[test]
fn direct_raise_infers_error_set() {
    let src = "mod m\n\
               \n\
               error ParseError { Empty, Overflow }\n\
               \n\
               fn digit(b: i64) -> !i64 {\n\
               \x20   if (b < 0) {\n\
               \x20       ParseError.Empty\n\
               \x20   } else {\n\
               \x20       b\n\
               \x20   }\n\
               }\n";
    let a = analyze(src);
    assert!(
        a.parse_error.is_none(),
        "should parse and lower: {:?}",
        a.parse_error
    );
    assert!(
        a.diagnostics.is_empty(),
        "no diagnostics expected, got {:?}",
        a.diagnostics
    );
    assert_eq!(
        def(&a, "digit").error_set,
        vec!["ParseError".to_string()],
        "digit raises ParseError, so its inferred error set is {{ParseError}}"
    );
}

/// The headline: a function that calls fallible functions with `?` inherits
/// their inferred error sets — full cross-call propagation, computed to a
/// fixpoint over the in-module call graph.
#[test]
fn try_propagates_callee_error_set() {
    let src = "mod m\n\
               \n\
               error ParseError { Empty, Overflow }\n\
               \n\
               fn digit(b: i64) -> !i64 {\n\
               \x20   if (b < 0) {\n\
               \x20       ParseError.Empty\n\
               \x20   } else {\n\
               \x20       b\n\
               \x20   }\n\
               }\n\
               \n\
               fn sum_two(x: i64, y: i64) -> !i64 {\n\
               \x20   let a = digit(x)?\n\
               \x20   let b = digit(y)?\n\
               \x20   (a + b)\n\
               }\n";
    let a = analyze(src);
    assert!(a.parse_error.is_none(), "should parse: {:?}", a.parse_error);
    assert!(
        a.diagnostics.is_empty(),
        "no diagnostics expected, got {:?}",
        a.diagnostics
    );
    // sum_two raises nothing itself; its set is inherited entirely from `digit`
    // through the two `?` calls.
    assert_eq!(
        def(&a, "sum_two").error_set,
        vec!["ParseError".to_string()],
        "sum_two's set is inherited from digit via `?` (cross-call inference)"
    );
}

/// A chain `c -> b -> a` propagates the leaf's error all the way up.
#[test]
fn error_set_propagates_transitively() {
    let src = "mod m\n\
               \n\
               error IoError { Closed }\n\
               \n\
               fn a(n: i64) -> !i64 {\n\
               \x20   if (n < 0) {\n\
               \x20       IoError.Closed\n\
               \x20   } else {\n\
               \x20       n\n\
               \x20   }\n\
               }\n\
               \n\
               fn b(n: i64) -> !i64 {\n\
               \x20   a(n)?\n\
               }\n\
               \n\
               fn c(n: i64) -> !i64 {\n\
               \x20   b(n)?\n\
               }\n";
    let a = analyze(src);
    assert!(a.parse_error.is_none(), "should parse: {:?}", a.parse_error);
    assert!(a.diagnostics.is_empty(), "clean, got {:?}", a.diagnostics);
    assert_eq!(def(&a, "c").error_set, vec!["IoError".to_string()]);
}

/// A plain (non-`!`) function that raises an error it does not declare is still
/// flagged — `!`-ness is what opens the error set, not raising itself.
#[test]
fn raise_without_error_union_is_reported() {
    let src = "mod m\n\
               \n\
               error ParseError { Empty }\n\
               \n\
               fn bad(b: i64) -> i64 {\n\
               \x20   ParseError.Empty\n\
               }\n";
    let a = analyze(src);
    assert!(a.parse_error.is_none(), "should parse: {:?}", a.parse_error);
    assert!(
        a.diagnostics.iter().any(|d| d.code == "E0120"),
        "expected a MissingError-style diagnostic, got {:?}",
        a.diagnostics
    );
}

/// The salsa/protocol path lowers strictly single-file (MARV-18): a file that
/// constructs an enum imported from another module surfaces the explicit
/// unresolved-import lower error in `parse_error` — never a silently wrong
/// analysis. (The CLI path resolves `import std.*` to source and lowers the
/// set together; snapshot-level module sets are MARV-14.)
#[test]
fn imported_enum_ctor_is_a_clear_error_single_file() {
    let a = analyze(
        "mod demo\nimport std.option (Option)\n\npure fn some(x: i64) -> Option[i64] {\n    \
         Option.Some(x)\n}\n",
    );
    let err = a
        .parse_error
        .expect("single-file analysis of an imported enum ctor must fail");
    assert!(
        err.contains("cannot resolve `Option`") && err.contains("std.option"),
        "error names the import and its module: {err}"
    );
    assert!(a.defs.is_empty(), "no defs are reported on a failed lower");
}
