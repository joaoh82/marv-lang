//! M5 acceptance gate: the WebAssembly backend agrees with the interpreter
//! oracle on the M4 corpus (run here through wasmtime), and a pure module
//! imports nothing while a capability-using module surfaces exactly the
//! capabilities it needs as host imports (`spec/01` §9).

use std::path::PathBuf;

use marv_core::ir::*;
use marv_core::{lower_module, lower_modules, symbol_hash};
use marv_interp::{Program, Value};
use marv_types::{OpSig, World, WorldBuilder};
use wasmtime::{Engine, Instance, Module, Store, Val};

type RuntimeDef = (Hash, String, Def);
type RuntimeAlias = (String, Hash);

/// Absolute path into the repository-level `tests/run/` corpus.
fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/run")
        .join(name)
}

/// Parse and lower a `.mv` file into `(module_path, defs, world)`.
fn load_source(name: &str) -> (String, Vec<(String, Def)>, World) {
    let src = std::fs::read_to_string(corpus(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let module_path = module.name.join(".");
    let lowered = if module
        .imports
        .iter()
        .any(|i| i.path.first().map(|s| s == "std").unwrap_or(false))
    {
        let std_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let mut modules = Vec::new();
        for entry in std::fs::read_dir(&std_dir).expect("read std/") {
            let path = entry.expect("std entry").path();
            if path.extension().and_then(|s| s.to_str()) == Some("mv") {
                let src = std::fs::read_to_string(&path).expect("read std module");
                modules.push(marv_syntax::parse(&src).expect("parse std module"));
            }
        }
        modules.push(module);
        lower_modules(&modules).unwrap_or_else(|e| panic!("lower {name} with std: {e}"))
    } else {
        vec![lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"))]
    };
    let world = World::from_modules(&lowered);
    let defs = lowered
        .iter()
        .find(|m| m.module.join(".") == module_path)
        .expect("lowered main module")
        .defs
        .clone()
        .into_iter()
        .map(|e| (e.name, e.def))
        .collect();
    (module_path, defs, world)
}

/// Parse/lower a `.mv` file and key all lowered modules by resolved symbol
/// names, so source-level std functions can execute through the backend corpus.
fn load_source_hashed(name: &str) -> (String, Vec<RuntimeDef>, Vec<RuntimeAlias>, World) {
    let src = std::fs::read_to_string(corpus(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
    let module = marv_syntax::parse(&src).unwrap_or_else(|e| panic!("parse {name}: {e}"));
    let module_path = module.name.join(".");
    let lowered = if module
        .imports
        .iter()
        .any(|i| i.path.first().map(|s| s == "std").unwrap_or(false))
    {
        let std_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let mut modules = Vec::new();
        for entry in std::fs::read_dir(&std_dir).expect("read std/") {
            let path = entry.expect("std entry").path();
            if path.extension().and_then(|s| s.to_str()) == Some("mv") {
                let src = std::fs::read_to_string(&path).expect("read std module");
                modules.push(marv_syntax::parse(&src).expect("parse std module"));
            }
        }
        modules.push(module);
        lower_modules(&modules).unwrap_or_else(|e| panic!("lower {name} with std: {e}"))
    } else {
        vec![lower_module(&module).unwrap_or_else(|e| panic!("lower {name}: {e}"))]
    };
    let world = World::from_modules(&lowered);
    let mut defs = Vec::new();
    let mut aliases = Vec::new();
    for lowered_module in &lowered {
        let lowered_module_path = lowered_module.module.join(".");
        for entry in &lowered_module.defs {
            let qualified = if lowered_module_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}.{}", lowered_module_path, entry.name)
            };
            let h = symbol_hash(&qualified);
            defs.push((h, qualified.clone(), entry.def.clone()));
            if lowered_module_path == module_path {
                aliases.push((qualified, h));
                aliases.push((entry.name.clone(), h));
            }
        }
    }
    (module_path, defs, aliases, world)
}

fn interp_i64(
    defs: Vec<RuntimeDef>,
    aliases: Vec<RuntimeAlias>,
    world: World,
    entry: &str,
    args: &[i64],
    file: &str,
) -> i64 {
    let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let program = Program::new_hashed(defs, aliases, world);
    let grant = if matches!(
        file,
        "list.mv"
            | "list_literals.mv"
            | "iter.mv"
            | "strings.mv"
            | "map_set.mv"
            | "bytes_utf8.mv"
            | "json.mv"
            | "json_dom.mv"
            | "app_tokenizer.mv"
            | "app_router.mv"
            | "app_invoice_summary.mv"
    ) {
        vec!["Alloc".to_string()]
    } else {
        Vec::new()
    };
    match program.run(entry, &grant, &arg_strs).expect("interp").value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected integer, got {other:?}"),
    }
}

fn interp_i64_module(
    module_path: &str,
    defs: Vec<(String, Def)>,
    world: World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let program = Program::new(module_path, defs, world);
    match program.run(entry, &[], &arg_strs).expect("interp").value {
        Value::Int(n) => n,
        other => panic!("interp {entry}: expected integer, got {other:?}"),
    }
}

/// Compile to wasm, instantiate under wasmtime (no imports), and call `entry`.
fn wasm_i64(
    module_path: &str,
    defs: &[RuntimeDef],
    aliases: &[RuntimeAlias],
    world: &World,
    entry: &str,
    args: &[i64],
) -> i64 {
    let artifact = marv_codegen_wasm::compile_hashed_reachable(
        defs,
        aliases,
        world,
        &marv_codegen_wasm::Options::default(),
        entry,
    )
    .expect("wasm compile");
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).expect("wasmtime module");
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).expect("instantiate (no imports)");
    let qualified = if module_path.is_empty() || entry.contains('.') {
        entry.to_string()
    } else {
        format!("{module_path}.{entry}")
    };
    let func = instance
        .get_func(&mut store, &qualified)
        .unwrap_or_else(|| panic!("export `{qualified}` not found"));
    let params: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
    let mut results = [Val::I64(0)];
    func.call(&mut store, &params, &mut results)
        .unwrap_or_else(|e| panic!("call {qualified}: {e}"));
    results[0].unwrap_i64()
}

