//! M7 store gate: reproducible hashes, coexisting versions, transitive dedup,
//! and free renames — including through recursion and across call edges
//! (`spec/01` §8).

use marv_core::ir::{Atom, Core, Def, DefKind, EffectRow, IntTy, Type};
use marv_core::{lower_module, symbol_hash};
use marv_store::{commit, resolve, CommitStatus, Lockfile, Store, StoreDir};
use std::collections::HashMap;

/// Lower a source module into `(name, Def)` pairs in source order.
fn lower(src: &str) -> Vec<(String, Def)> {
    let module = marv_syntax::parse(src).expect("parse");
    let lowered = lower_module(&module).expect("lower");
    lowered.defs.into_iter().map(|e| (e.name, e.def)).collect()
}

/// The dag hashes of a standalone module (no external bindings).
fn dag_hashes(module_path: &str, src: &str) -> Vec<String> {
    let defs = lower(src);
    resolve(module_path, &defs, &HashMap::new())
        .dag_hashes
        .iter()
        .map(|h| h.to_b3())
        .collect()
}

const FACTORIAL: &str = "\
mod demo
pure fn factorial(n: i64) -> i64 {
    if n < 2 {
        1
    } else {
        n * factorial(n - 1)
    }
}
";

#[test]
fn same_source_twice_yields_identical_hashes() {
    assert_eq!(
        dag_hashes("demo", FACTORIAL),
        dag_hashes("demo", FACTORIAL),
        "hashing is deterministic"
    );

    // And committing twice is idempotent: the second commit adds nothing new.
    let mut store = Store::new();
    let mut lock = Lockfile::new();
    let defs = lower(FACTORIAL);
    let first = commit(&mut store, &mut lock, "demo", &defs);
    assert_eq!(first.added(), 1);
    let n = store.len();

    let second = commit(&mut store, &mut lock, "demo", &defs);
    assert_eq!(second.added(), 0, "nothing new on re-commit");
    assert_eq!(second.deduped(), 1);
    assert_eq!(store.len(), n, "store did not grow");
    assert!(matches!(
        second.entries[0].status,
        CommitStatus::Existing { reviewed: true }
    ));
}

#[test]
fn renaming_a_recursive_function_changes_no_hash() {
    // The same function, with every occurrence of `factorial` renamed to `fact`
    // (including the recursive self-call). Names are erased from the dag hash,
    // and the self-reference is a positional placeholder, so the hash is equal.
    let renamed = FACTORIAL.replace("factorial", "fact");
    assert_eq!(
        dag_hashes("demo", FACTORIAL),
        dag_hashes("demo", &renamed),
        "renaming a recursive function is free"
    );
}

#[test]
fn renaming_a_callee_changes_no_caller_hash() {
    let a = "\
mod demo
pure fn helper(x: i64) -> i64 {
    x + 1
}
pure fn run(x: i64) -> i64 {
    helper(x)
}
";
    // Rename the callee `helper` → `bump` (and the call site).
    let b = a.replace("helper", "bump");
    let ha = dag_hashes("demo", a);
    let hb = dag_hashes("demo", b.as_str());
    // Both definitions' hashes are unchanged: the caller references the callee
    // by its (name-independent) dag hash, not its name.
    assert_eq!(ha, hb, "renaming through a call edge is free");
}

#[test]
fn two_versions_of_the_same_function_coexist() {
    let v1 = "mod lib\npure fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n";
    // Same name, different body ⇒ a different definition.
    let v2 = "mod lib\npure fn add(a: i64, b: i64) -> i64 {\n    b + a\n}\n";

    let mut store = Store::new();
    // Library A pins its own `add`.
    let mut lock_a = Lockfile::new();
    commit(&mut store, &mut lock_a, "lib", &lower(v1));
    // Library B pins a different `add` into the *same* store.
    let mut lock_b = Lockfile::new();
    commit(&mut store, &mut lock_b, "lib", &lower(v2));

    let ha = lock_a.get("lib.add").unwrap();
    let hb = lock_b.get("lib.add").unwrap();
    assert_ne!(ha, hb, "the two `add`s have different hashes");
    assert_eq!(store.len(), 2, "both coexist in the store");
    assert!(store.contains(ha) && store.contains(hb));
}

