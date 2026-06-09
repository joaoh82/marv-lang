//! M4 acceptance gate: the interpreter (`marv-interp`) and the Cranelift backend
//! (`marv-codegen-cl`) must agree on a corpus of small marv programs — and a
//! program that uses a capability absent from its effect row must fail to
//! compile (`spec/01` §9, the milestone gate).
//!
//! Each source case is loaded the way the CLI loads it (parse → lower → build a
//! [`World`]), then run through both backends; the two results must be equal to
//! each other *and* to a hand-computed golden value. The capability case is a
//! Core-IR snapshot (`tests/run/uses_ungranted_cap.core.json`); the real M2
//! checker must reject it, so it can never reach codegen.

use std::path::PathBuf;

use marv_core::ir::Def;
use marv_core::lower_module;
use marv_db::CoreModuleSpec;
use marv_interp::{Program, Value};
use marv_types::{check_def, Code, Severity, World};

/// Absolute path to a file in the repository-level `tests/run/` corpus.
fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/run")
        .join(name)
}

/// Parse and lower a `.mv` file into the triple the backends consume.
fn load_source(name: &str) -> (String, Vec<(String, Def)>, World) {
    let path = corpus(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let lowered = lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"));
    let world = World::from_module(&lowered);
    let module_path = module.name.join(".");
    let defs = lowered.defs.into_iter().map(|e| (e.name, e.def)).collect();
    (module_path, defs, world)
}

/// Interpret `entry(args)` and extract its integer result (the oracle).
fn interp_i64(
    module_path: &str,
    defs: Vec<(String, Def)>,
    world: World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let program = Program::new(module_path, defs, world);
    let outcome = program
        .run(entry, &[], &arg_strs)
        .unwrap_or_else(|e| panic!("interp {entry}: {e}"));
    match outcome.value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected an integer, got {other:?}"),
    }
}

/// JIT-compile the module and call `entry(args)` natively.
fn cranelift_i64(
    module_path: &str,
    defs: &[(String, Def)],
    world: &World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let jit = marv_codegen_cl::compile(module_path, defs, world)
        .unwrap_or_else(|e| panic!("cranelift compile: {e}"));
    jit.run_i64(entry, args)
        .unwrap_or_else(|e| panic!("cranelift run {entry}: {e}"))
}