fn corpus_cases() -> Vec<(&'static str, &'static str, Vec<i64>, i64)> {
    vec![
        ("arithmetic.mv", "main", vec![], 42),
        ("factorial.mv", "factorial", vec![0], 1),
        ("factorial.mv", "factorial", vec![5], 120),
        ("factorial.mv", "factorial", vec![10], 3_628_800),
        ("fib.mv", "fib", vec![0], 0),
        ("fib.mv", "fib", vec![1], 1),
        ("fib.mv", "fib", vec![10], 55),
        ("fib.mv", "fib", vec![15], 610),
        ("gcd.mv", "gcd", vec![48, 36], 12),
        ("gcd.mv", "gcd", vec![17, 5], 1),
        ("gcd.mv", "gcd", vec![100, 0], 100),
        ("clamp.mv", "clamp", vec![5, 0, 10], 5),
        ("clamp.mv", "clamp", vec![-3, 0, 10], 0),
        ("clamp.mv", "clamp", vec![99, 0, 10], 10),
        ("classify.mv", "classify", vec![5], 1),
        ("classify.mv", "classify", vec![-1], 0),
        ("classify.mv", "classify", vec![10], 0),
        ("ops.mv", "ops", vec![20, 6], 165),
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
        // Early return from inside loop bodies (MARV-58): returned paths exit the
        // enclosing function; non-returning paths keep threading carried state.
        ("loops.mv", "first_hit", vec![5, 3], 3),
        ("loops.mv", "first_hit", vec![5, 9], -1),
        ("loops.mv", "first_even_for", vec![], 4),
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
        // MARV-41: each iteration forces an aggregate box by calling a function
        // that returns a struct, then consumes it before the backedge. With the
        // old fixed bump pointer, the large case exhausted linear memory.
        ("alloc_reclaim.mv", "churn", vec![10], 100),
        ("alloc_reclaim.mv", "churn", vec![200_000], 40_000_000_000),
        // `as` casts (MARV-7): width truncation/wrapping must agree with the
        // interpreter's `eval_cast` bit-for-bit.
        ("casts.mv", "truncate_u8", vec![300], 44),
        ("casts.mv", "truncate_u8", vec![255], 255),
        ("casts.mv", "truncate_u8", vec![256], 0),
        ("casts.mv", "truncate_i8", vec![200], -56),
        ("casts.mv", "truncate_i8", vec![127], 127),
        ("casts.mv", "truncate_i8", vec![128], -128),
        ("casts.mv", "truncate_u16", vec![70000], 4464),
        ("casts.mv", "truncate_i32", vec![4_294_967_301], 5),
        ("casts.mv", "bool_cast", vec![0], 0),
        ("casts.mv", "bool_cast", vec![7], 1),
        ("casts.mv", "char_round", vec![65], 65),
        ("casts.mv", "chained", vec![300], 44),
        // prefix unary operators (MARV-23): `-e` and `not e`.
        ("unary.mv", "neg", vec![5], -5),
        ("unary.mv", "neg", vec![-3], 3),
        ("unary.mv", "abs", vec![-7], 7),
        ("unary.mv", "abs", vec![4], 4),
        ("unary.mv", "flip", vec![5], 1),
        ("unary.mv", "flip", vec![0], 0),
        // Aggregates & enums (MARV-9): boxed `[tag, fields…]` in linear memory,
        // crossing boundaries, projected, and matched. interp == cranelift == wasm.
        ("structs.mv", "manhattan", vec![3, 4], 7),
        ("structs.mv", "manhattan", vec![10, 20], 30),
        ("structs.mv", "manhattan", vec![-5, 5], 0),
        ("color.mv", "rank_of", vec![0], 1),
        ("color.mv", "rank_of", vec![1], 2),
        ("color.mv", "rank_of", vec![2], 3),
        ("shapes.mv", "circle_area", vec![5], 25),
        ("shapes.mv", "circle_area", vec![0], 0),
        ("shapes.mv", "rect_area", vec![3, 4], 12),
        ("shapes.mv", "rect_area", vec![7, 6], 42),
        // Monomorphized generics (MARV-26 / MARV-5): `max[T: Ord]` matching on the
        // `Ordering` enum, specialized to `i64` and dispatched to `impl Ord[i64]`.
        // Runnable here only since MARV-9 gave the enum a linear-memory layout.
        ("generics.mv", "max_of", vec![3, 7], 7),
        ("generics.mv", "max_of", vec![7, 3], 7),
        ("generics.mv", "max_of", vec![5, 5], 5),
        ("generics.mv", "max_of", vec![-4, -9], -4),
        // MARV-32 broadens the monomorphization corpus: a generic that calls
        // other generics, multiple concrete instantiations in one entry, and a
        // two-type-parameter generic whose bounds dispatch at i64 and i32.
        ("generics_broad.mv", "nested_clamp", vec![1, 5, 3], 3),
        ("generics_broad.mv", "nested_clamp", vec![1, -2, 3], 1),
        (
            "generics_broad.mv",
            "mixed_instantiations",
            vec![7, 3, 10, 4],
            33,
        ),
        (
            "generics_broad.mv",
            "mixed_instantiations",
            vec![-2, 5, 8, 12],
            11,
        ),
        (
            "generics_broad.mv",
            "mixed_pair_score",
            vec![1, 2, 5, 3],
            13,
        ),
        (
            "generics_broad.mv",
            "mixed_pair_score",
            vec![4, 4, -1, -1],
            22,
        ),
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
        // Growable lists: explicit Alloc construction/growth, get/set/pop, and
        // `for x in xs` over the list's len/index surface.
        ("list.mv", "exercise", vec![6], 53),
        // Explicit-allocation list literal sugar (MARV-51), lowered to
        // ListNew/ListPush and run across all backends.
        ("list_literals.mv", "exercise", vec![], 51),
        // Real Iter[T] protocol first slice (MARV-52): `for` over IndexIter
        // lowers through iter_len/iter_get instead of direct len/index.
        ("iter.mv", "exercise", vec![], 12),
        // Map/Set first slice (MARV-50) plus MARV-61 scalar-key hash-backed
        // paths: string behavior stays compatible, and i64 map/set operations
        // store explicit hashes beside their keys.
        ("map_set.mv", "exercise", vec![], 1208),
        // Strings: literal concat, slice, char access, `for c in s`, and
        // explicit-Alloc building from `List[char]`.
        ("strings.mv", "exercise", vec![], 324),
        // Bytes + UTF-8 backend-safe paths: source-level byte equality and
        // UTF-8 encoding over List[u8]. Decoding has typed error raises, so it
        // stays interpreter/check covered until result-value codegen lands.
        ("bytes_utf8.mv", "encode_multibyte", vec![], 435),
        ("bytes_utf8.mv", "compare_bytes", vec![], 3),
        // JSON first slice (MARV-55): deterministic scalar serialization with
        // explicit Alloc; parser/error paths are interpreter-smoked separately.
        ("json.mv", "exercise", vec![], 280),
        // Recursive/materialized JSON DOM (MARV-66): deterministic construction
        // and serialization of nested array/object values. Parse/error paths
        // stay interpreter/check covered until raise lowering reaches WASM.
        ("json_dom.mv", "exercise", vec![], 379),
        // MARV-40 app examples: app-shaped string/list programs with explicit
        // Alloc, pinned across interpreter, Cranelift, and WASM.
        ("app_tokenizer.mv", "main", vec![], 310),
        ("app_router.mv", "main", vec![], 512),
        ("app_invoice_summary.mv", "main", vec![], 3070),
    ]
}