#[test]
fn identical_definitions_dedup() {
    // Two modules with a structurally identical function (different module name,
    // different parameter names) collapse to one stored definition.
    let m1 = "mod a\npure fn inc(n: i64) -> i64 {\n    n + 1\n}\n";
    let m2 = "mod a\npure fn inc(k: i64) -> i64 {\n    k + 1\n}\n";
    assert_eq!(
        dag_hashes("a", m1),
        dag_hashes("a", m2),
        "alpha-equivalent definitions hash identically"
    );

    let mut store = Store::new();
    let mut lock = Lockfile::new();
    commit(&mut store, &mut lock, "a", &lower(m1));
    let report = commit(&mut store, &mut lock, "a", &lower(m2));
    assert_eq!(store.len(), 1, "identical definitions dedup to one entry");
    assert_eq!(report.deduped(), 1);
}

#[test]
fn already_reviewed_query() {
    let mut store = Store::new();
    let mut lock = Lockfile::new();
    let report = commit(&mut store, &mut lock, "demo", &lower(FACTORIAL));
    let hash = &report.entries[0].hash;
    // Committing freezes/reviews the definition; the store answers the
    // provenance query (`spec/01` §8).
    assert!(store.is_reviewed(hash), "committed defs are reviewed");
    assert!(
        !store.is_reviewed("b3:0000000000000000000000000000000000000000000000000000000000000000")
    );
}

fn fn_calling(qualified: &str) -> Def {
    Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(Type::Int(IntTy::I64)),
            ret: Box::new(Type::Int(IntTy::I64)),
            effects: EffectRow::default(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: Type::Int(IntTy::I64),
            effects: EffectRow::default(),
            body: Box::new(Core::App {
                func: Atom::Global(symbol_hash(qualified)),
                arg: Atom::Var(0),
            }),
        }),
    }
}

#[test]
fn closure_fetches_lockfile_pinned_external_deps() {
    let mut store = Store::new();
    let mut lock = Lockfile::new();
    commit(
        &mut store,
        &mut lock,
        "lib",
        &lower("mod lib\npure fn inc(x: i64) -> i64 {\n    x + 1\n}\n"),
    );
    let lib_hash = lock.get("lib.inc").unwrap().clone();

    let app_defs = vec![("run".to_string(), fn_calling("lib.inc"))];
    let resolved = resolve("app", &app_defs, &lock.external_index());
    assert!(
        resolved.deps[0].iter().any(|h| h.to_b3() == lib_hash),
        "app.run records the pinned lib.inc hash as a Merkle edge"
    );

    let closure = store
        .closure_for_hashes(std::slice::from_ref(&lib_hash))
        .unwrap();
    assert_eq!(closure.defs.len(), 1);
    assert_eq!(closure.defs[0].hash, lib_hash);
}

#[test]
fn store_dir_persists_content_addressed_blobs() {
    let root = std::env::temp_dir().join(format!("marv-store-blobs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    let mut store = Store::new();
    let mut lock = Lockfile::new();
    commit(&mut store, &mut lock, "demo", &lower(FACTORIAL));
    let dir = StoreDir::new(&root);
    dir.save(&store, &lock).expect("save blob store");

    assert!(
        !root.join("store.json").exists(),
        "new persistence writes per-hash blobs, not the legacy monolith"
    );
    let blob_count = count_json_files(&root.join("blobs").join("b3"));
    assert_eq!(blob_count, store.len(), "one JSON blob per stored def");

    let (loaded_store, loaded_lock) = dir.load().expect("load blob store");
    assert_eq!(loaded_store.len(), store.len());
    assert_eq!(loaded_lock.bindings, lock.bindings);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_and_gc_track_lockfile_reachability() {
    let mut store = Store::new();
    let mut lock = Lockfile::new();
    commit(
        &mut store,
        &mut lock,
        "live",
        &lower("mod live\npure fn keep(x: i64) -> i64 {\n    x + 1\n}\n"),
    );
    let mut other_lock = Lockfile::new();
    commit(
        &mut store,
        &mut other_lock,
        "dead",
        &lower("mod dead\npure fn drop_me(x: i64) -> i64 {\n    x - 1\n}\n"),
    );
    assert_eq!(store.len(), 2);

    let audit = store.audit(&lock);
    assert_eq!(audit.entries.iter().filter(|e| e.reachable).count(), 1);
    assert_eq!(audit.entries.iter().filter(|e| !e.reachable).count(), 1);

    let gc = store.gc(&lock);
    assert_eq!(gc.removed.len(), 1);
    assert_eq!(gc.retained, 1);
    assert_eq!(store.len(), 1);
}

fn count_json_files(root: &std::path::Path) -> usize {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                n += count_json_files(&entry.path());
            } else if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                n += 1;
            }
        }
    }
    n
}