/// The differential corpus: `(file, entry, args, expected)`. Every entry returns
/// an integer and uses only the integer/boolean Core both backends support.
fn corpus_cases() -> Vec<(&'static str, &'static str, Vec<i64>, i64)> {
    vec![
        // nullary entry + curried cross-function calls
        ("arithmetic.mv", "main", vec![], 42),
        // recursion + a single `if`
        ("factorial.mv", "factorial", vec![0], 1),
        ("factorial.mv", "factorial", vec![5], 120),
        ("factorial.mv", "factorial", vec![10], 3_628_800),
        // recursion with two self-calls
        ("fib.mv", "fib", vec![0], 0),
        ("fib.mv", "fib", vec![1], 1),
        ("fib.mv", "fib", vec![10], 55),
        ("fib.mv", "fib", vec![15], 610),
        // mutual-tail recursion via remainder
        ("gcd.mv", "gcd", vec![48, 36], 12),
        ("gcd.mv", "gcd", vec![17, 5], 1),
        ("gcd.mv", "gcd", vec![100, 0], 100),
        // nested `if`/`else if`/`else`
        ("clamp.mv", "clamp", vec![5, 0, 10], 5),
        ("clamp.mv", "clamp", vec![-3, 0, 10], 0),
        ("clamp.mv", "clamp", vec![99, 0, 10], 10),
        // boolean `and`, comparisons
        ("classify.mv", "classify", vec![5], 1),
        ("classify.mv", "classify", vec![-1], 0),
        ("classify.mv", "classify", vec![10], 0),
        // every arithmetic prim + comparisons in one body
        ("ops.mv", "ops", vec![20, 6], 165),
        // a < b, so the else arm: sum - diff = (3+8) - (3-8) = 11 - (-5) = 16
        ("ops.mv", "ops", vec![3, 8], 16),
        // `while` loops carrying two `var`s (sum, i) → `Core::Loop` (MARV-2)
        ("loops.mv", "sum_to", vec![0], 0),
        ("loops.mv", "sum_to", vec![1], 1),
        ("loops.mv", "sum_to", vec![5], 15),
        ("loops.mv", "sum_to", vec![10], 55),
        ("loops.mv", "sum_to", vec![100], 5050),
        ("loops.mv", "pow", vec![2, 10], 1024),
        ("loops.mv", "pow", vec![3, 4], 81),
        ("loops.mv", "pow", vec![5, 0], 1),
        // single-carried-variable loop with no invariant (the `k == 1` path)
        ("loops.mv", "count_down", vec![7], 0),
        ("loops.mv", "count_down", vec![0], 0),
        // Branch-join loop bodies (MARV-21): a loop body whose tail is an
        // `if`/`else if`/`match` threads the carried `var`s through the branch
        // join. Each branch yields the next-state tuple, kept in registers/locals
        // (never boxed) so loops stay alloc-free. interp == cranelift == wasm.
        // `if`/`else`.
        ("loops.mv", "weighted", vec![5], 23),
        ("loops.mv", "weighted", vec![0], 0),
        // `if` with no `else` (pass-through on the false branch).
        ("loops.mv", "count_high", vec![5], 2),
        ("loops.mv", "count_high", vec![2], 0),
        // `else if` chain.
        ("loops.mv", "bucket", vec![6], 123),
        ("loops.mv", "bucket", vec![3], 3),
        // `match` tail over an enum, arms reassigning the carried `acc`.
        ("loops.mv", "parity_score", vec![5], 32),
        ("loops.mv", "parity_score", vec![4], 22),
        // Regression: an outer carried `x` reassigned only in the `then` branch,
        // with a body-local `let x` shadow in the `else` branch — the carried `x`
        // is threaded through its lineage, not hijacked by the shadow.
        ("loops.mv", "shadow", vec![6], 30),
        ("loops.mv", "shadow", vec![2], 0),
        // A branch join nested inside a loop: `acc` is carried by the inner loop
        // and its lineage propagates out through the inner loop's final-state
        // projection (the `carried`-flag lineage under nesting).
        ("loops.mv", "nested_weighted", vec![4], 9),
        ("loops.mv", "nested_weighted", vec![3], 5),
        // `as` casts (MARV-7): integer width truncation/wrapping must agree
        // bit-for-bit with the interpreter's `eval_cast`.
        ("casts.mv", "truncate_u8", vec![300], 44),
        ("casts.mv", "truncate_u8", vec![255], 255),
        ("casts.mv", "truncate_u8", vec![256], 0),
        ("casts.mv", "truncate_i8", vec![200], -56),
        ("casts.mv", "truncate_i8", vec![127], 127),
        ("casts.mv", "truncate_i8", vec![128], -128),
        ("casts.mv", "truncate_u16", vec![70000], 4464),
        ("casts.mv", "truncate_i32", vec![4_294_967_301], 5),
        // `bool` cast: nonzero → true → 1.
        ("casts.mv", "bool_cast", vec![0], 0),
        ("casts.mv", "bool_cast", vec![7], 1),
        // `char` shares the integer (code-point) representation.
        ("casts.mv", "char_round", vec![65], 65),
        // chained casts narrow then widen.
        ("casts.mv", "chained", vec![300], 44),
        // prefix unary operators (MARV-23): `-e` and `not e` must agree across
        // both backends and the interpreter.
        ("unary.mv", "neg", vec![5], -5),
        ("unary.mv", "neg", vec![-3], 3),
        ("unary.mv", "abs", vec![-7], 7),
        ("unary.mv", "abs", vec![4], 4),
        ("unary.mv", "flip", vec![5], 1),
        ("unary.mv", "flip", vec![0], 0),
        // Aggregates & enums (MARV-9): heap-boxed `[tag, fields…]` crossing
        // function boundaries, projected, and matched. interp == cranelift == wasm.
        // struct `Ctor`/`Proj` + a struct returned from and passed to a function.
        ("structs.mv", "manhattan", vec![3, 4], 7),
        ("structs.mv", "manhattan", vec![10, 20], 30),
        ("structs.mv", "manhattan", vec![-5, 5], 0),
        // n-way `enum` `Match` (jump table on tag) over a boxed enum built behind
        // a call and through an `if`/`else`.
        ("color.mv", "rank_of", vec![0], 1),
        ("color.mv", "rank_of", vec![1], 2),
        ("color.mv", "rank_of", vec![2], 3),
        // payload-carrying variants + `Match` arms that bind fields (binds > 0).
        ("shapes.mv", "circle_area", vec![5], 25),
        ("shapes.mv", "circle_area", vec![0], 0),
        ("shapes.mv", "rect_area", vec![3, 4], 12),
        ("shapes.mv", "rect_area", vec![7, 6], 42),
        // Monomorphized generics (MARV-26 / MARV-5): a `max[T: Ord]` whose body
        // matches on the `Ordering` enum, specialized to `i64` and dispatched to
        // the coherent `impl Ord[i64]`. Now runnable on all three backends because
        // MARV-9 gave the enum a runtime layout. interp == cranelift == wasm.
        ("generics.mv", "max_of", vec![3, 7], 7),
        ("generics.mv", "max_of", vec![7, 3], 7),
        ("generics.mv", "max_of", vec![5, 5], 5),
        ("generics.mv", "max_of", vec![-4, -9], -4),
        // Arrays (MARV-30): array literals box to `[len, e0, …]`; `len` reads the
        // header word and `index` loads `[i + 1]`; an index *store* is a functional
        // element update (unrolled over the static length). interp == cranelift == wasm.
        // literal + indexed reads + arithmetic.
        ("arrays.mv", "sum3", vec![], 42),
        // index with a runtime subscript.
        ("arrays.mv", "nth", vec![0], 5),
        ("arrays.mv", "nth", vec![3], 8),
        // `len` over an array (the header word).
        ("arrays.mv", "length", vec![], 4),
        // `len` + index driving a `while` loop.
        ("arrays.mv", "sum_all", vec![], 15),
        // index store `a[i] = e` with a constant subscript, then read back.
        ("arrays.mv", "set_get", vec![], 42),
        // index store with a runtime subscript, then sum every element.
        ("arrays.mv", "set_sum", vec![0], 15),
        ("arrays.mv", "set_sum", vec![1], 14),
        ("arrays.mv", "set_sum", vec![2], 13),
        // `for x in a` over an array (desugared len/index loop).
        ("arrays.mv", "sum_for", vec![], 20),
        // Runtime-length slices (MARV-33): a slice shares the array's `[len, e0, …]`
        // layout but with a length known only at run time. `len`/`index` reads fall
        // out of that layout; the element *store* goes through `Core::IndexSet` —
        // an allocate-copy-store over the runtime length, not the array's static
        // unroll. interp == cranelift == wasm.
        // literal + indexed reads.
        ("slices.mv", "sum3", vec![], 42),
        // index with a runtime subscript.
        ("slices.mv", "nth", vec![0], 5),
        ("slices.mv", "nth", vec![3], 8),
        // `len` over a slice (the header word).
        ("slices.mv", "length", vec![], 4),
        // `len` + index driving a `while` loop.
        ("slices.mv", "sum_all", vec![], 15),
        // runtime-length element store with a constant subscript, then read back.
        ("slices.mv", "set_get", vec![], 42),
        // runtime-length element store with a runtime subscript, then sum back.
        ("slices.mv", "set_sum", vec![0], 15),
        ("slices.mv", "set_sum", vec![1], 14),
        ("slices.mv", "set_sum", vec![2], 13),
        // `examples/report.mv`'s `total`: a `while` over `len(sales)` reading
        // `sales[i].amount` from a slice of structs (MARV-33 + MARV-20 slice half).
        ("slices.mv", "total", vec![], 42),
        // `for x in s` over a runtime-length slice (MARV-20): the desugared
        // len/index loop drives a collection whose length is a runtime value.
        ("slices.mv", "sum_for", vec![], 20),
        // `for` over a slice of structs (`examples/report.mv`'s `total` shape).
        ("slices.mv", "total_for", vec![], 42),
        // nested `for`s: builder-depth-keyed index names stay unique.
        ("slices.mv", "nested_for", vec![], 180),
        // two sequential `for`s share one depth-keyed index name; the second
        // shadows the first without clobbering it.
        ("slices.mv", "rescan_for", vec![], 66),
    ]
}

