//! Round-trip and canonical-form tests for error handling (MARV-3): `error`
//! declarations, the `!T` / bare `!` error-union return type, the `?T` optional
//! type, and the postfix `?` propagation operator (`spec/02` §B). These forms
//! are not produced by the structural fuzzer in `roundtrip.rs`, so they are
//! covered here with hand-written canonical modules.

use marv_syntax::{format, format_module, parse};

/// Each string is already in canonical form: it must reprint to itself
/// (`format(src) == src`) and round-trip through the AST.
const CANONICAL: &[&str] = &[
    // An `error` declaration: variants inline like a one-line struct.
    "mod m\n\nerror LoadError { NotFound, BadFormat }\n",
    // `!T` return type and a bare-`!` (union over unit) return type.
    "mod m\n\nerror E { A }\n\nfn parse(b: i64) -> !i64 {\n    E.A\n}\n\nfn run(io: Io) -> ! {\n    io.go()\n}\n",
    // Postfix `?` in a `let` value and at a function tail.
    "mod m\n\nfn load(b: i64) -> !i64 {\n    let x = parse(b)?\n    next(x)?\n}\n",
    // `?T` optional return type.
    "mod m\n\nfn first(xs: []i64) -> ?i64 {\n    head(xs)\n}\n",
];

#[test]
fn canonical_error_modules_round_trip() {
    for (i, src) in CANONICAL.iter().enumerate() {
        assert_eq!(
            format(src),
            *src,
            "case {i}: source is not in canonical form"
        );
        // Idempotence.
        assert_eq!(
            format(&format(src)),
            format(src),
            "case {i}: not idempotent"
        );
        // AST round-trip: parse, reprint, reparse — the ASTs must match.
        let ast1 = parse(src).unwrap_or_else(|e| panic!("case {i}: parse failed: {e}"));
        let ast2 = parse(&format_module(&ast1)).expect("reparse");
        assert_eq!(ast1, ast2, "case {i}: AST did not round-trip");
    }
}

/// `!()` and `! ()` (explicit unit payload) normalize to the bare `!` form.
#[test]
fn explicit_unit_error_union_normalizes_to_bare() {
    let src = "mod m\n\nfn f(io: Io) -> !() {\n    io.go()\n}\n";
    assert_eq!(
        format(src),
        "mod m\n\nfn f(io: Io) -> ! {\n    io.go()\n}\n",
        "`!()` should canonicalize to bare `!`"
    );
}

/// A trailing comma in an `error` declaration is tolerated and dropped.
#[test]
fn error_decl_trailing_comma_normalized() {
    let src = "mod m\n\nerror E { A, B, }\n";
    assert_eq!(format(src), "mod m\n\nerror E { A, B }\n");
}
