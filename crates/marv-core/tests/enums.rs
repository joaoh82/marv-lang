//! Lowering of `enum` declarations and `match` expressions to the Core IR
//! (`spec/02-grammar-and-core-ir.md` §§C–D): constructor → `Ctor`, `match` →
//! tag-ordered `Match`, alpha-equivalence of the lowered enum, and the
//! cross-module prelude path.

use marv_core::ir::*;
use marv_core::{lower_module, lower_modules, DefEntry, LoweredModule};
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

const COLOR: &str = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn rank(c: Color) -> i64 {
    match c {
        Color.Red => 1,
        Color.Green => 2,
        Color.Blue => 3,
    }
}
";

#[test]
fn enum_def_is_recorded_with_variant_metadata() {
    let m = lower(COLOR);
    let e = def(&m, "Color");
    assert_eq!(e.def.kind, DefKind::Enum);
    let variants = e.enum_variants.as_ref().expect("enum variant metadata");
    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Red", "Green", "Blue"]);
    // All nullary.
    assert!(variants.iter().all(|v| v.fields.is_empty()));
}

#[test]
fn match_lowers_to_tag_ordered_branches() {
    let m = lower(COLOR);
    let body = def(&m, "rank").def.body.clone().expect("fn body");
    // `rank` is a one-parameter lambda whose body is the `Match`.
    let mat = match body {
        Core::Lam { body, .. } => *body,
        other => panic!("expected a lambda, got {other:?}"),
    };
    match mat {
        Core::Match { branches, .. } => {
            assert_eq!(branches.len(), 3, "all three variants covered");
            // Each nullary variant binds nothing; bodies are the tag literals
            // in tag order (Red=1, Green=2, Blue=3).
            let lits: Vec<i64> = branches
                .iter()
                .map(|b| match &b.body {
                    Core::Atom(Atom::Lit(Literal::Int(n))) => *n,
                    other => panic!("branch body not an int literal: {other:?}"),
                })
                .collect();
            assert_eq!(lits, [1, 2, 3]);
            assert!(branches.iter().all(|b| b.binds == 0));
        }
        other => panic!("expected a Match, got {other:?}"),
    }
}

#[test]
fn arm_order_does_not_change_tag_order() {
    // Arms written out of declaration order still lower to tag order, so the
    // hash is independent of how the match is written.
    let reordered = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn rank(c: Color) -> i64 {
    match c {
        Color.Blue => 3,
        Color.Red => 1,
        Color.Green => 2,
    }
}
";
    assert_eq!(
        def(&lower(COLOR), "rank").hash,
        def(&lower(reordered), "rank").hash
    );
}

#[test]
fn payload_variant_binds_and_projects() {
    // `Some(x) => x` binds the payload; the constructor lowers to a `Ctor`.
    let src = "\
mod demo

enum Box {
    Empty,
    Full(i64),
}

pure fn unwrap(b: Box, fallback: i64) -> i64 {
    match b {
        Box.Empty => fallback,
        Box.Full(x) => x,
    }
}

pure fn make() -> Box {
    Box.Full(7)
}
";
    let m = lower(src);
    // `make` constructs `Full(7)` → a Ctor with tag 1 and one field.
    let body = def(&m, "make").def.body.clone().expect("body");
    // Nullary fn: body is `\(). <ctor>` — peel the unit lambda.
    let inner = match body {
        Core::Lam { body, .. } => *body,
        other => panic!("expected lambda, got {other:?}"),
    };
    match inner {
        Core::Ctor { tag, fields, .. } => {
            assert_eq!(tag, 1, "Full is the second variant");
            assert_eq!(fields, vec![Atom::Lit(Literal::Int(7))]);
        }
        other => panic!("expected a Ctor, got {other:?}"),
    }

    // The `Full(x) => x` branch binds one field.
    let unwrap_body = def(&m, "unwrap").def.body.clone().expect("body");
    let mat = peel_to_match(unwrap_body);
    match mat {
        Core::Match { branches, .. } => {
            assert_eq!(branches.len(), 2);
            assert_eq!(branches[0].binds, 0, "Empty binds nothing");
            assert_eq!(branches[1].binds, 1, "Full binds its payload");
        }
        other => panic!("expected Match, got {other:?}"),
    }
}

/// Peel curried lambdas until the first non-lambda Core node (the body).
fn peel_to_match(mut c: Core) -> Core {
    loop {
        match c {
            Core::Lam { body, .. } => c = *body,
            other => return other,
        }
    }
}

#[test]
fn variant_names_are_not_identity() {
    // Renaming the *variants* (and the matching arms) changes neither the enum's
    // nor the function's content hash — variant names are erased (`spec/02` §F),
    // and the tags they desugar to are unchanged. (The enum *name* is a different
    // matter: M1 keys nominal references on it via `symbol_hash`, so renaming the
    // enum itself does shift dependents' hashes — true content identity is M7.)
    let a = COLOR;
    let b = "\
mod demo

enum Color {
    X,
    Y,
    Z,
}

pure fn rank(c: Color) -> i64 {
    match c {
        Color.X => 1,
        Color.Y => 2,
        Color.Z => 3,
    }
}
";
    assert_eq!(def(&lower(a), "Color").hash, def(&lower(b), "Color").hash);
    assert_eq!(def(&lower(a), "rank").hash, def(&lower(b), "rank").hash);
}

