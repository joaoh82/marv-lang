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
    assert!(ok.def.body.is_some(), "`ok` lowered to a body");
}