#[test]
fn wasm_agrees_with_interpreter() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            for (file, entry, args, expected) in corpus_cases() {
                let (module_path, defs, aliases, world) = load_source_hashed(file);
                let interp = interp_i64(
                    defs.clone(),
                    aliases.clone(),
                    world.clone(),
                    entry,
                    &args,
                    file,
                );
                let wasm = wasm_i64(&module_path, &defs, &aliases, &world, entry, &args);
                assert_eq!(
                    interp, wasm,
                    "interp/wasm disagree on {file}:{entry}({args:?}): interp={interp}, wasm={wasm}"
                );
                assert_eq!(
                    interp, expected,
                    "{file}:{entry}({args:?}) = {interp}, expected {expected}"
                );
            }
        })
        .expect("spawn larger-stack differential test")
        .join()
        .expect("larger-stack differential test panicked");
}

/// The out-of-bounds corpus (MARV-34): `(file, entry, args)` whose runtime
/// subscript falls outside `0..len`. Debug builds must *abort* on every backend
/// — the interpreter with a structured Tier-1 report, wasm with an
/// `unreachable` trap (a host abort hook would be an import, breaking the
/// "a pure module imports nothing" manifest). The Cranelift half is asserted by
/// the twin corpus in `marv-codegen-cl/tests/differential.rs`.
fn oob_cases() -> Vec<(&'static str, &'static str, Vec<i64>)> {
    vec![
        // slice read at `len` and at a negative subscript.
        ("slices.mv", "nth", vec![4]),
        ("slices.mv", "nth", vec![-1]),
        // slice element store (`Core::IndexSet`) at `len`.
        ("slices.mv", "set_sum", vec![3]),
        // fixed-length array read at `len` (the same `Prim::Index` path).
        ("arrays.mv", "nth", vec![4]),
    ]
}