#[test]
fn enum_identity_ignores_enum_name() {
    // The enum *definition* commits to its variant structure, not its own name,
    // so two structurally-identical enums hash identically at the def level
    // (even though functions that *reference* them by name would not — see
    // `variant_names_are_not_identity`).
    let a = "mod demo\n\nenum Color {\n    Red,\n    Green,\n    Blue,\n}\n";
    let b = "mod demo\n\nenum Hue {\n    A,\n    B,\n    C,\n}\n";
    assert_eq!(def(&lower(a), "Color").hash, def(&lower(b), "Hue").hash);
}

#[test]
fn std_prelude_lowers_cross_module() {
    // `result.mv` constructs `Option.Some/None` (imported) — it lowers only when
    // the prelude is lowered together so the shared registry knows Option.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let opt = parse(&std::fs::read_to_string(root.join("std/option.mv")).unwrap()).expect("parse");
    let res = parse(&std::fs::read_to_string(root.join("std/result.mv")).unwrap()).expect("parse");
    let lowered = lower_modules(&[opt, res]).expect("lower prelude");
    assert_eq!(lowered.len(), 2);
    // `ok` in result.mv builds `Option.Some(x)` → a Ctor whose enum is Option.
    let result_mod = &lowered[1];
    let ok = result_mod
        .defs
        .iter()
        .find(|d| d.name == "ok")
        .expect("ok def");
    let body = ok.def.body.as_ref().expect("`ok` lowered to a body");
    // `Option.Some(x)` / `Option.None` are *real* constructors of the imported
    // enum: `Ctor`s with the `std.option.Option` nominal and declaration-order
    // tags (None = 0, Some = 1) — not a method-call desugar.
    let option = marv_core::symbol_hash("std.option.Option");
    let ctors = collect_ctors(body);
    assert!(
        ctors.contains(&(option, 1, 1)),
        "`Option.Some(x)` is a tag-1, one-field Ctor of std.option.Option: {ctors:?}"
    );
    assert!(
        ctors.contains(&(option, 0, 0)),
        "`Option.None` is a tag-0, nullary Ctor of std.option.Option: {ctors:?}"
    );
}

/// Every `Ctor` in a Core term, as `(nominal hash, tag, field count)`.
fn collect_ctors(c: &Core) -> Vec<(Hash, u32, usize)> {
    let mut out = Vec::new();
    fn walk(c: &Core, out: &mut Vec<(Hash, u32, usize)>) {
        match c {
            Core::Ctor { ty, tag, fields } => out.push((*ty, *tag, fields.len())),
            Core::Let { value, body } => {
                walk(value, out);
                walk(body, out);
            }
            Core::Lam { body, .. } => walk(body, out),
            Core::Match { branches, .. } => branches.iter().for_each(|b| walk(&b.body, out)),
            Core::Loop { cond, body, .. } => {
                walk(cond, out);
                walk(body, out);
            }
            _ => {}
        }
    }
    walk(c, &mut out);
    out
}

// ---- single-file lowering of *imported* enums (MARV-18) ------------------

/// Single-file `lower_module` cannot see an imported enum's declaration — only
/// lowering the module set together can (the CLI's std resolution, or
/// [`lower_modules`]). Each reference form must fail with the explicit
/// [`LowerError::UnresolvedImportedEnum`], never a misleading projection error
/// or a silently wrong method-call desugar.
fn lower_err(src: &str) -> marv_core::LowerError {
    let m = parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    lower_module(&m).expect_err("single-file lowering of an imported enum should fail")
}

fn assert_unresolved_option(err: marv_core::LowerError) {
    match err {
        marv_core::LowerError::UnresolvedImportedEnum { name, module } => {
            assert_eq!(name, "Option");
            assert_eq!(module, "std.option");
        }
        other => panic!("expected UnresolvedImportedEnum, got: {other:?}"),
    }
}

#[test]
fn imported_enum_nullary_ctor_errors_clearly() {
    // Previously fell through to the projection path (`UnresolvedProjection`).
    assert_unresolved_option(lower_err(
        "mod demo\nimport std.option (Option)\n\npure fn none() -> Option[i64] {\n    \
         Option.None\n}\n",
    ));
}

#[test]
fn imported_enum_payload_ctor_errors_clearly() {
    // Previously desugared to a method call — it lowered without error but was
    // semantically wrong (an `App`, not a `Ctor`).
    assert_unresolved_option(lower_err(
        "mod demo\nimport std.option (Option)\n\npure fn some(x: i64) -> Option[i64] {\n    \
         Option.Some(x)\n}\n",
    ));
}

#[test]
fn imported_enum_match_pattern_errors_clearly() {
    // Previously `UnknownConstructor` (true but unhelpful — the constructor is
    // declared, just not in the lowered set).
    assert_unresolved_option(lower_err(
        "mod demo\nimport std.option (Option)\n\npure fn or_zero(opt: Option[i64]) -> i64 {\n    \
         match opt {\n        Option.Some(x) => x,\n        Option.None => 0,\n    }\n}\n",
    ));
}