#[test]
fn interpreter_and_cranelift_agree() {
    for (file, entry, args, expected) in corpus_cases() {
        let (module_path, defs, world) = load_source(file);
        let interp = interp_i64(&module_path, defs.clone(), world.clone(), entry, &args);
        let native = cranelift_i64(&module_path, &defs, &world, entry, &args);

        assert_eq!(
            interp, native,
            "backends disagree on {file}:{entry}({args:?}): interp={interp}, cranelift={native}"
        );
        assert_eq!(
            interp, expected,
            "{file}:{entry}({args:?}) produced {interp}, expected {expected}"
        );
    }
}

/// The gate's negative case: a function that `perform`s `Fs` while declaring the
/// empty (`pure`) effect row must be rejected by the checker — so it can never
/// be handed to either backend.
#[test]
fn capability_outside_effect_row_fails_to_compile() {
    let path = corpus("uses_ungranted_cap.core.json");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let spec: CoreModuleSpec = serde_json::from_str(&src).expect("fixture is valid Core JSON");
    let world = spec.world.build();

    let mut diags = Vec::new();
    for d in &spec.defs {
        diags.extend(check_def(&world, &d.def, Some(&d.name)));
    }

    let missing_cap = diags
        .iter()
        .find(|d| d.code == Code::MissingCapability)
        .expect("checker must report the missing capability");
    assert_eq!(missing_cap.severity, Severity::Error);
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error),
        "the snapshot must fail to compile"
    );
}