#[test]
fn out_of_bounds_traps_under_wasm_and_errors_in_the_interpreter() {
    for (file, entry, args) in oob_cases() {
        let (module_path, defs, world) = load_source(file);

        // Interpreter: a structured Tier-1 violation, never a value.
        let arg_strs: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let program = Program::new(&module_path, defs.clone(), world.clone());
        let err = program
            .run(entry, &[], &arg_strs)
            .expect_err("an out-of-bounds subscript must abort the run");
        assert!(
            matches!(err, marv_interp::RunError::BoundsCheckFailed { .. }),
            "{file}:{entry}({args:?}): expected a Tier-1 bounds violation, got {err:?}"
        );

        // wasm: the debug module traps on the emitted `unreachable`.
        let artifact = marv_codegen_wasm::compile(&module_path, &defs, &world)
            .unwrap_or_else(|e| panic!("wasm compile: {e}"));
        let engine = Engine::default();
        let module = Module::new(&engine, &artifact.bytes).expect("wasmtime module");
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate");
        let qualified = format!("{module_path}.{entry}");
        let func = instance
            .get_func(&mut store, &qualified)
            .unwrap_or_else(|| panic!("export `{qualified}` not found"));
        let params: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
        let mut results = [Val::I64(0)];
        let trapped = func.call(&mut store, &params, &mut results);
        assert!(
            trapped.is_err(),
            "{file}:{entry}({args:?}): expected a wasm trap, got {:?}",
            results[0]
        );
    }
}

