//! M3 incrementality gate: an edit recomputes only its dependents.
//!
//! This is the defining property of the salsa-backed query engine (`spec/03`
//! §1). [`marv_db::ANALYZE_RUNS`] counts real query executions, so we can assert
//! it directly: re-asking with no edit is free; editing file A re-runs A's
//! analysis but not B's.
//!
//! Kept as the *only* test in this binary because `ANALYZE_RUNS` is a process-
//! global counter — a sibling test calling `analyze` in parallel would race it.

use std::sync::atomic::Ordering;

use marv_db::{analyze, MarvDatabase, SourceFile, SourceKind, ANALYZE_RUNS};
use salsa::Setter;

fn runs() -> u64 {
    ANALYZE_RUNS.load(Ordering::Relaxed)
}

#[test]
fn edit_recomputes_only_dependents() {
    let mut db = MarvDatabase::default();

    let a = SourceFile::new(
        &db,
        "a.mv".to_string(),
        SourceKind::Source,
        "mod a\n\npure fn f(x: i32) -> i32 {\n    (x + 1)\n}\n".to_string(),
    );
    let b = SourceFile::new(
        &db,
        "b.mv".to_string(),
        SourceKind::Source,
        "mod b\n\npure fn g(y: i32) -> i32 {\n    (y + 2)\n}\n".to_string(),
    );

    // First demand of each: each executes exactly once.
    let before = runs();
    let _ = analyze(&db, a);
    let _ = analyze(&db, b);
    assert_eq!(runs() - before, 2, "first analysis of A and B runs both");

    // Re-asking with no edit is fully memoized — zero executions.
    let before = runs();
    let _ = analyze(&db, a);
    let _ = analyze(&db, b);
    assert_eq!(runs() - before, 0, "unchanged inputs are cached");

    // Edit A only. A's query is invalidated; B's is not.
    a.set_text(&mut db)
        .to("mod a\n\npure fn f(x: i32) -> i32 {\n    (x + 100)\n}\n".to_string());

    let before = runs();
    let ra = analyze(&db, a);
    let rb = analyze(&db, b);
    assert_eq!(
        runs() - before,
        1,
        "editing A recomputes only A, B stays memoized"
    );

    // And the recomputed analysis reflects the edit.
    assert!(ra.parse_error.is_none());
    assert_eq!(ra.defs.len(), 1);
    assert_eq!(ra.defs[0].qualified, "a.f");
    assert_eq!(rb.defs[0].qualified, "b.g");
}
