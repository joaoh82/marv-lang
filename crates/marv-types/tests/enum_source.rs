//! End-to-end checker coverage for enums + `match` driven from real `.mv`
//! source (parse → lower → [`World::from_module`] → [`check_def`]). The other
//! checker tests build enums by hand through [`WorldBuilder`]; these prove the
//! same rules now fire from source, which is the point of MARV-1.

use marv_core::lower_module;
use marv_syntax::parse;
use marv_types::{check_def, Code, Severity, World};

/// Lower a module and return every (def-name, diagnostic) the checker emits.
fn diagnostics(src: &str) -> Vec<(String, marv_types::Diagnostic)> {
    let module = parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    let world = World::from_module(&lowered);
    let mut out = Vec::new();
    for entry in &lowered.defs {
        for d in check_def(&world, &entry.def, Some(&entry.name)) {
            out.push((entry.name.clone(), d));
        }
    }
    out
}

const EXHAUSTIVE: &str = "\
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
fn exhaustive_match_checks_clean() {
    let errs: Vec<_> = diagnostics(EXHAUSTIVE)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected a clean check, got: {errs:?}");
}

#[test]
fn missing_arm_fires_exhaustiveness() {
    let src = "\
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
    }
}
";
    let ds = diagnostics(src);
    let hit = ds
        .iter()
        .find(|(_, d)| d.code == Code::NonExhaustiveMatch)
        .unwrap_or_else(|| panic!("expected NonExhaustiveMatch, got: {ds:?}"));
    assert!(
        hit.1.message.contains("Blue"),
        "diagnostic should name the missing variant: {}",
        hit.1.message
    );
    // A mechanical fix is attached (`spec/03` §2).
    assert!(
        !hit.1.fixes.is_empty(),
        "exhaustiveness fix should be offered"
    );
}

#[test]
fn wildcard_arm_makes_match_exhaustive() {
    let src = "\
mod demo

enum Color {
    Red,
    Green,
    Blue,
}

pure fn is_red(c: Color) -> bool {
    match c {
        Color.Red => true,
        _ => false,
    }
}
";
    let errs: Vec<_> = diagnostics(src)
        .into_iter()
        .filter(|(_, d)| d.severity == Severity::Error)
        .collect();
    assert!(
        errs.is_empty(),
        "`_` arm should satisfy exhaustiveness: {errs:?}"
    );
}