/// Pin the byte-level half of the release claim: an *indexing* module's debug
/// bytes contain the check (≠ release bytes), and a module with no runtime
/// indexing compiles byte-identically in both modes — so the docs' "release
/// in-bounds codegen is byte-identical to the unchecked output" stays an
/// enforced property, not a comment. (A future edit that, say, hoists the
/// check's scratch locals outside the `bounds_checks` gate would change
/// release bytes with every result-level test still green.)
#[test]
fn release_mode_bytes_pin_the_check_presence() {
    let debug = marv_codegen_wasm::Options {
        bounds_checks: true,
    };
    let release = marv_codegen_wasm::Options {
        bounds_checks: false,
    };

    let (module_path, defs, world) = load_source("slices.mv");
    let d = marv_codegen_wasm::compile_with(&module_path, &defs, &world, &debug).unwrap();
    let r = marv_codegen_wasm::compile_with(&module_path, &defs, &world, &release).unwrap();
    assert_ne!(
        d.bytes, r.bytes,
        "an indexing module must carry the bounds check in debug mode"
    );

    let (module_path, defs, world) = load_source("factorial.mv");
    let d = marv_codegen_wasm::compile_with(&module_path, &defs, &world, &debug).unwrap();
    let r = marv_codegen_wasm::compile_with(&module_path, &defs, &world, &release).unwrap();
    assert_eq!(
        d.bytes, r.bytes,
        "a module with no runtime indexing must compile identically in both modes"
    );
}

/// Release mode (`Options { bounds_checks: false }`) omits the check: the
/// in-bounds results are unchanged and the emitted module is the pre-MARV-34
/// one (the byte-level pin above). Out-of-bounds behavior in release is
/// undefined-by-design and not pinned.
#[test]
fn release_mode_in_bounds_results_are_unchanged() {
    let opts = marv_codegen_wasm::Options {
        bounds_checks: false,
    };
    for (file, entry, args, expected) in [
        ("slices.mv", "nth", vec![3], 8),
        ("slices.mv", "set_sum", vec![1], 14),
        ("arrays.mv", "sum_all", vec![], 15),
    ] {
        let (module_path, defs, world) = load_source(file);
        let artifact = marv_codegen_wasm::compile_with(&module_path, &defs, &world, &opts)
            .unwrap_or_else(|e| panic!("wasm compile (release): {e}"));
        let engine = Engine::default();
        let module = Module::new(&engine, &artifact.bytes).expect("wasmtime module");
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).expect("instantiate");
        let qualified = format!("{module_path}.{entry}");
        let func = instance
            .get_func(&mut store, &qualified)
            .unwrap_or_else(|| panic!("export `{qualified}` not found"));
        let params: Vec<Val> = args.iter().map(|a| Val::I64(*a)).collect();
        let mut results = [Val::I64(0)];
        func.call(&mut store, &params, &mut results)
            .unwrap_or_else(|e| panic!("call {qualified}: {e}"));
        assert_eq!(
            results[0].unwrap_i64(),
            expected,
            "{file}:{entry}({args:?}) in release mode"
        );
    }
}

#[test]
fn a_pure_module_imports_nothing() {
    let (module_path, defs, world) = load_source("factorial.mv");
    let artifact = marv_codegen_wasm::compile(&module_path, &defs, &world).unwrap();
    assert!(
        artifact.imports.is_empty(),
        "a pure module must import no capabilities, got {:?}",
        artifact.imports
    );
    // And it validates / instantiates with no imports at all.
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).unwrap();
    assert!(Instance::new(&mut Store::new(&engine, ()), &module, &[]).is_ok());
}

/// A `fetch(net: Net)` that performs `Net` op 0 must surface exactly one host
/// import — `Net`. This is the capability manifest the sandbox is built on.
#[test]
fn a_capability_using_module_imports_that_capability() {
    let net = symbol_hash("Net");
    let net_ty = Type::Nominal {
        def: net,
        args: Vec::new(),
    };
    let row = EffectRow {
        caps: vec![net],
        errors: Vec::new(),
    };
    // fetch : Net -{Net}-> () ; body = \net. perform net.op0()
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(net_ty.clone()),
            ret: Box::new(Type::Unit),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: net_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: Vec::new(),
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Net",
            vec![OpSig {
                consumes_receiver: true,
                params: Vec::new(),
                ret: Type::Unit,
                errors: Vec::new(),
            }],
        )
        .build();

    let artifact =
        marv_codegen_wasm::compile("sandbox", &[("fetch".to_string(), def)], &world).unwrap();
    assert_eq!(artifact.imports.len(), 1);
    assert_eq!(artifact.imports[0].cap, "Net");
    assert_eq!(artifact.imports[0].op, 0);

    // wasmtime confirms the module declares an import named ("Net", "op0").
    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).unwrap();
    let imports: Vec<(String, String)> = module
        .imports()
        .map(|i| (i.module().to_string(), i.name().to_string()))
        .collect();
    assert_eq!(imports, vec![("Net".to_string(), "op0".to_string())]);
}

