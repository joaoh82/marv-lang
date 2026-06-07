//! Interface-bound checking, coherence, and `marv/resolveImpl`
//! (`spec/01-design-spec.md` §§3.3–3.4): a satisfied bound checks clean, an
//! unsatisfied one reports `E0160`, two impls for one type report `E0161`, and
//! impl resolution names the selected definition.

use marv_core::{lower_module, LoweredModule};
use marv_types::{check_bounds, resolve_impls, Code};

fn lower(src: &str) -> LoweredModule {
    let m = marv_syntax::parse(src).expect("parse");
    lower_module(&m).expect("lower")
}

fn codes(m: &LoweredModule) -> Vec<Code> {
    check_bounds(std::slice::from_ref(m))
        .into_iter()
        .map(|(_, d)| d.code)
        .collect()
}

const PRELUDE: &str = "\
enum Ordering {
    Lt,
    Eq,
    Gt,
}

interface Ord[T] {
    fn cmp(a: T, b: T) -> Ordering
}

fn max[T: Ord](a: T, b: T) -> T {
    match cmp(a, b) {
        Ordering.Lt => b,
        Ordering.Eq => a,
        Ordering.Gt => a,
    }
}
";

fn with(header: &str, body: &str) -> String {
    format!("mod demo\n\n{PRELUDE}\n{body}\n{header}")
}

#[test]
fn satisfied_bound_checks_clean() {
    let src = with(
        "fn main() -> i32 {\n    max(3, 7)\n}\n",
        "impl Ord[i32] {\n    fn cmp(a: i32, b: i32) -> Ordering {\n        Ordering.Lt\n    }\n}\n",
    );
    let m = lower(&src);
    assert!(
        codes(&m).is_empty(),
        "a satisfied bound should produce no diagnostics, got {:?}",
        codes(&m)
    );
}

#[test]
fn unsatisfied_bound_is_reported() {
    // No `impl Ord[bool]`, but `main` calls `max(true, false)`.
    let src = with("fn main() -> bool {\n    max(true, false)\n}\n", "");
    let m = lower(&src);
    assert!(
        codes(&m).contains(&Code::UnsatisfiedBound),
        "expected E0160, got {:?}",
        codes(&m)
    );
}

#[test]
fn conflicting_impls_are_reported() {
    let two_impls = "\
impl Ord[i32] {
    fn cmp(a: i32, b: i32) -> Ordering {
        Ordering.Lt
    }
}

impl Ord[i32] {
    fn cmp(a: i32, b: i32) -> Ordering {
        Ordering.Gt
    }
}
";
    let src = with("fn main() -> i32 {\n    7\n}\n", two_impls);
    let m = lower(&src);
    assert!(
        codes(&m).contains(&Code::ConflictingImpl),
        "expected E0161, got {:?}",
        codes(&m)
    );
}

#[test]
fn resolve_impls_names_the_selected_definition() {
    let src = with(
        "fn main() -> i32 {\n    max(3, 7)\n}\n",
        "impl Ord[i32] {\n    fn cmp(a: i32, b: i32) -> Ordering {\n        Ordering.Lt\n    }\n}\n",
    );
    let m = lower(&src);
    let resolutions = resolve_impls(std::slice::from_ref(&m));
    let max_i32 = resolutions
        .iter()
        .find(|r| r.instance == "demo.max@i32")
        .expect("max@i32 resolution");
    assert_eq!(max_i32.generic, "max");
    let sel = &max_i32.selections[0];
    assert_eq!(sel.interface, "Ord");
    assert_eq!(sel.type_key, "i32");
    assert!(sel
        .methods
        .iter()
        .any(|(meth, def)| meth == "cmp" && def == "demo.cmp$Ord$i32"));
}