#[test]
fn capability_imports_can_return_string_handles() {
    let http = symbol_hash("Http");
    let http_ty = Type::Nominal {
        def: http,
        args: Vec::new(),
    };
    let row = EffectRow {
        caps: vec![http],
        errors: Vec::new(),
    };
    let def = Def {
        kind: DefKind::Fn,
        ty: Type::Arrow {
            param: Box::new(http_ty.clone()),
            ret: Box::new(Type::Str),
            effects: row.clone(),
        },
        requires: Vec::new(),
        ensures: Vec::new(),
        body: Some(Core::Lam {
            param: http_ty,
            effects: row,
            body: Box::new(Core::Perform {
                cap: Atom::Var(0),
                op: OpId(0),
                args: Vec::new(),
            }),
        }),
    };
    let world = WorldBuilder::new()
        .cap(
            "Http",
            vec![OpSig {
                consumes_receiver: true,
                params: Vec::new(),
                ret: Type::Str,
                errors: Vec::new(),
            }],
        )
        .build();

    let artifact =
        marv_codegen_wasm::compile("sandbox", &[("method".to_string(), def)], &world).unwrap();
    assert_eq!(artifact.imports.len(), 1);
    assert_eq!(artifact.imports[0].cap, "Http");
    assert_eq!(artifact.imports[0].op, 0);
    assert!(artifact.imports[0].returns_value);

    let engine = Engine::default();
    Module::new(&engine, &artifact.bytes).expect("string-returning import module validates");
}

#[test]
fn let_bound_capability_narrowing_imports_narrowed_ops() {
    let (_module_path, defs, aliases, world) = load_source_hashed("cap_narrow.mv");
    let artifact = marv_codegen_wasm::compile_hashed_reachable(
        &defs,
        &aliases,
        &world,
        &marv_codegen_wasm::Options::default(),
        "main",
    )
    .unwrap();

    assert_eq!(artifact.imports.len(), 2);
    assert_eq!(artifact.imports[0].cap, "Io");
    assert_eq!(artifact.imports[0].op, 5);
    assert_eq!(artifact.imports[0].params, 0);
    assert!(!artifact.imports[0].returns_value);
    assert_eq!(artifact.imports[1].cap, "Stream");
    assert_eq!(artifact.imports[1].op, 0);
    assert_eq!(artifact.imports[1].params, 1);
    assert!(!artifact.imports[1].returns_value);

    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).unwrap();
    let imports: Vec<(String, String)> = module
        .imports()
        .map(|i| (i.module().to_string(), i.name().to_string()))
        .collect();
    assert_eq!(
        imports,
        vec![
            ("Io".to_string(), "op5".to_string()),
            ("Stream".to_string(), "op0".to_string()),
        ]
    );
}

/// MARV-8: a module whose entry uses only the supported subset builds even when
/// a sibling definition uses a construct this backend cannot lower (here a
/// method call that lowers to an application of a non-function). The pruned
/// artifact exports only the entry's reachable closure; whole-module
/// compilation — the `commit`/audit path — still refuses the same module.
#[test]
fn reachability_pruned_compile_skips_unsupported_sibling() {
    let (module_path, defs, world) = load_source("pruned_sibling.mv");

    // Whole-module: the sibling blocks the build.
    let whole = marv_codegen_wasm::compile(&module_path, &defs, &world);
    assert!(
        whole.is_err(),
        "whole-module compilation must still reject the unsupported sibling"
    );

    // Pruned to the entry: builds, exports exactly the entry, runs, and agrees
    // with the interpreter oracle.
    let opts = marv_codegen_wasm::Options::default();
    let artifact =
        marv_codegen_wasm::compile_reachable(&module_path, &defs, &world, &opts, "double")
            .unwrap_or_else(|e| panic!("pruned wasm compile: {e}"));
    let exported: Vec<&str> = artifact.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        exported,
        ["pruned.double"],
        "only the reachable closure is exported"
    );

    let engine = Engine::default();
    let module = Module::new(&engine, &artifact.bytes).expect("wasmtime module");
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).expect("instantiate (no imports)");
    let func = instance
        .get_func(&mut store, "pruned.double")
        .expect("export `pruned.double` not found");
    let mut results = [Val::I64(0)];
    func.call(&mut store, &[Val::I64(21)], &mut results)
        .expect("call pruned.double");
    let got = results[0].unwrap_i64();
    let want = interp_i64_module(&module_path, defs, world, "double", &[21]);
    assert_eq!(got, 42);
    assert_eq!(got, want, "pruned build agrees with the oracle");
}
